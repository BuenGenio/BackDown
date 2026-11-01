//! Documents: PDF, OLE2 compound files (old Office, Outlook .msg) and SQLite databases.

use std::io;

use crate::magic::{be, le, utf16};

use super::*;

// ---------- PDF

pub(super) const PDF_MAX: u64 = GIB;
/// With no end found yet, give up this far past another file's signature.
pub(super) const PDF_PAST_HEADER: u64 = 64 * MIB;
/// Every PDF object ends with `endobj`: this far without one, it's not PDF any more.
pub(super) const PDF_GAP: u64 = 64 * MIB;

/// PDF: the last `%%EOF` whose `startxref` points back at a cross-reference table of this very
/// file (so another PDF's end isn't taken for this one's). One followed by more PDF (an
/// incremental update appends objects, a new table and a new `%%EOF`) isn't the end yet; one
/// followed by anything else, or by another file's signature at a sector boundary, is.
pub(super) fn pdf(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let h = s.get(off, 16)?;
    if h.len() < 8 || !(h[5].is_ascii_digit() && h[6] == b'.' && h[7].is_ascii_digit()) {
        return Ok(None);
    }
    let limit = off.saturating_add(PDF_MAX).min(s.end);
    let mut st = Stream::new(*s, off);
    let mut good: Option<u64> = None;
    let mut first_hdr: Option<u64> = None;
    let mut stop_at: Option<u64> = None;
    let (mut zeros, mut zeros_at) = (0u64, 0u64);
    let mut nb = s.align_up(off + 1);
    let mut p = off + 5;
    let mut last_obj = off;
    'scan: while p < limit {
        st.seek(p);
        let w = st.peek(128 << 10)?;
        let len = w.len().min((limit - p) as usize);
        if len == 0 {
            break;
        }
        let last = p + len as u64 >= limit || len < 128;
        // positions fully judged in this window
        let u = if last { len } else { len - 64 };
        let mut i = 0;
        loop {
            let q = find(&w[..(u + 4).min(len)], b"%%EOF", i).filter(|&q| q < u);
            let upto = p + q.unwrap_or(u) as u64;
            while nb < upto {
                let b = &w[(nb - p) as usize..len];
                if strong(b) {
                    match good {
                        Some(g) if nb >= g => break 'scan,
                        Some(_) => {}
                        None => {
                            first_hdr.get_or_insert(nb);
                        }
                    }
                }
                if good.is_none() {
                    let z = &b[..b.len().min(s.sector as usize)];
                    if z.iter().all(|&c| c == 0) {
                        if zeros == 0 {
                            zeros_at = nb;
                        }
                        zeros += s.sector;
                        if zeros >= MIB {
                            stop_at = Some(zeros_at);
                            break 'scan;
                        }
                    } else {
                        zeros = 0;
                    }
                }
                nb += s.sector;
            }
            let Some(q) = q else { break };
            if let Some(e) = pdf_eof(s, off, p + q as u64)? {
                good = Some(e);
                if !pdf_continues(s, e)? {
                    break 'scan;
                }
            }
            i = q + 5;
        }
        if good.is_none() && first_hdr.is_some_and(|f| p + u as u64 > f + PDF_PAST_HEADER) {
            stop_at = first_hdr;
            break;
        }
        if let Some(k) = rfind(&w[..(u + 6).min(len)], b"endobj") {
            last_obj = last_obj.max(p + k as u64 + 6);
        }
        if p + u as u64 > last_obj + PDF_GAP {
            if good.is_none() {
                stop_at = Some(s.align_up(last_obj).min(limit));
            }
            break;
        }
        p += u as u64;
        if last {
            break;
        }
    }
    let hit = Hit::new("pdf", 0);
    Ok(Some(match good {
        Some(e) => Hit::new("pdf", pdf_comments(s, e)? - off),
        None => {
            let end = stop_at.or(first_hdr).unwrap_or(limit);
            if end >= s.end {
                hit.cut(s, off)
            } else if end >= limit {
                hit.partial(off, end, capped(PDF_MAX))
            } else {
                hit.partial(off, end, NO_END)
            }
        }
    }))
}

/// Where the `%%EOF` at `at` ends (with its line ending), if the `startxref` just before it points
/// back at a cross-reference table (or stream) of the PDF starting at `off`.
pub(super) fn pdf_eof(s: &Src, off: u64, at: u64) -> io::Result<Option<u64>> {
    let back = (at - off).min(128) as usize;
    let b = s.get(at - back as u64, back + 7)?;
    let pre = &b[..back];
    let Some(k) = rfind(pre, b"startxref") else {
        return Ok(None);
    };
    let num = pre[k + 9..].trim_ascii();
    if num.is_empty() || num.len() > 12 || !num.iter().all(u8::is_ascii_digit) {
        return Ok(None);
    }
    let x: u64 = std::str::from_utf8(num)
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    if x < 9 || off + x >= at {
        return Ok(None); // 0 in a linearized file's first trailer
    }
    let t = s.get(off + x, 32)?;
    if !(t.starts_with(b"xref") || is_obj(&t)) {
        return Ok(None);
    }
    let tail = b.get(back + 5..).unwrap_or_default();
    let eol = if tail.starts_with(b"\r\n") {
        2
    } else if matches!(tail.first(), Some(b'\n' | b'\r')) {
        1
    } else {
        0
    };
    Ok(Some(at + 5 + eol))
}

/// "12 0 obj"
pub(super) fn is_obj(t: &[u8]) -> bool {
    let mut i = 0;
    let digits = |i: &mut usize| {
        let s = *i;
        while t.get(*i).is_some_and(u8::is_ascii_digit) {
            *i += 1;
        }
        *i > s
    };
    let spaces = |i: &mut usize| {
        let s = *i;
        while t.get(*i).is_some_and(u8::is_ascii_whitespace) {
            *i += 1;
        }
        *i > s
    };
    digits(&mut i)
        && spaces(&mut i)
        && digits(&mut i)
        && spaces(&mut i)
        && t[i..].starts_with(b"obj")
}

/// Comment lines some writers add after the last `%%EOF` (CUPS: `%%#PDF-BANNER`…): kept, so the
/// file comes back byte for byte.
pub(super) fn pdf_comments(s: &Src, mut e: u64) -> io::Result<u64> {
    let b = s.get(e, 4096)?;
    let mut i = 0;
    while b.get(i) == Some(&b'%') && !b[i..].starts_with(b"%PDF") {
        let Some(n) = b[i..].iter().position(|&c| c == b'\n') else {
            break;
        };
        if !b[i..i + n]
            .iter()
            .all(|&c| c == b'\t' || c == b'\r' || (0x20..0x7F).contains(&c))
        {
            break;
        }
        i += n + 1;
        e += n as u64 + 1;
    }
    Ok(e)
}

/// After a `%%EOF`: more of the same PDF (an update's objects, table or comment), or something else?
pub(super) fn pdf_continues(s: &Src, e: u64) -> io::Result<bool> {
    let b = s.get(e, 64)?;
    let t = b.trim_ascii_start();
    Ok(match t.first() {
        Some(c) if c.is_ascii_digit() => true,
        Some(b'%') => !t.starts_with(b"%PDF"),
        _ => t.starts_with(b"xref"),
    })
}

// ---------- OLE2 compound files (old Office, Outlook .msg)

pub(super) const CFB_MAX: u64 = GIB;
pub(super) const FREESECT: u64 = 0xFFFF_FFFF;
pub(super) const MAXREGSECT: u64 = 0xFFFF_FFFA;

/// Compound files: the size is where the highest sector in use (by the FAT) ends; what the root's
/// streams are called says what it is.
pub(super) fn cfb(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let h = s.get(off, 512)?;
    if h.len() < 512 {
        return Ok(None);
    }
    let (major, shift) = (le(&h, 26, 2), le(&h, 30, 2));
    if le(&h, 28, 2) != 0xFFFE
        || le(&h, 32, 2) != 6
        || le(&h, 56, 4) != 4096
        || !matches!((major, shift), (3, 9) | (4, 12))
    {
        return Ok(None);
    }
    let ss = 1u64 << shift;
    let per = ss / 4;
    let (nfat, first_dir, first_difat, ndifat) =
        (le(&h, 44, 4), le(&h, 48, 4), le(&h, 68, 4), le(&h, 72, 4));
    let max_sectors = CFB_MAX / ss;
    if nfat == 0 || nfat > max_sectors / per + 1 || ndifat > max_sectors / per + 1 {
        return Ok(None);
    }
    let sector = |n: u64| off + (n + 1) * ss;
    // the FAT's own sectors: the first 109 listed in the header, the rest in a chain of DIFAT sectors
    let mut fat_secs: Vec<u64> = (0..nfat.min(109))
        .map(|i| le(&h, 76 + 4 * i as usize, 4))
        .collect();
    let (mut d, mut hops) = (first_difat, 0);
    while (fat_secs.len() as u64) < nfat && d < MAXREGSECT && hops <= ndifat {
        let b = s.get(sector(d), ss as usize)?;
        if b.len() < ss as usize {
            return Ok(Some(Hit::new("ole", 0).cut(s, off)));
        }
        for i in 0..(per - 1) as usize {
            if (fat_secs.len() as u64) < nfat {
                fat_secs.push(le(&b, 4 * i, 4));
            }
        }
        d = le(&b, (ss - 4) as usize, 4);
        hops += 1;
    }
    if (fat_secs.len() as u64) < nfat {
        return Ok(None);
    }
    let entries = nfat * per;
    let mut fat: Vec<u32> = Vec::with_capacity(entries as usize);
    let mut hi = 0u64;
    for &fs in &fat_secs {
        if fs >= max_sectors {
            return Ok(None);
        }
        let b = s.get(sector(fs), ss as usize)?;
        if b.len() < ss as usize {
            return Ok(Some(Hit::new("ole", 0).cut(s, off)));
        }
        for v in b.as_chunks::<4>().0.iter().map(|c| le(c, 0, 4)) {
            if v != FREESECT {
                if v < MAXREGSECT && v >= entries {
                    return Ok(None); // points past the FAT
                }
                hi = fat.len() as u64;
            }
            fat.push(v as u32);
        }
    }
    // the FAT's first sector is marked as one
    if fat.get(fat_secs[0] as usize) != Some(&0xFFFF_FFFD) {
        return Ok(None);
    }
    let size = (hi + 2) * ss;
    if size > CFB_MAX {
        return Ok(None);
    }
    let Some(ext) = cfb_kind(s, off, ss, first_dir, &fat)? else {
        return Ok(None);
    };
    Ok(Some(Hit::new(ext, size)))
}

/// What a compound file is, from the names of the streams in its root.
pub(super) fn cfb_kind(
    s: &Src,
    off: u64,
    ss: u64,
    first_dir: u64,
    fat: &[u32],
) -> io::Result<Option<&'static str>> {
    struct Entry {
        name: String,
        typ: u8,
        left: u64,
        right: u64,
        child: u64,
    }
    let mut ents: Vec<Entry> = Vec::new();
    let (mut sec, mut hops) = (first_dir, 0);
    while sec < MAXREGSECT && hops < 64 {
        let b = s.get(off + (sec + 1) * ss, ss as usize)?;
        if b.len() < ss as usize {
            break;
        }
        for e in b.as_chunks::<128>().0 {
            let nl = (le(e, 64, 2) as usize).min(64);
            ents.push(Entry {
                name: utf16(&e[..nl], false),
                typ: e[66],
                left: le(e, 68, 4),
                right: le(e, 72, 4),
                child: le(e, 76, 4),
            });
        }
        sec = fat.get(sec as usize).map_or(FREESECT, |&v| u64::from(v));
        hops += 1;
    }
    let Some(root) = ents.first().filter(|r| r.typ == 5) else {
        return Ok(None);
    };
    // the root's children: a tree of siblings
    let mut names: Vec<&str> = Vec::new();
    let mut todo = vec![root.child];
    while let Some(i) = todo.pop() {
        if i >= MAXREGSECT || names.len() > 4096 {
            continue;
        }
        let Some(e) = ents.get(i as usize) else {
            continue;
        };
        names.push(&e.name);
        todo.push(e.left);
        todo.push(e.right);
    }
    let named = |n: &str| names.contains(&n);
    Ok(Some(
        if names.iter().any(|n| n.starts_with("__substg1.0_")) {
            "msg"
        } else if named("WordDocument") {
            "doc"
        } else if named("PowerPoint Document") {
            "ppt"
        } else if named("Workbook") || named("Book") {
            "xls"
        } else {
            "ole"
        },
    ))
}

// ---------- SQLite

pub(super) const SQLITE_MAX: u64 = 16 * GIB;

/// SQLite: page size × page count from the header, when the header's size is known to be current
/// (its "valid-for" counter matches the change counter).
pub(super) fn sqlite(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let h = s.get(off, 101)?;
    if h.len() < 101 {
        return Ok(None);
    }
    let ps = be(&h, 16, 2);
    let page = if ps == 1 { 65_536 } else { ps };
    if page < 512
        || !page.is_power_of_two()
        || !(1..=2).contains(&h[18])
        || !(1..=2).contains(&h[19])
        || h[21] != 64
        || h[22] != 32
        || h[23] != 32
        || be(&h, 44, 4) > 4
        || be(&h, 56, 4) > 3
        || !matches!(h[100], 0x0D | 0x05)
    {
        return Ok(None);
    }
    let (changes, count, valid) = (be(&h, 24, 4), be(&h, 28, 4), be(&h, 92, 4));
    if count > 0 && valid == changes {
        let size = page * count;
        return Ok((size <= SQLITE_MAX).then(|| Hit::new("sqlite", size)));
    }
    // an old writer that didn't keep the size in the header
    let e = until_next(s, off + page, OPEN_MAX, false)?;
    Ok(Some(Hit::new("sqlite", 0).partial(off, e, NO_END)))
}
