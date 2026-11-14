//! Finding content by its tags and where it was taken: the query side of [`crate::tags`].
//!
//! A search is any mix of a centre and a radius ("within 120 km of HKG"), tags that must all be
//! there (country = Hong Kong, event = …, a tag's id from a facet), and a date range. It answers
//! with the matching content, newest or nearest first, each with its copies on the indexed drives;
//! the tag counts across every match (facets, for narrowing); and where the located matches are
//! (for a map). A tag stands for the tags under it too, so an event that has days under it finds
//! the photos tagged with any of them.
//!
//! Also the reading side shared by `backdown tag exif` and the daemon's `places` job: the photos
//! and videos the index knows under a folder, and reading one (hashing it first if `hash` never
//! did) into date, location and place tags.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::{browse, exif, fmt, geo, names, paths};

/// The radius a search around a place uses unless told otherwise: as far as a photo's nearest
/// airport may be (geo.rs's limit for airport tags).
pub const DEFAULT_KM: f64 = 120.0;

/// Tag kinds that are per photo rather than something to narrow by: too many values to count.
const NOT_FACETS: &[&str] = &["location", "date"];

/// The order facets come in; kinds not listed follow, alphabetically.
const FACET_ORDER: &[&str] = &[
    "country", "region", "city", "airport", "year", "event", "trip", "place", "album", "memory",
    "owner", "kind",
];

/// Past this many matches, details are read in one pass over every tagged item rather than one
/// item at a time.
const SCAN_ABOVE: usize = 4000;

/// A circle on the map.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Near {
    pub lat: f64,
    pub lon: f64,
    pub km: f64,
}

/// A tag every match must carry (or one of the tags under it).
#[derive(Debug, Clone, PartialEq)]
pub enum TagFilter {
    /// a tag by its id (what the facets and `tag list` show)
    Id(i64),
    /// every tag of this kind with this value, in any case (`country=hong kong`)
    Value { kind: String, value: String },
}

impl TagFilter {
    /// `ID` or `KIND=VALUE` (also `KIND:VALUE`).
    pub fn parse(s: &str) -> Result<TagFilter> {
        let s = s.trim();
        if let Ok(id) = s.parse::<i64>() {
            return Ok(TagFilter::Id(id));
        }
        let Some((k, v)) = s.split_once('=').or_else(|| s.split_once(':')) else {
            bail!(
                "a tag to look for is KIND=VALUE (e.g. airport=HKG, country=Hong Kong) or a tag id"
            );
        };
        let (kind, value) = (k.trim(), v.trim());
        if kind.is_empty() || value.is_empty() {
            bail!("a tag to look for needs both a kind and a value: {s:?}");
        }
        Ok(TagFilter::Value {
            kind: kind.to_lowercase(),
            value: value.to_string(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    /// nearest first around a centre, newest first otherwise
    #[default]
    Auto,
    Nearest,
    Newest,
    Oldest,
}

impl Sort {
    pub fn parse(s: &str) -> Result<Sort> {
        Ok(match s.trim().to_lowercase().as_str() {
            "" | "auto" => Sort::Auto,
            "near" | "nearest" | "distance" => Sort::Nearest,
            "new" | "newest" | "recent" => Sort::Newest,
            "old" | "oldest" => Sort::Oldest,
            other => bail!("sort by nearest, newest or oldest, not {other:?}"),
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct Query {
    pub near: Option<Near>,
    pub tags: Vec<TagFilter>,
    /// taken on or after: YYYY, YYYY-MM or YYYY-MM-DD
    pub from: Option<String>,
    /// taken on or before, likewise (`2019` reaches 31 December)
    pub to: Option<String>,
    pub sort: Sort,
    pub offset: usize,
    /// how many hits to return (with their copies); 0 for all of them
    pub limit: usize,
}

/// A tag as a hit shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TagRef {
    pub id: i64,
    pub kind: String,
    pub value: String,
}

/// A copy of a hit's content on an indexed drive, as the browser addresses it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Copy {
    /// the file's row (what /api/content and the viewers take)
    pub id: i64,
    pub path: String,
    pub name: String,
    pub size: u64,
    pub mtime: i64,
    /// browse keys of the file and of its folder
    pub key: String,
    pub dir: String,
    /// the indexed drive or folder it's on
    pub root: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Hit {
    /// `hex(sha256)`: the `content:` key
    pub key: String,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    /// from the centre, when there is one
    pub km: Option<f64>,
    /// the day it was taken (YYYY-MM-DD), when its metadata says
    pub date: Option<String>,
    /// its tags but the per-photo ones (location, date)
    pub tags: Vec<TagRef>,
    /// where it is now; empty when no indexed drive has it any more
    pub copies: Vec<Copy>,
    /// the decisions made about it (keep, drop, reviewed, …)
    pub flags: Vec<String>,
}

/// How many matches carry a tag.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Facet {
    pub id: i64,
    pub kind: String,
    pub value: String,
    pub parent_id: Option<i64>,
    pub count: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Found {
    /// every match
    pub total: usize,
    /// the matches with a position
    pub located: usize,
    /// the page asked for (`offset`, `limit`), in order
    pub hits: Vec<Hit>,
    /// tag counts over every match, kind by kind, most common first
    pub facets: Vec<Facet>,
    /// where the located matches are: `[lat, lon, how many]`, nearby ones merged
    pub points: Vec<[f64; 3]>,
    /// the box around them: `[south, west, north, east]`
    pub bounds: Option<[f64; 4]>,
}

/// What's known of one match before it's paged.
#[derive(Debug, Default)]
struct Detail {
    lat: Option<f64>,
    lon: Option<f64>,
    date: Option<String>,
    tags: Vec<(i64, String, String, Option<i64>)>, // id, kind, value, parent
}

/// Run a search.
pub fn find(conn: &Connection, q: &Query) -> Result<Found> {
    let mut cand: Option<HashSet<String>> = None;
    let narrow = |cand: &mut Option<HashSet<String>>, set: HashSet<String>| {
        *cand = Some(match cand.take() {
            None => set,
            Some(mut c) => {
                c.retain(|k| set.contains(k));
                c
            }
        });
    };
    for f in &q.tags {
        let ids = tag_ids(conn, f)?;
        narrow(&mut cand, keys_with_any(conn, &ids)?);
        if cand.as_ref().is_some_and(HashSet::is_empty) {
            return Ok(Found::default());
        }
    }
    if q.from.is_some() || q.to.is_some() {
        narrow(
            &mut cand,
            keys_dated(conn, q.from.as_deref(), q.to.as_deref())?,
        );
    }
    let mut km: HashMap<String, f64> = HashMap::new();
    if let Some(n) = q.near {
        let mut set = HashSet::new();
        for (key, lat, lon) in geo::content_locations(conn)? {
            if cand.as_ref().is_some_and(|c| !c.contains(&key)) {
                continue;
            }
            let d = geo::haversine_km(n.lat, n.lon, lat, lon);
            if d <= n.km {
                let e = km.entry(key.clone()).or_insert(d);
                *e = e.min(d);
                set.insert(key);
            }
        }
        narrow(&mut cand, set);
    }
    let cand = match cand {
        Some(c) => c,
        None => every_tagged(conn)?,
    };
    if cand.is_empty() {
        return Ok(Found::default());
    }

    let details = details(conn, &cand)?;
    let mut found = Found {
        total: cand.len(),
        ..Found::default()
    };
    summarise(&details, &mut found);

    // the order, then the page
    let sort = match q.sort {
        Sort::Auto if q.near.is_some() => Sort::Nearest,
        Sort::Auto => Sort::Newest,
        s => s,
    };
    let mut order: Vec<&String> = cand.iter().collect();
    let date = |k: &String| details.get(k).and_then(|d| d.date.as_deref());
    order.sort_by(|a, b| {
        let by_date = |newest: bool| match (date(a), date(b)) {
            (Some(x), Some(y)) if newest => y.cmp(x),
            (Some(x), Some(y)) => x.cmp(y),
            (Some(_), None) => std::cmp::Ordering::Less, // undated last either way
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        };
        match sort {
            Sort::Nearest => {
                let (x, y) = (
                    km.get(*a).copied().unwrap_or(f64::INFINITY),
                    km.get(*b).copied().unwrap_or(f64::INFINITY),
                );
                x.total_cmp(&y).then_with(|| by_date(true))
            }
            Sort::Oldest => by_date(false),
            _ => by_date(true),
        }
        .then_with(|| a.cmp(b))
    });
    let page =
        order
            .into_iter()
            .skip(q.offset)
            .take(if q.limit == 0 { usize::MAX } else { q.limit });
    for key in page {
        let d = details.get(key);
        found.hits.push(Hit {
            key: key.clone(),
            lat: d.and_then(|d| d.lat),
            lon: d.and_then(|d| d.lon),
            km: km.get(key).copied(),
            date: d.and_then(|d| d.date.clone()),
            tags: d
                .map(|d| {
                    let mut t: Vec<TagRef> = d
                        .tags
                        .iter()
                        .filter(|(_, k, _, _)| !NOT_FACETS.contains(&k.as_str()))
                        .map(|(id, k, v, _)| TagRef {
                            id: *id,
                            kind: k.clone(),
                            value: v.clone(),
                        })
                        .collect();
                    t.sort_by(|a, b| {
                        kind_rank(&a.kind)
                            .cmp(&kind_rank(&b.kind))
                            .then_with(|| a.kind.cmp(&b.kind))
                            .then_with(|| a.value.cmp(&b.value))
                    });
                    t
                })
                .unwrap_or_default(),
            copies: copies(conn, key)?,
            flags: flags(conn, key)?,
        });
    }
    Ok(found)
}

/// Where a kind of tag comes among the others.
fn kind_rank(k: &str) -> usize {
    FACET_ORDER
        .iter()
        .position(|x| *x == k)
        .unwrap_or(FACET_ORDER.len())
}

/// Facets, map points and bounds over every match.
fn summarise(details: &HashMap<String, Detail>, found: &mut Found) {
    let mut counts: HashMap<i64, (usize, &str, &str, Option<i64>)> = HashMap::new();
    let mut cells: HashMap<(i64, i64), usize> = HashMap::new();
    let mut bounds: Option<[f64; 4]> = None;
    for d in details.values() {
        for (id, kind, value, parent) in &d.tags {
            if NOT_FACETS.contains(&kind.as_str()) {
                continue;
            }
            counts
                .entry(*id)
                .or_insert((0, kind.as_str(), value.as_str(), *parent))
                .0 += 1;
        }
        if let (Some(lat), Some(lon)) = (d.lat, d.lon) {
            found.located += 1;
            // about 100 m: photos taken in one spot are one point
            *cells
                .entry(((lat * 1000.0).round() as i64, (lon * 1000.0).round() as i64))
                .or_default() += 1;
            let b = bounds.get_or_insert([lat, lon, lat, lon]);
            b[0] = b[0].min(lat);
            b[1] = b[1].min(lon);
            b[2] = b[2].max(lat);
            b[3] = b[3].max(lon);
        }
    }
    let mut facets: Vec<Facet> = counts
        .into_iter()
        .map(|(id, (count, kind, value, parent_id))| Facet {
            id,
            kind: kind.to_string(),
            value: value.to_string(),
            parent_id,
            count,
        })
        .collect();
    facets.sort_by(|a, b| {
        kind_rank(&a.kind)
            .cmp(&kind_rank(&b.kind))
            .then_with(|| a.kind.cmp(&b.kind))
            .then_with(|| b.count.cmp(&a.count))
            .then_with(|| a.value.cmp(&b.value))
    });
    found.facets = facets;
    // a map of tens of thousands of points is merged further, to about a kilometre
    if cells.len() > 20_000 {
        let mut coarse: HashMap<(i64, i64), usize> = HashMap::new();
        for ((la, lo), n) in cells {
            *coarse.entry((la / 10, lo / 10)).or_default() += n;
        }
        found.points = coarse
            .into_iter()
            .map(|((la, lo), n)| [la as f64 / 100.0, lo as f64 / 100.0, n as f64])
            .collect();
    } else {
        found.points = cells
            .into_iter()
            .map(|((la, lo), n)| [la as f64 / 1000.0, lo as f64 / 1000.0, n as f64])
            .collect();
    }
    found
        .points
        .sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    found.bounds = bounds;
}

/// The tags a filter stands for: the ones it names, and every tag under them.
fn tag_ids(conn: &Connection, f: &TagFilter) -> Result<Vec<i64>> {
    let named: Vec<i64> = match f {
        TagFilter::Id(id) => vec![*id],
        TagFilter::Value { kind, value } => {
            let mut st = conn.prepare_cached(
                "SELECT id FROM tags WHERE kind = ?1 AND value = ?2 COLLATE NOCASE",
            )?;
            st.query_map(params![kind, value], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        }
    };
    let mut all = Vec::new();
    let mut st = conn.prepare_cached(
        "WITH RECURSIVE sub(id) AS (SELECT ?1 UNION SELECT t.id FROM tags t JOIN sub ON t.parent_id = sub.id)
         SELECT id FROM sub",
    )?;
    for id in named {
        let ids: Vec<i64> = st
            .query_map(params![id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        all.extend(ids);
    }
    all.sort_unstable();
    all.dedup();
    Ok(all)
}

/// Content carrying any of these tags.
fn keys_with_any(conn: &Connection, ids: &[i64]) -> Result<HashSet<String>> {
    let mut st = conn.prepare_cached(
        "SELECT target_key FROM taggings WHERE tag_id = ?1 AND target_type = 'content'",
    )?;
    let mut out = HashSet::new();
    for id in ids {
        for k in st.query_map(params![id], |r| r.get::<_, String>(0))? {
            out.insert(k?);
        }
    }
    Ok(out)
}

/// `YYYY`, `YYYY-MM` or `YYYY-MM-DD` as the first or last day it covers.
fn day_bound(s: &str, end: bool) -> Result<String> {
    let s = s.trim();
    let ok = |n: usize| {
        s.len() == n
            && s.bytes().enumerate().all(|(i, b)| {
                if i == 4 || i == 7 {
                    b == b'-'
                } else {
                    b.is_ascii_digit()
                }
            })
    };
    Ok(match s.len() {
        4 if ok(4) => format!("{s}-{}", if end { "12-31" } else { "01-01" }),
        7 if ok(7) => format!("{s}-{}", if end { "31" } else { "01" }),
        10 if ok(10) => s.to_string(),
        _ => bail!("a date is YYYY, YYYY-MM or YYYY-MM-DD, not {s:?}"),
    })
}

/// Content whose date tag falls between the two (either may be open).
fn keys_dated(conn: &Connection, from: Option<&str>, to: Option<&str>) -> Result<HashSet<String>> {
    let lo = from.map(|f| day_bound(f, false)).transpose()?;
    let hi = to.map(|t| day_bound(t, true)).transpose()?;
    let mut st = conn.prepare_cached(
        "SELECT g.target_key FROM tags t JOIN taggings g ON g.tag_id = t.id
         WHERE t.kind = 'date' AND g.target_type = 'content'
           AND (?1 IS NULL OR t.value >= ?1) AND (?2 IS NULL OR t.value <= ?2)",
    )?;
    let rows = st.query_map(params![lo, hi], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Every piece of content with any tag (what a search with no conditions finds).
fn every_tagged(conn: &Connection) -> Result<HashSet<String>> {
    let mut st = conn
        .prepare_cached("SELECT DISTINCT target_key FROM taggings WHERE target_type = 'content'")?;
    let rows = st.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn details(conn: &Connection, keys: &HashSet<String>) -> Result<HashMap<String, Detail>> {
    let mut out: HashMap<String, Detail> = HashMap::with_capacity(keys.len());
    let mut add = |key: String, id: i64, kind: String, value: String, parent: Option<i64>| {
        let d = out.entry(key).or_default();
        match kind.as_str() {
            "location" => {
                if let Some((la, lo)) = value.split_once(',')
                    && let (Ok(la), Ok(lo)) = (la.parse(), lo.parse())
                {
                    d.lat = Some(la);
                    d.lon = Some(lo);
                }
            }
            "date" if d.date.as_ref().is_none_or(|x| value < *x) => {
                d.date = Some(value.clone());
            }
            _ => {}
        }
        d.tags.push((id, kind, value, parent));
    };
    let select = "SELECT g.target_key, t.id, t.kind, t.value, t.parent_id
                  FROM taggings g JOIN tags t ON t.id = g.tag_id
                  WHERE g.target_type = 'content'";
    if keys.len() > SCAN_ABOVE {
        let mut st = conn.prepare(select)?;
        let mut rows = st.query([])?;
        while let Some(r) = rows.next()? {
            let key: String = r.get(0)?;
            if keys.contains(&key) {
                add(key, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?);
            }
        }
    } else {
        let mut st = conn.prepare_cached(&format!("{select} AND g.target_key = ?1"))?;
        for key in keys {
            let mut rows = st.query(params![key])?;
            while let Some(r) = rows.next()? {
                add(key.clone(), r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?);
            }
        }
    }
    Ok(out)
}

/// The current copies of a piece of content on indexed drives, drive by drive.
pub fn copies(conn: &Connection, key: &str) -> Result<Vec<Copy>> {
    let Some(sha) = fmt::unhex(key) else {
        return Ok(Vec::new());
    };
    let mut st = conn.prepare_cached(
        "SELECT f.id, f.path, f.size, f.mtime, r.path FROM files f JOIN roots r ON r.id = f.root_id
         WHERE f.sha256 = ?1 AND f.current = 1 ORDER BY r.path, f.path",
    )?;
    let rows = st.query_map(params![sha], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, Vec<u8>>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, Vec<u8>>(4)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, path, size, mtime, root) = row?;
        let path = paths::from_bytes(&path);
        let name = path.file_name().map_or_else(
            || paths::show(&path),
            |n| names::nfc(&n.to_string_lossy()).into_owned(),
        );
        out.push(Copy {
            id,
            name,
            size: size.max(0) as u64,
            mtime,
            key: browse::key(&path),
            dir: path.parent().map(browse::key).unwrap_or_default(),
            root: paths::show(&paths::from_bytes(&root)),
            path: names::nfc(&paths::show(&path)).into_owned(),
        });
    }
    Ok(out)
}

/// The flags on a piece of content, by name.
fn flags(conn: &Connection, key: &str) -> Result<Vec<String>> {
    let mut st = conn.prepare_cached(
        "SELECT state FROM item_state WHERE target_type = 'content' AND target_key = ?1 ORDER BY state",
    )?;
    let rows = st.query_map(params![key], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// How many pieces of content carry any tag, and how many a position.
pub fn counts(conn: &Connection) -> Result<(u64, u64)> {
    let tagged: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT target_key) FROM taggings WHERE target_type = 'content'",
        [],
        |r| r.get(0),
    )?;
    let located: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT g.target_key) FROM tags t JOIN taggings g ON g.tag_id = t.id
         WHERE t.kind = 'location' AND g.target_type = 'content'",
        [],
        |r| r.get(0),
    )?;
    Ok((tagged.max(0) as u64, located.max(0) as u64))
}

// ---------- reading dates and places from the files

/// Extensions of the photos and videos whose own metadata can say when and where (`nom-exif`:
/// JPEG, HEIC/HEIF, AVIF, PNG, WebP, TIFF and the raw formats built on it, CR3, RAF, IIQ; MP4,
/// MOV, 3GP, MKV, WebM).
pub const MEDIA_EXTS: &[&str] = &[
    "jpg", "jpeg", "jpe", "heic", "heif", "avif", "png", "webp", "tif", "tiff", "dng", "nef",
    "arw", "cr2", "cr3", "orf", "rw2", "raf", "iiq", "mov", "mp4", "m4v", "3gp", "mkv", "webm",
];

/// Whether a path's extension is one [`MEDIA_EXTS`] lists.
pub fn is_media(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| MEDIA_EXTS.iter().any(|m| m.eq_ignore_ascii_case(e)))
}

/// An indexed photo or video, as the index has it now.
#[derive(Debug, Clone)]
pub struct MediaFile {
    pub id: i64,
    pub path: PathBuf,
    pub size: u64,
    pub mtime: i64,
    pub sha256: Option<Vec<u8>>,
}

/// The photos and videos the index has under `dir` (or `dir` itself, when it's one), as now.
pub fn media_under(conn: &Connection, dir: &Path) -> Result<Vec<MediaFile>> {
    let lo = paths::dir_prefix(dir);
    let mut hi = lo.clone();
    if let Some(l) = hi.last_mut() {
        *l += 1; // a separator is never 0xFF
    }
    let this = paths::to_bytes(dir);
    let mut st = conn.prepare(
        "SELECT id, path, size, mtime, sha256 FROM files
         WHERE current = 1 AND ((path >= ?1 AND path < ?2) OR path = ?3)",
    )?;
    let mut rows = st.query(params![lo, hi, this])?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        let path = paths::from_bytes(&r.get::<_, Vec<u8>>(1)?);
        if !is_media(&path) {
            continue;
        }
        out.push(MediaFile {
            id: r.get(0)?,
            path,
            size: r.get::<_, i64>(2)?.max(0) as u64,
            mtime: r.get(3)?,
            sha256: r.get(4)?,
        });
    }
    Ok(out)
}

/// What reading a file found.
#[derive(Debug, Clone, PartialEq)]
pub enum Read {
    /// read before (its content has tags from its metadata): nothing to do
    Already { key: String },
    /// it changed since it was indexed (or can't be read); the index needs to look again
    Unreadable { why: String },
    /// its metadata has no date or position
    Nothing { key: String },
    /// tags attached, as `kind=value`
    Tagged {
        key: String,
        applied: Vec<String>,
        located: bool,
    },
}

/// The content key of an indexed file: its hash, or read now (and recorded, as `hash` would)
/// when `hash` left it alone, which it does to any file whose size no other file shares, so
/// most photos. The file has to be as it was indexed.
pub fn content_key(
    conn: &Connection,
    f: &MediaFile,
) -> Result<std::result::Result<String, String>> {
    if let Some(sha) = &f.sha256 {
        return Ok(Ok(fmt::hex(sha)));
    }
    match crate::hash::full_of(&f.path, f.size, f.mtime) {
        Ok(sha) => {
            conn.execute(
                "UPDATE files SET sha256 = ?1 WHERE id = ?2",
                params![sha, f.id],
            )?;
            Ok(Ok(fmt::hex(&sha)))
        }
        Err(why) => Ok(Err(why)),
    }
}

/// Whether content already carries tags that its own metadata gave it.
pub fn already_read(conn: &Connection, key: &str, source: &str) -> Result<bool> {
    Ok(conn
        .prepare_cached(
            "SELECT 1 FROM taggings WHERE target_type = 'content' AND target_key = ?1 AND source = ?2 LIMIT 1",
        )?
        .query_row(params![key, source], |_| Ok(()))
        .optional()?
        .is_some())
}

/// Read one photo or video: its date and position become tags on its content (`exif_source`),
/// and, given the reference tables, the position its country, region, city and nearest airport
/// (`geo_source`). With `again`, content read before is read once more.
pub fn read_file(
    conn: &Connection,
    f: &MediaFile,
    geo: Option<&geo::GeoRef>,
    exif_source: &str,
    geo_source: &str,
    again: bool,
) -> Result<Read> {
    let key = match content_key(conn, f)? {
        Ok(k) => k,
        Err(why) => return Ok(Read::Unreadable { why }),
    };
    if !again && already_read(conn, &key, exif_source)? {
        return Ok(Read::Already { key });
    }
    let meta = exif::read_meta(&f.path);
    if meta.is_empty() {
        return Ok(Read::Nothing { key });
    }
    let mut applied = exif::derive_into(conn, &key, &meta, exif_source)?;
    let located = matches!((meta.lat, meta.lon), (Some(_), Some(_)));
    if let (Some(g), Some(lat), Some(lon)) = (geo, meta.lat, meta.lon) {
        applied.extend(geo::derive_into(conn, g, &key, lat, lon, geo_source)?);
    }
    Ok(Read::Tagged {
        key,
        applied,
        located,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::native;
    use crate::tags::{self, TARGET_CONTENT};

    /// A path as the index keeps it here.
    fn stored(p: &str) -> Vec<u8> {
        paths::to_bytes(&native(p))
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        c.execute_batch(tags::SCHEMA).unwrap();
        c.execute_batch(
            "CREATE TABLE roots (id INTEGER PRIMARY KEY, path BLOB NOT NULL UNIQUE, last_scan INTEGER);
             CREATE TABLE files (id INTEGER PRIMARY KEY, root_id INTEGER NOT NULL, path BLOB NOT NULL,
                size INTEGER NOT NULL, mtime INTEGER NOT NULL, sha256 BLOB, current INTEGER NOT NULL DEFAULT 1);",
        )
        .unwrap();
        c
    }

    /// A photo: located, dated, with a country, a city and an airport.
    fn photo(c: &Connection, key: &str, at: (f64, f64), date: &str, place: (&str, &str, &str)) {
        let loc = tags::upsert_tag(
            c,
            "location",
            &format!("{:.6},{:.6}", at.0, at.1),
            None,
            None,
        )
        .unwrap();
        tags::attach(c, TARGET_CONTENT, key, loc, "exif", 1.0).unwrap();
        let year = tags::upsert_tag(c, "year", &date[..4], None, None).unwrap();
        tags::attach(c, TARGET_CONTENT, key, year, "exif", 1.0).unwrap();
        let day = tags::upsert_tag(c, "date", date, Some(year), None).unwrap();
        tags::attach(c, TARGET_CONTENT, key, day, "exif", 1.0).unwrap();
        let country = tags::upsert_tag(c, "country", place.0, None, None).unwrap();
        tags::attach(c, TARGET_CONTENT, key, country, "geo", 1.0).unwrap();
        let city = tags::upsert_tag(c, "city", place.1, Some(country), None).unwrap();
        tags::attach(c, TARGET_CONTENT, key, city, "geo", 1.0).unwrap();
        let airport = tags::upsert_tag(c, "airport", place.2, None, None).unwrap();
        tags::attach(c, TARGET_CONTENT, key, airport, "geo", 1.0).unwrap();
    }

    const HKG: (f64, f64) = (22.3089, 113.9146);

    fn sample() -> Connection {
        let c = db();
        photo(
            &c,
            "aa",
            (22.2810, 114.1589),
            "2023-04-18",
            ("Hong Kong", "Central", "HHP"),
        ); // ~25 km
        photo(
            &c,
            "bb",
            (22.2890, 113.9410),
            "2019-11-02",
            ("Hong Kong", "Tung Chung", "HKG"),
        ); // ~3 km
        photo(
            &c,
            "cc",
            (22.5431, 114.0579),
            "2021-06-01",
            ("China", "Shenzhen", "SZX"),
        ); // ~30 km
        photo(
            &c,
            "dd",
            (26.1003, -80.1281),
            "2019-11-03",
            ("United States", "Fort Lauderdale", "FLL"),
        );
        // a photo with an event and no position
        let ev = tags::upsert_tag(&c, "event", "Boat show 2019", None, None).unwrap();
        tags::attach(&c, TARGET_CONTENT, "ee", ev, "user", 1.0).unwrap();
        tags::attach(&c, TARGET_CONTENT, "dd", ev, "user", 1.0).unwrap();
        c
    }

    fn keys(f: &Found) -> Vec<&str> {
        f.hits.iter().map(|h| h.key.as_str()).collect()
    }

    #[test]
    fn near_a_centre_nearest_first() {
        let c = sample();
        let q = Query {
            near: Some(Near {
                lat: HKG.0,
                lon: HKG.1,
                km: DEFAULT_KM,
            }),
            ..Query::default()
        };
        let f = find(&c, &q).unwrap();
        assert_eq!(keys(&f), ["bb", "aa", "cc"]);
        assert_eq!(f.total, 3);
        assert_eq!(f.located, 3);
        assert!(f.hits[0].km.unwrap() < 5.0);
        // a tighter circle leaves Central and Shenzhen out
        let q = Query {
            near: Some(Near {
                lat: HKG.0,
                lon: HKG.1,
                km: 10.0,
            }),
            ..Query::default()
        };
        assert_eq!(keys(&find(&c, &q).unwrap()), ["bb"]);
    }

    #[test]
    fn tags_dates_and_hierarchy_narrow_it_down() {
        let c = sample();
        // by kind and value, in any case
        let q = Query {
            tags: vec![TagFilter::parse("country=hong kong").unwrap()],
            ..Query::default()
        };
        assert_eq!(keys(&find(&c, &q).unwrap()), ["aa", "bb"]); // newest first
        // two tags: both must be there
        let q = Query {
            tags: vec![
                TagFilter::parse("country=Hong Kong").unwrap(),
                TagFilter::parse("airport=HKG").unwrap(),
            ],
            ..Query::default()
        };
        assert_eq!(keys(&find(&c, &q).unwrap()), ["bb"]);
        // a year stands for the dates under it
        let q = Query {
            tags: vec![TagFilter::parse("year=2019").unwrap()],
            sort: Sort::Oldest,
            ..Query::default()
        };
        assert_eq!(keys(&find(&c, &q).unwrap()), ["bb", "dd"]);
        // a date range, year to year
        let q = Query {
            from: Some("2020".into()),
            to: Some("2023-04".into()),
            ..Query::default()
        };
        assert_eq!(keys(&find(&c, &q).unwrap()), ["aa", "cc"]);
        // a tag that isn't there finds nothing
        let q = Query {
            tags: vec![TagFilter::parse("airport=CDG").unwrap()],
            ..Query::default()
        };
        assert_eq!(find(&c, &q).unwrap(), Found::default());
    }

    #[test]
    fn no_conditions_finds_everything_tagged_with_facets_and_points() {
        let c = sample();
        let f = find(&c, &Query::default()).unwrap();
        assert_eq!(f.total, 5);
        assert_eq!(f.located, 4);
        // the undated, unlocated one comes last
        assert_eq!(keys(&f).last(), Some(&"ee"));
        // facets: countries first, most common first; no location or date facets
        assert_eq!(f.facets[0].kind, "country");
        assert_eq!(
            (f.facets[0].value.as_str(), f.facets[0].count),
            ("Hong Kong", 2)
        );
        assert!(
            !f.facets
                .iter()
                .any(|x| x.kind == "location" || x.kind == "date")
        );
        let event = f.facets.iter().find(|x| x.kind == "event").unwrap();
        assert_eq!(event.count, 2);
        assert_eq!(f.points.len(), 4);
        let b = f.bounds.unwrap();
        assert!(
            b[0] < 22.3 && b[2] > 26.0 && b[1] < -80.0 && b[3] > 114.0,
            "{b:?}"
        );
        // paging
        let page = find(
            &c,
            &Query {
                offset: 1,
                limit: 2,
                ..Query::default()
            },
        )
        .unwrap();
        assert_eq!(page.hits.len(), 2);
        assert_eq!(page.total, 5);
    }

    #[test]
    fn copies_come_from_the_index() {
        let c = sample();
        let sha = [0xbbu8; 32];
        let key = fmt::hex(&sha);
        let loc = tags::upsert_tag(&c, "location", "22.289000,113.941000", None, None).unwrap();
        tags::attach(&c, TARGET_CONTENT, &key, loc, "exif", 1.0).unwrap();
        c.execute(
            "INSERT INTO roots(id, path) VALUES (1, ?1)",
            params![stored("/photos")],
        )
        .unwrap();
        for (id, p, current) in [
            (1, "/photos/2019/IMG_1.jpg", 1),
            (2, "/photos/old/IMG_1.jpg", 0),
        ] {
            c.execute(
                "INSERT INTO files(id, root_id, path, size, mtime, sha256, current) VALUES (?1, 1, ?2, 10, 0, ?3, ?4)",
                params![id, stored(p), sha.to_vec(), current],
            )
            .unwrap();
        }
        let q = Query {
            near: Some(Near {
                lat: HKG.0,
                lon: HKG.1,
                km: 5.0,
            }),
            ..Query::default()
        };
        let f = find(&c, &q).unwrap();
        let hit = f.hits.iter().find(|h| h.key == key).unwrap();
        assert_eq!(hit.copies.len(), 1, "only the current copy");
        assert_eq!(hit.copies[0].name, "IMG_1.jpg");
        assert_eq!(hit.copies[0].root, native("/photos").display().to_string());
        assert_eq!(
            browse::path_of(&hit.copies[0].dir).unwrap(),
            native("/photos/2019")
        );
    }

    #[test]
    fn media_under_a_folder() {
        let c = db();
        c.execute(
            "INSERT INTO roots(id, path) VALUES (1, ?1)",
            params![stored("/d")],
        )
        .unwrap();
        for (id, p) in [
            (1, "/d/a/IMG_1.JPG"),
            (2, "/d/a/clip.mov"),
            (3, "/d/a/notes.txt"),
            (4, "/d/ab/IMG_2.jpg"),
            (5, "/d/a/sub/IMG_3.heic"),
        ] {
            c.execute(
                "INSERT INTO files(id, root_id, path, size, mtime) VALUES (?1, 1, ?2, 10, 0)",
                params![id, stored(p)],
            )
            .unwrap();
        }
        let mut ids: Vec<i64> = media_under(&c, &native("/d/a"))
            .unwrap()
            .iter()
            .map(|m| m.id)
            .collect();
        ids.sort();
        assert_eq!(
            ids,
            [1, 2, 5],
            "not the text file, not the folder beside it"
        );
        assert_eq!(
            media_under(&c, &native("/d/ab/IMG_2.jpg")).unwrap().len(),
            1
        );
    }

    #[test]
    fn filters_parse() {
        assert_eq!(TagFilter::parse("12").unwrap(), TagFilter::Id(12));
        assert_eq!(
            TagFilter::parse("Airport=HKG").unwrap(),
            TagFilter::Value {
                kind: "airport".into(),
                value: "HKG".into()
            }
        );
        assert!(TagFilter::parse("HKG").is_err());
        assert!(TagFilter::parse("country=").is_err());
        assert_eq!(Sort::parse("near").unwrap(), Sort::Nearest);
        assert!(Sort::parse("sideways").is_err());
        assert_eq!(day_bound("2019", true).unwrap(), "2019-12-31");
        assert_eq!(day_bound("2019-02", false).unwrap(), "2019-02-01");
        assert!(day_bound("19-02", false).is_err());
    }
}
