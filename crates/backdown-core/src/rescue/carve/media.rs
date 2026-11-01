//! Audio and video: RIFF (WebP, WAV, AVI) and AIFF, ISO base media (MP4, MOV, HEIC, M4A, 3GP,
//! CR3), MP3, FLAC, Ogg, Matroska/WebM, Windows Media and MPEG streams.

use std::io;

use crate::magic::{be, has, le};

use super::*;

// ---------- RIFF (WebP, WAV, AVI), AIFF

pub(super) const RIFF_MAX: u64 = 4 * GIB + 8;

/// RIFF: the size in its header, checked by walking the chunks inside, which have to add up to it.
/// AVIs over a gigabyte go on in "AVIX" extension RIFFs right after.
pub(super) fn riff(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let h = s.get(off, 64)?;
    if h.len() < 32 {
        return Ok(None);
    }
    let rf64 = has(&h, 0, b"RF64");
    let ext = match &h[8..12] {
        b"WEBP" if !rf64 => "webp",
        b"WAVE" => "wav",
        b"AVI " if !rf64 => "avi",
        _ => return Ok(None),
    };
    let first = &h[12..16];
    let first_ok = match ext {
        "webp" => matches!(first, b"VP8 " | b"VP8L" | b"VP8X"),
        "avi" => first == b"LIST",
        _ => first.iter().all(|c| c.is_ascii_graphic() || *c == b' '),
    };
    if !first_ok {
        return Ok(None);
    }
    let mut hit = Hit::new(ext, 0);
    if ext == "webp" {
        hit.dims = match first {
            b"VP8X" => Some(((le(&h, 24, 3) + 1) as u32, (le(&h, 27, 3) + 1) as u32)),
            b"VP8L" if h[20] == 0x2F => {
                let v = le(&h, 21, 4);
                Some((((v & 0x3FFF) + 1) as u32, (((v >> 14) & 0x3FFF) + 1) as u32))
            }
            b"VP8 " if has(&h, 23, &[0x9D, 0x01, 0x2A]) => Some((
                (le(&h, 26, 2) & 0x3FFF) as u32,
                (le(&h, 28, 2) & 0x3FFF) as u32,
            )),
            _ => None,
        };
    }
    if rf64 {
        // RF64: the real sizes are in a "ds64" chunk (a WAV over 4 GB)
        if !has(&h, 12, b"ds64") {
            return Ok(None);
        }
        hit.size = le(&h, 20, 8).saturating_add(8);
        return Ok((hit.size <= ISO_MAX).then_some(hit));
    }
    let declared = le(&h, 4, 4);
    if matches!(declared, 0 | 0xFFFF_FFFF) && ext != "webp" {
        // a recorder that stopped before writing the sizes (a flat battery, a pulled card)
        let e = until_next(s, off + 12, OPEN_MAX, true)?;
        return Ok(Some(hit.partial(off, e, NO_SIZES)));
    }
    let size = declared + 8;
    if !(20..=RIFF_MAX).contains(&size) {
        return Ok(None);
    }
    let walked = riff_walk(s, off + 12, off + size)?;
    let mut end = off + size;
    if walked != end && walked != end + 1 {
        if walked > s.end {
            return Ok(Some(hit.cut(s, off)));
        }
        // a recorder that never wrote the final size, or chunks that don't add up
        return Ok(Some(hit.partial(
            off,
            walked.max(off + 12).min(end),
            DAMAGED,
        )));
    }
    end = walked;
    if ext == "avi" {
        loop {
            let x = s.get(end, 12)?;
            if !(has(&x, 0, b"RIFF") && has(&x, 8, b"AVIX")) {
                break;
            }
            end += le(&x, 4, 4) + 8;
        }
    }
    hit.size = end - off;
    Ok(Some(hit))
}

/// Walk RIFF chunks from `pos` towards `end`: where the walk stops (at `end` when they add up).
pub(super) fn riff_walk(s: &Src, mut pos: u64, end: u64) -> io::Result<u64> {
    let mut n = 0;
    while pos + 8 <= end && n < 100_000 {
        let b = s.get(pos, 8)?;
        if b.len() < 8 {
            return Ok(pos.max(s.end + 1)); // runs past the scanned area
        }
        if !b[..4].iter().all(|c| c.is_ascii_graphic() || *c == b' ') {
            break;
        }
        let size = le(&b, 4, 4);
        pos += 8 + size + (size & 1);
        n += 1;
    }
    Ok(pos)
}

/// AIFF: a big-endian IFF "FORM" whose size covers the file.
pub(super) fn aiff(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let h = s.get(off, 16)?;
    if h.len() < 16
        || !matches!(&h[8..12], b"AIFF" | b"AIFC")
        || !matches!(&h[12..16], b"COMM" | b"FVER")
    {
        return Ok(None);
    }
    let size = be(&h, 4, 4) + 8;
    if size < 20 {
        return Ok(None);
    }
    Ok(Some(Hit::new("aif", size)))
}

// ---------- ISO base media (MP4, MOV, HEIC, M4A, 3GP, CR3)

pub(super) const ISO_MAX: u64 = 64 * GIB;
/// What can be at the top level of an ISO media file.
pub(super) const ISO_TOP: &[&[u8]] = &[
    b"ftyp", b"moov", b"mdat", b"free", b"skip", b"wide", b"uuid", b"meta", b"pnot", b"PICT",
    b"pdin", b"moof", b"mfra", b"styp", b"sidx", b"ssix", b"prft", b"emsg", b"udta", b"junk",
];

#[derive(Debug, Default)]
pub(super) struct IsoWalk {
    /// where the last whole box ends
    pub(super) end: u64,
    pub(super) moov: Option<u64>,
    pub(super) mdat: bool,
    pub(super) meta: bool,
    pub(super) moof: bool,
    /// a box runs past the end of the scanned area
    pub(super) cut: bool,
    /// where the data of a box "running to the end of the file" starts (its size is 0)
    pub(super) open: Option<u64>,
    /// larger than `max`
    pub(super) big: bool,
}

/// Walk top-level boxes from `off` while they're plausible: the file ends after the last one.
pub(super) fn iso_walk(s: &Src, off: u64, max: u64) -> io::Result<IsoWalk> {
    let mut w = IsoWalk {
        end: off,
        ..IsoWalk::default()
    };
    let mut pos = off;
    while pos < s.end {
        let h = s.get(pos, 16)?;
        if h.len() < 8 {
            break;
        }
        let typ = &h[4..8];
        if !ISO_TOP.contains(&typ) {
            break;
        }
        // the next file, starting right after this one
        if pos > off
            && (typ == b"ftyp"
                || (w.moov.is_some() && w.mdat && matches!(typ, b"wide" | b"moov" | b"pnot")))
        {
            break;
        }
        let (mut size, mut hdr) = (be(&h, 0, 4), 8);
        if size == 1 {
            if h.len() < 16 {
                break;
            }
            size = be(&h, 8, 8);
            hdr = 16;
            if size < 16 {
                break;
            }
        } else if size != 0 && size < 8 {
            break;
        }
        match typ {
            b"moov" => {
                if w.moov.is_some() {
                    break;
                }
                w.moov = Some(pos);
            }
            b"mdat" => w.mdat = true,
            b"meta" => w.meta = true,
            b"moof" => w.moof = true,
            _ => {}
        }
        if size == 0 {
            w.open = Some(pos + hdr);
            break;
        }
        if (pos - off).saturating_add(size) > max {
            w.big = true;
            break;
        }
        pos += size;
        if pos > s.end {
            w.cut = true;
            break;
        }
        w.end = pos;
    }
    Ok(w)
}

/// The extension an ISO media file's brands say it has.
pub(super) fn iso_ext(major: &[u8], compat: &[u8]) -> &'static str {
    let brand = |x: &[u8]| compat.as_chunks::<4>().0.iter().any(|c| c == x);
    match major {
        b"qt  " => "mov",
        b"crx " => "cr3",
        b"avif" | b"avis" => "avif",
        b"heic" | b"heix" | b"heim" | b"heis" | b"hevc" | b"hevx" => "heic",
        b"mif1" | b"msf1" if brand(b"avif") => "avif",
        b"mif1" | b"msf1" => "heic",
        b"M4A " | b"M4B " | b"M4P " => "m4a",
        b"M4V " | b"M4VH" | b"M4VP" => "m4v",
        m if m.starts_with(b"3g") => "3gp",
        _ => "mp4",
    }
}

/// ISO media: `ftyp` (whose brands say what it is), then the top-level boxes. Old QuickTime movies
/// have no `ftyp` and start straight with their boxes (`old`).
pub(super) fn iso(s: &Src, off: u64, old: bool) -> io::Result<Option<Hit>> {
    let h = s.get(off, 256)?;
    if h.len() < 16 {
        return Ok(None);
    }
    let ext = if old {
        "mov"
    } else {
        let n = be(&h, 0, 4) as usize;
        if !(16..=256).contains(&n)
            || !n.is_multiple_of(4)
            || n > h.len()
            || !h[8..12]
                .iter()
                .all(|c| c.is_ascii_alphanumeric() || *c == b' ')
        {
            return Ok(None);
        }
        iso_ext(&h[8..12], &h[16..n])
    };
    let w = iso_walk(s, off, ISO_MAX)?;
    if w.big {
        return Ok(None);
    }
    let mut hit = Hit::new(ext, w.end - off);
    if let Some(m) = w.moov {
        let b = s.get(m, 40)?;
        let hdr = if be(&b, 0, 4) == 1 { 16 } else { 8 };
        let child = b.get(hdr + 4..hdr + 8).unwrap_or_default();
        if child.len() == 4
            && !child
                .iter()
                .all(|c| c.is_ascii_alphanumeric() || *c == b' ')
        {
            return Ok(None); // not a real movie header
        }
        hit.taken = mvhd_time(&b, hdr);
    }
    let whole = match ext {
        "heic" | "avif" => w.meta && w.mdat,
        _ => (w.moov.is_some() || w.moof) && w.mdat,
    };
    if old && !(w.mdat && w.moov.is_some()) {
        return Ok(None); // a weaker signature: only a whole movie counts
    }
    if w.cut {
        return Ok(Some(hit.cut(s, off)));
    }
    if let Some(data) = w.open {
        let e = until_next(s, data, OPEN_MAX, true)?;
        return Ok(Some(hit.partial(off, e, NO_END)));
    }
    if !whole {
        return Ok((w.mdat && !matches!(ext, "heic" | "avif"))
            .then(|| hit.partial(off, w.end, UNFINISHED)));
    }
    Ok(Some(hit))
}

/// When a movie was made, from its `mvhd` box (seconds since 1904).
pub(super) fn mvhd_time(b: &[u8], hdr: usize) -> Option<i64> {
    if !has(b, hdr + 4, b"mvhd") {
        return None;
    }
    let t = if *b.get(hdr + 8)? == 1 {
        be(b, hdr + 12, 8)
    } else {
        be(b, hdr + 12, 4)
    };
    let unix = t as i64 - 2_082_844_800;
    // 1990 to 2100: a clock that was never set says 1904 or 1970
    (631_152_000..4_102_444_800).contains(&unix).then_some(unix)
}

// ---------- MP3

pub(super) const MP3_MAX: u64 = GIB;
/// A run of MPEG audio frames without an ID3 tag has to be at least this long (about 2.6 s).
pub(super) const MIN_BARE_FRAMES: u64 = 100;

#[derive(Debug, Clone, Copy)]
pub(super) struct Mpa {
    pub(super) len: u64,
    /// version, layer and sample rate: the same all through a stream
    pub(super) key: u32,
    pub(super) layer3: bool,
}

/// An MPEG audio frame header: how long the frame is.
pub(super) fn mpa(h: u32) -> Option<Mpa> {
    if h >> 21 != 0x7FF {
        return None;
    }
    let (ver, layer) = ((h >> 19) & 3, (h >> 17) & 3);
    let (br, sr) = (((h >> 12) & 15) as usize, ((h >> 10) & 3) as usize);
    if ver == 1 || layer == 0 || br == 0 || br == 15 || sr == 3 || h & 3 == 2 {
        return None;
    }
    let v1 = ver == 3;
    let kbps: u64 = match (v1, layer) {
        (true, 3) => [
            0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448,
        ][br],
        (true, 2) => [
            0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384,
        ][br],
        (true, _) => [
            0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
        ][br],
        (false, 3) => [
            0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256,
        ][br],
        (false, _) => [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160][br],
    };
    let rate: u64 = [
        [11_025, 12_000, 8_000],
        [0; 3],
        [22_050, 24_000, 16_000],
        [44_100, 48_000, 32_000],
    ][ver as usize][sr];
    let pad = u64::from((h >> 9) & 1);
    let len = match layer {
        3 => (12 * kbps * 1000 / rate + pad) * 4,
        1 if !v1 => 72 * kbps * 1000 / rate + pad,
        _ => 144 * kbps * 1000 / rate + pad,
    };
    Some(Mpa {
        len,
        key: h & 0xFFFE_0C00,
        layer3: layer == 1,
    })
}

#[derive(Debug, Default)]
pub(super) struct Run {
    pub(super) end: u64,
    pub(super) frames: u64,
    pub(super) cut: bool,
    pub(super) big: bool,
}

/// Follow MPEG audio frames from `from` while they're valid and alike.
pub(super) fn mpa_run(s: &Src, from: u64, max: u64) -> io::Result<Run> {
    let mut st = Stream::new(*s, from);
    let mut r = Run {
        end: from,
        ..Run::default()
    };
    let mut key = None;
    loop {
        let at = st.offset();
        if at - from > max {
            r.big = true;
            break;
        }
        let b = st.peek(4)?;
        if b.len() < 4 {
            break;
        }
        let Some(f) = mpa(be(b, 0, 4) as u32) else {
            break;
        };
        if key.is_some_and(|k| k != f.key) {
            break;
        }
        key = Some(f.key);
        if at + f.len > s.end {
            r.cut = true;
            break;
        }
        st.advance(f.len);
        r.frames += 1;
        r.end = at + f.len;
    }
    Ok(r)
}

/// Tags after the audio: an APEv2 tag (with its header) and an ID3v1 tag.
pub(super) fn mp3_tail(s: &Src, mut end: u64) -> io::Result<u64> {
    let b = s.get(end, 32)?;
    if has(&b, 0, b"APETAGEX") && le(&b, 20, 4) & (1 << 29) != 0 {
        end += 32 + le(&b, 12, 4);
    }
    if has(&s.get(end, 3)?, 0, b"TAG") && end + 128 <= s.end {
        end += 128;
    }
    Ok(end)
}

/// An MP3 with an ID3v2 tag: the tag's size, then the audio frames until they stop.
pub(super) fn id3(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let h = s.get(off, 10)?;
    if h.len() < 10
        || !(2..=4).contains(&h[3])
        || h[4] == 0xFF
        || h[6..10].iter().any(|&c| c >= 0x80)
    {
        return Ok(None);
    }
    let size = h[6..10].iter().fold(0u64, |a, &c| a << 7 | u64::from(c));
    let mut pos = off + 10 + size + if h[5] & 0x10 != 0 { 10 } else { 0 };
    // some taggers leave padding outside the tag's size
    let pad = s.get(pos, 4096)?;
    pos += pad.iter().take_while(|&&c| c == 0).count() as u64;
    let r = mpa_run(s, pos, MP3_MAX)?;
    let hit = Hit::new("mp3", 0);
    if r.cut || pos >= s.end {
        return Ok(Some(hit.cut(s, off)));
    }
    if r.frames < 4 {
        return Ok(None);
    }
    if r.big {
        return Ok(Some(hit.partial(off, r.end, capped(MP3_MAX))));
    }
    Ok(Some(Hit::new("mp3", mp3_tail(s, r.end)? - off)))
}

/// MPEG audio frames with no tag in front: only a long run of them counts.
pub(super) fn mpeg(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let mut at = off;
    let mut key = None;
    for _ in 0..3 {
        let b = s.get(at, 4)?;
        if b.len() < 4 {
            return Ok(None);
        }
        let Some(f) = mpa(be(&b, 0, 4) as u32).filter(|f| f.layer3) else {
            return Ok(None);
        };
        if key.is_some_and(|k| k != f.key) {
            return Ok(None);
        }
        key = Some(f.key);
        at += f.len;
    }
    let r = mpa_run(s, off, MP3_MAX)?;
    if r.frames < MIN_BARE_FRAMES {
        return Ok(None);
    }
    let hit = Hit::new("mp3", 0);
    if r.cut {
        return Ok(Some(hit.cut(s, off)));
    }
    if r.big {
        return Ok(Some(hit.partial(off, r.end, capped(MP3_MAX))));
    }
    Ok(Some(Hit::new("mp3", mp3_tail(s, r.end)? - off)))
}

// ---------- FLAC

pub(super) const FLAC_MAX: u64 = 2 * GIB;

#[derive(Debug, Clone, Copy)]
pub(super) struct FlacFrame {
    pub(super) block: u64,
    pub(super) num: u64,
    pub(super) variable: bool,
    pub(super) hdr: u64,
}

/// A FLAC frame header, checked with its CRC-8.
pub(super) fn flac_frame(b: &[u8]) -> Option<FlacFrame> {
    if b.len() < 6 || b[0] != 0xFF || b[1] & 0xFE != 0xF8 {
        return None;
    }
    let (bs, rate, ch, ss) = (b[2] >> 4, b[2] & 15, b[3] >> 4, (b[3] >> 1) & 7);
    if bs == 0 || rate == 15 || ch > 10 || ss == 3 || b[3] & 1 != 0 {
        return None;
    }
    let first = b[4];
    let n = match first.leading_ones() {
        0 => 1,
        k @ 2..=7 => k as usize,
        _ => return None,
    };
    let mut num = if n == 1 {
        u64::from(first)
    } else {
        u64::from(first & (0x7F >> n))
    };
    for i in 1..n {
        let c = *b.get(4 + i)?;
        if c & 0xC0 != 0x80 {
            return None;
        }
        num = num << 6 | u64::from(c & 0x3F);
    }
    let mut i = 4 + n;
    let block: u64 = match bs {
        1 => 192,
        2..=5 => 576 << (bs - 2),
        6 => {
            let v = u64::from(*b.get(i)?) + 1;
            i += 1;
            v
        }
        7 => {
            let v = be(b, i, 2) + 1;
            i += 2;
            v
        }
        _ => 256 << (bs - 8),
    };
    i += match rate {
        12 => 1,
        13 | 14 => 2,
        _ => 0,
    };
    if crc8(b.get(..i)?) != *b.get(i)? {
        return None;
    }
    Some(FlacFrame {
        block,
        num,
        variable: b[1] & 1 == 1,
        hdr: (i + 1) as u64,
    })
}

/// FLAC: the metadata blocks, then the frames. Frames don't record their length: each ends where
/// its CRC-16 checks out and the next frame's header (with the next number) follows, the last one
/// where the stream's sample count is reached.
pub(super) fn flac(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let mut pos = off + 4;
    let mut info: Option<Vec<u8>> = None;
    for i in 0.. {
        let b = s.get(pos, 4)?;
        if b.len() < 4 || i > 1000 {
            return Ok(None);
        }
        let (last, typ, len) = (b[0] & 0x80 != 0, b[0] & 0x7F, be(&b, 1, 3));
        if typ > 6 || (i == 0 && (typ != 0 || len != 34)) {
            return Ok(None);
        }
        if typ == 0 {
            let d = s.get(pos + 4, 34)?;
            if d.len() < 34 {
                return Ok(None);
            }
            info = Some(d.into_owned());
        }
        pos += 4 + len;
        if last {
            break;
        }
    }
    let Some(si) = info else { return Ok(None) };
    let (min_frame, max_frame) = (be(&si, 4, 3), be(&si, 7, 3));
    let total = (u64::from(si[13] & 0x0F) << 32) | be(&si, 14, 4);
    if be(&si, 10, 3) >> 4 == 0 || be(&si, 2, 2) < be(&si, 0, 2) {
        return Ok(None);
    }
    let hit = Hit::new("flac", 0);
    let Some(mut fr) = flac_frame(&s.get(pos, 32)?) else {
        return Ok(if pos >= s.end {
            Some(hit.cut(s, off))
        } else {
            None
        });
    };
    let longest = if max_frame > 0 { max_frame + 16 } else { MIB };
    let (mut cur, mut samples) = (pos, 0u64);
    let mut st = Stream::new(*s, cur);
    loop {
        if cur - off > FLAC_MAX {
            return Ok(Some(hit.partial(off, cur, capped(FLAC_MAX))));
        }
        let last = total > 0 && samples + fr.block >= total;
        let expected = if fr.variable {
            fr.num + fr.block
        } else {
            fr.num + 1
        };
        let shortest = min_frame.max(fr.hdr + 2);
        let limit = cur + longest;
        let (mut crc, mut p) = (0u16, cur);
        let (mut end, mut next, mut fallback) = (None, None, None);
        st.seek(cur);
        'bytes: while p < limit {
            let w = st.peek(1)?;
            if w.is_empty() {
                break;
            }
            let take = w.len().min((limit - p) as usize);
            for (i, &x) in w[..take].iter().enumerate() {
                crc = (crc << 8) ^ CRC16[usize::from((crc >> 8) as u8 ^ x)];
                let q = p + i as u64 + 1;
                if crc != 0 || q - cur < shortest {
                    continue;
                }
                if last {
                    end = Some(q);
                    break 'bytes;
                }
                let nb = s.get(q, 32)?;
                match flac_frame(&nb) {
                    Some(nf) if nf.variable == fr.variable && nf.num == expected => {
                        (end, next) = (Some(q), Some(nf));
                        break 'bytes;
                    }
                    // no next frame: the stream is over here, or what follows is damaged (or
                    // the CRC matched by chance, if a proper next frame turns up further on)
                    _ if fallback.is_none() => {
                        let sync = nb.len() >= 2 && nb[0] == 0xFF && nb[1] & 0xFE == 0xF8;
                        fallback = Some((q, sync));
                    }
                    _ => {}
                }
            }
            st.advance(take as u64);
            p += take as u64;
        }
        let e = match (end, fallback) {
            (Some(e), _) => e,
            (None, Some((q, false))) if total == 0 => q,
            (None, Some((q, _))) => return Ok(Some(hit.partial(off, q, DAMAGED))),
            (None, None) if p >= s.end => return Ok(Some(hit.cut(s, off))),
            (None, None) => return Ok((samples > 0).then(|| hit.partial(off, cur, DAMAGED))),
        };
        samples += fr.block;
        cur = e;
        match next {
            Some(nf) => fr = nf,
            None => return Ok(Some(Hit::new("flac", cur - off))),
        }
    }
}

// ---------- Ogg

pub(super) const OGG_MAX: u64 = 4 * GIB;

/// Ogg: page after page (each with its CRC checked) until the pages stop.
pub(super) fn ogg(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let mut st = Stream::new(*s, off);
    let (mut pages, mut ext, mut eos) = (0u64, "ogg", false);
    loop {
        let at = st.offset();
        if at - off > OGG_MAX {
            return Ok(Some(Hit::new(ext, 0).partial(off, at, capped(OGG_MAX))));
        }
        let b = st.peek(27 + 255)?;
        if b.len() < 27 || !has(b, 0, b"OggS") || b[4] != 0 || b[5] & 0xF8 != 0 {
            break;
        }
        let flags = b[5];
        let nseg = usize::from(b[26]);
        if b.len() < 27 + nseg {
            return Ok((pages > 0).then(|| Hit::new(ext, 0).cut(s, off)));
        }
        if pages == 0 && (flags & 2 == 0 || le(b, 18, 4) != 0) {
            return Ok(None); // the first page begins a stream
        }
        if pages > 0 && eos && flags & 2 != 0 {
            break; // a new stream after the last one ended: the next file
        }
        let len = 27
            + nseg
            + b[27..27 + nseg]
                .iter()
                .map(|&x| usize::from(x))
                .sum::<usize>();
        let page = st.peek(len)?;
        if page.len() < len {
            return Ok((pages > 0).then(|| Hit::new(ext, 0).cut(s, off)));
        }
        let crc = ogg_crc(ogg_crc(ogg_crc(0, &page[..22]), &[0; 4]), &page[26..len]);
        if crc != le(page, 22, 4) as u32 {
            if pages == 0 {
                return Ok(None);
            }
            break;
        }
        if pages == 0 {
            let p = &page[27 + nseg..len];
            ext = if p.starts_with(b"OpusHead") {
                "opus"
            } else if p.starts_with(b"\x80theora") {
                "ogv"
            } else {
                "ogg"
            };
        }
        eos = flags & 4 != 0;
        st.advance(len as u64);
        pages += 1;
    }
    if pages < 2 {
        return Ok(None);
    }
    let hit = Hit::new(ext, st.offset() - off);
    Ok(Some(if eos {
        hit
    } else {
        let e = st.offset();
        hit.partial(off, e, DAMAGED)
    }))
}

// ---------- Matroska / WebM

pub(super) const MKV_MAX: u64 = 64 * GIB;
pub(super) const CLUSTER: u64 = 0x1F43_B675;
pub(super) const SEG_KIDS: &[u64] = &[
    0x114D_9B74,
    0x1549_A966,
    0x1654_AE6B,
    CLUSTER,
    0x1C53_BB6B,
    0x1043_A770,
    0x1254_C367,
    0x1941_A469,
    0xEC,
    0xBF,
];
pub(super) const CLUSTER_KIDS: &[u64] = &[0xE7, 0x5854, 0xA7, 0xAB, 0xA3, 0xA0, 0xAF, 0xEC, 0xBF];

/// An EBML variable-length number: its value, its length, and whether it means "unknown" (all
/// ones). `id` keeps the length marker, as element IDs do.
pub(super) fn vint(b: &[u8], id: bool) -> Option<(u64, usize, bool)> {
    let first = *b.first()?;
    if first == 0 {
        return None;
    }
    let n = first.leading_zeros() as usize + 1;
    if b.len() < n || (id && n > 4) {
        return None;
    }
    let mut v = if id {
        u64::from(first)
    } else {
        u64::from(first) & (0xFF >> n)
    };
    for &c in &b[1..n] {
        v = v << 8 | u64::from(c);
    }
    let unknown = !id && v == (1u64 << (7 * n)) - 1;
    Some((v, n, unknown))
}

/// Matroska and WebM: the EBML header (whose DocType says which), then the Segment, whose size
/// covers the rest. Live recordings leave that size unknown: then its elements are walked.
pub(super) fn ebml(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let h = s.get(off, 16)?;
    let Some((hs, n, unk)) = vint(h.get(4..).unwrap_or_default(), false) else {
        return Ok(None);
    };
    if unk || hs > 1024 {
        return Ok(None);
    }
    let body = s.get(off + 4 + n as u64, hs as usize)?;
    let mut doc: Option<&[u8]> = None;
    let mut i = 0;
    while i < body.len() {
        let Some((id, a, _)) = vint(&body[i..], true) else {
            break;
        };
        let Some((sz, b, _)) = vint(&body[i + a..], false) else {
            break;
        };
        let d = i + a + b;
        if id == 0x4282 {
            doc = body.get(d..d + sz as usize);
        }
        i = d + sz as usize;
    }
    let ext = match doc.map(|d| &d[..d.iter().rposition(|&c| c != 0).map_or(0, |i| i + 1)]) {
        Some(b"matroska") => "mkv",
        Some(b"webm") => "webm",
        _ => return Ok(None),
    };
    let seg = off + 4 + n as u64 + hs;
    let sh = s.get(seg, 16)?;
    if sh.is_empty() {
        return Ok(Some(Hit::new(ext, 0).cut(s, off)));
    }
    if !has(&sh, 0, &[0x18, 0x53, 0x80, 0x67]) {
        return Ok(None);
    }
    let Some((ss, n2, unk)) = vint(&sh[4..], false) else {
        return Ok(None);
    };
    let data = seg + 4 + n2 as u64;
    let c = s.get(data, 8)?;
    if c.is_empty() {
        return Ok(Some(Hit::new(ext, 0).cut(s, off)));
    }
    if !vint(&c, true).is_some_and(|(id, _, _)| SEG_KIDS.contains(&id)) {
        return Ok(None);
    }
    if !unk {
        return Ok((data + ss - off <= MKV_MAX).then(|| Hit::new(ext, data + ss - off)));
    }
    // a live recording: walk the segment's elements (clusters of unknown size, too)
    let mut st = Stream::new(*s, data);
    let mut in_cluster = false;
    loop {
        let at = st.offset();
        if at - off > MKV_MAX {
            return Ok(Some(Hit::new(ext, 0).partial(off, at, capped(MKV_MAX))));
        }
        let b = st.peek(16)?;
        let Some((id, il, _)) = vint(b, true) else {
            break;
        };
        let Some((sz, sl, unk)) = vint(&b[il..], false) else {
            break;
        };
        let neutral = matches!(id, 0xEC | 0xBF);
        let top = SEG_KIDS.contains(&id);
        if !(top || (in_cluster && CLUSTER_KIDS.contains(&id))) {
            break;
        }
        if top && !neutral {
            in_cluster = false;
        }
        if id == CLUSTER && unk {
            in_cluster = true;
            st.advance((il + sl) as u64);
            continue;
        }
        if unk {
            break;
        }
        st.advance((il + sl) as u64 + sz);
        if st.offset() > s.end {
            return Ok(Some(Hit::new(ext, 0).cut(s, off)));
        }
    }
    let e = st.offset();
    Ok(Some(Hit::new(ext, e - off)))
}

// ---------- Windows Media, MPEG streams

pub(super) const ASF_MAX: u64 = 64 * GIB;
pub(super) const ASF_TOP: &[[u8; 16]] = &[
    // data, simple index, index, media object index
    [
        0x36, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62, 0xCE,
        0x6C,
    ],
    [
        0x90, 0x08, 0x00, 0x33, 0xB1, 0xE5, 0xCF, 0x11, 0x89, 0xF4, 0x00, 0xA0, 0xC9, 0x03, 0x49,
        0xCB,
    ],
    [
        0xD3, 0x29, 0xE2, 0xD6, 0xDA, 0x35, 0xD1, 0x11, 0x90, 0x34, 0x00, 0xA0, 0xC9, 0x03, 0x49,
        0xBE,
    ],
    [
        0xF8, 0x03, 0xB1, 0xFE, 0xAD, 0x12, 0x64, 0x4C, 0x84, 0x0F, 0x2A, 0x1D, 0x2F, 0x7A, 0xD4,
        0x8C,
    ],
];
pub(super) const ASF_VIDEO: &[u8] = &[
    0xC0, 0xEF, 0x19, 0xBC, 0x4D, 0x5B, 0xCF, 0x11, 0xA8, 0xFD, 0x00, 0x80, 0x5F, 0x5C, 0x44, 0x2B,
];

/// Windows Media (ASF): the header object, then the data and index objects, each with its size.
pub(super) fn asf(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let h = s.get(off, 30)?;
    if h.len() < 30 {
        return Ok(None);
    }
    let (hs, nobj) = (le(&h, 16, 8), le(&h, 24, 4));
    if !(30..=16 * MIB).contains(&hs) || nobj == 0 || nobj > 1000 || h[28] != 1 || h[29] != 2 {
        return Ok(None);
    }
    let head = s.get(off, hs as usize)?;
    let ext = if find(&head, ASF_VIDEO, 0).is_some() {
        "wmv"
    } else {
        "wma"
    };
    let mut pos = off + hs;
    loop {
        let b = s.get(pos, 24)?;
        if b.len() < 24 || !ASF_TOP.iter().any(|g| has(&b, 0, g)) {
            break;
        }
        let size = le(&b, 16, 8);
        if size < 24 || pos + size - off > ASF_MAX {
            break;
        }
        pos += size;
    }
    Ok((pos > off + hs).then(|| Hit::new(ext, pos - off)))
}

pub(super) const TS_MAX: u64 = 64 * GIB;
pub(super) const MIN_TS_PACKETS: u64 = 256;

/// MPEG transport streams (camcorders' AVCHD .MTS: 192-byte packets): packets for as long as their
/// sync bytes keep coming.
pub(super) fn ts(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let b = s.get(off, 8)?;
    let (pk, sync, ext) = if b.first() == Some(&0x47) {
        (188usize, 0usize, "ts")
    } else {
        (192, 4, "mts")
    };
    let mut st = Stream::new(*s, off);
    let mut n = 0u64;
    loop {
        if st.offset() - off > TS_MAX {
            break;
        }
        let w = st.peek(pk)?;
        if w.len() < pk {
            if !w.is_empty() && n >= MIN_TS_PACKETS {
                return Ok(Some(Hit::new(ext, 0).cut(s, off)));
            }
            break;
        }
        let full = w.len() / pk;
        let k = w.chunks_exact(pk).take_while(|p| p[sync] == 0x47).count();
        n += k as u64;
        st.advance((k * pk) as u64);
        if k < full {
            break;
        }
    }
    Ok((n >= MIN_TS_PACKETS).then(|| Hit::new(ext, n * pk as u64)))
}

pub(super) const PS_MAX: u64 = 64 * GIB;

/// MPEG program streams (DVD .VOB, .MPG): pack headers and packets, each with its length, up to
/// the end code or until they stop.
pub(super) fn ps(s: &Src, off: u64) -> io::Result<Option<Hit>> {
    let mut st = Stream::new(*s, off);
    let mut packs = 0u64;
    loop {
        if st.offset() - off > PS_MAX {
            break;
        }
        let b = st.peek(16)?;
        if b.len() < 6 || !has(b, 0, &[0, 0, 1]) {
            break;
        }
        let len = match b[3] {
            0xBA if b.len() >= 14 && b[4] >> 6 == 1 => 14 + u64::from(b[13] & 7),
            0xBA if b[4] >> 4 == 2 => 12,
            0xB9 => {
                st.advance(4);
                break;
            }
            0xBB..=0xFF => 6 + be(b, 4, 2),
            _ => break,
        };
        packs += u64::from(b[3] == 0xBA);
        st.advance(len);
    }
    let e = st.offset();
    Ok((packs >= 16).then(|| Hit::new("mpg", e - off)))
}
