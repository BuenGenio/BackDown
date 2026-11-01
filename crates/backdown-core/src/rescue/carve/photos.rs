//! Photos: JPEG (with its EXIF and multi-picture data), TIFF and the camera RAW formats built on
//! it, Fujifilm RAF, PNG, GIF and BMP.

use std::borrow::Cow;
use std::io;

use crate::magic::{be, field, has, le};

use super::*;

// ---------- JPEG

pub(super) const JPEG_MAX: u64 = 256 * MIB;

/// JPEG: the segments up to the first scan, then the compressed data (which never holds an FF
/// byte except as FF 00, a restart marker FF D0–D7, or before the next marker), scan after scan
/// for progressive JPEGs, up to the end-of-image marker. Then what cameras put after it: the
/// larger preview listed in an MPF index, or a motion photo's video.
pub(super) fn jpeg(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let h = s.get(off, 4)?;
    if h.len() < 4 || !matches!(h[3], 0xC0..=0xC7 | 0xC9..=0xCF | 0xDB | 0xDD | 0xE0..=0xEF | 0xFE)
    {
        return Ok(None);
    }
    let cap = off + JPEG_MAX;
    let mut st = Stream::new(*s, off + 2);
    let (mut sof, mut dqt, mut scans) = (false, false, 0u32);
    let mut hit = Hit::new("jpg", 0);
    let mut mpf: Option<(u64, Vec<u8>)> = None;
    // where it stops making sense: worth keeping only once there's image data
    let broken = |hit: Hit, at: u64, scans: u32, why: String| -> Option<Hit> {
        (scans > 0).then(|| hit.partial(off, at, why))
    };
    let eoi = loop {
        let at = st.offset();
        if at >= cap {
            return Ok(broken(hit, at, scans, capped(JPEG_MAX)));
        }
        let b = st.peek(4)?;
        if b.len() < 2 {
            return Ok((scans > 0).then(|| hit.cut(s, off)));
        }
        if b[0] != 0xFF {
            return Ok(broken(hit, at, scans, DAMAGED.into()));
        }
        let m = b[1];
        match m {
            0xFF => {
                st.advance(1); // fill byte
                continue;
            }
            0xD9 if scans > 0 => break at + 2,
            0xD0..=0xD7 | 0x01 if scans > 0 => {
                st.advance(2);
                continue;
            }
            0xC0..=0xCF | 0xDA..=0xFE => {}
            _ => return Ok(broken(hit, at, scans, DAMAGED.into())),
        }
        if b.len() < 4 {
            return Ok((scans > 0).then(|| hit.cut(s, off)));
        }
        let len = be(b, 2, 2);
        if len < 2 {
            return Ok(broken(hit, at, scans, DAMAGED.into()));
        }
        let seg = st.peek(2 + len as usize)?;
        if seg.len() < 2 + len as usize {
            return Ok((scans > 0).then(|| hit.cut(s, off)));
        }
        let p = &seg[4..];
        match m {
            0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => {
                let nc = u64::from(p.get(5).copied().unwrap_or(0));
                if p.len() < 6
                    || !matches!(p[0], 8 | 12 | 16)
                    || !(1..=4).contains(&nc)
                    || len != 8 + 3 * nc
                    || be(p, 3, 2) == 0
                {
                    return Ok(broken(hit, at, scans, DAMAGED.into()));
                }
                if !sof {
                    hit.dims = Some((be(p, 3, 2) as u32, be(p, 1, 2) as u32));
                }
                sof = true;
            }
            0xDB => dqt = true,
            0xE1 if scans == 0 && hit.camera.is_none() && p.starts_with(b"Exif\0\0") => {
                let t = &p[6..];
                if let Some(i) = tiff_walk(&Src::mem(t), 0, t.len() as u64, false)? {
                    hit.taken = i.taken;
                    hit.camera = camera(i.make, i.model);
                }
            }
            0xE2 if mpf.is_none() && p.starts_with(b"MPF\0") => {
                mpf = Some((at + 8, p[4..].to_vec()));
            }
            0xDA => {
                let ns = u64::from(p.first().copied().unwrap_or(0));
                if !sof || !dqt || !(1..=4).contains(&ns) || len != 6 + 2 * ns {
                    return Ok(broken(hit, at, scans, DAMAGED.into()));
                }
                st.advance(2 + len);
                scans += 1;
                if !skip_entropy(&mut st, cap)? {
                    let at = st.offset();
                    return Ok(Some(if at >= cap {
                        hit.partial(off, at, capped(JPEG_MAX))
                    } else {
                        hit.cut(s, off)
                    }));
                }
                continue;
            }
            _ => {}
        }
        st.advance(2 + len);
    };
    let mut end = eoi;
    if let Some((base, t)) = mpf
        && let Some(e) = mpf_end(s, base, &t)?
        && e > end
        && e - off <= JPEG_MAX
    {
        end = e;
    }
    // Google's motion photos: a short video right after the photo
    if has(&s.get(end, 8)?, 4, b"ftyp") {
        let w = iso_walk(s, end, JPEG_MAX)?;
        if w.moov.is_some() && w.mdat && !w.cut && w.open.is_none() {
            end = w.end;
            hit.note = Some("a motion photo: the photo with its short video".into());
        }
    }
    hit.size = end - off;
    Ok(Some(hit))
}

/// Skip compressed image data, leaving the stream on the next marker's FF. False when the data ran
/// out first (the end of the scanned area, or `cap`).
pub(super) fn skip_entropy(st: &mut Stream, cap: u64) -> io::Result<bool> {
    loop {
        if st.offset() >= cap {
            return Ok(false);
        }
        let w = st.peek(2)?;
        if w.len() < 2 {
            let n = w.len() as u64;
            st.advance(n);
            return Ok(false);
        }
        match w[..w.len() - 1].iter().position(|&c| c == 0xFF) {
            None => {
                let n = (w.len() - 1) as u64;
                st.advance(n);
            }
            Some(i) => match w[i + 1] {
                0x00 | 0xD0..=0xD7 => st.advance((i + 2) as u64),
                0xFF => st.advance((i + 1) as u64),
                _ => {
                    st.advance(i as u64);
                    return Ok(true);
                }
            },
        }
    }
}

/// Where the images listed in a JPEG's MPF index end (the photo itself is the one at offset 0;
/// the others follow it). `base` is where the index's TIFF header is in the source.
pub(super) fn mpf_end(s: &Src, base: u64, t: &[u8]) -> io::Result<Option<u64>> {
    let e = match t.get(..4) {
        Some(b"II*\0") => End(true),
        Some(b"MM\0*") => End(false),
        _ => return Ok(None),
    };
    let ifd = e.u(t, 4, 4) as usize;
    let n = e.u(t, ifd, 2) as usize;
    let mut end = None;
    for i in 0..n.min(64) {
        let en = ifd + 2 + 12 * i;
        if e.u(t, en, 2) != 0xB002 {
            continue;
        }
        let (count, at) = (e.u(t, en + 4, 4) as usize, e.u(t, en + 8, 4) as usize);
        let Some(list) = at.checked_add(count).and_then(|x| t.get(at..x)) else {
            return Ok(None);
        };
        for x in list.as_chunks::<16>().0 {
            let (size, o) = (e.u(x, 4, 4), e.u(x, 8, 4));
            if o == 0 {
                continue;
            }
            if !has(&s.get(base + o, 2)?, 0, &[0xFF, 0xD8]) {
                return Ok(None);
            }
            end = end.max(Some(base + o + size));
        }
    }
    Ok(end)
}

// ---------- TIFF and EXIF

#[derive(Clone, Copy)]
pub(super) struct End(bool);

impl End {
    fn u(self, b: &[u8], at: usize, n: usize) -> u64 {
        if self.0 { le(b, at, n) } else { be(b, at, n) }
    }
}

#[derive(Debug, Default)]
pub(super) struct TiffInfo {
    /// how far its structure reaches from the header
    pub(super) size: u64,
    pub(super) make: Option<String>,
    pub(super) model: Option<String>,
    pub(super) taken: Option<i64>,
    pub(super) dng: bool,
    pub(super) dims: Option<(u32, u32)>,
    /// image data: strips, tiles or a JPEG
    pub(super) image: bool,
    /// image data whose length isn't recorded
    pub(super) open: bool,
}

pub(super) const MAX_IFDS: usize = 64;
pub(super) const MAX_VALUES: u64 = 1 << 16;

/// Walk a TIFF structure at `base` (a TIFF or camera RAW file, or a JPEG's EXIF block): every IFD
/// it links to, and every block of data they point at, `limit` bytes at most. `strict` (for a file
/// found by its signature alone) wants the first IFD to look right: known types, tags in order.
pub(super) fn tiff_walk(
    s: &Src,
    base: u64,
    limit: u64,
    strict: bool,
) -> io::Result<Option<TiffInfo>> {
    let h = s.get(base, 8)?;
    if h.len() < 8 {
        return Ok(None);
    }
    let e = match &h[..2] {
        b"II" => End(true),
        b"MM" => End(false),
        _ => return Ok(None),
    };
    let mut info = TiffInfo {
        size: 8,
        ..TiffInfo::default()
    };
    let (mut original, mut digitized, mut modified) = (None, None, None);
    let mut todo = vec![(e.u(&h, 4, 4), true)];
    let mut seen: Vec<u64> = Vec::new();
    while let Some((o, first)) = todo.pop() {
        if o < 8 || o >= limit || seen.contains(&o) || seen.len() >= MAX_IFDS {
            if first {
                return Ok(None);
            }
            continue;
        }
        seen.push(o);
        let nb = s.get(base + o, 2)?;
        let n = e.u(&nb, 0, 2);
        let len = 12 * n as usize + 4;
        let raw = s.get(base + o + 2, len)?;
        if nb.len() < 2 || n == 0 || n > 1000 || raw.len() < len {
            if first {
                return Ok(None);
            }
            continue;
        }
        info.size = info.size.max(o + 2 + len as u64);
        let (mut bad, mut unsorted, mut prev) = (0u64, 0u64, 0u64);
        let (mut strips, mut counts) = (Vec::new(), Vec::new());
        let (mut jpeg_at, mut jpeg_len) = (None, None);
        for en in raw[..len - 4].as_chunks::<12>().0 {
            let (tag, typ, count) = (e.u(en, 0, 2), e.u(en, 2, 2), e.u(en, 4, 4));
            let unit = match typ {
                1 | 2 | 6 | 7 => 1,
                3 | 8 => 2,
                4 | 9 | 11 | 13 => 4,
                5 | 10 | 12 => 8,
                _ => {
                    bad += 1;
                    continue;
                }
            };
            if tag < prev {
                unsorted += 1;
            }
            prev = tag;
            let bytes = count.saturating_mul(unit);
            let at = if bytes > 4 {
                let v = e.u(en, 8, 4);
                if v.saturating_add(bytes) > limit {
                    bad += 1;
                    continue;
                }
                info.size = info.size.max(v + bytes);
                Some(v)
            } else {
                None
            };
            let first_value = || -> io::Result<Option<u64>> {
                Ok(tiff_values(s, base, e, typ, count.min(1), en, at)?
                    .first()
                    .copied())
            };
            match tag {
                0x010F => info.make = tiff_text(s, base, en, at, count)?,
                0x0110 => info.model = tiff_text(s, base, en, at, count)?,
                0x0132 => modified = tiff_text(s, base, en, at, count)?,
                0x9003 => original = tiff_text(s, base, en, at, count)?,
                0x9004 => digitized = tiff_text(s, base, en, at, count)?,
                0x0100 if first && info.dims.is_none() => {
                    if let Some(w) = first_value()? {
                        info.dims = Some((w as u32, 0));
                    }
                }
                0x0101 if first => {
                    if let (Some(h), Some(d)) = (first_value()?, info.dims.as_mut()) {
                        d.1 = h as u32;
                    }
                }
                0x0111 | 0x0144 => strips = tiff_values(s, base, e, typ, count, en, at)?,
                0x0117 | 0x0145 => counts = tiff_values(s, base, e, typ, count, en, at)?,
                0x0201 => jpeg_at = first_value()?,
                0x0202 => jpeg_len = first_value()?,
                0x014A => {
                    for v in tiff_values(s, base, e, typ, count, en, at)? {
                        todo.push((v, false));
                    }
                }
                0x8769 | 0x8825 | 0xA005 => {
                    if let Some(v) = first_value()? {
                        todo.push((v, false));
                    }
                }
                0xC612 => info.dng = true,
                _ => {}
            }
        }
        if strict && first && (bad * 2 > n || unsorted * 4 > n) {
            return Ok(None);
        }
        for (i, &so) in strips.iter().enumerate() {
            match counts.get(i) {
                Some(&c) if c > 0 && so.saturating_add(c) <= limit => {
                    info.size = info.size.max(so + c);
                    info.image = true;
                }
                _ if so < limit => {
                    info.size = info.size.max(so);
                    info.image = true;
                    info.open = true;
                }
                _ => {}
            }
        }
        if let (Some(a), Some(l)) = (jpeg_at, jpeg_len)
            && l > 0
            && a.saturating_add(l) <= limit
        {
            info.size = info.size.max(a + l);
            info.image = true;
        }
        let next = e.u(&raw, len - 4, 4);
        if next != 0 {
            todo.push((next, false));
        }
    }
    if info.dims.is_some_and(|d| d.0 == 0 || d.1 == 0) {
        info.dims = None;
    }
    info.taken = original
        .or(digitized)
        .or(modified)
        .as_deref()
        .and_then(exif_time);
    Ok(Some(info))
}

/// A TIFF entry's numbers (bytes, shorts, longs).
pub(super) fn tiff_values(
    s: &Src,
    base: u64,
    e: End,
    typ: u64,
    count: u64,
    en: &[u8],
    at: Option<u64>,
) -> io::Result<Vec<u64>> {
    let unit = match typ {
        1 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 13 => 4,
        _ => return Ok(Vec::new()),
    };
    let n = count.min(MAX_VALUES) as usize;
    let data: Cow<[u8]> = match at {
        None => Cow::Borrowed(&en[8..12]),
        Some(v) => s.get(base + v, n * unit)?,
    };
    Ok((0..n)
        .take_while(|i| (i + 1) * unit <= data.len())
        .map(|i| e.u(&data, i * unit, unit))
        .collect())
}

pub(super) fn tiff_text(
    s: &Src,
    base: u64,
    en: &[u8],
    at: Option<u64>,
    count: u64,
) -> io::Result<Option<String>> {
    let n = count.min(256) as usize;
    let d: Cow<[u8]> = match at {
        None => Cow::Borrowed(&en[8..8 + n.min(4)]),
        Some(v) => s.get(base + v, n)?,
    };
    let t = field(&d, 0, d.len());
    Ok((!t.is_empty()).then_some(t))
}

/// "2019:08:14 10:22:33" as Unix seconds (the camera's clock, read as UTC).
pub(super) fn exif_time(t: &str) -> Option<i64> {
    let b = t.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        std::str::from_utf8(&b[r]).ok()?.trim().parse().ok()
    };
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, se) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1970..=2100).contains(&y)
        || !(1..=12).contains(&mo)
        || !(1..=31).contains(&d)
        || !(0..24).contains(&h)
        || !(0..60).contains(&mi)
        || !(0..=60).contains(&se)
    {
        return None;
    }
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + se)
}

/// Days since 1970-01-01 of a calendar date (Howard Hinnant's algorithm).
pub(super) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// "Canon EOS 80D", "Apple iPhone 12": the model, with the brand in front when it isn't already.
pub(super) fn camera(make: Option<String>, model: Option<String>) -> Option<String> {
    match (make, model) {
        (Some(make), Some(model)) => {
            let brand = make.split_whitespace().next().unwrap_or_default();
            if model.to_lowercase().starts_with(&brand.to_lowercase()) {
                Some(model)
            } else {
                Some(format!("{brand} {model}"))
            }
        }
        (make, model) => model.or(make),
    }
}

pub(super) const RAW_MAX: u64 = GIB;
pub(super) const OPEN_MAX: u64 = 4 * GIB;

/// TIFF and the camera RAW formats built on it: the furthest byte any IFD points at.
pub(super) fn tiff(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let h = s.get(off, 16)?;
    let magic = h.get(..4).unwrap_or_default();
    let orf = matches!(magic, b"IIRO" | b"IIRS" | b"MMOR");
    let rw2 = magic == b"IIU\0";
    let Some(t) = tiff_walk(s, off, RAW_MAX, true)? else {
        return Ok(None);
    };
    if !t.image {
        return Ok(None);
    }
    let make = t.make.as_deref().unwrap_or_default().to_ascii_uppercase();
    let ext = if has(&h, 8, b"CR\x02\0") {
        "cr2"
    } else if orf {
        "orf"
    } else if rw2 {
        "rw2"
    } else if t.dng {
        "dng"
    } else if make.starts_with("NIKON") {
        "nef"
    } else if make.starts_with("SONY") {
        "arw"
    } else if make.starts_with("PENTAX") || make.starts_with("RICOH") {
        "pef"
    } else if make.starts_with("SAMSUNG") {
        "srw"
    } else {
        "tif"
    };
    let mut hit = Hit::new(ext, t.size);
    hit.taken = t.taken;
    hit.dims = t.dims;
    hit.camera = camera(t.make, t.model);
    if t.open {
        let e = until_next(s, off + t.size, OPEN_MAX, true)?;
        return Ok(Some(hit.partial(off, e, NO_END)));
    }
    Ok(Some(hit))
}

pub(super) const RAF_MAX: u64 = 512 * MIB;

/// Fujifilm RAF: its header lists where the preview JPEG and the raw data are, and how long.
pub(super) fn raf(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let h = s.get(off, 108)?;
    if h.len() < 108 {
        return Ok(None);
    }
    let mut end = 0;
    for at in [84, 92, 100] {
        let (o, l) = (be(&h, at, 4), be(&h, at + 4, 4));
        if o == 0 && l == 0 {
            continue;
        }
        if o < 108 {
            return Ok(None);
        }
        end = end.max(o + l);
    }
    if end == 0 || end > RAF_MAX || !has(&s.get(off + be(&h, 84, 4), 3)?, 0, &[0xFF, 0xD8, 0xFF]) {
        return Ok(None);
    }
    let mut hit = Hit::new("raf", end);
    let model = field(&h, 28, 32);
    hit.camera = (!model.is_empty()).then(|| format!("Fujifilm {model}"));
    Ok(Some(hit))
}

// ---------- PNG, GIF, BMP

pub(super) const PNG_MAX: u64 = 512 * MIB;

/// PNG: chunk after chunk, each with its CRC checked, up to IEND.
pub(super) fn png(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let mut st = Stream::new(*s, off + 8);
    let mut hit = Hit::new("png", 0);
    let (mut first, mut idat) = (true, false);
    let mut good = off + 8;
    loop {
        let at = st.offset();
        if at - off > PNG_MAX {
            return Ok(idat.then(|| hit.partial(off, good, capped(PNG_MAX))));
        }
        let b = st.peek(8 + 13)?;
        if b.len() < 8 {
            return Ok((!first).then(|| hit.cut(s, off)));
        }
        let len = be(b, 0, 4);
        let typ = [b[4], b[5], b[6], b[7]];
        let broken = len > 0x7FFF_FFFF
            || !typ.iter().all(u8::is_ascii_alphabetic)
            || (first && (&typ != b"IHDR" || len != 13));
        if broken {
            return Ok(idat.then(|| hit.partial(off, good, DAMAGED)));
        }
        if first {
            if b.len() < 21 {
                return Ok(None);
            }
            let d = &b[8..21];
            if !matches!(d[8], 1 | 2 | 4 | 8 | 16)
                || !matches!(d[9], 0 | 2 | 3 | 4 | 6)
                || d[10] != 0
                || d[11] != 0
                || d[12] > 1
            {
                return Ok(None);
            }
            hit.dims = Some((be(d, 0, 4) as u32, be(d, 4, 4) as u32));
        }
        let mut crc = crc32_update(0, &typ);
        st.advance(8);
        let mut left = len;
        while left > 0 {
            let w = st.peek(1)?;
            if w.is_empty() {
                return Ok((!first).then(|| hit.cut(s, off)));
            }
            let k = (w.len() as u64).min(left) as usize;
            crc = crc32_update(crc, &w[..k]);
            st.advance(k as u64);
            left -= k as u64;
        }
        let c = st.peek(4)?;
        if c.len() < 4 {
            return Ok((!first).then(|| hit.cut(s, off)));
        }
        if be(c, 0, 4) as u32 != crc {
            return Ok(idat.then(|| hit.partial(off, good, DAMAGED)));
        }
        st.advance(4);
        good = st.offset();
        first = false;
        match &typ {
            b"IDAT" => idat = true,
            b"IEND" => {
                hit.size = good - off;
                return Ok(Some(hit));
            }
            _ => {}
        }
    }
}

pub(super) const GIF_MAX: u64 = 128 * MIB;

/// GIF: the header, then blocks (extensions and images, each a run of sub-blocks) up to the
/// trailer byte.
pub(super) fn gif(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let h = s.get(off, 13)?;
    if h.len() < 13 {
        return Ok(None);
    }
    let (w, ht) = (le(&h, 6, 2), le(&h, 8, 2));
    if w == 0 || ht == 0 {
        return Ok(None);
    }
    let flags = h[10];
    let gct = if flags & 0x80 != 0 {
        3 << ((flags & 7) + 1)
    } else {
        0
    };
    let mut hit = Hit::new("gif", 0);
    hit.dims = Some((w as u32, ht as u32));
    let mut st = Stream::new(*s, off + 13 + gct);
    let mut images = 0;
    loop {
        let at = st.offset();
        if at - off > GIF_MAX {
            return Ok((images > 0).then(|| hit.partial(off, at, capped(GIF_MAX))));
        }
        let b = st.peek(10)?;
        let Some(&c) = b.first() else {
            return Ok(Some(hit.cut(s, off)));
        };
        match c {
            0x3B => {
                hit.size = at + 1 - off;
                return Ok((images > 0).then_some(hit));
            }
            0x21 => {
                st.advance(2);
            }
            0x2C => {
                if b.len() < 10 {
                    return Ok(Some(hit.cut(s, off)));
                }
                let f = b[9];
                let lct = if f & 0x80 != 0 { 3 << ((f & 7) + 1) } else { 0 };
                st.advance(10 + lct);
                let m = st.peek(1)?;
                let Some(&bits) = m.first() else {
                    return Ok(Some(hit.cut(s, off)));
                };
                if !(1..=11).contains(&bits) {
                    return Ok((images > 0).then(|| hit.partial(off, at, DAMAGED)));
                }
                st.advance(1);
                images += 1;
            }
            _ => return Ok((images > 0).then(|| hit.partial(off, at, DAMAGED))),
        }
        // sub-blocks: a length byte and that many bytes, up to a zero length
        loop {
            let b = st.peek(1)?;
            let Some(&n) = b.first() else {
                return Ok(Some(hit.cut(s, off)));
            };
            st.advance(1 + u64::from(n));
            if n == 0 {
                break;
            }
        }
    }
}

pub(super) const BMP_MAX: u64 = 512 * MIB;

/// BMP: the size in its header, checked against the image's dimensions.
pub(super) fn bmp(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let h = s.get(off, 64)?;
    if h.len() < 30 || le(&h, 6, 4) != 0 {
        return Ok(None);
    }
    let (size, data, dib) = (le(&h, 2, 4), le(&h, 10, 4), le(&h, 14, 4));
    let (w, ht, planes, bpp, comp) = match dib {
        12 => (
            le(&h, 18, 2) as i64,
            le(&h, 20, 2) as i64,
            le(&h, 22, 2),
            le(&h, 24, 2),
            0,
        ),
        40 | 52 | 56 | 64 | 108 | 124 if h.len() >= 34 => (
            i64::from(le(&h, 18, 4) as u32 as i32),
            i64::from(le(&h, 22, 4) as u32 as i32),
            le(&h, 26, 2),
            le(&h, 28, 2),
            le(&h, 30, 4),
        ),
        _ => return Ok(None),
    };
    if planes != 1
        || !matches!(bpp, 1 | 2 | 4 | 8 | 16 | 24 | 32 | 64)
        || comp > 13
        || !(1..=65_536).contains(&w)
        || ht == 0
        || ht.abs() > 65_536
        || data < 14 + dib
        || data > size
        || size > BMP_MAX
    {
        return Ok(None);
    }
    let raw = (w as u64 * bpp).div_ceil(32) * 4 * ht.unsigned_abs();
    if matches!(comp, 0 | 3 | 6) {
        if size < data + raw || size > data + raw + MIB {
            return Ok(None);
        }
    } else if size <= data {
        return Ok(None);
    }
    let mut hit = Hit::new("bmp", size);
    hit.dims = Some((w as u32, ht.unsigned_abs() as u32));
    Ok(Some(hit))
}
