//! Archives: a zip's central directory read record by record (never all at once), compressed
//! streams (gzip, bzip2, xz, zstd) and tar, and one entry's bytes.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use backdown_core::fmt;
use backdown_core::magic::{has, le, read_at};
use serde_json::{Value, json};

use super::*;

// ---- zip: the central directory, read record by record (never all at once)

pub(super) struct ZipEntry {
    pub(super) name: String,
    pub(super) dir: bool,
    pub(super) size: u64,
    pub(super) csize: u64,
    pub(super) method: u64,
    pub(super) flags: u64,
    /// the local header's absolute position
    pub(super) header: u64,
    pub(super) mtime: Option<i64>,
}

pub(super) struct ZipDir {
    pub(super) entries: u64,
    pub(super) start: u64,
    pub(super) size: u64,
    /// bytes in front of the archive proper (a self-extractor's stub), added to every offset
    pub(super) shift: u64,
}

/// The end-of-central-directory record (zip64 aware), or None if this isn't a zip.
pub(super) fn zip_dir(f: &File, len: u64) -> Result<Option<ZipDir>> {
    if len < 22 {
        return Ok(None);
    }
    let tail_len = len.min(22 + 0xFFFF);
    let base = len - tail_len;
    let tail = read_at(f, base, tail_len as usize)?;
    let Some(i) = (0..=tail.len().saturating_sub(22)).rev().find(|&i| {
        has(&tail, i, b"PK\x05\x06") && i + 22 + le(&tail, i + 20, 2) as usize <= tail.len()
    }) else {
        return Ok(None);
    };
    let eocd = base + i as u64;
    let (mut entries, mut size, mut offset, mut end) = (
        le(&tail, i + 10, 2),
        le(&tail, i + 12, 4),
        le(&tail, i + 16, 4),
        eocd,
    );
    if eocd >= 20 {
        let loc = read_at(f, eocd - 20, 20)?;
        if has(&loc, 0, b"PK\x06\x07") {
            // the zip64 record sits right before its locator, unless it carries extra data
            for pos in [(eocd - 20).saturating_sub(56), le(&loc, 8, 8)] {
                let rec = read_at(f, pos, 56)?;
                if has(&rec, 0, b"PK\x06\x06") {
                    (entries, size, offset, end) =
                        (le(&rec, 32, 8), le(&rec, 40, 8), le(&rec, 48, 8), pos);
                    break;
                }
            }
        }
    }
    let Some(start) = end.checked_sub(size) else {
        return Ok(None);
    };
    Ok(Some(ZipDir {
        entries,
        start,
        size,
        shift: start.saturating_sub(offset),
    }))
}

pub(super) fn dos_time(date: u64, time: u64) -> Option<i64> {
    let (m, d) = ((date >> 5) & 15, date & 31);
    if m == 0 || d == 0 {
        return None;
    }
    let day = days_from_civil(1980 + (date >> 9) as i64, m as i64, d as i64);
    Some(day * 86_400 + ((time >> 11) * 3600 + ((time >> 5) & 63) * 60 + (time & 31) * 2) as i64)
}

/// Names from zips made without the UTF-8 flag are in the DOS code page.
pub(super) fn cp437(b: &[u8]) -> String {
    const HIGH: &str = concat!(
        "ÇüéâäàåçêëèïîìÄÅ",
        "ÉæÆôöòûùÿÖÜ¢£¥₧ƒ",
        "áíóúñÑªº¿⌐¬½¼¡«»",
        "░▒▓│┤╡╢╖╕╣║╗╝╜╛┐",
        "└┴┬├─┼╞╟╚╔╩╦╠═╬╧",
        "╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀",
        "αßΓπΣσµτΦΘΩδ∞φε∩",
        "≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}",
    );
    b.iter()
        .map(|&c| {
            if c < 128 {
                c as char
            } else {
                HIGH.chars().nth(c as usize - 128).unwrap_or('?')
            }
        })
        .collect()
}

/// Calls `each` for every central-directory record in order; stops when it returns false or the
/// budget is spent. Returns whether the whole directory was walked.
pub(super) fn zip_walk(
    f: &File,
    d: &ZipDir,
    until: Instant,
    mut each: impl FnMut(ZipEntry) -> bool,
) -> Result<bool> {
    let mut r = f;
    r.seek(SeekFrom::Start(d.start))?;
    let mut r = BufReader::with_capacity(1 << 16, r.take(d.size));
    let mut fixed = [0u8; 46];
    let mut var = Vec::new();
    let mut n = 0u64;
    loop {
        n += 1;
        if n.is_multiple_of(4096) && Instant::now() > until {
            return Ok(false);
        }
        match r.read_exact(&mut fixed) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(true),
            Err(e) => return Err(e.into()),
        }
        if !has(&fixed, 0, b"PK\x01\x02") {
            return Ok(true);
        }
        let (nl, xl, cl) = (
            le(&fixed, 28, 2) as usize,
            le(&fixed, 30, 2) as usize,
            le(&fixed, 32, 2) as usize,
        );
        var.resize(nl + xl + cl, 0);
        r.read_exact(&mut var)?;
        let flags = le(&fixed, 8, 2);
        let mut e = ZipEntry {
            name: String::new(),
            dir: false,
            size: le(&fixed, 24, 4),
            csize: le(&fixed, 20, 4),
            method: le(&fixed, 10, 2),
            flags,
            header: le(&fixed, 42, 4),
            mtime: dos_time(le(&fixed, 14, 2), le(&fixed, 12, 2)),
        };
        let mut unicode = None;
        let mut x = &var[nl..nl + xl];
        while x.len() >= 4 {
            let (id, len) = (le(x, 0, 2), le(x, 2, 2) as usize);
            let body = &x[4..(4 + len).min(x.len())];
            match id {
                // zip64: only the fields whose 32-bit slot is saturated, in this order
                0x0001 => {
                    let mut o = 0;
                    for v in [&mut e.size, &mut e.csize, &mut e.header] {
                        if *v == 0xFFFF_FFFF {
                            *v = le(body, o, 8);
                            o += 8;
                        }
                    }
                }
                // extended timestamp (Info-ZIP): a Unix mtime
                0x5455 if body.first().is_some_and(|b| b & 1 != 0) && body.len() >= 5 => {
                    e.mtime = Some(i64::from(le(body, 1, 4) as u32 as i32));
                }
                // NTFS times (7-Zip, Windows): FILETIME in tag 1
                0x000a if le(body, 4, 2) == 1 && body.len() >= 16 => {
                    let ft = le(body, 8, 8) as i64;
                    e.mtime = Some(ft / 10_000_000 - 11_644_473_600);
                }
                // Info-ZIP Unicode path
                0x7075 if body.first() == Some(&1) && body.len() > 5 => {
                    unicode = std::str::from_utf8(&body[5..]).ok().map(str::to_string);
                }
                _ => {}
            }
            x = &x[(4 + len).min(x.len())..];
        }
        let raw = &var[..nl];
        e.name = unicode.unwrap_or_else(|| {
            if flags & 0x800 != 0 {
                String::from_utf8_lossy(raw).into_owned()
            } else {
                std::str::from_utf8(raw).map_or_else(|_| cp437(raw), str::to_string)
            }
        });
        e.dir = e.name.ends_with('/') || e.name.ends_with('\\');
        e.header += d.shift;
        if !each(e) {
            return Ok(false);
        }
    }
}

pub(super) fn zip_method(m: u64) -> &'static str {
    match m {
        1 => "Shrink",
        6 => "Implode",
        14 => "LZMA",
        98 => "PPMd",
        99 => "AES encryption",
        _ => "unknown",
    }
}

/// The decompressed bytes of one entry (at most its declared size).
pub(super) fn zip_open(f: File, e: &ZipEntry) -> Result<Box<dyn Read + Send>> {
    if e.flags & 1 != 0 || e.method == 99 {
        bail!("{} is encrypted", e.name);
    }
    let lh = read_at(&f, e.header, 30)?;
    if !has(&lh, 0, b"PK\x03\x04") {
        bail!("damaged archive: {} has no local header", e.name);
    }
    let mut f = f;
    f.seek(SeekFrom::Start(
        e.header + 30 + le(&lh, 26, 2) + le(&lh, 28, 2),
    ))?;
    let raw = BufReader::new(f.take(e.csize));
    let r: Box<dyn Read + Send> = match e.method {
        0 => Box::new(raw),
        8 => Box::new(flate2::bufread::DeflateDecoder::new(raw)),
        9 => Box::new(deflate64::Deflate64Decoder::with_buffer(raw)),
        12 => Box::new(bzip2::read::BzDecoder::new(raw)),
        93 => Box::new(Zstd::new(raw)?),
        95 => Box::new(lzma_rust2::XzReader::new_mem_limit(raw, true, XZ_MEM_KB)),
        m => bail!(
            "{} is compressed with a method BackDown can't read ({}, #{m})",
            e.name,
            zip_method(m)
        ),
    };
    Ok(Box::new(r.take(e.size)))
}

/// A zip-based document's parts by name (for slide decks).
pub(super) struct Zip {
    pub(super) file: File,
    pub(super) entries: HashMap<String, ZipEntry>,
}

impl Zip {
    pub(super) fn open(path: &Path) -> Result<Zip> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        let d = zip_dir(&file, len)?.context("not a zip-based document")?;
        let mut entries = HashMap::new();
        zip_walk(&file, &d, Instant::now() + LIST_BUDGET, |e| {
            entries.insert(e.name.clone(), e);
            entries.len() < 100_000
        })?;
        Ok(Zip { file, entries })
    }

    /// Part names are case-insensitive in OOXML; try the exact name first.
    pub(super) fn get(&self, name: &str) -> Option<&ZipEntry> {
        self.entries.get(name).or_else(|| {
            self.entries
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v)
        })
    }

    pub(super) fn read(&self, name: &str, cap: u64) -> Result<Option<Vec<u8>>> {
        let Some(e) = self.get(name) else {
            return Ok(None);
        };
        if e.size > cap {
            bail!("{name} is too large ({})", fmt::bytes(e.size));
        }
        let mut buf = Vec::with_capacity(e.size as usize);
        zip_open(self.file.try_clone()?, e)?.read_to_end(&mut buf)?;
        Ok(Some(buf))
    }
}

// ---- compressed streams and tar

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Codec {
    Plain,
    Gzip,
    Bzip2,
    Xz,
    Zstd,
}

impl Codec {
    pub fn sniff(head: &[u8]) -> Codec {
        if head.starts_with(&[0x1F, 0x8B]) {
            Codec::Gzip
        } else if head.starts_with(b"BZh") {
            Codec::Bzip2
        } else if head.starts_with(&[0xFD, b'7', b'z', b'X', b'Z', 0]) {
            Codec::Xz
        } else if head.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
            Codec::Zstd
        } else {
            Codec::Plain
        }
    }

    pub(super) fn ext(self) -> &'static str {
        match self {
            Codec::Plain => "",
            Codec::Gzip => "gz",
            Codec::Bzip2 => "bz2",
            Codec::Xz => "xz",
            Codec::Zstd => "zst",
        }
    }

    pub fn open(self, f: File) -> Result<Box<dyn Read + Send>> {
        let r = BufReader::with_capacity(1 << 16, f);
        Ok(match self {
            Codec::Plain => Box::new(r),
            Codec::Gzip => Box::new(flate2::bufread::MultiGzDecoder::new(r)),
            Codec::Bzip2 => Box::new(bzip2::read::MultiBzDecoder::new(r)),
            Codec::Xz => Box::new(lzma_rust2::XzReader::new_mem_limit(r, true, XZ_MEM_KB)),
            Codec::Zstd => Box::new(Zstd::new(r)?),
        })
    }
}

/// Zstandard files may hold several frames back to back; the streaming decoder reads one, so this
/// starts a new one at each frame boundary.
pub(super) struct Zstd<R: BufRead> {
    pub(super) dec: Option<ruzstd::decoding::StreamingDecoder<R, ruzstd::decoding::FrameDecoder>>,
}

impl<R: BufRead> Zstd<R> {
    pub(super) fn new(r: R) -> Result<Self> {
        let dec = ruzstd::decoding::StreamingDecoder::new(r)
            .map_err(|e| anyhow::anyhow!("not a Zstandard stream: {e}"))?;
        Ok(Zstd { dec: Some(dec) })
    }
}

impl<R: BufRead> Read for Zstd<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let Some(d) = self.dec.as_mut() else {
                return Ok(0);
            };
            let n = d.read(buf)?;
            if n > 0 || buf.is_empty() {
                return Ok(n);
            }
            let Some(d) = self.dec.take() else {
                return Ok(0);
            };
            let mut src = d.into_inner();
            if src.fill_buf()?.is_empty() {
                return Ok(0);
            }
            self.dec = Some(
                ruzstd::decoding::StreamingDecoder::new(src)
                    .map_err(|e| io::Error::other(e.to_string()))?,
            );
        }
    }
}

/// A tar header block: its checksum adds up (an all-zero block doesn't count).
pub(super) fn is_tar(b: &[u8]) -> bool {
    let Some(h) = b.get(..512) else {
        return false;
    };
    let digits: String = h[148..156]
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as char)
        .collect();
    let Ok(want) = u32::from_str_radix(digits.trim(), 8) else {
        return false;
    };
    let sum: u32 = h
        .iter()
        .enumerate()
        .map(|(i, &c)| {
            if (148..156).contains(&i) {
                32
            } else {
                u32::from(c)
            }
        })
        .sum();
    want == sum && sum != 256
}

pub(super) struct TarWalk {
    pub(super) entries: Vec<Value>,
    pub(super) count: u64,
    pub(super) bytes: u64,
    pub(super) complete: bool,
    pub(super) note: Option<String>,
}

pub(super) fn tar_walk<R: Read>(
    entries: tar::Entries<'_, R>,
    limit: usize,
    until: Instant,
) -> TarWalk {
    let mut w = TarWalk {
        entries: Vec::new(),
        count: 0,
        bytes: 0,
        complete: false,
        note: None,
    };
    for e in entries {
        if Instant::now() > until {
            w.note = Some(STOPPED.into());
            return w;
        }
        let e = match e {
            Ok(e) => e,
            Err(err) => {
                w.note = Some(if err.kind() == io::ErrorKind::TimedOut {
                    STOPPED.into()
                } else {
                    format!("stopped at a damaged or truncated part: {err}")
                });
                return w;
            }
        };
        let t = e.header().entry_type();
        if t.is_pax_global_extensions() || t.is_pax_local_extensions() {
            continue;
        }
        w.count += 1;
        if t.is_file() || t.is_contiguous() || t.is_gnu_sparse() {
            w.bytes += e.size();
        }
        if w.entries.len() < limit {
            let link = if t.is_symlink() || t.is_hard_link() {
                e.link_name_bytes()
                    .map(|l| String::from_utf8_lossy(&l).into_owned())
            } else {
                None
            };
            w.entries.push(json!({
                "name": String::from_utf8_lossy(&e.path_bytes()),
                "size": if t.is_dir() || link.is_some() { 0 } else { e.size() },
                "mtime": e.header().mtime().ok(),
                "dir": t.is_dir(),
                "link": link,
            }));
        }
    }
    w.complete = true;
    w
}

/// The name a lone compressed file had: the gzip header's, else ours minus the extension.
pub(super) fn single_name(path: &Path, codec: Codec, head: &[u8]) -> String {
    if codec == Codec::Gzip && head.get(3).is_some_and(|f| f & 8 != 0) {
        let mut i = 10;
        if head[3] & 4 != 0 {
            i += 2 + le(head, 10, 2) as usize;
        }
        if let Some(rest) = head.get(i..)
            && let Some(end) = rest.iter().position(|&c| c == 0)
        {
            let n = String::from_utf8_lossy(&rest[..end]).into_owned();
            let n = leaf(&n).to_string();
            if !n.is_empty() {
                return n;
            }
        }
    }
    let own = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = own
        .rsplit_once('.')
        .filter(|(_, x)| {
            matches!(
                x.to_ascii_lowercase().as_str(),
                "gz" | "gzip" | "z" | "bz2" | "bz" | "xz" | "zst" | "zstd"
            )
        })
        .map_or(own.as_str(), |(s, _)| s);
    if stem.is_empty() {
        "data".into()
    } else {
        stem.to_string()
    }
}

/// An archive's entries: zip, tar, tar in gzip/bzip2/xz/zstd, or a single compressed file.
/// `quick`: a short time budget, for a preview.
pub fn archive(path: &Path, limit: usize, quick: bool) -> Result<Value> {
    let limit = limit.clamp(1, MAX_ENTRIES);
    let until = Instant::now() + if quick { QUICK_BUDGET } else { LIST_BUDGET };
    let f = File::open(path)?;
    let len = f.metadata()?.len();
    let head = read_at(&f, 0, 4096)?;
    let codec = Codec::sniff(&head);
    if codec == Codec::Plain {
        if is_tar(&head) {
            let mut ar = tar::Archive::new(File::open(path)?);
            let w = tar_walk(ar.entries_with_seek()?, limit, until);
            return Ok(tar_json("tar", w));
        }
        if let Some(d) = zip_dir(&f, len)? {
            let (mut entries, mut count, mut bytes) = (Vec::new(), 0u64, 0u64);
            let complete = zip_walk(&f, &d, until, |e| {
                count += 1;
                bytes += e.size;
                if entries.len() < limit {
                    entries.push(json!({
                        "name": e.name, "size": e.size, "csize": e.csize, "mtime": e.mtime,
                        "dir": e.dir, "encrypted": e.flags & 1 != 0 || e.method == 99,
                    }));
                }
                true
            })?;
            return Ok(json!({
                "format": "zip",
                "entries": entries,
                "total": d.entries.max(count),
                "total_size": bytes,
                "complete": complete,
                "note": (!complete).then_some(STOPPED),
            }));
        }
        bail!("not an archive BackDown can read");
    }
    // its own handle: a try_clone would share the file position with `f`
    let mut r = codec.open(File::open(path)?)?;
    let mut first = vec![0u8; 512];
    let n = fill(&mut r, &mut first).context("can't decompress this file")?;
    first.truncate(n);
    if is_tar(&first) {
        let stream = Deadline {
            inner: io::Cursor::new(first).chain(r),
            until,
        };
        let mut ar = tar::Archive::new(stream);
        let w = tar_walk(ar.entries()?, limit, until);
        return Ok(tar_json(&format!("tar.{}", codec.ext()), w));
    }
    // one compressed file: its name, and its size if cheap (gzip says it) or countable in time
    let name = single_name(path, codec, &head);
    let size = if codec == Codec::Gzip && len >= 18 {
        Some(le(&read_at(&f, len - 4, 4)?, 0, 4))
    } else {
        let mut count = n as u64;
        let mut buf = vec![0u8; 1 << 16];
        loop {
            if Instant::now() > until {
                break None;
            }
            match r.read(&mut buf) {
                Ok(0) => break Some(count),
                Ok(k) => count += k as u64,
                Err(_) => break None,
            }
        }
    };
    let mtime = (codec == Codec::Gzip)
        .then(|| le(&head, 4, 4))
        .filter(|&t| t > 0);
    Ok(json!({
        "format": codec.ext(),
        "entries": [{ "name": name, "size": size, "mtime": mtime, "dir": false }],
        "total": 1,
        "total_size": size.unwrap_or(0),
        "complete": true,
        "note": if codec == Codec::Gzip && len > u64::from(u32::MAX) {
            Some("gzip records sizes modulo 4 GiB; this one is larger")
        } else if size.is_none() {
            Some("uncompressed size unknown (too large to count here)")
        } else {
            None
        },
    }))
}

pub(super) fn tar_json(format: &str, w: TarWalk) -> Value {
    json!({
        "format": format,
        "entries": w.entries,
        "total": w.complete.then_some(w.count),
        "counted": w.count,
        "total_size": w.bytes,
        "complete": w.complete,
        "note": w.note,
    })
}

/// One entry's bytes, chosen by its name exactly as the listing gave it (never a filesystem path).
/// At most `max` bytes (and never more than `ENTRY_CAP`); without `max`, an entry larger than the
/// cap is refused rather than cut short.
pub fn entry(path: &Path, name: &str, max: Option<u64>) -> Result<Part> {
    let f = File::open(path)?;
    let len = f.metadata()?.len();
    let head = read_at(&f, 0, 4096)?;
    let cap = max.unwrap_or(ENTRY_CAP).min(ENTRY_CAP);
    let fits = |size: u64| -> Result<u64> {
        if size > ENTRY_CAP && max.is_none() {
            bail!(
                "{} is too large to open from inside the archive ({}; the limit is {})",
                leaf(name),
                fmt::bytes(size),
                fmt::bytes(ENTRY_CAP)
            );
        }
        Ok(size.min(cap))
    };
    let missing = || anyhow::anyhow!("no entry named {name:?} in this archive");
    let codec = Codec::sniff(&head);
    if codec == Codec::Plain {
        if is_tar(&head) {
            let mut ar = tar::Archive::new(File::open(path)?);
            let mut found = None;
            for e in ar.entries_with_seek()? {
                let mut e = e?;
                if String::from_utf8_lossy(&e.path_bytes()) != name {
                    continue;
                }
                let t = e.header().entry_type();
                if t.is_dir() {
                    bail!("{name} is a folder");
                }
                if t.is_file() || t.is_contiguous() {
                    found = Some((e.raw_file_position(), e.size()));
                } else {
                    // links and sparse files: read through the tar reader
                    let n = fits(e.size())?;
                    let mut buf = Vec::new();
                    (&mut e).take(n).read_to_end(&mut buf)?;
                    return Ok(part(name, buf));
                }
                break;
            }
            let (pos, size) = found.ok_or_else(missing)?;
            let n = fits(size)?;
            let mut f = f;
            f.seek(SeekFrom::Start(pos))?;
            return Ok(Part {
                name: leaf(name).to_string(),
                len: Some(n),
                body: Box::new(f.take(n)),
            });
        }
        if let Some(v) = iso_volume(&f)? {
            let e = iso_find(&f, &v, name)?;
            if e.dir {
                bail!("{name} is a folder");
            }
            let n = fits(e.size)?;
            let mut f = f;
            f.seek(SeekFrom::Start(e.lba * v.block))?;
            return Ok(Part {
                name: e.name,
                len: Some(n),
                body: Box::new(f.take(n)),
            });
        }
        if let Some(d) = zip_dir(&f, len)? {
            let mut hit = None;
            zip_walk(&f, &d, Instant::now() + SCAN_BUDGET, |e| {
                if e.name == name {
                    hit = Some(e);
                    false
                } else {
                    true
                }
            })?;
            let e = hit.ok_or_else(missing)?;
            if e.dir {
                bail!("{name} is a folder");
            }
            let n = fits(e.size)?;
            return Ok(Part {
                name: leaf(name).to_string(),
                len: Some(n),
                body: Box::new(zip_open(f, &e)?.take(n)),
            });
        }
        bail!("not an archive BackDown can read");
    }
    let mut r = codec.open(File::open(path)?)?;
    let mut first = vec![0u8; 512];
    let got = fill(&mut r, &mut first).context("can't decompress this file")?;
    first.truncate(got);
    if is_tar(&first) {
        // sequential: decompress up to the entry (within the scan budget), then read it whole
        let stream = Deadline {
            inner: io::Cursor::new(first).chain(r),
            until: Instant::now() + SCAN_BUDGET,
        };
        let mut ar = tar::Archive::new(stream);
        for e in ar.entries()? {
            let mut e = e.map_err(|err| {
                if err.kind() == io::ErrorKind::TimedOut {
                    anyhow::anyhow!(
                        "gave up looking for {name} after {} s",
                        SCAN_BUDGET.as_secs()
                    )
                } else {
                    err.into()
                }
            })?;
            if String::from_utf8_lossy(&e.path_bytes()) != name {
                continue;
            }
            if e.header().entry_type().is_dir() {
                bail!("{name} is a folder");
            }
            let n = fits(e.size())?;
            let mut buf = Vec::with_capacity(n as usize);
            (&mut e).take(n).read_to_end(&mut buf)?;
            return Ok(part(name, buf));
        }
        return Err(missing());
    }
    if name != single_name(path, codec, &head) {
        return Err(missing());
    }
    if codec == Codec::Gzip && len >= 18 && len <= u64::from(u32::MAX) {
        fits(le(&read_at(&f, len - 4, 4)?, 0, 4))?;
    }
    Ok(Part {
        name: leaf(name).to_string(),
        len: None,
        body: Box::new(io::Cursor::new(first).chain(r).take(cap)),
    })
}
