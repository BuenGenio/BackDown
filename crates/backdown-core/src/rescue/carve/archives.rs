//! Archives: ZIP and what's built on it (Office, OpenDocument, EPUB), RAR and 7z.

use std::io;

use crate::magic::{has, le};

use super::*;

// ---------- ZIP and what's built on it (Office, OpenDocument, EPUB)

pub(super) const ZIP_MAX: u64 = 64 * GIB;
/// How far to search for the end record when the entries can't be walked.
pub(super) const ZIP_SEARCH: u64 = 4 * GIB;
/// How much of a central directory is read to see what the archive is.
pub(super) const ZIP_CD_READ: u64 = 16 * MIB;
/// Searching for the end record stops this far past the last ZIP record of any kind.
pub(super) const ZIP_GAP: u64 = 64 * MIB;

#[derive(Debug, Default)]
pub(super) struct ZipNames {
    pub(super) content_types: bool,
    pub(super) word: bool,
    pub(super) xl: bool,
    pub(super) ppt: bool,
    pub(super) container: bool,
    pub(super) mimetype: Option<String>,
}

impl ZipNames {
    fn add(&mut self, name: &[u8]) {
        match name {
            b"[Content_Types].xml" => self.content_types = true,
            b"META-INF/container.xml" => self.container = true,
            n if n.starts_with(b"word/") => self.word = true,
            n if n.starts_with(b"xl/") => self.xl = true,
            n if n.starts_with(b"ppt/") => self.ppt = true,
            _ => {}
        }
    }

    fn ext(&self) -> &'static str {
        match self.mimetype.as_deref().map(str::trim) {
            Some("application/vnd.oasis.opendocument.text") => return "odt",
            Some("application/vnd.oasis.opendocument.spreadsheet") => return "ods",
            Some("application/vnd.oasis.opendocument.presentation") => return "odp",
            Some("application/epub+zip") if self.container => return "epub",
            _ => {}
        }
        match (self.content_types, self.word, self.xl, self.ppt) {
            (true, true, _, _) => "docx",
            (true, _, true, _) => "xlsx",
            (true, _, _, true) => "pptx",
            _ => "zip",
        }
    }
}

/// ZIP: walk the entries by their recorded sizes, then the central directory, to the
/// end-of-central-directory record, which has to agree with where the archive starts. Entries whose
/// sizes come after their data (written as a stream) can't be walked: then the end record is
/// searched for. What the entries are called says whether it's a Word document, an OpenDocument…
pub(super) fn zip(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let h = s.get(off, 30)?;
    if h.len() < 30 {
        return Ok(None);
    }
    let (ver, method, csize) = (le(&h, 4, 2), le(&h, 8, 2), le(&h, 18, 4));
    let (nlen, xlen) = (le(&h, 26, 2), le(&h, 28, 2));
    if ver & 0xFF > 63 || !matches!(method, 0..=20 | 93 | 95..=99) || !(1..=1024).contains(&nlen) {
        return Ok(None);
    }
    let name = s.get(off + 30, nlen as usize)?;
    if name.len() < nlen as usize || name.iter().any(|&c| c < 0x20) {
        return Ok(None);
    }
    let mut names = ZipNames::default();
    if &name[..] == b"mimetype" && method == 0 && csize < 128 {
        let d = s.get(off + 30 + nlen + xlen, csize as usize)?;
        names.mimetype = Some(String::from_utf8_lossy(&d).into_owned());
    }
    let limit = off.saturating_add(ZIP_MAX);
    let (mut pos, mut entries, mut in_cd) = (off, 0u64, false);
    let stuck = loop {
        if pos >= s.end {
            break pos;
        }
        let b = s.get(pos, 46)?;
        if b.len() < 4 {
            break pos;
        }
        let next = match &b[..4] {
            b"PK\x03\x04" if !in_cd && b.len() >= 30 => {
                let (fl, n, x) = (le(&b, 6, 2), le(&b, 26, 2), le(&b, 28, 2));
                let mut cs = le(&b, 18, 4);
                let nm = s.get(pos + 30, (n + x) as usize)?;
                if n == 0 || nm.len() < (n + x) as usize {
                    break pos;
                }
                let nm_only = &nm[..n as usize];
                names.add(nm_only);
                let mut z64 = false;
                if cs == 0xFFFF_FFFF {
                    match zip64_csize(&nm[n as usize..]) {
                        Some(c) => (cs, z64) = (c, true),
                        None => break pos,
                    }
                }
                if fl & 8 != 0 && cs == 0 && !nm_only.ends_with(b"/") {
                    entries += 1;
                    break pos; // its size comes after the data
                }
                let mut e = pos + 30 + n + x + cs;
                if fl & 8 != 0 {
                    let d = s.get(e, 4)?;
                    e += if has(&d, 0, b"PK\x07\x08") { 4 } else { 0 } + if z64 { 20 } else { 12 };
                }
                entries += 1;
                e
            }
            b"PK\x01\x02" if b.len() >= 46 => {
                in_cd = true;
                let (n, x, c) = (le(&b, 28, 2), le(&b, 30, 2), le(&b, 32, 2));
                names.add(&s.get(pos + 46, n as usize)?);
                pos + 46 + n + x + c
            }
            b"PK\x06\x06" if b.len() >= 12 => pos + 12 + le(&b, 4, 8),
            b"PK\x06\x07" => pos + 20,
            b"PK\x05\x05" if b.len() >= 6 => pos + 6 + le(&b, 4, 2),
            b"PK\x05\x06" => {
                if let Some((end, _, _)) = zip_eocd(s, off, pos)? {
                    return Ok(Some(Hit::new(names.ext(), end - off)));
                }
                break pos;
            }
            _ => break pos,
        };
        if next > limit {
            break pos;
        }
        pos = next;
    };
    if entries == 0 && !in_cd {
        return Ok(None);
    }
    if stuck < s.end
        && let Some((end, cd, cd_size)) = zip_search(s, off, stuck, ZIP_SEARCH)?
    {
        if !in_cd {
            zip_cd_names(s, cd, cd_size, &mut names)?;
        }
        return Ok(Some(Hit::new(names.ext(), end - off)));
    }
    // no end record: cut off, or the rest is elsewhere or gone
    let hit = Hit::new(names.ext(), 0);
    let e = if stuck >= s.end {
        s.end
    } else {
        until_next(s, stuck, ZIP_GAP, true)?
    };
    Ok(Some(if e >= s.end {
        hit.cut(s, off)
    } else {
        hit.partial(off, e, DAMAGED)
    }))
}

/// The compressed size from a local header's ZIP64 extra field.
pub(super) fn zip64_csize(extra: &[u8]) -> Option<u64> {
    let mut i = 0;
    while i + 4 <= extra.len() {
        let (id, n) = (le(extra, i, 2), le(extra, i + 2, 2) as usize);
        if id == 1 {
            return (n >= 16).then(|| le(extra, i + 12, 8));
        }
        i += 4 + n;
    }
    None
}

/// If the end-of-central-directory record at `at` belongs to the archive starting at `off` (its
/// central directory sits right before it, or before the ZIP64 records, and starts where it says
/// relative to `off`): where the archive ends, and where its central directory is and how big.
pub(super) fn zip_eocd(s: &Src, off: u64, at: u64) -> io::Result<Option<(u64, u64, u64)>> {
    let e = s.get(at, 22)?;
    if e.len() < 22 || !matches!(le(&e, 4, 2), 0 | 0xFFFF) {
        return Ok(None);
    }
    let (mut cd_size, mut cd_off) = (le(&e, 12, 4), le(&e, 16, 4));
    let mut cd_end = at;
    if cd_size == 0xFFFF_FFFF || cd_off == 0xFFFF_FFFF || le(&e, 10, 2) == 0xFFFF {
        if at < off + 20 {
            return Ok(None);
        }
        let l = s.get(at - 20, 20)?;
        if !has(&l, 0, b"PK\x06\x07") {
            return Ok(None);
        }
        let rec = off + le(&l, 8, 8);
        let r = s.get(rec, 56)?;
        if !has(&r, 0, b"PK\x06\x06") || r.len() < 56 {
            return Ok(None);
        }
        (cd_size, cd_off, cd_end) = (le(&r, 40, 8), le(&r, 48, 8), rec);
    }
    let cd = off.saturating_add(cd_off);
    if cd.saturating_add(cd_size) != cd_end {
        return Ok(None);
    }
    if cd_size > 0 && !has(&s.get(cd, 4)?, 0, b"PK\x01\x02") {
        return Ok(None);
    }
    Ok(Some((at + 22 + le(&e, 20, 2), cd, cd_size)))
}

/// Search from `from` for an end record that belongs to the archive at `off` (giving up where the
/// ZIP records of any kind stop).
pub(super) fn zip_search(
    s: &Src,
    off: u64,
    from: u64,
    max: u64,
) -> io::Result<Option<(u64, u64, u64)>> {
    let limit = from.saturating_add(max).min(s.end);
    let mut st = Stream::new(*s, from);
    let (mut p, mut last_sig) = (from, from);
    while p < limit && p <= last_sig + ZIP_GAP {
        st.seek(p);
        let w = st.peek(128 << 10)?;
        let len = w.len().min((limit - p) as usize);
        if len < 4 {
            break;
        }
        let mut i = 0;
        while let Some(q) = find(&w[..len], b"PK", i) {
            match w.get(q + 2..q + 4) {
                Some([5, 6]) => {
                    if let Some(r) = zip_eocd(s, off, p + q as u64)? {
                        return Ok(Some(r));
                    }
                }
                Some([3, 4] | [7, 8] | [1, 2]) => last_sig = p + q as u64,
                _ => {}
            }
            i = q + 1;
        }
        p += (len - 3) as u64;
    }
    Ok(None)
}

pub(super) fn zip_cd_names(s: &Src, cd: u64, size: u64, names: &mut ZipNames) -> io::Result<()> {
    let b = s.get(cd, size.min(ZIP_CD_READ) as usize)?;
    let mut i = 0;
    while has(&b, i, b"PK\x01\x02") && i + 46 <= b.len() {
        let (n, x, c) = (
            le(&b, i + 28, 2) as usize,
            le(&b, i + 30, 2) as usize,
            le(&b, i + 32, 2) as usize,
        );
        if let Some(nm) = b.get(i + 46..i + 46 + n) {
            names.add(nm);
        }
        i += 46 + n + x + c;
    }
    Ok(())
}

// ---------- RAR, 7z

pub(super) const RAR_MAX: u64 = 64 * GIB;

/// RAR (versions 4 and 5): block after block, each header's CRC checked, up to the end-of-archive
/// block.
pub(super) fn rar(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let h = s.get(off, 8)?;
    if has(&h, 0, b"Rar!\x1a\x07\x00") {
        rar4(s, off)
    } else if has(&h, 0, b"Rar!\x1a\x07\x01\x00") {
        rar5(s, off)
    } else {
        Ok(None)
    }
}

pub(super) fn rar4(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let (mut pos, mut files) = (off + 7, 0u64);
    loop {
        if pos - off > RAR_MAX {
            return Ok(None);
        }
        let b = s.get(pos, 7)?;
        if b.len() < 7 {
            break;
        }
        let (typ, flags, hs) = (b[2], le(&b, 3, 2), le(&b, 5, 2));
        if !(0x72..=0x7B).contains(&typ) || hs < 7 {
            break;
        }
        let head = s.get(pos, hs as usize)?;
        if head.len() < hs as usize {
            return Ok((files > 0).then(|| Hit::new("rar", 0).cut(s, off)));
        }
        if u64::from(crc32(&head[2..]) & 0xFFFF) != le(&b, 0, 2) {
            break;
        }
        let mut add = 0;
        if typ == 0x74 || typ == 0x7A {
            add = le(&head, 7, 4);
            if flags & 0x100 != 0 && hs >= 36 {
                add |= le(&head, 32, 4) << 32;
            }
            files += u64::from(typ == 0x74);
        } else if flags & 0x8000 != 0 && hs >= 11 {
            add = le(&head, 7, 4);
        }
        pos += hs + add;
        if typ == 0x7B {
            return Ok(Some(Hit::new("rar", pos - off)));
        }
    }
    // old versions don't always end with an end block
    Ok((files > 0).then(|| Hit::new("rar", pos - off)))
}

/// A RAR5 variable-length number.
pub(super) fn vint5(b: &[u8]) -> Option<(u64, usize)> {
    let mut v = 0u64;
    for (i, &c) in b.iter().enumerate().take(10) {
        v |= u64::from(c & 0x7F) << (7 * i);
        if c & 0x80 == 0 {
            return Some((v, i + 1));
        }
    }
    None
}

pub(super) fn rar5(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let (mut pos, mut blocks) = (off + 8, 0u64);
    loop {
        if pos - off > RAR_MAX {
            return Ok(None);
        }
        let b = s.get(pos, 16)?;
        if b.len() < 7 {
            break;
        }
        let Some((hs, n)) = vint5(&b[4..]).filter(|&(hs, _)| hs > 0 && hs <= 2 * MIB) else {
            break;
        };
        let head = s.get(pos + 4, n + hs as usize)?;
        if head.len() < n + hs as usize {
            return Ok((blocks > 0).then(|| Hit::new("rar", 0).cut(s, off)));
        }
        if u64::from(crc32(&head)) != le(&b, 0, 4) {
            break;
        }
        let body = &head[n..];
        let Some((typ, a)) = vint5(body) else { break };
        let Some((flags, c)) = vint5(&body[a..]) else {
            break;
        };
        let mut i = a + c;
        if flags & 1 != 0 {
            let Some((_, k)) = vint5(&body[i..]) else {
                break;
            };
            i += k;
        }
        let data = if flags & 2 != 0 {
            let Some((d, _)) = vint5(&body[i..]) else {
                break;
            };
            d
        } else {
            0
        };
        if !(1..=5).contains(&typ) {
            break;
        }
        pos += 4 + n as u64 + hs + data;
        blocks += 1;
        if typ == 5 {
            return Ok(Some(Hit::new("rar", pos - off)));
        }
    }
    Ok((blocks > 0).then(|| Hit::new("rar", 0).partial(off, pos, DAMAGED)))
}

pub(super) const SEVENZ_MAX: u64 = 64 * GIB;

/// 7z: the start header (checked by its CRC) says where the archive's last header is and how long.
pub(super) fn sevenz(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let h = s.get(off, 32)?;
    if h.len() < 32 || h[6] != 0 || h[7] > 4 || u64::from(crc32(&h[12..32])) != le(&h, 8, 4) {
        return Ok(None);
    }
    let (next_off, next_size) = (le(&h, 12, 8), le(&h, 20, 8));
    let size = 32u64.saturating_add(next_off).saturating_add(next_size);
    if next_size == 0 || size > SEVENZ_MAX {
        return Ok(None);
    }
    let mut hit = Hit::new("7z", size);
    if next_size <= 16 * MIB && off + size <= s.end {
        let t = s.get(off + 32 + next_off, next_size as usize)?;
        if u64::from(crc32(&t)) != le(&h, 28, 4) {
            hit = hit.partial(off, off + size, DAMAGED);
        }
    }
    Ok(Some(hit))
}
