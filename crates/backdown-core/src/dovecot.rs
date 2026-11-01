//! Dovecot's own index files, read for what they already know about a mailbox's messages.
//!
//! A Dovecot mailbox keeps `dovecot.index` (one fixed-size record per message: UID, flags, and
//! extension fields) and `dovecot.index.cache` (per message, the header fields Dovecot decided to
//! cache: `hdr.subject`, `hdr.from`, `hdr.date`, …, so IMAP clients can list a folder without
//! opening the messages). BackDown's content search wants the same fields, so when the cache is
//! there and matches the index it is read instead of opening every message file: a mailbox of a
//! million messages is a few hundred megabytes of cache rather than a million opens.
//!
//! The layouts are Dovecot's `struct mail_index_header`, `mail_index_ext_header`,
//! `mail_cache_header`, `mail_cache_header_fields` and `mail_cache_record` (lib-index, Dovecot
//! 2.3+; index major version 7, cache major version 1). Everything is checked before it's trusted:
//! versions, the index id shared by both files, the cache file sequence against the "cache"
//! extension's reset id, every offset against the file. Anything odd returns `None` and the
//! caller reads the messages themselves. Nothing is ever written.
//!
//! Dovecot's full-text plugins keep their own indexes beside these (`fts-flatcurve/`,
//! `xapian-indexes/`, `lucene-indexes/`, Squat's `dovecot.index.search`); `fts_plugin` names
//! them, so a mailbox can say "a full-text index exists here", but they are not read.

use std::path::Path;

/// One message's cached headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedMessage {
    pub uid: u32,
    /// (header name, lowercase; value with the name stripped, lines joined with a space)
    pub headers: Vec<(String, String)>,
}

/// What the cache held for a mailbox.
#[derive(Debug, Clone, Default)]
pub struct Cached {
    pub messages: Vec<CachedMessage>,
    /// messages in the index with no cache record (or none of the wanted headers)
    pub uncached: u32,
    /// the header fields Dovecot caches here, lowercase ("subject", "from", …)
    pub fields: Vec<String>,
}

/// The largest index or cache file read whole (bigger ones are left alone).
const MAX_FILE: u64 = 1 << 30;
const INDEX_MAJOR: u8 = 7;
const CACHE_MAJOR: u8 = 1;

/// Little- or big-endian, as the index's compat flags say (Dovecot writes the host's).
#[derive(Clone, Copy)]
struct Endian(bool);

impl Endian {
    fn u32(self, b: &[u8], at: usize) -> Option<u32> {
        let x: [u8; 4] = b.get(at..at + 4)?.try_into().ok()?;
        Some(if self.0 {
            u32::from_le_bytes(x)
        } else {
            u32::from_be_bytes(x)
        })
    }
    fn u16(self, b: &[u8], at: usize) -> Option<u16> {
        let x: [u8; 2] = b.get(at..at + 2)?.try_into().ok()?;
        Some(if self.0 {
            u16::from_le_bytes(x)
        } else {
            u16::from_be_bytes(x)
        })
    }
}

/// `mail_index_offset_to_uint32`: four 7-bit groups with marker bits, stored big-endian; 0 when
/// it isn't one.
fn decode_offset(raw_be: [u8; 4]) -> u32 {
    let v = u32::from_be_bytes(raw_be);
    if v & 0x8080_8080 != 0x8080_8080 {
        return 0;
    }
    ((v & 0x7f) | ((v & 0x7f00) >> 8 << 7) | ((v & 0x7f_0000) >> 16 << 14) | ((v & 0x7f00_0000) >> 24 << 21)) << 2
}

/// `mail_index_uint32_to_offset`, for tests.
#[cfg(test)]
fn encode_offset(offset: u32) -> [u8; 4] {
    let o = offset >> 2;
    let v = 0x80 | (o & 0x7f)
        | 0x8000 | ((o & 0x3f80) >> 7 << 8)
        | 0x80_0000 | ((o & 0x1f_c000) >> 14 << 16)
        | 0x8000_0000 | ((o & 0x0fe0_0000) >> 21 << 24);
    v.to_be_bytes()
}

fn align8(n: usize) -> usize {
    (n + 7) & !7
}

struct Index {
    endian: Endian,
    indexid: u32,
    /// where the "cache" extension's u32 sits in each record, and its reset id
    cache_ext: Option<(usize, u32)>,
    records: Vec<(u32, u32)>, // (uid, cache offset)
}

fn read_whole(path: &Path, max: u64) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > max {
        return None;
    }
    std::fs::read(path).ok()
}

fn read_index(path: &Path) -> Option<Index> {
    let b = read_whole(path, MAX_FILE)?;
    if b.len() < 120 || b[0] != INDEX_MAJOR {
        return None;
    }
    let endian = Endian(b[12] & 1 == 1);
    let base_header_size = endian.u16(&b, 2)? as usize;
    let header_size = endian.u32(&b, 4)? as usize;
    let record_size = endian.u32(&b, 8)? as usize;
    let indexid = endian.u32(&b, 16)?;
    let messages = endian.u32(&b, 32)? as usize;
    if base_header_size < 120 || header_size < base_header_size || header_size > b.len() || record_size < 5 {
        return None;
    }
    // extension headers: fixed part, name, then data at the next 8-byte boundary
    let mut cache_ext = None;
    let mut at = base_header_size;
    while at + 16 <= header_size {
        let hdr_size = endian.u32(&b, at)? as usize;
        let reset_id = endian.u32(&b, at + 4)?;
        let record_offset = endian.u16(&b, at + 8)? as usize;
        let rec_size = endian.u16(&b, at + 10)? as usize;
        let name_size = endian.u16(&b, at + 14)? as usize;
        let name_at = at + 16;
        let name = b.get(name_at..name_at + name_size)?;
        if name == b"cache" && rec_size == 4 && record_offset + 4 <= record_size {
            cache_ext = Some((record_offset, reset_id));
        }
        at = align8(align8(name_at + name_size) + hdr_size);
        if hdr_size > header_size {
            return None;
        }
    }
    let mut records = Vec::with_capacity(messages.min(1 << 24));
    let mut pos = header_size;
    for _ in 0..messages {
        if pos + record_size > b.len() {
            break;
        }
        let uid = endian.u32(&b, pos)?;
        let off = match cache_ext {
            Some((o, _)) => endian.u32(&b, pos + o)?,
            None => 0,
        };
        records.push((uid, off));
        pos += record_size;
    }
    Some(Index {
        endian,
        indexid,
        cache_ext,
        records,
    })
}

struct Fields {
    names: Vec<String>,
    /// u32::MAX for variable-size fields
    sizes: Vec<u32>,
}

fn read_fields(c: &[u8], endian: Endian, first: u32) -> Option<Fields> {
    // follow the chain of updated headers to the newest
    let mut at = first as usize;
    let mut hops = 0;
    loop {
        let next = decode_offset(c.get(at..at + 4)?.try_into().ok()?) as usize;
        if next == 0 {
            break;
        }
        if next == at || next >= c.len() || hops > 1000 {
            return None;
        }
        at = next;
        hops += 1;
    }
    let size = endian.u32(c, at + 4)? as usize;
    let n = endian.u32(c, at + 8)? as usize;
    if n > 1 << 16 || size < 12 + 10 * n || at + size > c.len() {
        return None;
    }
    let sizes_at = at + 12 + 4 * n;
    let names_at = at + 12 + 10 * n;
    let mut sizes = Vec::with_capacity(n);
    for i in 0..n {
        sizes.push(endian.u32(c, sizes_at + 4 * i)?);
    }
    let mut names = Vec::with_capacity(n);
    let mut p = names_at;
    for _ in 0..n {
        let end = c[p..at + size].iter().position(|&x| x == 0)? + p;
        names.push(String::from_utf8_lossy(&c[p..end]).into_owned());
        p = end + 1;
    }
    Some(Fields { names, sizes })
}

/// "Subject: hello\n world" → "hello world": the name Dovecot stored with the line, dropped.
fn header_value(name: &str, text: &str) -> String {
    let mut out = String::new();
    for line in text.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let v = if line.len() > name.len() + 1
            && line.as_bytes()[name.len()] == b':'
            && line[..name.len()].eq_ignore_ascii_case(name)
        {
            line[name.len() + 1..].trim()
        } else {
            line.trim()
        };
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(v);
    }
    out
}

/// The headers Dovecot cached for every message in `mailbox` (the folder holding
/// `dovecot.index` and `dovecot.index.cache`), limited to `wanted` names (lowercase, without the
/// "hdr." prefix; empty for all). `None` when there is no usable cache.
pub fn cached_headers(mailbox: &Path, wanted: &[&str]) -> Option<Cached> {
    let idx = read_index(&mailbox.join("dovecot.index"))?;
    let (ext_off, reset_id) = idx.cache_ext?;
    let _ = ext_off;
    let c = read_whole(&mailbox.join("dovecot.index.cache"), MAX_FILE)?;
    if c.len() < 32 || c[0] != CACHE_MAJOR {
        return None;
    }
    let e = idx.endian;
    if e.u32(&c, 4)? != idx.indexid || e.u32(&c, 8)? != reset_id {
        return None; // another index's cache, or one purged since the index pointed into it
    }
    let first = decode_offset(c[28..32].try_into().ok()?);
    if first == 0 || first as usize >= c.len() {
        return None;
    }
    let fields = read_fields(&c, e, first)?;
    // which file fields are headers we want
    let hdr: Vec<Option<String>> = fields
        .names
        .iter()
        .map(|n| {
            let h = n.strip_prefix("hdr.")?.to_lowercase();
            (wanted.is_empty() || wanted.contains(&h.as_str())).then_some(h)
        })
        .collect();
    let mut out = Cached {
        fields: fields
            .names
            .iter()
            .filter_map(|n| n.strip_prefix("hdr.").map(str::to_lowercase))
            .collect(),
        ..Cached::default()
    };
    for &(uid, off) in &idx.records {
        let mut headers: Vec<(String, String)> = Vec::new();
        let mut at = off as usize;
        let mut hops = 0;
        while at != 0 {
            if at % 4 != 0 || at + 8 > c.len() || hops > 64 {
                break;
            }
            let prev = e.u32(&c, at)? as usize;
            let size = e.u32(&c, at + 4)? as usize;
            if size < 8 || at + size > c.len() {
                break;
            }
            let end = at + size;
            let mut p = at + 8;
            while p + 4 <= end {
                let field = e.u32(&c, p)? as usize;
                p += 4;
                let Some(&fsize) = fields.sizes.get(field) else {
                    break;
                };
                let data_size = if fsize == u32::MAX {
                    let s = e.u32(&c, p)? as usize;
                    p += 4;
                    s
                } else {
                    fsize as usize
                };
                if p + data_size > end {
                    break;
                }
                if let Some(Some(name)) = hdr.get(field) {
                    // header data: line numbers up to a 0, then the text
                    let mut q = p;
                    while q + 4 <= p + data_size && e.u32(&c, q)? != 0 {
                        q += 4;
                    }
                    q += 4;
                    if q <= p + data_size {
                        let text = String::from_utf8_lossy(&c[q..p + data_size]);
                        let v = header_value(name, &text);
                        if !v.is_empty() && !headers.iter().any(|(n, _)| n == name) {
                            headers.push((name.clone(), v));
                        }
                    }
                }
                p += (data_size + 3) & !3;
            }
            at = prev;
            hops += 1;
        }
        if headers.is_empty() {
            out.uncached += 1;
        } else {
            out.messages.push(CachedMessage { uid, headers });
        }
    }
    Some(out)
}

/// `dovecot-uidlist`: UID → message file name, for the messages the cache doesn't cover.
pub fn uidlist(mailbox: &Path) -> Option<Vec<(u32, String)>> {
    let text = read_whole(&mailbox.join("dovecot-uidlist"), 256 << 20)?;
    let text = String::from_utf8_lossy(&text);
    let mut out = Vec::new();
    for line in text.lines().skip(1) {
        // "123 W1234 G<guid> :1500000000.M1P1.host:2,S" — the file name follows " :"
        let Some((head, name)) = line.split_once(" :") else {
            continue;
        };
        let Some(uid) = head.split_whitespace().next().and_then(|u| u.parse().ok()) else {
            continue;
        };
        out.push((uid, name.to_string()));
    }
    Some(out)
}

/// A full-text index a Dovecot plugin keeps in or beside this mailbox, by its name.
pub fn fts_plugin(mailbox: &Path) -> Option<&'static str> {
    let has = |n: &str| mailbox.join(n).exists();
    if has("fts-flatcurve") {
        Some("fts-flatcurve (Xapian)")
    } else if has("xapian-indexes") {
        Some("fts-xapian")
    } else if has("lucene-indexes") {
        Some("fts-lucene")
    } else if has("dovecot.index.search") {
        Some("fts-squat")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn le(v: u32) -> [u8; 4] {
        v.to_le_bytes()
    }

    /// An index with one extension ("cache") and `uids` records pointing at `offsets`.
    fn index(indexid: u32, reset_id: u32, msgs: &[(u32, u32)]) -> Vec<u8> {
        let mut b = vec![0u8; 120];
        b[0] = 7;
        b[1] = 3;
        b[2..4].copy_from_slice(&120u16.to_le_bytes());
        // ext header: 16 fixed + "cache" (5) → 21 → aligned 24; no data
        let header_size = 120 + 24;
        b[4..8].copy_from_slice(&le(header_size as u32));
        let record_size = 12u32; // uid 4, flags 1 + pad 3, cache 4
        b[8..12].copy_from_slice(&le(record_size));
        b[12] = 1; // little-endian
        b[16..20].copy_from_slice(&le(indexid));
        b[32..36].copy_from_slice(&le(msgs.len() as u32));
        // the extension
        b.extend_from_slice(&le(0)); // hdr_size
        b.extend_from_slice(&le(reset_id));
        b.extend_from_slice(&8u16.to_le_bytes()); // record_offset
        b.extend_from_slice(&4u16.to_le_bytes()); // record_size
        b.extend_from_slice(&4u16.to_le_bytes()); // record_align
        b.extend_from_slice(&5u16.to_le_bytes()); // name_size
        b.extend_from_slice(b"cache");
        b.resize(header_size, 0);
        for &(uid, off) in msgs {
            b.extend_from_slice(&le(uid));
            b.extend_from_slice(&[0, 0, 0, 0]);
            b.extend_from_slice(&le(off));
        }
        b
    }

    /// A cache with fields [hdr.subject (var), flags (fixed 4), hdr.from (var)] and records.
    fn cache(indexid: u32, file_seq: u32, records: &mut Vec<(u32, Vec<(u32, Vec<u8>)>)>) -> (Vec<u8>, Vec<u32>) {
        let mut c = vec![0u8; 32];
        c[0] = 1;
        c[1] = 8;
        c[2] = 1;
        c[4..8].copy_from_slice(&le(indexid));
        c[8..12].copy_from_slice(&le(file_seq));
        // fields header at 32
        let names = b"hdr.subject\0flags\0hdr.from\0";
        let n = 3u32;
        let size = 12 + 10 * 3 + names.len();
        let mut f = Vec::new();
        f.extend_from_slice(&[0, 0, 0, 0]); // next_offset: none
        f.extend_from_slice(&le(size as u32));
        f.extend_from_slice(&le(n));
        for _ in 0..3 {
            f.extend_from_slice(&le(0)); // last_used
        }
        for s in [u32::MAX, 4, u32::MAX] {
            f.extend_from_slice(&le(s));
        }
        f.extend_from_slice(&[4, 0, 4]); // types: header, fixed, header
        f.extend_from_slice(&[2, 2, 2]); // decisions
        f.extend_from_slice(names);
        while f.len() % 4 != 0 {
            f.push(0);
        }
        c[28..32].copy_from_slice(&encode_offset(32));
        c.extend_from_slice(&f);
        // records: each (prev, fields: (field, data))
        let mut offsets = Vec::new();
        for (prev, fields) in records.iter_mut() {
            let at = c.len();
            offsets.push(at as u32);
            let mut body = Vec::new();
            for (field, data) in fields.iter() {
                body.extend_from_slice(&le(*field));
                if *field != 1 {
                    body.extend_from_slice(&le(data.len() as u32));
                }
                body.extend_from_slice(data);
                while body.len() % 4 != 0 {
                    body.push(0);
                }
            }
            c.extend_from_slice(&le(*prev));
            c.extend_from_slice(&le((8 + body.len()) as u32));
            c.extend_from_slice(&body);
        }
        (c, offsets)
    }

    fn hdr(lines: &[u32], text: &str) -> Vec<u8> {
        let mut d = Vec::new();
        for l in lines {
            d.extend_from_slice(&le(*l));
        }
        d.extend_from_slice(&le(0));
        d.extend_from_slice(text.as_bytes());
        d
    }

    #[test]
    fn offsets_round_trip() {
        for o in [4u32, 32, 1024, 0x3fff_fffc] {
            assert_eq!(decode_offset(encode_offset(o)), o);
        }
        assert_eq!(decode_offset([0, 0, 0, 0]), 0);
        assert_eq!(decode_offset(16u32.to_le_bytes()), 0);
    }

    #[test]
    fn reads_what_dovecot_cached() {
        let tmp = tempfile::tempdir().unwrap();
        let mb = tmp.path();
        // message 1: subject + from in one record; message 2: subject in an older record chained
        // behind a newer one holding from; message 3: nothing cached
        let mut recs = vec![
            (0, vec![(0, hdr(&[3], "Subject: hello there")), (1, le(7).to_vec()), (2, hdr(&[1], "From: Alice <alice@example.org>"))]),
            (0, vec![(0, hdr(&[2], "Subject: a long\n subject line"))]),
        ];
        let (mut c, offs) = cache(42, 7, &mut recs);
        // the newer record for message 2, chained to the older
        let newer_at = c.len() as u32;
        let body = {
            let mut b = Vec::new();
            b.extend_from_slice(&le(2));
            let d = hdr(&[1], "From: bob@example.org");
            b.extend_from_slice(&le(d.len() as u32));
            b.extend_from_slice(&d);
            while b.len() % 4 != 0 {
                b.push(0);
            }
            b
        };
        c.extend_from_slice(&le(offs[1]));
        c.extend_from_slice(&le((8 + body.len()) as u32));
        c.extend_from_slice(&body);
        std::fs::write(mb.join("dovecot.index.cache"), &c).unwrap();
        std::fs::write(
            mb.join("dovecot.index"),
            index(42, 7, &[(10, offs[0]), (11, newer_at), (12, 0)]),
        )
        .unwrap();
        let got = cached_headers(mb, &["subject", "from"]).unwrap();
        assert_eq!(got.fields, ["subject", "from"]);
        assert_eq!(got.uncached, 1);
        assert_eq!(got.messages.len(), 2);
        assert_eq!(got.messages[0].uid, 10);
        assert_eq!(
            got.messages[0].headers,
            [("subject".to_string(), "hello there".to_string()), ("from".to_string(), "Alice <alice@example.org>".to_string())]
        );
        assert_eq!(got.messages[1].uid, 11);
        assert_eq!(
            got.messages[1].headers,
            [("from".to_string(), "bob@example.org".to_string()), ("subject".to_string(), "a long subject line".to_string())]
        );
        // only subjects, when that's all that's wanted
        let got = cached_headers(mb, &["subject"]).unwrap();
        assert!(got.messages.iter().all(|m| m.headers.len() == 1));
        // a cache purged since the index pointed into it isn't trusted
        std::fs::write(mb.join("dovecot.index"), index(42, 8, &[(10, offs[0])])).unwrap();
        assert!(cached_headers(mb, &[]).is_none());
        // nor another index's
        std::fs::write(mb.join("dovecot.index"), index(43, 7, &[(10, offs[0])])).unwrap();
        assert!(cached_headers(mb, &[]).is_none());
        // and garbage is just none
        std::fs::write(mb.join("dovecot.index"), b"not an index at all").unwrap();
        assert!(cached_headers(mb, &[]).is_none());
    }

    #[test]
    fn uidlist_maps_uids_to_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("dovecot-uidlist"),
            "3 V1234567890 N4 G0123456789abcdef\n1 :1500000000.M1P1.host:2,S\n2 W1234 :1500000001.M2P2.host\n3 G0123456789abcdef0123456789abcdef :1500000002.M3P3.host:2,\n",
        )
        .unwrap();
        let l = uidlist(tmp.path()).unwrap();
        assert_eq!(l.len(), 3);
        assert_eq!(l[0], (1, "1500000000.M1P1.host:2,S".to_string()));
        assert_eq!(l[1].1, "1500000001.M2P2.host");
        assert_eq!(l[2].0, 3);
    }
}
