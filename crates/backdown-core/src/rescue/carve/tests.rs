use super::*;

/// Bytes that are no file's signature, after a file: its end has to come from its structure.
fn junk(n: usize) -> Vec<u8> {
    (0..n).map(|i| 0x20 + (i * 7 % 90) as u8).collect()
}

fn with_junk(mut v: Vec<u8>) -> Vec<u8> {
    v.extend(junk(3000));
    v
}

fn carve_with(img: &[u8], opts: &Options) -> Vec<Found> {
    let m = img.to_vec();
    let mut out = Vec::new();
    scan_medium(
        &m,
        m.len() as u64,
        opts,
        &mut |_| {},
        &AtomicBool::new(false),
        &mut |f| {
            out.push(f.clone());
            Ok(())
        },
    )
    .unwrap();
    out
}

fn carve(img: &[u8]) -> Vec<Found> {
    carve_with(img, &Options::default())
}

/// What the parser for the signature at the start of `b` makes of it.
fn one(b: &[u8]) -> Option<Hit> {
    let s = Src::mem(b);
    parse(sniff(b)?, &s, 0).unwrap()
}

fn at(img: &mut Vec<u8>, off: usize, b: &[u8]) {
    if img.len() < off + b.len() {
        img.resize(off + b.len(), 0x33);
    }
    img[off..off + b.len()].copy_from_slice(b);
}

// ---------- JPEG

fn seg(m: u8, p: &[u8]) -> Vec<u8> {
    let mut v = vec![0xFF, m];
    v.extend(((p.len() + 2) as u16).to_be_bytes());
    v.extend(p);
    v
}

fn jpeg_head(sof: u8) -> Vec<u8> {
    let mut v = vec![0xFF, 0xD8];
    v.extend(seg(0xE0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0"));
    v.extend(seg(0xDB, &[0; 65]));
    v.extend(seg(sof, &[8, 0, 6, 0, 8, 1, 1, 0x11, 0]));
    v.extend(seg(0xC4, &[0; 17]));
    v
}

fn sos() -> Vec<u8> {
    seg(0xDA, &[1, 1, 0, 0, 63, 0])
}

fn baseline() -> Vec<u8> {
    let mut v = jpeg_head(0xC0);
    v.extend(sos());
    v.extend([
        0x12, 0xFF, 0x00, 0x34, 0xFF, 0xD3, 0x56, 0xFF, 0xFF, 0xD4, 0x78,
    ]);
    v.extend([0xFF, 0xD9]);
    v
}

#[test]
fn jpeg_ends_at_its_end_marker_past_stuffing_and_restarts() {
    let j = baseline();
    let h = one(&with_junk(j.clone())).unwrap();
    assert_eq!((h.ext, h.size, h.complete), ("jpg", j.len() as u64, true));
    assert_eq!(h.dims, Some((8, 6)));
}

#[test]
fn progressive_jpeg_has_several_scans() {
    let mut j = jpeg_head(0xC2);
    j.extend(sos());
    j.extend([1, 2, 3, 0xFF, 0x00, 4]);
    j.extend(seg(0xC4, &[0x10; 17]));
    j.extend(sos());
    j.extend([5, 6, 0xFF, 0xD0, 7]);
    j.extend([0xFF, 0xD9]);
    let h = one(&with_junk(j.clone())).unwrap();
    assert_eq!((h.size, h.complete), (j.len() as u64, true));
}

#[test]
fn jpeg_signature_in_junk_is_nothing() {
    assert!(one(&with_junk(vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 16, 1, 2, 3])).is_none());
    // no scan before the end marker
    let mut j = jpeg_head(0xC0);
    j.extend([0xFF, 0xD9]);
    assert!(one(&with_junk(j)).is_none());
    // a scan without the tables before it
    let mut j = vec![0xFF, 0xD8];
    j.extend(sos());
    j.extend([1, 2, 3, 0xFF, 0xD9]);
    assert!(one(&with_junk(j)).is_none());
    // FF D8 FF followed by a marker that can't open a JPEG
    assert!(one(&with_junk(vec![0xFF, 0xD8, 0xFF, 0x05, 0, 4, 0, 0])).is_none());
}

#[test]
fn a_split_up_jpeg_is_kept_to_where_it_breaks() {
    let mut j = jpeg_head(0xC0);
    j.extend(sos());
    j.extend([1, 2, 3, 4, 5]);
    let broken_at = j.len();
    j.extend([0xFF, 0xD8, 0xFF, 0xE0]); // another photo's start, where this one's data should go on
    let h = one(&with_junk(j)).unwrap();
    assert_eq!((h.size, h.complete), (broken_at as u64, false));
    assert!(h.note.unwrap().contains("damaged"));
    // cut off by the end of what's scanned
    let mut j = jpeg_head(0xC0);
    j.extend(sos());
    j.extend([1, 2, 3, 4, 5]);
    let h = one(&j).unwrap();
    assert_eq!((h.size, h.complete), (j.len() as u64, false));
}

#[test]
fn jpeg_mpf_preview_is_part_of_the_photo() {
    let preview = baseline();
    let mut rest = jpeg_head(0xC0)[2..].to_vec();
    rest.extend(sos());
    rest.extend([9, 8, 7]);
    rest.extend([0xFF, 0xD9]);
    // APP2 "MPF": a TIFF-style index of two images; offsets from the index's TIFF header
    let app2_len = 4 + 4 + 8 + 2 + 2 * 12 + 4 + 32;
    let main_len = 2 + app2_len + rest.len();
    let base = 2 + 4 + 4;
    let mut t = b"II*\0".to_vec();
    t.extend(8u32.to_le_bytes());
    t.extend(2u16.to_le_bytes());
    t.extend([0x00, 0xB0, 7, 0, 4, 0, 0, 0]);
    t.extend(b"0100");
    t.extend([0x02, 0xB0, 7, 0, 32, 0, 0, 0]);
    t.extend(38u32.to_le_bytes());
    t.extend(0u32.to_le_bytes());
    for (size, o) in [(main_len, 0), (preview.len(), main_len - base)] {
        t.extend(0u32.to_le_bytes());
        t.extend((size as u32).to_le_bytes());
        t.extend((o as u32).to_le_bytes());
        t.extend([0; 4]);
    }
    let mut p = b"MPF\0".to_vec();
    p.extend(t);
    let mut j = vec![0xFF, 0xD8];
    j.extend(seg(0xE2, &p));
    assert_eq!(j.len(), 2 + app2_len);
    j.extend(rest);
    j.extend(&preview);
    let h = one(&with_junk(j.clone())).unwrap();
    assert_eq!((h.size, h.complete), (j.len() as u64, true));
}

#[test]
fn exif_dates_and_cameras() {
    assert_eq!(days_from_civil(1970, 1, 1), 0);
    assert_eq!(days_from_civil(2000, 3, 1), 11_017);
    assert_eq!(exif_time("2019:08:14 10:22:33"), Some(1_565_778_153));
    assert_eq!(exif_time("0000:00:00 00:00:00"), None);
    assert_eq!(exif_time("    :  :     :  :  "), None);
    assert_eq!(exif_time("2019:13:14 10:22:33"), None);
    let c = |a: Option<&str>, b: Option<&str>| camera(a.map(Into::into), b.map(Into::into));
    assert_eq!(
        c(Some("Canon"), Some("Canon EOS 80D")).unwrap(),
        "Canon EOS 80D"
    );
    assert_eq!(
        c(Some("NIKON CORPORATION"), Some("NIKON D750")).unwrap(),
        "NIKON D750"
    );
    assert_eq!(
        c(Some("Apple"), Some("iPhone 12")).unwrap(),
        "Apple iPhone 12"
    );
    assert_eq!(c(None, Some("X100")).unwrap(), "X100");
    assert_eq!(c(None, None), None);
}

// ---------- PNG, GIF, BMP, RIFF

fn chunk(typ: &[u8], data: &[u8]) -> Vec<u8> {
    let mut v = (data.len() as u32).to_be_bytes().to_vec();
    v.extend(typ);
    v.extend(data);
    v.extend(crc32_update(crc32(typ), data).to_be_bytes());
    v
}

fn png_file() -> Vec<u8> {
    let mut v = PNG_SIG.to_vec();
    v.extend(chunk(b"IHDR", &[0, 0, 0, 3, 0, 0, 0, 2, 8, 6, 0, 0, 0]));
    v.extend(chunk(b"IDAT", &[0x78, 1, 1, 2, 0, 0xFD, 0xFF, 0, 0]));
    v.extend(chunk(b"IEND", b""));
    v
}

#[test]
fn png_chunks_and_their_crcs() {
    let p = png_file();
    let h = one(&with_junk(p.clone())).unwrap();
    assert_eq!(
        (h.ext, h.size, h.complete, h.dims),
        ("png", p.len() as u64, true, Some((3, 2)))
    );
    // a wrong CRC in the header: not a PNG
    let mut bad = p.clone();
    bad[29] ^= 1;
    assert!(one(&with_junk(bad)).is_none());
    // a wrong CRC after the image data: kept up to there
    let mut bad = p.clone();
    let iend = p.len() - 12;
    bad[iend + 8] ^= 1;
    let h = one(&with_junk(bad)).unwrap();
    assert_eq!((h.size, h.complete), (iend as u64, false));
}

#[test]
fn gif_bmp_and_riff() {
    let mut g = b"GIF89a\x01\0\x01\0\x80\0\0".to_vec();
    g.extend([0, 0, 0, 255, 255, 255]);
    g.extend([0x21, 0xF9, 4, 0, 0, 0, 0, 0]);
    g.extend([0x2C, 0, 0, 0, 0, 1, 0, 1, 0, 0, 2, 2, 0x4C, 1, 0, 0x3B]);
    let h = one(&with_junk(g.clone())).unwrap();
    assert_eq!(
        (h.ext, h.size, h.dims),
        ("gif", g.len() as u64, Some((1, 1)))
    );

    let mut b = b"BM".to_vec();
    b.extend(70u32.to_le_bytes());
    b.extend([0; 4]);
    b.extend(54u32.to_le_bytes());
    b.extend(40u32.to_le_bytes());
    b.extend(2i32.to_le_bytes());
    b.extend(2i32.to_le_bytes());
    b.extend(1u16.to_le_bytes());
    b.extend(24u16.to_le_bytes());
    b.extend([0; 24]);
    b.extend([0x10; 16]);
    let h = one(&with_junk(b.clone())).unwrap();
    assert_eq!((h.ext, h.size, h.dims), ("bmp", 70, Some((2, 2))));
    // a size that doesn't fit the pixels
    b[2..6].copy_from_slice(&60u32.to_le_bytes());
    assert!(one(&with_junk(b)).is_none());

    let mut w = b"RIFF".to_vec();
    w.extend((4 + 24 + 8 + 100u32).to_le_bytes());
    w.extend(b"WAVEfmt ");
    w.extend(16u32.to_le_bytes());
    w.extend([1, 0, 1, 0, 0x44, 0xAC, 0, 0, 0x88, 0x58, 1, 0, 2, 0, 16, 0]);
    w.extend(b"data");
    w.extend(100u32.to_le_bytes());
    w.extend([0x11; 100]);
    let h = one(&with_junk(w.clone())).unwrap();
    assert_eq!((h.ext, h.size, h.complete), ("wav", w.len() as u64, true));
    // chunks that don't add up to the RIFF size
    w[40..44].copy_from_slice(&90u32.to_le_bytes());
    let h = one(&with_junk(w.clone())).unwrap();
    assert!(!h.complete);
    // a recording whose sizes were never written: up to the next file
    w[4..8].copy_from_slice(&0u32.to_le_bytes());
    w[40..44].copy_from_slice(&0u32.to_le_bytes());
    w.resize(1024, 0x11);
    w.extend(png_file());
    w.extend(junk(600));
    let h = one(&w).unwrap();
    assert_eq!((h.ext, h.size, h.complete), ("wav", 1024, false));

    let mut p = b"RIFF".to_vec();
    p.extend((4 + 8 + 10u32).to_le_bytes());
    p.extend(b"WEBPVP8L");
    p.extend(10u32.to_le_bytes());
    // signature, then width-1 and height-1 in 14 bits each
    let dims: u32 = 99 | (49 << 14);
    p.push(0x2F);
    p.extend(dims.to_le_bytes());
    p.extend([0; 5]);
    let h = one(&with_junk(p.clone())).unwrap();
    assert_eq!(
        (h.ext, h.size, h.dims),
        ("webp", p.len() as u64, Some((100, 50)))
    );
}

// ---------- ISO media

fn bx(t: &[u8], body: &[u8]) -> Vec<u8> {
    let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend(t);
    v.extend(body);
    v
}

fn ftyp(brand: &[u8]) -> Vec<u8> {
    let mut b = brand.to_vec();
    b.extend([0, 0, 0, 0]);
    b.extend(brand);
    bx(b"ftyp", &b)
}

fn moov() -> Vec<u8> {
    let mut mvhd = vec![0u8; 100];
    mvhd[4..8].copy_from_slice(&(1_600_000_000u32 + 2_082_844_800).to_be_bytes());
    bx(b"moov", &bx(b"mvhd", &mvhd))
}

#[test]
fn iso_brands() {
    assert_eq!(iso_ext(b"qt  ", b""), "mov");
    assert_eq!(iso_ext(b"heic", b"mif1heic"), "heic");
    assert_eq!(iso_ext(b"mif1", b"mif1avif"), "avif");
    assert_eq!(iso_ext(b"M4A ", b"M4A isom"), "m4a");
    assert_eq!(iso_ext(b"3gp5", b""), "3gp");
    assert_eq!(iso_ext(b"crx ", b""), "cr3");
    assert_eq!(iso_ext(b"isom", b"isomavc1"), "mp4");
}

#[test]
fn iso_boxes_large_sizes_and_open_ends() {
    let mut v = ftyp(b"isom");
    v.extend(moov());
    // a 64-bit size
    let data = [0x42u8; 1000];
    v.extend(1u32.to_be_bytes());
    v.extend(b"mdat");
    v.extend((16 + data.len() as u64).to_be_bytes());
    v.extend(data);
    let h = one(&with_junk(v.clone())).unwrap();
    assert_eq!((h.ext, h.size, h.complete), ("mp4", v.len() as u64, true));
    assert!(h.taken.is_some());

    // size 0: "to the end of the file", which a carver doesn't know: up to the next file
    let mut v = ftyp(b"qt  ");
    v.extend(moov());
    v.extend(0u32.to_be_bytes());
    v.extend(b"mdat");
    v.resize(4096, 0x42);
    v.extend(png_file());
    v.extend(junk(1000));
    let h = one(&v).unwrap();
    assert_eq!((h.ext, h.size, h.complete), ("mov", 4096, false));

    // a recording that was never finished: media data but no index
    let mut v = ftyp(b"isom");
    v.extend(bx(b"mdat", &[1; 500]));
    let h = one(&with_junk(v.clone())).unwrap();
    assert_eq!((h.size, h.complete), (v.len() as u64, false));

    // an ftyp alone isn't a file
    assert!(one(&with_junk(ftyp(b"isom"))).is_none());

    // HEIC: meta and mdat, no moov
    let mut v = ftyp(b"heic");
    v.extend(bx(b"meta", &[0; 40]));
    v.extend(bx(b"mdat", &[7; 300]));
    let h = one(&with_junk(v.clone())).unwrap();
    assert_eq!((h.ext, h.size, h.complete), ("heic", v.len() as u64, true));
}

#[test]
fn old_quicktime_movies_back_to_back() {
    let mut m = bx(b"wide", b"");
    m.extend(bx(b"mdat", &[5; 700]));
    m.extend(moov());
    let mut img = m.clone();
    let second = img.len().div_ceil(512) * 512;
    img.resize(second, 0x33);
    img.extend(&m);
    img.extend(junk(2000));
    let f = carve(&img);
    let got: Vec<(u64, u64, &str)> = f.iter().map(|f| (f.offset, f.size, f.ext)).collect();
    assert_eq!(
        got,
        [
            (0, m.len() as u64, "mov"),
            (second as u64, m.len() as u64, "mov")
        ]
    );
    // two exactly a sector long, one right after the other with no gap: still two
    let mut m = bx(b"wide", b"");
    m.extend(bx(b"mdat", &[6; 1024 - 8 - 8 - 116]));
    m.extend(moov());
    assert_eq!(m.len(), 1024);
    let mut img = m.clone();
    img.extend(&m);
    img.extend(junk(2000));
    let got: Vec<(u64, u64)> = carve(&img).iter().map(|f| (f.offset, f.size)).collect();
    assert_eq!(got, [(0, 1024), (1024, 1024)]);
}

// ---------- PDF

fn pdf_file(extra_eof_at_start: bool) -> Vec<u8> {
    let mut v = b"%PDF-1.7\n".to_vec();
    if extra_eof_at_start {
        // a linearized file's first trailer points at 0
        v.extend(b"trailer\n<< >>\nstartxref\n0\n%%EOF\n");
    }
    let o1 = v.len();
    v.extend(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let x = v.len();
    v.extend(format!("xref\n0 2\n0000000000 65535 f \n{o1:010} 00000 n \ntrailer\n<< /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\r\n").as_bytes());
    v
}

#[test]
fn pdf_ends_at_its_own_last_eof() {
    let p = pdf_file(true);
    let h = one(&with_junk(p.clone())).unwrap();
    assert_eq!((h.ext, h.size, h.complete), ("pdf", p.len() as u64, true));

    // another PDF right after (its startxref counts from its own start)
    let mut img = p.clone();
    img.resize(img.len().div_ceil(512) * 512, b' ');
    let second = img.len();
    img.extend(pdf_file(false));
    img.extend(junk(1000));
    let f = carve(&img);
    let got: Vec<(u64, u64)> = f.iter().map(|f| (f.offset, f.size)).collect();
    assert_eq!(
        got,
        [
            (0, p.len() as u64),
            (second as u64, img.len() as u64 - 1000 - second as u64)
        ]
    );
}

#[test]
fn a_pdf_without_its_end_stops_at_the_next_file() {
    let mut v = b"%PDF-1.4\n1 0 obj\n<< >>\nendobj\n".to_vec();
    v.resize(2048, b' ');
    let next = v.len();
    v.extend(png_file());
    v.extend(junk(2000));
    let h = one(&v).unwrap();
    assert_eq!((h.size, h.complete), (next as u64, false));
    // slack after the end that happens to look like an update: still ends at the last good
    // %%EOF, found once the next file shows up
    let p = pdf_file(false);
    let mut img = p.clone();
    img.extend(b"12 junk that looks like an update");
    img.resize(img.len().div_ceil(512) * 512, b'7');
    let next = img.len();
    img.extend(png_file());
    img.extend(junk(600));
    let got: Vec<(u64, u64)> = carve(&img).iter().map(|f| (f.offset, f.size)).collect();
    assert_eq!(
        got,
        [(0, p.len() as u64), (next as u64, png_file().len() as u64)]
    );
    // a startxref pointing at something that isn't a table doesn't end it
    let mut v = b"%PDF-1.4\nhello\nstartxref\n9\n%%EOF\n".to_vec();
    v.extend(junk(100));
    let h = one(&v).unwrap();
    assert!(!h.complete);
}

// ---------- ZIP

fn local(name: &str, data: &[u8], streamed: bool) -> Vec<u8> {
    let mut v = b"PK\x03\x04".to_vec();
    v.extend(20u16.to_le_bytes());
    v.extend((if streamed { 8u16 } else { 0 }).to_le_bytes());
    v.extend([0; 6]);
    let (crc, size) = if streamed {
        (0, 0)
    } else {
        (crc32(data), data.len() as u32)
    };
    v.extend(crc.to_le_bytes());
    v.extend(size.to_le_bytes());
    v.extend(size.to_le_bytes());
    v.extend((name.len() as u16).to_le_bytes());
    v.extend(0u16.to_le_bytes());
    v.extend(name.as_bytes());
    v.extend(data);
    if streamed {
        v.extend(b"PK\x07\x08");
        v.extend(crc32(data).to_le_bytes());
        v.extend((data.len() as u32).to_le_bytes());
        v.extend((data.len() as u32).to_le_bytes());
    }
    v
}

fn central(name: &str, data: &[u8], at: usize) -> Vec<u8> {
    let mut v = b"PK\x01\x02".to_vec();
    v.extend([20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    v.extend(crc32(data).to_le_bytes());
    v.extend((data.len() as u32).to_le_bytes());
    v.extend((data.len() as u32).to_le_bytes());
    v.extend((name.len() as u16).to_le_bytes());
    v.extend([0; 12]);
    v.extend((at as u32).to_le_bytes());
    v.extend(name.as_bytes());
    v
}

fn zip_file(entries: &[(&str, &[u8])], streamed: bool, zip64: bool) -> Vec<u8> {
    let mut v = Vec::new();
    let mut cd = Vec::new();
    for (n, d) in entries {
        cd.extend(central(n, d, v.len()));
        v.extend(local(n, d, streamed));
    }
    let cd_at = v.len();
    v.extend(&cd);
    if zip64 {
        let rec = v.len();
        v.extend(b"PK\x06\x06");
        v.extend(44u64.to_le_bytes());
        v.extend([45, 0, 45, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        v.extend((entries.len() as u64).to_le_bytes());
        v.extend((entries.len() as u64).to_le_bytes());
        v.extend((cd.len() as u64).to_le_bytes());
        v.extend((cd_at as u64).to_le_bytes());
        v.extend(b"PK\x06\x07");
        v.extend(0u32.to_le_bytes());
        v.extend((rec as u64).to_le_bytes());
        v.extend(1u32.to_le_bytes());
        v.extend(b"PK\x05\x06");
        v.extend([0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF]);
        v.extend([0xFF; 8]);
        v.extend(0u16.to_le_bytes());
    } else {
        v.extend(b"PK\x05\x06");
        v.extend([0; 4]);
        v.extend((entries.len() as u16).to_le_bytes());
        v.extend((entries.len() as u16).to_le_bytes());
        v.extend((cd.len() as u32).to_le_bytes());
        v.extend((cd_at as u32).to_le_bytes());
        v.extend(5u16.to_le_bytes());
        v.extend(b"hello");
    }
    v
}

#[test]
fn zip_end_record_has_to_agree_with_the_start() {
    let z = zip_file(&[("a.txt", b"alpha"), ("b.txt", b"beta")], false, false);
    let h = one(&with_junk(z.clone())).unwrap();
    assert_eq!((h.ext, h.size, h.complete), ("zip", z.len() as u64, true));

    // written as a stream (sizes after the data): the end record is searched for
    let z = zip_file(
        &[
            ("[Content_Types].xml", b"<Types/>"),
            ("xl/workbook.xml", b"<w/>"),
        ],
        true,
        false,
    );
    let h = one(&with_junk(z.clone())).unwrap();
    assert_eq!((h.ext, h.size, h.complete), ("xlsx", z.len() as u64, true));

    // ZIP64 end records
    let z = zip_file(
        &[("[Content_Types].xml", b"<Types/>"), ("ppt/p.xml", b"<p/>")],
        false,
        true,
    );
    let h = one(&with_junk(z.clone())).unwrap();
    assert_eq!((h.ext, h.size, h.complete), ("pptx", z.len() as u64, true));

    // an OpenDocument says what it is in its first entry
    let z = zip_file(
        &[
            ("mimetype", b"application/vnd.oasis.opendocument.text"),
            ("content.xml", b"<x/>"),
        ],
        false,
        false,
    );
    assert_eq!(one(&with_junk(z)).unwrap().ext, "odt");

    // a ZIP inside a ZIP (stored): the inner end record doesn't end the outer one
    let inner = zip_file(&[("in.txt", b"inside")], false, false);
    let outer = zip_file(&[("inner.zip", &inner), ("c.txt", b"gamma")], true, false);
    let h = one(&with_junk(outer.clone())).unwrap();
    assert_eq!(h.size, outer.len() as u64);

    // no end record at all: kept up to what follows
    let mut z = local("a.txt", b"alpha", false);
    z.resize(1024, 0x33);
    z.extend(png_file());
    z.extend(junk(1000));
    let h = one(&z).unwrap();
    assert_eq!((h.size, h.complete), (1024, false));
}

// ---------- compound files, SQLite

fn cfb_file(stream: &str) -> Vec<u8> {
    let free = 0xFFFF_FFFFu32;
    let mut h = vec![0u8; 512];
    h[..8].copy_from_slice(CFB_SIG);
    h[24..34].copy_from_slice(&[0x3E, 0, 3, 0, 0xFE, 0xFF, 9, 0, 6, 0]);
    h[44..48].copy_from_slice(&1u32.to_le_bytes());
    h[48..52].copy_from_slice(&1u32.to_le_bytes());
    h[56..60].copy_from_slice(&4096u32.to_le_bytes());
    h[60..64].copy_from_slice(&0xFFFF_FFFEu32.to_le_bytes());
    h[68..72].copy_from_slice(&0xFFFF_FFFEu32.to_le_bytes());
    for i in 0..109 {
        let v = if i == 0 { 0 } else { free };
        h[76 + 4 * i..80 + 4 * i].copy_from_slice(&v.to_le_bytes());
    }
    let mut fat = vec![0xFFFF_FFFDu32, 0xFFFF_FFFE];
    fat.extend(3..=9u32);
    fat.push(0xFFFF_FFFE);
    fat.resize(128, free);
    let entry = |name: &str, typ: u8, child: u32, start: u32, size: u32| {
        let mut e = vec![0u8; 128];
        let units: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
        e[..units.len()].copy_from_slice(&units);
        let nl = if name.is_empty() { 0 } else { units.len() + 2 };
        e[64..66].copy_from_slice(&(nl as u16).to_le_bytes());
        e[66] = typ;
        e[68..72].copy_from_slice(&free.to_le_bytes());
        e[72..76].copy_from_slice(&free.to_le_bytes());
        e[76..80].copy_from_slice(&child.to_le_bytes());
        e[116..120].copy_from_slice(&start.to_le_bytes());
        e[120..124].copy_from_slice(&size.to_le_bytes());
        e
    };
    let mut v = h;
    v.extend(fat.iter().flat_map(|x| x.to_le_bytes()));
    v.extend(entry("Root Entry", 5, 1, 0xFFFF_FFFE, 0));
    v.extend(entry(stream, 2, free, 2, 4096));
    v.extend(entry("", 0, free, 0, 0));
    v.extend(entry("", 0, free, 0, 0));
    v.extend(vec![0x44; 4096]);
    v
}

#[test]
fn compound_files_are_named_by_their_streams() {
    for (stream, ext) in [
        ("WordDocument", "doc"),
        ("Workbook", "xls"),
        ("PowerPoint Document", "ppt"),
        ("__substg1.0_0037001F", "msg"),
        ("Something", "ole"),
    ] {
        let c = cfb_file(stream);
        let h = one(&with_junk(c.clone())).unwrap();
        assert_eq!(
            (h.ext, h.size, h.complete),
            (ext, c.len() as u64, true),
            "{stream}"
        );
    }
    // a FAT pointing past itself: not a compound file
    let mut c = cfb_file("WordDocument");
    c[512 + 8..512 + 12].copy_from_slice(&5000u32.to_le_bytes());
    assert!(one(&with_junk(c)).is_none());
}

#[test]
fn sqlite_size_from_its_header() {
    let mut d = vec![0u8; 2 * 65_536];
    d[..16].copy_from_slice(b"SQLite format 3\0");
    d[16..18].copy_from_slice(&1u16.to_be_bytes()); // 65536
    d[18..24].copy_from_slice(&[1, 1, 0, 64, 32, 32]);
    d[24..28].copy_from_slice(&3u32.to_be_bytes());
    d[28..32].copy_from_slice(&2u32.to_be_bytes());
    d[92..96].copy_from_slice(&3u32.to_be_bytes());
    d[100] = 0x0D;
    let h = one(&with_junk(d.clone())).unwrap();
    assert_eq!((h.ext, h.size, h.complete), ("sqlite", 131_072, true));
    // a size that isn't current: up to the next file
    d[92..96].copy_from_slice(&2u32.to_be_bytes());
    let mut img = d.clone();
    img.extend(png_file());
    img.extend(junk(600));
    let h = one(&img).unwrap();
    assert_eq!((h.size, h.complete), (131_072, false));
}

// ---------- audio

#[test]
fn mpeg_audio_frame_lengths() {
    assert_eq!(mpa(0xFFFB_9000).unwrap().len, 417);
    assert_eq!(mpa(0xFFFB_9200).unwrap().len, 418);
    assert_eq!(mpa(0xFFF3_8000).unwrap().len, 208); // MPEG-2 layer III, 64 kbit/s, 22.05 kHz
    assert_eq!(mpa(0xFFFF_1000).unwrap().len, 32); // MPEG-1 layer I, 32 kbit/s, 44.1 kHz
    assert!(mpa(0xFFFB_F000).is_none()); // bad bitrate
    assert!(mpa(0xFFFB_9C00).is_none()); // reserved sample rate
    assert!(mpa(0xFFE9_9000).is_none()); // reserved version
}

fn frames(n: usize) -> Vec<u8> {
    let mut v = Vec::new();
    for i in 0..n {
        v.extend([0xFF, 0xFB, 0x90, 0x00]);
        v.extend((0..413).map(|k| ((i * 31 + k * 7) % 251) as u8));
    }
    v
}

#[test]
fn mp3_with_and_without_a_tag() {
    // a long enough run of bare frames
    let f = frames(150);
    let h = one(&with_junk(f.clone())).unwrap();
    assert_eq!((h.ext, h.size), ("mp3", f.len() as u64));
    // a short run isn't enough without a tag
    assert!(one(&with_junk(frames(50))).is_none());
    // with an ID3v2 tag, a few frames are
    let mut t = b"ID3\x04\0\0\0\0\0\x0A".to_vec();
    t.extend([0; 10]);
    t.extend(frames(5));
    t.extend(b"TAG");
    t.extend([b' '; 125]);
    let h = one(&with_junk(t.clone())).unwrap();
    assert_eq!((h.ext, h.size, h.complete), ("mp3", t.len() as u64, true));
}

fn flac_file(n: usize) -> Vec<u8> {
    let mut v = b"fLaC".to_vec();
    v.extend([0x80, 0, 0, 34]);
    let mut si = vec![0u8; 34];
    si[0..2].copy_from_slice(&4096u16.to_be_bytes());
    si[2..4].copy_from_slice(&4096u16.to_be_bytes());
    si[7..10].copy_from_slice(&708u32.to_be_bytes()[1..]); // the longest frame
    let packed: u64 = (44_100 << 44) | (1 << 41) | (15 << 36) | (n as u64 * 4096);
    si[10..18].copy_from_slice(&packed.to_be_bytes());
    v.extend(si);
    for i in 0..n {
        let mut f = vec![0xFF, 0xF8, 0xC9, 0x18, i as u8];
        f.push(crc8(&f));
        f.extend((0..700).map(|k| ((i * 13 + k * 5) % 239) as u8));
        let mut crc = 0u16;
        for &x in &f {
            crc = (crc << 8) ^ CRC16[usize::from((crc >> 8) as u8 ^ x)];
        }
        f.extend(crc.to_be_bytes());
        v.extend(f);
    }
    v
}

#[test]
fn flac_frames_end_where_the_samples_do() {
    let f = flac_file(6);
    let h = one(&with_junk(f.clone())).unwrap();
    assert_eq!((h.ext, h.size, h.complete), ("flac", f.len() as u64, true));
    // damaged in the middle: kept up to the frame that breaks
    let mut d = f.clone();
    let third = 8 + 34 + 2 * 708;
    d[third + 4] ^= 0x40;
    d[third + 100] ^= 0x40;
    let h = one(&with_junk(d)).unwrap();
    assert!(!h.complete);
    assert_eq!(h.size, third as u64);
}

fn ogg_page(flags: u8, seq: u32, body: &[u8]) -> Vec<u8> {
    let mut p = b"OggS\0".to_vec();
    p.push(flags);
    p.extend(0u64.to_le_bytes());
    p.extend(0x1234u32.to_le_bytes());
    p.extend(seq.to_le_bytes());
    p.extend([0; 4]);
    let mut table = vec![255u8; body.len() / 255];
    table.push((body.len() % 255) as u8);
    p.push(table.len() as u8);
    p.extend(table);
    p.extend(body);
    let crc = ogg_crc(0, &p);
    p[22..26].copy_from_slice(&crc.to_le_bytes());
    p
}

#[test]
fn ogg_pages_with_their_crcs() {
    let mut o = ogg_page(2, 0, b"\x01vorbis\0\0\0\0\x02\x44\xAC\0\0");
    o.extend(ogg_page(0, 1, &[3; 600]));
    o.extend(ogg_page(4, 2, &[4; 100]));
    let h = one(&with_junk(o.clone())).unwrap();
    assert_eq!((h.ext, h.size, h.complete), ("ogg", o.len() as u64, true));
    let mut o = ogg_page(2, 0, b"OpusHead\x01\x02");
    o.extend(ogg_page(4, 1, &[1; 10]));
    assert_eq!(one(&with_junk(o)).unwrap().ext, "opus");
    // a page with a wrong CRC ends the file (without its end-of-stream page: incomplete)
    let mut o = ogg_page(2, 0, b"\x01vorbis\0\0");
    let good = {
        o.extend(ogg_page(0, 1, &[3; 60]));
        o.len()
    };
    let mut bad = ogg_page(4, 2, &[4; 60]);
    bad[30] ^= 1;
    o.extend(bad);
    let h = one(&with_junk(o)).unwrap();
    assert_eq!((h.size, h.complete), (good as u64, false));
}

// ---------- video, archives, raw

fn ebml_file(unknown: bool) -> Vec<u8> {
    let mut v = vec![0x1A, 0x45, 0xDF, 0xA3, 0x8B, 0x42, 0x82, 0x88];
    v.extend(b"matroska");
    let mut kids = vec![0x15, 0x49, 0xA9, 0x66, 0x84, 1, 2, 3, 4];
    kids.extend([0x1F, 0x43, 0xB6, 0x75]);
    if unknown {
        kids.push(0xFF); // a cluster of unknown size: its children follow
        kids.extend([0xE7, 0x81, 0x00, 0xA3, 0x85, 0x81, 0, 0, 0x80, 9]);
        kids.extend([0xA3, 0x83, 0x81, 0, 1]);
    } else {
        kids.push(0x90);
        kids.extend([0x55; 16]);
    }
    v.extend([0x18, 0x53, 0x80, 0x67]);
    if unknown {
        v.extend([0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
    } else {
        v.push(0x01);
        v.extend(&(kids.len() as u64).to_be_bytes()[1..]);
    }
    v.extend(kids);
    v
}

#[test]
fn matroska_known_and_unknown_sizes() {
    let m = ebml_file(false);
    let h = one(&with_junk(m.clone())).unwrap();
    assert_eq!((h.ext, h.size, h.complete), ("mkv", m.len() as u64, true));
    let m = ebml_file(true);
    let h = one(&with_junk(m.clone())).unwrap();
    assert_eq!((h.ext, h.size), ("mkv", m.len() as u64));
}

#[test]
fn rar_and_7z() {
    let block = |body: &[u8]| {
        let mut head = vec![body.len() as u8];
        head.extend(body);
        let mut v = crc32(&head).to_le_bytes().to_vec();
        v.extend(head);
        v
    };
    let mut r = b"Rar!\x1a\x07\x01\x00".to_vec();
    r.extend(block(&[1, 0, 0]));
    r.extend(block(&[5, 0, 0]));
    let h = one(&with_junk(r.clone())).unwrap();
    assert_eq!((h.ext, h.size, h.complete), ("rar", r.len() as u64, true));

    let old = |typ: u8, flags: u16, rest: &[u8]| {
        let mut b = vec![typ];
        b.extend(flags.to_le_bytes());
        b.extend(((5 + rest.len()) as u16 + 2).to_le_bytes());
        b.extend(rest);
        let mut v = ((crc32(&b) & 0xFFFF) as u16).to_le_bytes().to_vec();
        v.extend(b);
        v
    };
    let mut r = b"Rar!\x1a\x07\x00".to_vec();
    r.extend(old(0x73, 0, &[0; 6]));
    r.extend(old(0x7B, 0x4000, &[]));
    let h = one(&with_junk(r.clone())).unwrap();
    assert_eq!((h.ext, h.size), ("rar", r.len() as u64));

    let next = [1u8, 4, 6, 0, 1, 9, 0x20, 0, 0, 0];
    let mut start = 100u64.to_le_bytes().to_vec();
    start.extend((next.len() as u64).to_le_bytes());
    start.extend(crc32(&next).to_le_bytes());
    let mut z = SEVENZ_SIG.to_vec();
    z.extend([0, 4]);
    z.extend(crc32(&start).to_le_bytes());
    z.extend(start);
    z.extend([0x77; 100]);
    z.extend(next);
    let h = one(&with_junk(z.clone())).unwrap();
    assert_eq!((h.ext, h.size, h.complete), ("7z", z.len() as u64, true));
}

fn tiff_file(make: Option<&str>) -> Vec<u8> {
    let e = |tag: u16, typ: u16, count: u32, value: u32| {
        let mut v = tag.to_le_bytes().to_vec();
        v.extend(typ.to_le_bytes());
        v.extend(count.to_le_bytes());
        v.extend(value.to_le_bytes());
        v
    };
    let n: u16 = if make.is_some() { 5 } else { 4 };
    let mut t = b"II*\0\x08\0\0\0".to_vec();
    t.extend(n.to_le_bytes());
    t.extend(e(0x100, 3, 1, 4));
    t.extend(e(0x101, 3, 1, 4));
    if let Some(m) = make {
        t.extend(e(0x10F, 2, m.len() as u32 + 1, 200));
    }
    t.extend(e(0x111, 4, 1, 300));
    t.extend(e(0x117, 4, 1, 48));
    t.extend(0u32.to_le_bytes());
    t.resize(200, 0);
    if let Some(m) = make {
        t.extend(m.as_bytes());
    }
    t.resize(300, 0);
    t.extend([0x80; 48]);
    t
}

#[test]
fn tiff_and_camera_raw() {
    let t = tiff_file(None);
    let h = one(&with_junk(t.clone())).unwrap();
    assert_eq!((h.ext, h.size, h.dims), ("tif", 348, Some((4, 4))));
    let t = tiff_file(Some("NIKON CORPORATION"));
    let h = one(&with_junk(t)).unwrap();
    assert_eq!((h.ext, h.size), ("nef", 348));
    assert_eq!(h.camera.as_deref(), Some("NIKON CORPORATION"));
    // a TIFF header over junk isn't a file
    assert!(one(&with_junk(b"II*\0\x08\0\0\0\xFF\xFF".to_vec())).is_none());
}

#[test]
fn camcorder_streams() {
    let mut m = Vec::new();
    for i in 0..300u32 {
        m.extend(i.to_be_bytes());
        m.push(0x47);
        m.extend(if i == 0 { [0x40, 0x00] } else { [0x01, 0x00] });
        m.extend([0x10; 185]);
    }
    let h = one(&with_junk(m.clone())).unwrap();
    assert_eq!((h.ext, h.size), ("mts", m.len() as u64));

    let mut p = Vec::new();
    for _ in 0..20 {
        p.extend([0, 0, 1, 0xBA, 0x44, 0, 4, 0, 4, 1, 1, 0x89, 0xC3, 0xF8]);
        p.extend([0, 0, 1, 0xE0, 0, 10]);
        p.extend([0x42; 10]);
    }
    p.extend([0, 0, 1, 0xB9]);
    let h = one(&with_junk(p.clone())).unwrap();
    assert_eq!((h.ext, h.size), ("mpg", p.len() as u64));
}

// ---------- the scan

#[test]
fn files_are_looked_for_at_sector_boundaries_only() {
    let mut img = junk(40_000);
    at(&mut img, 4096 * 2, &png_file());
    at(&mut img, 4096 * 4 + 512, &png_file());
    at(&mut img, 4096 * 6 + 3, &png_file());
    let offs = |sector| {
        carve_with(
            &img,
            &Options {
                sector,
                ..Options::default()
            },
        )
        .iter()
        .map(|f| f.offset)
        .collect::<Vec<_>>()
    };
    assert_eq!(offs(512), [8192, 16_896]);
    assert_eq!(offs(4096), [8192]);
    assert_eq!(offs(1), [8192, 16_896, 24_579]);
}

#[test]
fn choosing_types() {
    let want = |k: &[&str]| Want::new(Some(&k.iter().map(|x| x.to_string()).collect::<Vec<_>>()));
    let w = want(&["Photos"]).unwrap();
    assert!(w.reports("jpg") && w.reports("cr3") && !w.reports("mp4"));
    assert!(w.on[Fmt::Iso as usize] && !w.on[Fmt::Pdf as usize]);
    let w = want(&[".JPEG", "video"]).unwrap();
    assert!(w.reports("jpg") && w.reports("mov") && !w.reports("png"));
    assert!(want(&["raw"]).unwrap().reports("nef"));
    assert!(want(&["office"]).unwrap().reports("doc"));
    assert!(want(&["nope"]).is_err());
    assert!(want(&[]).is_err());
    assert!(Want::new(None).unwrap().reports("anything"));
    for t in TYPES {
        assert!(
            Fmt::ALL.iter().any(|f| f.exts().contains(&t.0)),
            "{} has no parser",
            t.0
        );
    }
    for f in Fmt::ALL {
        for e in f.exts() {
            assert!(TYPES.iter().any(|t| t.0 == *e), "{e} isn't described");
        }
    }
}

/// A source with a bad area: reads touching it fail.
struct Flaky {
    data: Vec<u8>,
    bad: std::ops::Range<u64>,
}

impl Medium for Flaky {
    fn pread(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        if off < self.bad.end && off + buf.len() as u64 > self.bad.start {
            return Err(io::Error::other("bad sector"));
        }
        self.data.pread(off, buf)
    }
}

#[test]
fn unreadable_areas_are_skipped_not_fatal() {
    let mut data = junk(1 << 20);
    at(&mut data, 700_416, &png_file());
    let m = Flaky {
        data,
        bad: 200_000..200_512,
    };
    let mut found = Vec::new();
    let stats = scan_medium(
        &m,
        1 << 20,
        &Options::default(),
        &mut |_| {},
        &AtomicBool::new(false),
        &mut |f| {
            found.push(f.offset);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(found, [700_416]);
    assert_eq!(stats.unreadable, 64 << 10);
}

#[test]
fn file_names_follow_the_sector() {
    assert_eq!(file_stem(0), "f0000000000");
    assert_eq!(file_stem(1_234_567 * 512), "f0001234567");
    assert_eq!(file_stem(1024 + 3), "f0000000002_3");
}

#[test]
fn find_and_crcs() {
    assert_eq!(find(b"abcabc", b"bc", 0), Some(1));
    assert_eq!(find(b"abcabc", b"bc", 2), Some(4));
    assert_eq!(find(b"abcabc", b"cd", 0), None);
    assert_eq!(find(b"ab", b"abc", 0), None);
    assert_eq!(rfind(b"abcabc", b"bc"), Some(4));
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    assert_eq!(crc8(b"123456789"), 0xF4);
    assert_eq!(ogg_crc(0, b"123456789"), 0x89A1_897F);
}
