//! Carving files back out of a synthetic memory card: random bytes (what's left of old files) with
//! real, minimal files placed at sector boundaries, the way a camera or a computer writes them.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use backdown_core::rescue::carve::{self, Found, Options};

// ---------- building blocks

/// xorshift64*: the same "random" card every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn bytes(&mut self, n: usize) -> Vec<u8> {
        let mut v = vec![0u8; n];
        for c in v.chunks_mut(8) {
            let r = self.next().to_le_bytes();
            c.copy_from_slice(&r[..c.len()]);
        }
        v
    }
}

fn crc32(b: &[u8]) -> u32 {
    let mut c = !0u32;
    for &x in b {
        c ^= u32::from(x);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
    }
    !c
}

fn be16(v: usize) -> [u8; 2] {
    (v as u16).to_be_bytes()
}

fn le16(v: usize) -> [u8; 2] {
    (v as u16).to_le_bytes()
}

fn le32(v: usize) -> [u8; 4] {
    (v as u32).to_le_bytes()
}

/// Compressed-looking image data: never FF except as FF 00, with restart markers now and then.
fn entropy(rng: &mut Rng, n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n + n / 64);
    for (i, b) in rng.bytes(n).into_iter().enumerate() {
        out.push(b);
        if b == 0xFF {
            out.push(0);
        }
        if i % 997 == 996 {
            out.extend([0xFF, 0xD0 + ((i / 997) % 8) as u8]);
        }
    }
    out
}

fn segment(marker: u8, payload: &[u8]) -> Vec<u8> {
    let mut v = vec![0xFF, marker];
    v.extend(be16(payload.len() + 2));
    v.extend(payload);
    v
}

/// A baseline JPEG: tables, one 16×16 grayscale frame, one scan, then `app1` (EXIF) if given.
fn jpeg(rng: &mut Rng, app1: Option<&[u8]>, data: usize, eoi: bool) -> Vec<u8> {
    let mut v = vec![0xFF, 0xD8];
    match app1 {
        Some(a) => v.extend(segment(0xE1, a)),
        None => v.extend(segment(0xE0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0")),
    }
    let mut dqt = vec![0u8];
    dqt.extend(1..=64u8);
    v.extend(segment(0xDB, &dqt));
    v.extend(segment(0xC0, &[8, 0, 16, 0, 16, 1, 1, 0x11, 0]));
    let mut dht = vec![0u8, 0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
    dht.extend(0..12u8);
    v.extend(segment(0xC4, &dht));
    v.extend(segment(0xDD, &[0, 8]));
    v.extend(segment(0xDA, &[1, 1, 0, 0, 63, 0]));
    v.extend(entropy(rng, data));
    if eoi {
        v.extend([0xFF, 0xD9]);
    }
    v
}

/// An EXIF block (little-endian TIFF) with a camera, a date, and a thumbnail JPEG at `thumb_at`
/// from the start of the TIFF header.
fn exif(make: &str, model: &str, date: &str, thumb: &[u8], thumb_at: usize) -> Vec<u8> {
    let entry = |tag: u16, typ: u16, count: usize, value: usize| {
        let mut e = tag.to_le_bytes().to_vec();
        e.extend(typ.to_le_bytes());
        e.extend(le32(count));
        e.extend(le32(value));
        e
    };
    let make_z = format!("{make}\0");
    let model_z = format!("{model}\0");
    let date_z = format!("{date}\0");
    let ifd0 = 8;
    let make_at = ifd0 + 2 + 3 * 12 + 4;
    let model_at = make_at + make_z.len();
    let exif_ifd = (model_at + model_z.len() + 1) & !1;
    let date_at = exif_ifd + 2 + 12 + 4;
    let ifd1 = date_at + date_z.len();
    let mut t = b"II*\0".to_vec();
    t.extend(le32(ifd0));
    t.extend(le16(3));
    t.extend(entry(0x010F, 2, make_z.len(), make_at));
    t.extend(entry(0x0110, 2, model_z.len(), model_at));
    t.extend(entry(0x8769, 4, 1, exif_ifd));
    t.extend(le32(ifd1));
    t.extend(make_z.as_bytes());
    t.extend(model_z.as_bytes());
    t.resize(exif_ifd, 0);
    t.extend(le16(1));
    t.extend(entry(0x9003, 2, date_z.len(), date_at));
    t.extend(le32(0));
    t.extend(date_z.as_bytes());
    t.extend(le16(2));
    t.extend(entry(0x0201, 4, 1, thumb_at));
    t.extend(entry(0x0202, 4, 1, thumb.len()));
    t.extend(le32(0));
    assert!(t.len() <= thumb_at);
    t.resize(thumb_at, 0);
    t.extend(thumb);
    let mut a = b"Exif\0\0".to_vec();
    a.extend(t);
    a
}

fn png_chunk(typ: &[u8], data: &[u8]) -> Vec<u8> {
    let mut v = (data.len() as u32).to_be_bytes().to_vec();
    v.extend(typ);
    v.extend(data);
    let mut c = typ.to_vec();
    c.extend(data);
    v.extend(crc32(&c).to_be_bytes());
    v
}

/// A real, decodable RGB PNG (its pixels in a stored deflate block).
fn png(rng: &mut Rng, w: usize, h: usize) -> Vec<u8> {
    let mut raw = Vec::new();
    for _ in 0..h {
        raw.push(0);
        raw.extend(rng.bytes(w * 3));
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + u32::from(x)) % 65_521;
        b = (b + a) % 65_521;
    }
    let mut z = vec![0x78, 0x01, 0x01];
    z.extend(le16(raw.len()));
    z.extend(le16(!raw.len() & 0xFFFF));
    z.extend(&raw);
    z.extend((b << 16 | a).to_be_bytes());
    let mut ihdr = (w as u32).to_be_bytes().to_vec();
    ihdr.extend((h as u32).to_be_bytes());
    ihdr.extend([8, 2, 0, 0, 0]);
    let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
    v.extend(png_chunk(b"IHDR", &ihdr));
    v.extend(png_chunk(b"tEXt", b"Comment\0made for a test"));
    v.extend(png_chunk(b"IDAT", &z));
    v.extend(png_chunk(b"IEND", b""));
    v
}

fn iso_box(typ: &[u8], body: &[u8]) -> Vec<u8> {
    let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend(typ);
    v.extend(body);
    v
}

/// A camera-style MP4: ftyp, the media data, then the index (moov) with its creation time.
fn mp4(rng: &mut Rng, created: u64, media: usize) -> Vec<u8> {
    let mut v = iso_box(b"ftyp", b"isom\0\0\x02\0isommp41");
    v.extend(iso_box(b"free", &[0; 8]));
    v.extend(iso_box(b"mdat", &rng.bytes(media)));
    let mut mvhd = vec![0u8; 4];
    mvhd.extend(((created + 2_082_844_800) as u32).to_be_bytes());
    mvhd.extend(((created + 2_082_844_800) as u32).to_be_bytes());
    mvhd.extend(1000u32.to_be_bytes());
    mvhd.extend(5000u32.to_be_bytes());
    mvhd.resize(100, 0);
    let mut moov = iso_box(b"mvhd", &mvhd);
    moov.extend(iso_box(b"trak", &rng.bytes(300)));
    v.extend(iso_box(b"moov", &moov));
    v
}

/// A ZIP with stored (uncompressed) entries, its central directory and end record.
fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut v = Vec::new();
    let mut cd = Vec::new();
    for (name, data) in entries {
        let at = v.len();
        let crc = crc32(data);
        v.extend(b"PK\x03\x04");
        v.extend(le16(20));
        v.extend(le16(0));
        v.extend(le16(0));
        v.extend([0, 0x60, 0x21, 0x52]); // time, date
        v.extend(crc.to_le_bytes());
        v.extend(le32(data.len()));
        v.extend(le32(data.len()));
        v.extend(le16(name.len()));
        v.extend(le16(0));
        v.extend(name.as_bytes());
        v.extend(*data);
        cd.extend(b"PK\x01\x02");
        cd.extend(le16(20));
        cd.extend(le16(20));
        cd.extend(le16(0));
        cd.extend(le16(0));
        cd.extend([0, 0x60, 0x21, 0x52]);
        cd.extend(crc.to_le_bytes());
        cd.extend(le32(data.len()));
        cd.extend(le32(data.len()));
        cd.extend(le16(name.len()));
        cd.extend([0; 12]);
        cd.extend(le32(at));
        cd.extend(name.as_bytes());
    }
    let cd_at = v.len();
    v.extend(&cd);
    v.extend(b"PK\x05\x06");
    v.extend([0; 4]);
    v.extend(le16(entries.len()));
    v.extend(le16(entries.len()));
    v.extend(le32(cd.len()));
    v.extend(le32(cd_at));
    v.extend(le16(0));
    v
}

fn docx() -> Vec<u8> {
    zip(&[
        (
            "[Content_Types].xml",
            b"<?xml version=\"1.0\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"/>",
        ),
        ("_rels/.rels", b"<?xml version=\"1.0\"?><Relationships/>"),
        (
            "word/document.xml",
            b"<?xml version=\"1.0\"?><w:document><w:body><w:p>Hello</w:p></w:body></w:document>",
        ),
    ])
}

/// A PDF saved, then updated in place: two `%%EOF`s, the second one the end.
fn pdf() -> Vec<u8> {
    let mut v = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let o1 = v.len();
    v.extend(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let o2 = v.len();
    v.extend(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");
    let x1 = v.len();
    v.extend(
        format!(
            "xref\n0 3\n0000000000 65535 f \n{o1:010} 00000 n \n{o2:010} 00000 n \ntrailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{x1}\n%%EOF\n"
        )
        .as_bytes(),
    );
    let o3 = v.len();
    v.extend(b"3 0 obj\n<< /Producer (a test) >>\nendobj\n");
    let x2 = v.len();
    v.extend(
        format!(
            "xref\n0 1\n0000000000 65535 f \n3 1\n{o3:010} 00000 n \ntrailer\n<< /Size 4 /Root 1 0 R /Info 3 0 R /Prev {x1} >>\nstartxref\n{x2}\n%%EOF\n"
        )
        .as_bytes(),
    );
    v
}

/// A SQLite database of `pages` pages of 1 KiB, its size in the header.
fn sqlite(rng: &mut Rng, pages: usize) -> Vec<u8> {
    let mut v = rng.bytes(pages * 1024);
    v[..16].copy_from_slice(b"SQLite format 3\0");
    v[16..18].copy_from_slice(&be16(1024));
    v[18..24].copy_from_slice(&[1, 1, 0, 64, 32, 32]);
    v[24..28].copy_from_slice(&7u32.to_be_bytes());
    v[28..32].copy_from_slice(&(pages as u32).to_be_bytes());
    v[32..92].fill(0);
    v[44..48].copy_from_slice(&4u32.to_be_bytes());
    v[56..60].copy_from_slice(&1u32.to_be_bytes());
    v[92..96].copy_from_slice(&7u32.to_be_bytes());
    v[96..100].copy_from_slice(&3_045_000u32.to_be_bytes());
    v[100..108].copy_from_slice(&[0x0D, 0, 0, 0, 0, 0x04, 0, 0]);
    v
}

/// An MP3: an ID3v2 tag, 20 frames (MPEG-1 layer III, 128 kbit/s, 44.1 kHz), an ID3v1 tag.
fn mp3(rng: &mut Rng) -> Vec<u8> {
    let mut frame = b"TIT2".to_vec();
    frame.extend(5u32.to_be_bytes());
    frame.extend([0, 0, 0]);
    frame.extend(b"Test");
    let tag = frame.len() + 100;
    let mut v = b"ID3\x03\0\0".to_vec();
    v.extend([
        (tag >> 21 & 0x7F) as u8,
        (tag >> 14 & 0x7F) as u8,
        (tag >> 7 & 0x7F) as u8,
        (tag & 0x7F) as u8,
    ]);
    v.extend(frame);
    v.extend([0; 100]);
    for _ in 0..20 {
        v.extend([0xFF, 0xFB, 0x90, 0x00]);
        v.extend(rng.bytes(417 - 4));
    }
    v.extend(b"TAG");
    v.extend([b' '; 125]);
    v
}

/// An old-format Word document: a compound file with a root and a "WordDocument" stream.
fn doc(rng: &mut Rng) -> Vec<u8> {
    let free = 0xFFFF_FFFFu32;
    let mut h = vec![0u8; 512];
    h[..8].copy_from_slice(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]);
    h[24..26].copy_from_slice(&le16(0x3E));
    h[26..28].copy_from_slice(&le16(3));
    h[28..30].copy_from_slice(&le16(0xFFFE));
    h[30..32].copy_from_slice(&le16(9));
    h[32..34].copy_from_slice(&le16(6));
    h[44..48].copy_from_slice(&le32(1)); // FAT sectors
    h[48..52].copy_from_slice(&le32(1)); // first directory sector
    h[56..60].copy_from_slice(&le32(4096));
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
        e[64..66].copy_from_slice(&le16(if name.is_empty() { 0 } else { units.len() + 2 }));
        e[66] = typ;
        e[67] = 1;
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
    v.extend(entry("WordDocument", 2, free, 2, 4096));
    v.extend(entry("", 0, free, 0, 0));
    v.extend(entry("", 0, free, 0, 0));
    v.extend(rng.bytes(4096));
    v
}

struct Planted {
    offset: usize,
    bytes: Vec<u8>,
    ext: &'static str,
    complete: bool,
}

struct Card {
    image: Vec<u8>,
    files: Vec<Planted>,
}

/// About 6 MB of old random bytes with files written over it at sector boundaries: some right
/// after each other, some after gaps, the last one cut off by the end of the card.
fn card() -> Card {
    let mut rng = Rng(0x5EED_CA4D_2026);
    let mut image = rng.bytes(6_000_000);
    let mut files = Vec::new();
    let thumb = jpeg(&mut rng, None, 300, true);
    // the thumbnail lands on a sector boundary inside the photo: it mustn't be reported
    let app1 = exif("TestCam", "TestCam Z1", "2021:06:15 14:30:00", &thumb, 500);
    let plan: Vec<(usize, Vec<u8>, &str)> = vec![
        (65_536, jpeg(&mut rng, Some(&app1), 150_000, true), "jpg"),
        (0, png(&mut rng, 40, 30), "png"), // right after the photo
        (400_000, mp4(&mut rng, 1_577_934_245, 700_000), "mp4"),
        (0, docx(), "docx"),
        (1_500_000, pdf(), "pdf"),
        (1_700_000, sqlite(&mut rng, 6), "sqlite"),
        (0, mp3(&mut rng), "mp3"),
        (2_300_000, doc(&mut rng), "doc"),
        (
            0,
            zip(&[
                ("notes.txt", b"just some notes\n"),
                ("list.csv", b"a,b\n1,2\n"),
            ]),
            "zip",
        ),
        (3_000_000, jpeg(&mut rng, None, 40_000, true), "jpg"),
    ];
    let mut at = 0;
    for (want, bytes, ext) in plan {
        let off = if want == 0 { at } else { want };
        let off = off.div_ceil(512) * 512;
        image[off..off + bytes.len()].copy_from_slice(&bytes);
        at = off + bytes.len();
        files.push(Planted {
            offset: off,
            bytes,
            ext,
            complete: true,
        });
    }
    // cut off by the end of the card
    let cut = jpeg(&mut rng, None, 20_000, true);
    let off = (image.len() - 8_000).div_ceil(512) * 512;
    let n = image.len() - off;
    image[off..].copy_from_slice(&cut[..n]);
    files.push(Planted {
        offset: off,
        bytes: cut[..n].to_vec(),
        ext: "jpg",
        complete: false,
    });
    Card { image, files }
}

fn write_card(dir: &Path, card: &Card) -> std::path::PathBuf {
    let p = dir.join("card.img");
    fs::write(&p, &card.image).unwrap();
    p
}

fn carve_all(src: &Path, opts: &Options) -> (Vec<Found>, carve::Stats) {
    let mut out = Vec::new();
    let stop = AtomicBool::new(false);
    let stats = carve::scan(src, opts, &mut |_| {}, &stop, &mut |f| {
        out.push(f.clone());
        Ok(())
    })
    .unwrap();
    (out, stats)
}

// ---------- the tests

#[test]
fn finds_every_planted_file_exactly_and_nothing_else() {
    let tmp = tempfile::tempdir().unwrap();
    let card = card();
    let src = write_card(tmp.path(), &card);
    let (found, stats) = carve_all(&src, &Options::default());
    let got: Vec<(u64, u64, &str, bool)> = found
        .iter()
        .map(|f| (f.offset, f.size, f.ext, f.complete))
        .collect();
    let want: Vec<(u64, u64, &str, bool)> = card
        .files
        .iter()
        .map(|p| (p.offset as u64, p.bytes.len() as u64, p.ext, p.complete))
        .collect();
    assert_eq!(got, want);
    assert_eq!(stats.found, want.len() as u64);
    assert_eq!(stats.incomplete, 1);
    assert_eq!(stats.scanned, card.image.len() as u64);
    assert!(!stats.stopped);
    assert_eq!(stats.by_group.photos, 4);
    assert_eq!(stats.by_group.documents, 3);

    // what the photo's EXIF and the video's header say
    let photo = &found[0];
    assert_eq!(photo.camera.as_deref(), Some("TestCam Z1"));
    assert_eq!(photo.taken, Some(1_623_767_400));
    assert_eq!((photo.width, photo.height), (Some(16), Some(16)));
    assert_eq!(photo.kind, "JPEG photo");
    assert_eq!(photo.group, "photos");
    assert_eq!((found[1].width, found[1].height), (Some(40), Some(30)));
    assert_eq!(found[2].taken, Some(1_577_934_245));
    assert_eq!(found[3].kind, "Word document");
    let cut = found.last().unwrap();
    assert!(cut.note.as_deref().unwrap().contains("cut off"));
}

#[test]
fn saved_files_are_byte_identical_and_listed() {
    let tmp = tempfile::tempdir().unwrap();
    let card = card();
    let src = write_card(tmp.path(), &card);
    let dest = tmp.path().join("Recovered");
    let stop = AtomicBool::new(false);
    let stats = carve::scan_and_save(&src, &Options::default(), &dest, &mut |_| {}, &stop).unwrap();
    assert_eq!(stats.saved, card.files.len() as u64);
    let index = fs::read_to_string(dest.join(carve::INDEX)).unwrap();
    let lines: Vec<serde_json::Value> = index
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), card.files.len());
    for (p, line) in card.files.iter().zip(&lines) {
        assert_eq!(line["offset"], p.offset as u64);
        let file = line["file"].as_str().unwrap();
        let group = line["group"].as_str().unwrap();
        assert_eq!(
            file,
            format!("{group}/f{:010}.{}", p.offset / 512, p.ext),
            "named after the sector it starts at"
        );
        assert_eq!(fs::read(dest.join(file)).unwrap(), p.bytes, "{file}");
    }
    assert_eq!(lines[0]["camera"], "TestCam Z1");

    // saving again finds each file already there; a different file in the way isn't overwritten
    let (found, _) = carve_all(&src, &Options::default());
    assert_eq!(
        carve::save(&src, &found[0], &dest).unwrap(),
        dest.join(lines[0]["file"].as_str().unwrap())
    );
    let other = dest
        .join(found[1].group)
        .join(format!("f{:010}.png", found[1].offset / 512));
    fs::write(&other, b"something else").unwrap();
    let again = carve::save(&src, &found[1], &dest).unwrap();
    assert_eq!(
        again.file_name().unwrap().to_string_lossy(),
        format!("f{:010}-1.png", found[1].offset / 512)
    );
    assert_eq!(fs::read(&other).unwrap(), b"something else");
    assert_eq!(fs::read(&again).unwrap(), card.files[1].bytes);
}

#[test]
fn only_what_starts_inside_the_range_is_found() {
    let tmp = tempfile::tempdir().unwrap();
    let card = card();
    let src = write_card(tmp.path(), &card);
    let mp4 = &card.files[2];
    let pdf = &card.files[4];
    // from just after the photo's start (where its thumbnail is), to the middle of the video
    let photo = card.files[0].offset as u64;
    let opts = Options {
        start: photo + 512,
        end: Some(mp4.offset as u64 + 100_000),
        ..Options::default()
    };
    let (found, stats) = carve_all(&src, &opts);
    let got: Vec<(u64, &str, bool)> = found
        .iter()
        .map(|f| (f.offset, f.ext, f.complete))
        .collect();
    // the photo starts before the range, so its thumbnail is found on its own this time; the
    // video runs past the range's end, so it's cut off there
    assert_eq!(
        got,
        vec![
            (photo + 512, "jpg", true),
            (card.files[1].offset as u64, "png", true),
            (mp4.offset as u64, "mp4", false)
        ]
    );
    assert_eq!(found[2].size, 100_000);
    assert_eq!(stats.scanned, opts.end.unwrap() - opts.start);

    // a range starting at the PDF, looking for documents only
    let opts = Options {
        start: pdf.offset as u64,
        kinds: Some(vec!["documents".into()]),
        ..Options::default()
    };
    let (found, _) = carve_all(&src, &opts);
    let exts: Vec<&str> = found.iter().map(|f| f.ext).collect();
    assert_eq!(exts, ["pdf", "doc"]);
}

#[test]
fn kinds_pick_types_and_groups() {
    let tmp = tempfile::tempdir().unwrap();
    let card = card();
    let src = write_card(tmp.path(), &card);
    let only = |kinds: &[&str]| {
        let opts = Options {
            kinds: Some(kinds.iter().map(|k| k.to_string()).collect()),
            ..Options::default()
        };
        carve_all(&src, &opts)
            .0
            .into_iter()
            .map(|f| f.ext)
            .collect::<Vec<_>>()
    };
    assert_eq!(only(&["photos"]), ["jpg", "png", "jpg", "jpg"]);
    assert_eq!(only(&["jpeg", "mp3"]), ["jpg", "mp3", "jpg", "jpg"]);
    assert_eq!(only(&["docx", "sqlite"]), ["docx", "sqlite"]);
    assert_eq!(only(&["archives"]), ["zip"]);
    let opts = Options {
        kinds: Some(vec!["xyz".into()]),
        ..Options::default()
    };
    let err = carve::scan(
        &src,
        &opts,
        &mut |_| {},
        &AtomicBool::new(false),
        &mut |_| Ok(()),
    )
    .unwrap_err();
    assert!(err.to_string().contains("unknown file type"));
}

#[test]
fn min_size_leaves_out_small_files() {
    let tmp = tempfile::tempdir().unwrap();
    let card = card();
    let src = write_card(tmp.path(), &card);
    let opts = Options {
        min_size: 100_000,
        ..Options::default()
    };
    let (found, _) = carve_all(&src, &opts);
    assert!(found.iter().all(|f| f.size >= 100_000));
    let exts: Vec<&str> = found.iter().map(|f| f.ext).collect();
    assert_eq!(exts, ["jpg", "mp4"]);
}

#[test]
fn stopping_returns_what_was_found_so_far() {
    let tmp = tempfile::tempdir().unwrap();
    let card = card();
    let src = write_card(tmp.path(), &card);
    let stop = AtomicBool::new(false);
    let mut found = Vec::new();
    let stats = carve::scan(&src, &Options::default(), &mut |_| {}, &stop, &mut |f| {
        found.push(f.offset);
        stop.store(true, Ordering::Relaxed);
        Ok(())
    })
    .unwrap();
    assert!(stats.stopped);
    assert!(!found.is_empty() && found.len() < card.files.len());
    assert!(stats.scanned < card.image.len() as u64);

    // an error from the callback stops the scan and comes back
    let err = carve::scan(
        &src,
        &Options::default(),
        &mut |_| {},
        &AtomicBool::new(false),
        &mut |_| anyhow::bail!("disk full"),
    )
    .unwrap_err();
    assert!(err.to_string().contains("disk full"));
}

#[test]
fn progress_reaches_the_end() {
    let tmp = tempfile::tempdir().unwrap();
    let card = card();
    let src = write_card(tmp.path(), &card);
    let mut last = None;
    carve::scan(
        &src,
        &Options::default(),
        &mut |p| last = Some(p.clone()),
        &AtomicBool::new(false),
        &mut |_| Ok(()),
    )
    .unwrap();
    let p = last.unwrap();
    assert_eq!(p.done, p.size);
    assert_eq!(p.size, card.image.len() as u64);
    assert_eq!(p.found, card.files.len() as u64);
}

#[test]
fn nothing_in_random_bytes_or_zeros() {
    let tmp = tempfile::tempdir().unwrap();
    let mut rng = Rng(42);
    let src = tmp.path().join("noise.img");
    let mut img = rng.bytes(8 << 20);
    img.extend(vec![0u8; 2 << 20]);
    fs::write(&src, &img).unwrap();
    let (found, stats) = carve_all(&src, &Options::default());
    assert!(found.is_empty(), "{found:?}");
    assert!(stats.problems.is_empty());
}

#[cfg(unix)]
#[test]
fn an_image_file_can_be_saved_next_to() {
    let tmp = tempfile::tempdir().unwrap();
    let card = card();
    let src = write_card(tmp.path(), &card);
    // the same filesystem: that's the same device for an image file…
    assert!(carve::same_device(&src, tmp.path()));
    assert!(carve::same_device(&src, &tmp.path().join("not/yet/made")));
    // …but saving there can't overwrite anything in the image, so it's allowed
    let (found, _) = carve_all(&src, &Options::default());
    let saved = carve::save(&src, &found[1], &tmp.path().join("out")).unwrap();
    assert_eq!(fs::read(saved).unwrap(), card.files[1].bytes);
}

/// A real FAT filesystem made by mkfs.vfat, files written into it, then deleted the way FAT does
/// (the name's first byte and the cluster chain cleared): they come back. Skipped without mkfs.vfat.
#[cfg(unix)]
#[test]
fn deleted_files_come_back_from_a_fat_card() {
    use std::process::Command;
    let tmp = tempfile::tempdir().unwrap();
    let img = tmp.path().join("fat.img");
    fs::write(&img, vec![0u8; 16 << 20]).unwrap();
    let made = ["mkfs.vfat", "/sbin/mkfs.vfat", "/usr/sbin/mkfs.vfat"]
        .iter()
        .any(|m| {
            Command::new(m)
                .args(["-F", "16", "-s", "4", "-n", "TESTCARD"])
                .arg(&img)
                .output()
                .is_ok_and(|o| o.status.success())
        });
    if !made {
        eprintln!("mkfs.vfat isn't available: skipped");
        return;
    }
    let mut b = fs::read(&img).unwrap();
    let u16at = |b: &[u8], at: usize| usize::from(u16::from_le_bytes([b[at], b[at + 1]]));
    let bps = u16at(&b, 11);
    let spc = usize::from(b[13]);
    let reserved = u16at(&b, 14);
    let nfats = usize::from(b[16]);
    let root_entries = u16at(&b, 17);
    let fat_sectors = u16at(&b, 22);
    let fat = reserved * bps;
    let root = fat + nfats * fat_sectors * bps;
    let data = root + root_entries * 32;
    let cluster = spc * bps;
    let mut rng = Rng(7);
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("PHOTO1  JPG", jpeg(&mut rng, None, 30_000, true)),
        ("DOC     DOCX", docx()),
        ("PIC     PNG", png(&mut rng, 20, 20)),
        ("REPORT  PDF", pdf()),
    ];
    let mut next = 2usize;
    let mut placed = Vec::new();
    for (i, (name, bytes)) in files.iter().enumerate() {
        let first = next;
        let n = bytes.len().div_ceil(cluster);
        let at = data + (first - 2) * cluster;
        b[at..at + bytes.len()].copy_from_slice(bytes);
        for c in first..first + n {
            let v: u16 = if c + 1 == first + n {
                0xFFFF
            } else {
                (c + 1) as u16
            };
            for f in 0..nfats {
                let o = fat + f * fat_sectors * bps + 2 * c;
                b[o..o + 2].copy_from_slice(&v.to_le_bytes());
            }
        }
        let e = root + 32 * (i + 1);
        let (stem, ext) = name.split_at(8);
        let mut entry = [b' '; 11];
        entry[..8].copy_from_slice(stem.as_bytes());
        entry[8..11].copy_from_slice(&ext.as_bytes()[..3]);
        b[e..e + 11].copy_from_slice(&entry);
        b[e + 11] = 0x20;
        b[e + 26..e + 28].copy_from_slice(&(first as u16).to_le_bytes());
        b[e + 28..e + 32].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
        placed.push((at as u64, bytes.clone()));
        next += n + 1; // a free cluster between files
    }
    // delete them all, as FAT does
    for i in 0..files.len() {
        b[root + 32 * (i + 1)] = 0xE5;
    }
    for f in 0..nfats {
        let o = fat + f * fat_sectors * bps;
        b[o + 4..o + fat_sectors * bps].fill(0);
    }
    fs::write(&img, &b).unwrap();
    let (found, _) = carve_all(&img, &Options::default());
    let got: Vec<(u64, u64)> = found.iter().map(|f| (f.offset, f.size)).collect();
    let want: Vec<(u64, u64)> = placed.iter().map(|(o, b)| (*o, b.len() as u64)).collect();
    assert_eq!(got, want);
    let exts: Vec<&str> = found.iter().map(|f| f.ext).collect();
    assert_eq!(exts, ["jpg", "docx", "png", "pdf"]);
}

/// How fast a scan of random bytes goes (reading dominated): run with
/// `cargo test --release -p backdown-core --test rescue_carve -- --ignored --nocapture`.
#[test]
#[ignore]
fn throughput() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("big.img");
    let mut rng = Rng(1);
    let mut img = rng.bytes(200 << 20);
    // a few real files too
    let card = card();
    img[(100 << 20)..(100 << 20) + card.image.len()].copy_from_slice(&card.image);
    fs::write(&src, &img).unwrap();
    drop(img);
    let timed = |what: &str| {
        let _ = carve_all(&src, &Options::default()); // page cache warm
        let t = std::time::Instant::now();
        let (found, stats) = carve_all(&src, &Options::default());
        let secs = t.elapsed().as_secs_f64();
        eprintln!(
            "{what}: {} MB in {secs:.3} s, {:.0} MB/s, {} files",
            stats.scanned / 1_000_000,
            stats.scanned as f64 / secs / 1e6,
            found.len()
        );
        found
    };
    assert_eq!(timed("mostly random").len(), card.files.len());

    // a card full of photos: every byte is followed through JPEG data
    let mut img = Vec::with_capacity(210 << 20);
    let mut n = 0;
    while img.len() < 200 << 20 {
        img.extend(jpeg(&mut rng, None, 3_000_000, true));
        img.resize(img.len().div_ceil(512) * 512, 0);
        n += 1;
    }
    fs::write(&src, &img).unwrap();
    drop(img);
    assert_eq!(timed("photos back to back").len(), n);
}
