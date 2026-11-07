//! Deriving tags from a photo or video's own metadata: capture date and GPS location. Uses
//! `nom-exif`, which reads JPEG/HEIC/TIFF/PNG stills and MOV/MP4 video through one API. The
//! derived tags attach to *content* (see [`crate::tags`]), so they follow the file across copies.
//! See docs/TAGGING.md §4.

use std::path::Path;

use anyhow::Result;
use rusqlite::Connection;

use crate::tags::{self, TARGET_CONTENT};

/// What a file's own metadata told us.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct PhotoMeta {
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub date: Option<String>, // YYYY-MM-DD
    pub year: Option<String>, // YYYY
}

impl PhotoMeta {
    pub fn is_empty(&self) -> bool {
        self.lat.is_none() && self.lon.is_none() && self.date.is_none()
    }
}

/// Read capture date and GPS from a photo or video. Never fails loudly: a file without usable
/// metadata, or one `nom-exif` can't parse, yields an empty [`PhotoMeta`].
pub fn read_meta(path: &Path) -> PhotoMeta {
    use nom_exif::{ExifTag, Metadata, TrackInfoTag, read_metadata};
    let mut m = PhotoMeta::default();
    let Ok(meta) = read_metadata(path) else {
        return m;
    };
    match meta {
        Metadata::Exif(exif) => {
            if let Some(g) = exif.gps_info() {
                m.lat = g.latitude_decimal();
                m.lon = g.longitude_decimal();
            }
            if let Some(v) = exif.get(ExifTag::DateTimeOriginal) {
                set_date(&mut m, &v.to_string());
            }
        }
        Metadata::Track(track) => {
            if let Some(v) = track.get(TrackInfoTag::GpsIso6709)
                && let Some((la, lo)) = parse_iso6709(&v.to_string())
            {
                m.lat = Some(la);
                m.lon = Some(lo);
            }
            if let Some(v) = track.get(TrackInfoTag::CreateDate) {
                set_date(&mut m, &v.to_string());
            }
        }
    }
    m
}

/// Pull YYYY-MM-DD and YYYY out of an EXIF/QuickTime datetime rendering: naive
/// `2019-07-09 20:36:33` or RFC 3339 `2019-07-09T20:36:33+08:00` — both carry the date in the
/// first ten characters.
fn set_date(m: &mut PhotoMeta, s: &str) {
    let b = s.as_bytes();
    if b.len() >= 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b[..10].iter().all(|&c| c == b'-' || c.is_ascii_digit())
    {
        m.date = Some(s[..10].to_string());
        m.year = Some(s[..4].to_string());
    }
}

/// Parse an ISO 6709 point such as `+27.1281+100.2508+000.000/` into (latitude, longitude).
fn parse_iso6709(s: &str) -> Option<(f64, f64)> {
    let s = s.trim().trim_end_matches('/');
    let b = s.as_bytes();
    if b.is_empty() || (b[0] != b'+' && b[0] != b'-') {
        return None;
    }
    // longitude starts at the next sign after index 0
    let mut i = 1;
    while i < b.len() && b[i] != b'+' && b[i] != b'-' {
        i += 1;
    }
    if i >= b.len() {
        return None;
    }
    let lat: f64 = s[..i].parse().ok()?;
    // longitude runs to the next sign (the optional altitude) or the end
    let rest = &s[i..];
    let rb = rest.as_bytes();
    let mut j = 1;
    while j < rb.len() && rb[j] != b'+' && rb[j] != b'-' {
        j += 1;
    }
    let lon: f64 = rest[..j].parse().ok()?;
    Some((lat, lon))
}

/// Attach the derived tags to a piece of content (`sha256_hex`): a `year` tag, a `date` tag under
/// it, and a `location` tag carrying the coordinates. Returns the labels applied.
pub fn derive_into(
    conn: &Connection,
    sha256_hex: &str,
    meta: &PhotoMeta,
    source: &str,
) -> Result<Vec<String>> {
    let mut applied = Vec::new();
    if let Some(y) = &meta.year {
        let yid = tags::upsert_tag(conn, "year", y, None, None)?;
        tags::attach(conn, TARGET_CONTENT, sha256_hex, yid, source, 1.0)?;
        applied.push(format!("year={y}"));
        if let Some(d) = &meta.date {
            let did = tags::upsert_tag(conn, "date", d, Some(yid), None)?;
            tags::attach(conn, TARGET_CONTENT, sha256_hex, did, source, 1.0)?;
            applied.push(format!("date={d}"));
        }
    }
    if let (Some(la), Some(lo)) = (meta.lat, meta.lon) {
        let val = format!("{la:.6},{lo:.6}");
        let data = format!("{{\"lat\":{la},\"lon\":{lo}}}");
        let lid = tags::upsert_tag(conn, "location", &val, None, Some(&data))?;
        tags::attach(conn, TARGET_CONTENT, sha256_hex, lid, source, 1.0)?;
        applied.push(format!("location={val}"));
    }
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso6709_parsing() {
        assert_eq!(
            parse_iso6709("+27.1281+100.2508+000.000/"),
            Some((27.1281, 100.2508))
        );
        assert_eq!(
            parse_iso6709("+27.2939+112.6932/"),
            Some((27.2939, 112.6932))
        );
        assert_eq!(
            parse_iso6709("-33.8688+151.2093/"),
            Some((-33.8688, 151.2093))
        );
        assert_eq!(parse_iso6709("garbage"), None);
        assert_eq!(parse_iso6709(""), None);
    }

    #[test]
    fn date_extraction() {
        let mut m = PhotoMeta::default();
        set_date(&mut m, "2019-07-09 20:36:33");
        assert_eq!(m.date.as_deref(), Some("2019-07-09"));
        assert_eq!(m.year.as_deref(), Some("2019"));

        let mut m2 = PhotoMeta::default();
        set_date(&mut m2, "2024-02-03T07:05:38+08:00");
        assert_eq!(m2.date.as_deref(), Some("2024-02-03"));
        assert_eq!(m2.year.as_deref(), Some("2024"));

        let mut m3 = PhotoMeta::default();
        set_date(&mut m3, "not a date");
        assert!(m3.date.is_none() && m3.year.is_none());
    }

    #[test]
    fn derive_applies_tags() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        c.execute_batch(tags::SCHEMA).unwrap();
        let meta = PhotoMeta {
            lat: Some(22.28),
            lon: Some(114.15),
            date: Some("2019-07-09".into()),
            year: Some("2019".into()),
        };
        let applied = derive_into(&c, "abc123", &meta, "exif").unwrap();
        assert_eq!(applied.len(), 3); // year, date, location
        let got = tags::tags_of(&c, TARGET_CONTENT, "abc123").unwrap();
        let kinds: Vec<_> = got.iter().map(|t| t.tag.kind.as_str()).collect();
        assert!(kinds.contains(&"year"));
        assert!(kinds.contains(&"date"));
        assert!(kinds.contains(&"location"));
        // the date tag hangs under its year
        let date = got.iter().find(|t| t.tag.kind == "date").unwrap();
        let year = got.iter().find(|t| t.tag.kind == "year").unwrap();
        assert_eq!(date.tag.parent_id, Some(year.tag.id));
    }
}
