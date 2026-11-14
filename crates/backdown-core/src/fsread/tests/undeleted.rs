//! Deleted files on real images, made by mkfs where it's installed (or formatted here) and filled,
//! then files deleted the way a delete does it: FAT, E5 over each of the file's entries and its
//! chain freed; exFAT, the entry set's in-use bits cleared and its clusters freed in the
//! allocation bitmap; NTFS, the MFT record marked free with its sequence number moved on and its
//! clusters freed in $Bitmap. Then listed, assessed, and read back byte for byte; space other
//! files have taken since comes back as zeros; damage gives errors, never panics or hangs.

use super::*;
use std::ops::Range;

fn deleted_of<R: Read + Seek>(v: &mut Volume<R>, path: &str) -> Vec<Entry> {
    let mut e = v.deleted_path(path, MAX_LIST).unwrap().entries;
    e.sort_by(|a, b| a.name.cmp(&b.name));
    e
}

fn got<R: Read + Seek>(v: &mut Volume<R>, e: &Entry) -> Vec<u8> {
    let mut out = Vec::new();
    v.reader(e).unwrap().read_to_end(&mut out).unwrap();
    assert_eq!(out.len() as u64, e.size, "{}: the listing's size", e.name);
    out
}

fn by_name<'a>(list: &'a [Entry], name: &str) -> &'a Entry {
    list.iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("no {name} among {:?}", names_of(list)))
}

fn names_of(list: &[Entry]) -> Vec<&str> {
    list.iter().map(|e| e.name.as_str()).collect()
}

fn state(e: &Entry) -> (&'static str, u8) {
    let d = e.deleted.as_ref().expect("a deleted entry");
    (d.state, d.confidence)
}

/// Everything reachable from the root, deleted or not (up to `max` entries), each file read up to
/// `cap` bytes: what a person browsing deleted files does, and what must never panic or hang.
fn walk_deleted<R: Read + Seek>(v: &mut Volume<R>, max: usize, cap: u64) -> usize {
    let mut todo = vec![(String::new(), 0usize)];
    let mut seen = 0;
    while let Some((path, depth)) = todo.pop() {
        let Ok(dir) = v.find(&path) else { continue };
        let mut all = v.list(&dir, 64).map(|l| l.entries).unwrap_or_default();
        if dir.deleted.is_none() {
            all.extend(v.deleted(&dir, 64).map(|l| l.entries).unwrap_or_default());
        }
        for e in all {
            seen += 1;
            if seen > max {
                return seen;
            }
            let seg = e.deleted.as_ref().map_or(e.name.clone(), |d| d.seg.clone());
            let p = if path.is_empty() {
                seg
            } else {
                format!("{path}/{seg}")
            };
            if e.dir {
                if depth < 5 {
                    todo.push((p, depth + 1));
                }
            } else if let Ok(mut rd) = v.reader(&e) {
                let _ = (&mut rd).take(cap).read_to_end(&mut Vec::new());
            }
        }
    }
    seen
}

/// Like `survive_damage`, walking deleted entries too.
fn survive_damage_deleted(img: &[u8], hot: &[u64], rounds: u64) {
    let rounds = std::env::var("FSREAD_FUZZ")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(rounds);
    let mut rng = Rng(0xDE1E_7ED0 + img.len() as u64);
    let mut bad = img.to_vec();
    let len = bad.len() as u64;
    let (mut opened, mut walked) = (0, 0);
    for round in 0..rounds {
        let mut undo = Vec::new();
        for _ in 0..1 + rng.below(24) {
            let at = match rng.below(4) {
                0 => rng.below(len),
                1 => rng.below(8192.min(len)),
                _ => {
                    let h = hot[rng.below(hot.len() as u64) as usize];
                    (h + rng.below(16384)).min(len - 1)
                }
            } as usize;
            undo.push((at, bad[at]));
            bad[at] = match rng.below(3) {
                0 => 0,
                1 => 0xFF,
                _ => rng.next() as u8,
            };
        }
        let cut = if round % 7 == 3 {
            rng.below(len) as usize
        } else {
            bad.len()
        };
        if let Ok(mut v) = Volume::open(Cursor::new(&bad[..cut]), 0, len) {
            opened += 1;
            v.set_deadline(std::time::Instant::now() + std::time::Duration::from_secs(20));
            walked += walk_deleted(&mut v, 300, 1 << 16);
        }
        for (at, b) in undo.into_iter().rev() {
            bad[at] = b;
        }
    }
    eprintln!("damage (deleted): {rounds} rounds, {opened} opened, {walked} entries walked");
}

// ---- FAT

fn fat_slot(w: &FatW, dir: u32, k: usize) -> usize {
    if dir == 0 && w.bits != 32 {
        w.root_at + k * 32
    } else {
        let chain = &w.dirs[&dir].0;
        let per = w.cluster / 32;
        w.off(chain[k / per]) + (k % per) * 32
    }
}

/// A file in `dir`: its first cluster, and the folder slots its entries took (the long name's
/// parts, then the short entry).
fn fat_file(w: &mut FatW, dir: u32, name: &str, bytes: &[u8]) -> (u32, Range<usize>) {
    let a = w.dirs[&dir].1;
    let first = w.file(dir, name, bytes);
    (first, a..w.dirs[&dir].1)
}

/// Deletes what `slots` hold as a delete does: E5 over each entry's first byte, and the chain
/// from `first` freed.
fn fat_delete(w: &mut FatW, dir: u32, slots: Range<usize>, first: u32) {
    for k in slots {
        let o = fat_slot(w, dir, k);
        w.img[o] = 0xE5;
    }
    let mut c = first;
    while (2..=w.max).contains(&c) {
        let next = w.get(c);
        w.set(c, 0);
        c = next;
    }
}

/// A file saved since (in folder `dir`) into cluster `c`, filled with `fill`.
fn fat_taker(w: &mut FatW, dir: u32, name: &str, c: u32, fill: u8) {
    let eoc = w.eoc();
    w.set(c, eoc);
    let (o, cl) = (w.off(c), w.cluster);
    w.img[o..o + cl].fill(fill);
    w.add(dir, name, 0x20, c, cl as u32);
}

struct FatDel {
    img: Vec<u8>,
    cluster: usize,
    photo: Vec<u8>,
    reused: Vec<u8>,
    lost: Vec<u8>,
    beach: Vec<u8>,
}

fn fat_with_deletes(img: Vec<u8>) -> FatDel {
    let mut w = FatW::new(img);
    let cl = w.cluster;
    let photo = noise(21, cl * 5 + 123);
    let reused = noise(22, cl * 4);
    let lost = noise(23, cl * 2);
    let beach = noise(24, cl * 2 + 5);
    w.file(0, "keep.txt", b"kept\n");
    let (p, ps) = fat_file(&mut w, 0, "Holiday photo 2024.jpg", &photo);
    let (n, ns) = fat_file(&mut w, 0, "NOTE.TXT", b"a note\n");
    let (s, ss) = fat_file(&mut w, 0, "small.txt", b"lower case\n");
    let (r, rs) = fat_file(&mut w, 0, "Reused file.bin", &reused);
    let (l, ls) = fat_file(&mut w, 0, "Lost file.bin", &lost);
    let (g, gs) = fat_file(&mut w, 0, "GONE.BIN", b"gone for good\n");
    // where the files saved since go (made now, so it doesn't grow into freed clusters itself)
    let later = w.mkdir(0, "LATER");
    let a = w.dirs[&0].1;
    let trip = w.mkdir(0, "Trip to the sea");
    let ts = a..w.dirs[&0].1;
    let (b, bs) = fat_file(&mut w, trip, "beach day.jpg", &beach);
    let (d, ds) = fat_file(&mut w, trip, "DIARY.TXT", b"day one\n");
    for (dir, slots, first) in [
        (0, ps, p),
        (0, ns, n),
        (0, ss, s),
        (0, rs, r),
        (0, ls, l),
        (0, gs, g),
        (trip, bs, b),
        (trip, ds, d),
        (0, ts, trip),
    ] {
        fat_delete(&mut w, dir, slots, first);
    }
    // saved since: into the reused file's second cluster, the lost one's first, GONE.BIN's only one
    fat_taker(&mut w, later, "TAKER1.BIN", r + 1, 0xAA);
    fat_taker(&mut w, later, "TAKER2.BIN", l, 0xBB);
    fat_taker(&mut w, later, "TAKER3.BIN", g, 0xCC);
    if w.bits == 32 {
        let info = le(&w.img, 48, 2) as usize * 512;
        put(&mut w.img, info + 488, 0xFFFF_FFFF, 8);
    }
    FatDel {
        img: w.img,
        cluster: cl,
        photo,
        reused,
        lost,
        beach,
    }
}

fn check_fat_deleted(fx: &FatDel) {
    let mut v = open(&fx.img);
    let cl = fx.cluster;
    assert!(v.undelete_unsupported().is_none());
    // the live listing is as before: nothing deleted in it
    assert_eq!(names(&mut v, ""), sorted(&["keep.txt", "LATER"]));
    assert_eq!(
        names(&mut v, "LATER"),
        sorted(&["TAKER1.BIN", "TAKER2.BIN", "TAKER3.BIN"])
    );
    assert!(deleted_of(&mut v, "LATER").is_empty());
    let gone = deleted_of(&mut v, "");
    assert_eq!(
        names_of(&gone),
        [
            "Holiday photo 2024.jpg",
            "Lost file.bin",
            "Reused file.bin",
            "Trip to the sea",
            "_ONE.BIN",
            "_OTE.TXT",
            "_mall.txt",
        ]
    );
    // a long name comes back whole; a short one without its first letter
    let photo = by_name(&gone, "Holiday photo 2024.jpg");
    assert_eq!(state(photo), ("recoverable", 70), "{photo:?}");
    assert_eq!(
        (photo.size, photo.mtime),
        (fx.photo.len() as u64, Some(DOS_WHEN))
    );
    assert_eq!(got(&mut v, photo), fx.photo);
    let note = by_name(&gone, "_OTE.TXT");
    assert_eq!(state(note), ("recoverable", 95));
    assert!(note.deleted.as_ref().unwrap().why.contains("first letter"));
    assert_eq!(got(&mut v, note), b"a note\n");
    assert_eq!(got(&mut v, by_name(&gone, "_mall.txt")), b"lower case\n");
    // a cluster taken since comes back empty
    let reused = by_name(&gone, "Reused file.bin");
    let (st, conf) = state(reused);
    assert_eq!(st, "partly overwritten");
    assert!(conf < 70 && conf > 15, "{conf}");
    let mut want = fx.reused.clone();
    want[cl..2 * cl].fill(0);
    assert_eq!(got(&mut v, reused), want);
    let lost = by_name(&gone, "Lost file.bin");
    let (st, conf) = state(lost);
    assert!(st == "partly overwritten" && conf <= 15, "{st} {conf}");
    let mut want = fx.lost.clone();
    want[..cl].fill(0);
    assert_eq!(got(&mut v, lost), want);
    let g = by_name(&gone, "_ONE.BIN");
    assert_eq!(state(g).0, "overwritten");
    assert!(g.unreadable.is_some() && v.reader(g).is_err());
    // a deleted folder, and what was in it
    let trip = by_name(&gone, "Trip to the sea");
    assert!(trip.dir && state(trip).0 == "recoverable");
    let seg = trip.deleted.as_ref().unwrap().seg.clone();
    let inside = deleted_of(&mut v, &seg);
    assert_eq!(names_of(&inside), ["_IARY.TXT", "beach day.jpg"]);
    assert_eq!(v.list_path(&seg, MAX_LIST).unwrap().entries.len(), 2);
    let beach = by_name(&inside, "beach day.jpg");
    assert_eq!(got(&mut v, beach), fx.beach);
    // found again by path, through the listings: the folder's and the file's segments
    let path = format!("{seg}/{}", beach.deleted.as_ref().unwrap().seg);
    let again = v.find(&path).unwrap();
    assert_eq!(got(&mut v, &again), fx.beach);
    // a deleted entry is only picked by its own token, never by name alone from a live folder
    let d = photo.deleted.as_ref().unwrap();
    assert!(v.find(&undelete::seg(&photo.name, d.token + 32)).is_err());
    assert!(v.find("Holiday photo 2024.jpg").is_err());
    assert!(v.find(&d.seg).is_ok());
}

#[test]
fn fat_deleted_files_come_back() {
    for (bits, size, spc) in [(12, 2 << 20, 1), (16, 24 << 20, 4), (32, 40 << 20, 1)] {
        let fx = fat_with_deletes(format_fat(bits, size, spc));
        check_fat_deleted(&fx);
        if let Some(fsck) = tool("fsck.fat") {
            let t = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(t.path(), &fx.img).unwrap();
            run(Command::new(fsck).arg("-n").arg(t.path()));
        }
        if bits == 16 {
            let w = FatW::new(fx.img.clone());
            let hot = [0, w.fat_at as u64, w.root_at as u64, w.data_at as u64];
            survive_damage_deleted(&fx.img, &hot, 120);
        }
    }
}

#[test]
fn fat_deleted_files_on_mkfs_volumes() {
    let mkfs = need!("mkfs.fat");
    for (bits, size, extra) in [(16, 20 << 20, "-s4"), (32, 40 << 20, "-s1")] {
        let t = tempfile::NamedTempFile::new().unwrap();
        t.as_file().set_len(size).unwrap();
        run(Command::new(&mkfs)
            .args(["-F", &bits.to_string(), extra, "-n", "DELVOL"])
            .arg(t.path()));
        let fx = fat_with_deletes(std::fs::read(t.path()).unwrap());
        std::fs::write(t.path(), &fx.img).unwrap();
        if let Some(fsck) = tool("fsck.fat") {
            run(Command::new(fsck).arg("-n").arg(t.path()));
        }
        check_fat_deleted(&fx);
        // through a real file
        let mut v = Volume::open(File::open(t.path()).unwrap(), 0, size).unwrap();
        let gone = deleted_of(&mut v, "");
        assert_eq!(
            got(&mut v, by_name(&gone, "Holiday photo 2024.jpg")),
            fx.photo
        );
    }
}

/// Copying out (rescue::files) of a FAT partition 1 MiB into a disk whose table is wiped, as
/// Rescue's "Copy these files out" does for a partition it found.
#[test]
fn copied_out_of_a_disk_with_a_wiped_table() {
    use crate::rescue::files::{Options, copy_out};
    use std::sync::atomic::AtomicBool;
    let fx = fat_with_deletes(format_fat(16, 24 << 20, 4));
    let mut disk = vec![0u8; 1 << 20];
    disk.extend(&fx.img);
    let dir = tempfile::tempdir().unwrap();
    let img = dir.path().join("disk.img");
    std::fs::write(&img, &disk).unwrap();
    let (start, len) = (1u64 << 20, fx.img.len() as u64);
    // chosen as the browser chooses them: from the listings
    let mut v = Volume::open(File::open(&img).unwrap(), start, len).unwrap();
    let gone = deleted_of(&mut v, "");
    let seg = |n: &str| by_name(&gone, n).deleted.as_ref().unwrap().seg.clone();
    let opts = Options {
        start,
        len: Some(len),
        items: vec![
            seg("Holiday photo 2024.jpg"),
            seg("Trip to the sea"),
            "LATER".into(),
            "keep.txt".into(),
            "no such thing".into(),
        ],
        with_deleted: false,
        resume: false,
    };
    let out = dir.path().join("out");
    let never = AtomicBool::new(false);
    let mut seen = Vec::new();
    let s = copy_out(&img, &opts, &out, &mut |p| seen.push(*p), &never).unwrap();
    assert_eq!(
        (s.files, s.deleted, s.partly, s.failed, s.renamed, s.stopped),
        (7, 3, 0, 1, 0, false),
        "{s:?}"
    );
    assert!(s.problems[0].contains("no such thing"), "{:?}", s.problems);
    let want_bytes = (fx.photo.len() + fx.beach.len() + 8 + 3 * fx.cluster + 5) as u64;
    assert_eq!(s.bytes, want_bytes);
    let last = seen.last().unwrap();
    assert_eq!((last.files, last.total_files), (7, 7));
    let cl = fx.cluster;
    let rd = |p: &str| std::fs::read(out.join(p)).unwrap();
    assert_eq!(rd("Holiday photo 2024.jpg"), fx.photo);
    assert_eq!(rd("Trip to the sea/beach day.jpg"), fx.beach);
    assert_eq!(rd("Trip to the sea/_IARY.TXT"), b"day one\n");
    assert_eq!(rd("LATER/TAKER1.BIN"), vec![0xAA; cl]);
    assert_eq!(rd("keep.txt"), b"kept\n");
    let t = std::fs::metadata(out.join("keep.txt"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(
        t.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64,
        DOS_WHEN
    );
    // carrying on after a stop: what's there already is left as it is
    let again = Options {
        resume: true,
        ..opts.clone()
    };
    let s = copy_out(&img, &again, &out, &mut |_| {}, &never).unwrap();
    assert_eq!((s.files, s.renamed), (7, 0));
    assert!(!out.join("Holiday photo 2024 (2).jpg").exists());
    // again from scratch: nothing is overwritten
    let s = copy_out(&img, &opts, &out, &mut |_| {}, &never).unwrap();
    assert_eq!((s.files, s.renamed), (7, 7));
    assert_eq!(rd("Holiday photo 2024 (2).jpg"), fx.photo);
    assert_eq!(rd("keep.txt"), b"kept\n");
    // the whole filesystem, deleted files included
    let all = Options {
        items: vec![String::new()],
        with_deleted: true,
        ..opts.clone()
    };
    let out2 = dir.path().join("all");
    let s = copy_out(&img, &all, &out2, &mut |_| {}, &never).unwrap();
    assert_eq!((s.partly, s.failed), (2, 1), "{s:?}");
    let mut want = fx.reused.clone();
    want[cl..2 * cl].fill(0);
    assert_eq!(
        std::fs::read(out2.join("BD TEST/Reused file.bin")).unwrap(),
        want
    );
    assert_eq!(
        std::fs::read(out2.join("BD TEST/Trip to the sea/beach day.jpg")).unwrap(),
        fx.beach
    );
    assert!(
        s.problems.iter().any(|p| p.starts_with("_ONE.BIN")),
        "{:?}",
        s.problems
    );
    // stopped before it starts: nothing half-written
    let out3 = dir.path().join("stopped");
    let s = copy_out(&img, &opts, &out3, &mut |_| {}, &AtomicBool::new(true)).unwrap();
    assert!(s.stopped && s.files == 0);
    assert_eq!(std::fs::read_dir(&out3).unwrap().count(), 0);
}

/// Copied out as Rescue's job does it: every file is in the chain of custody, from the image, the
/// filesystem's place in it and the path inside, to where it went, with its hash.
#[test]
fn copied_out_files_are_in_the_chain_of_custody() {
    use crate::rescue::files::{Options, copy_out_recorded};
    use sha2::{Digest, Sha256};
    use std::sync::atomic::AtomicBool;
    let fx = fat_with_deletes(format_fat(16, 24 << 20, 4));
    let mut disk = vec![0u8; 1 << 20];
    disk.extend(&fx.img);
    let dir = tempfile::tempdir().unwrap();
    let img = dir.path().join("card.img");
    std::fs::write(&img, &disk).unwrap();
    let db = crate::Db::open(&dir.path().join("i.db")).unwrap();
    let opts = Options {
        start: 1 << 20,
        len: Some(fx.img.len() as u64),
        items: vec!["keep.txt".into(), "LATER".into()],
        with_deleted: false,
        resume: false,
    };
    let out = dir.path().join("out");
    let never = AtomicBool::new(false);
    let s = copy_out_recorded(&img, &opts, &out, (&db, 7), &mut |_| {}, &never).unwrap();
    let mut stmt = db
        .conn
        .prepare("SELECT action, src, src_sha256, dest, dest_sha256, size, ref FROM custody ORDER BY seq")
        .unwrap();
    type Row = (String, Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>, i64, String);
    let rows: Vec<Row> = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(rows.len() as u64, s.files, "{rows:?}");
    assert!(
        rows.iter()
            .all(|r| r.0 == "rescue-copy" && r.6 == "rescue:7" && r.2 == r.4)
    );
    let keep = rows
        .iter()
        .find(|r| {
            r.1.ends_with(&crate::paths::to_bytes(std::path::Path::new("/keep.txt")))
        })
        .expect("keep.txt");
    let from = format!("{}@{}:/keep.txt", img.display(), 1 << 20);
    assert_eq!(keep.1, crate::paths::to_bytes(std::path::Path::new(&from)));
    assert_eq!(keep.3, crate::paths::to_bytes(&out.join("keep.txt")));
    assert_eq!(keep.2, Sha256::digest(b"kept\n").to_vec());
    assert_eq!(keep.5, 5);
    assert!(crate::custody::check(&db.conn).unwrap().ok);
}

/// Not a test by itself: writes the disk images the browser check uses into $BD_FIXTURES (a card
/// image with an MBR and the FAT16 volume above 1 MiB in, the same with its table wiped, and the
/// deleted files' contents to compare with). `cargo test -- --ignored browser_fixtures`.
#[test]
#[ignore]
fn browser_fixtures() {
    let Some(dir) = std::env::var_os("BD_FIXTURES").map(PathBuf::from) else {
        eprintln!("set BD_FIXTURES to a folder");
        return;
    };
    let fx = fat_with_deletes(format_fat(16, 24 << 20, 4));
    let mut card = vec![0u8; 1 << 20];
    card[446 + 4] = 0x06;
    put(&mut card, 446 + 8, 2048, 4);
    put(&mut card, 446 + 12, (fx.img.len() / 512) as u64, 4);
    card[510] = 0x55;
    card[511] = 0xAA;
    card.extend(&fx.img);
    std::fs::create_dir_all(dir.join("drive")).unwrap();
    std::fs::write(dir.join("drive/card.img"), &card).unwrap();
    let mut wiped = card.clone();
    wiped[..512].fill(0);
    std::fs::write(dir.join("wiped.img"), &wiped).unwrap();
    std::fs::write(dir.join("expect-photo.bin"), &fx.photo).unwrap();
    std::fs::write(dir.join("expect-beach.bin"), &fx.beach).unwrap();
}

// ---- exFAT

enum Lay {
    /// One piece, marked "no FAT chain".
    Contiguous,
    /// Every other free cluster, linked in the FAT.
    Spread,
    /// One piece, but linked in the FAT (the chain zeroed on delete).
    InOrder,
}

/// A file as ExW::file makes it, saying where its entry set is and which clusters it took.
fn ex_file(w: &mut ExW, dir: u32, name: &str, bytes: &[u8], lay: Lay) -> (usize, Vec<u32>) {
    let n = bytes.len().div_ceil(w.cluster);
    let chain = w.alloc(n, !matches!(lay, Lay::Spread));
    if let Lay::InOrder = lay {
        for pair in chain.windows(2) {
            put(
                &mut w.img,
                w.fat_at + pair[0] as usize * 4,
                u64::from(pair[1]),
                4,
            );
        }
        let last = *chain.last().unwrap();
        put(&mut w.img, w.fat_at + last as usize * 4, 0xFFFF_FFFF, 4);
    }
    for (i, part) in bytes.chunks(w.cluster).enumerate() {
        let o = w.off(chain[i]);
        w.img[o..o + part.len()].copy_from_slice(part);
    }
    let len = bytes.len() as u64;
    let at = w.set(
        dir,
        name,
        0x20,
        chain[0],
        len,
        len,
        matches!(lay, Lay::Contiguous),
    );
    (at, chain)
}

/// Deletes an entry set as a delete does: its entries' in-use bits cleared, its clusters freed in
/// the bitmap (and, with `unlink`, its FAT chain zeroed).
fn ex_delete(w: &mut ExW, at: usize, name: &str, clusters: &[u32], unlink: bool) {
    let count = 2 + name.encode_utf16().count().div_ceil(15);
    for k in 0..count {
        w.img[at + 32 * k] &= 0x7F;
    }
    for &c in clusters {
        let i = (c - 2) as usize;
        w.img[w.bitmap_at + i / 8] &= !(1 << (i % 8));
        if unlink {
            put(&mut w.img, w.fat_at + c as usize * 4, 0, 4);
        }
    }
}

#[test]
fn exfat_deleted_files_come_back() {
    let mkfs = need!("mkfs.exfat");
    let t = tempfile::NamedTempFile::new().unwrap();
    t.as_file().set_len(8 << 20).unwrap();
    run(Command::new(mkfs).args(["-L", "EXDEL"]).arg(t.path()));
    let mut w = ExW::new(std::fs::read(t.path()).unwrap());
    let root = le(&w.img, 96, 4) as u32;
    let cl = w.cluster;
    let photo = noise(31, cl * 3 + 77);
    let video = noise(32, cl * 5 + 1);
    let ordered = noise(33, cl * 3);
    let over = noise(34, cl * 4);
    let letter = noise(35, 999);
    w.file(root, "keep.txt", b"kept\n", true);
    let (pa, pc) = ex_file(&mut w, root, "Sunset photo.jpg", &photo, Lay::Contiguous);
    let (va, vc) = ex_file(&mut w, root, "fragmented video.mp4", &video, Lay::Spread);
    let (oa, oc) = ex_file(&mut w, root, "in order.bin", &ordered, Lay::InOrder);
    let (xa, xc) = ex_file(&mut w, root, "overwritten.bin", &over, Lay::Contiguous);
    // a folder with a letter in it
    let dir = w.alloc(1, false)[0];
    w.dirs.insert(dir, 0);
    let da = w.set(root, "Old stuff", 0x10, dir, cl as u64, cl as u64, false);
    let (la, lc) = ex_file(
        &mut w,
        dir,
        "letter to grandma.txt",
        &letter,
        Lay::Contiguous,
    );
    ex_delete(&mut w, pa, "Sunset photo.jpg", &pc, false);
    ex_delete(&mut w, va, "fragmented video.mp4", &vc, false);
    ex_delete(&mut w, oa, "in order.bin", &oc, true);
    ex_delete(&mut w, xa, "overwritten.bin", &xc, false);
    ex_delete(&mut w, la, "letter to grandma.txt", &lc, false);
    ex_delete(&mut w, da, "Old stuff", &[dir], false);
    // another file has since taken the third cluster of overwritten.bin
    w.take(xc[2]);
    let o = w.off(xc[2]);
    w.img[o..o + cl].fill(0xCC);
    std::fs::write(t.path(), &w.img).unwrap();
    if let Some(fsck) = tool("fsck.exfat") {
        run(Command::new(fsck).arg("-n").arg(t.path()));
    }
    let mut v = open(&w.img);
    assert_eq!(names(&mut v, ""), ["keep.txt"]);
    let gone = deleted_of(&mut v, "");
    assert_eq!(
        names_of(&gone),
        [
            "Old stuff",
            "Sunset photo.jpg",
            "fragmented video.mp4",
            "in order.bin",
            "overwritten.bin"
        ]
    );
    let p = by_name(&gone, "Sunset photo.jpg");
    assert_eq!(state(p), ("recoverable", 95));
    assert_eq!(p.mtime, Some(DOS_WHEN + 1 - 2 * 3600));
    assert_eq!(got(&mut v, p), photo);
    // the FAT chain survives a delete on exFAT: followed, so a fragmented file comes back whole
    let f = by_name(&gone, "fragmented video.mp4");
    assert_eq!(state(f), ("recoverable", 90));
    assert_eq!(got(&mut v, f), video);
    // a chain that was zeroed: taken to be in one piece (right, here)
    let i = by_name(&gone, "in order.bin");
    assert_eq!(state(i), ("recoverable", 60));
    assert_eq!(got(&mut v, i), ordered);
    let x = by_name(&gone, "overwritten.bin");
    assert_eq!(state(x).0, "partly overwritten");
    let mut want = over.clone();
    want[2 * cl..3 * cl].fill(0);
    assert_eq!(got(&mut v, x), want);
    let d = by_name(&gone, "Old stuff");
    assert!(d.dir && state(d).0 == "recoverable");
    let seg = d.deleted.as_ref().unwrap().seg.clone();
    let inside = deleted_of(&mut v, &seg);
    assert_eq!(names_of(&inside), ["letter to grandma.txt"]);
    assert_eq!(got(&mut v, &inside[0]), letter);
    assert!(
        v.find(&format!(
            "{seg}/{}",
            inside[0].deleted.as_ref().unwrap().seg
        ))
        .is_ok()
    );
    // a set whose checksum no longer matches (its name damaged since) isn't listed
    let mut bad = w.img.clone();
    bad[pa + 64 + 2] ^= 0x20;
    let mut b = open(&bad);
    assert!(!names_of(&deleted_of(&mut b, "")).contains(&"Sunset photo.jpg"));
    survive_damage_deleted(
        &w.img,
        &[
            0,
            le(&w.img, 80, 4) * 512,
            w.off(root) as u64,
            w.bitmap_at as u64,
        ],
        150,
    );
}

// ---- NTFS: mkntfs and ntfscp make it; deletes are written here (there's no ntfsrm)

/// An NTFS image, for changing records the way ntfs-3g's delete does.
struct NtfsW {
    img: Vec<u8>,
    cluster: u64,
    rec: usize,
    mft: Vec<(u64, u64, Option<u64>)>,
}

/// Where a non-resident stream's byte `pos` is in the image, by its runs.
fn mapped(runs: &[(u64, u64, Option<u64>)], cluster: u64, pos: u64) -> usize {
    let vcn = pos / cluster;
    let (v, _, l) = runs
        .iter()
        .find(|(v, n, _)| (*v..v + n).contains(&vcn))
        .expect("inside the stream");
    ((l.unwrap() + vcn - v) * cluster + pos % cluster) as usize
}

/// The attributes of a record: (offset, kind).
fn attrs_in(b: &[u8]) -> Vec<(usize, u32)> {
    let mut out = Vec::new();
    let mut a = le(b, 20, 2) as usize;
    while a + 16 <= b.len() {
        let kind = le(b, a, 4) as u32;
        if kind == 0xFFFF_FFFF {
            break;
        }
        out.push((a, kind));
        a += le(b, a + 4, 4) as usize;
    }
    out
}

/// The runs of a record's unnamed non-resident attribute `kind`.
fn runs_of(b: &[u8], kind: u32) -> Vec<(u64, u64, Option<u64>)> {
    for (a, k) in attrs_in(b) {
        if k == kind && b[a + 8] == 1 && b[a + 9] == 0 {
            let len = le(b, a + 4, 4) as usize;
            let off = le(b, a + 32, 2) as usize;
            return ntfs::decode_runs(&b[a + off..a + len], le(b, a + 16, 8)).unwrap();
        }
    }
    Vec::new()
}

impl NtfsW {
    fn new(img: Vec<u8>) -> NtfsW {
        let cluster = le(&img, 11, 2) * u64::from(img[13]);
        let rec = match img[64] as i8 {
            n if n < 0 => 1usize << n.unsigned_abs(),
            n => n as usize * cluster as usize,
        };
        let at = (le(&img, 48, 8) * cluster) as usize;
        let mut rec0 = img[at..at + rec].to_vec();
        unprotect(&mut rec0);
        let mft = runs_of(&rec0, 0x80);
        NtfsW {
            img,
            cluster,
            rec,
            mft,
        }
    }

    fn read(&self, n: u64) -> Vec<u8> {
        let mut b: Vec<u8> = (0..self.rec as u64)
            .map(|i| self.img[mapped(&self.mft, self.cluster, n * self.rec as u64 + i)])
            .collect();
        unprotect(&mut b);
        b
    }

    fn write(&mut self, n: u64, b: &[u8]) {
        let mut b = b.to_vec();
        protect(&mut b);
        for (i, &x) in b.iter().enumerate() {
            let at = mapped(&self.mft, self.cluster, n * self.rec as u64 + i as u64);
            self.img[at] = x;
        }
    }

    /// Sets or clears bits `first..first + count` of record `n`'s unnamed stream `kind` (a bitmap).
    fn bits(&mut self, n: u64, kind: u32, first: u64, count: u64, used: bool) {
        let runs = runs_of(&self.read(n), kind);
        for bit in first..first + count {
            let at = mapped(&runs, self.cluster, bit / 8);
            if used {
                self.img[at] |= 1 << (bit % 8);
            } else {
                self.img[at] &= !(1 << (bit % 8));
            }
        }
    }

    /// Marks record `n` free (as a folder's, with `dir`), its sequence number moved on and its
    /// clusters freed; returns the sequence number it had.
    fn delete(&mut self, n: u64, dir: bool) -> u16 {
        let mut b = self.read(n);
        let flags = le(&b, 22, 2) as u16;
        let flags = if dir { (flags | 2) & !1 } else { flags & !1 };
        b[22..24].copy_from_slice(&flags.to_le_bytes());
        let seq = le(&b, 16, 2) as u16;
        b[16..18].copy_from_slice(&(seq + 1).to_le_bytes());
        for (_, len, lcn) in runs_of(&b, 0x80) {
            if let Some(l) = lcn {
                self.bits(6, 0x80, l, len, false);
            }
        }
        self.write(n, &b);
        seq
    }

    /// Points every $FILE_NAME of record `n` at a parent folder.
    fn set_parent(&mut self, n: u64, parent: u64, seq: u16) {
        let mut b = self.read(n);
        for (a, k) in attrs_in(&b) {
            if k == 0x30 {
                let v = a + le(&b, a + 20, 2) as usize;
                put(&mut b, v, parent | (u64::from(seq) << 48), 8);
            }
        }
        self.write(n, &b);
    }

    fn seq(&self, n: u64) -> u16 {
        le(&self.read(n), 16, 2) as u16
    }
}

fn unprotect(b: &mut [u8]) {
    let (usa, count) = (le(b, 4, 2) as usize, le(b, 6, 2) as usize);
    for i in 1..count {
        let end = i * 512;
        b[end - 2] = b[usa + 2 * i];
        b[end - 1] = b[usa + 2 * i + 1];
    }
}

fn protect(b: &mut [u8]) {
    let (usa, count) = (le(b, 4, 2) as usize, le(b, 6, 2) as usize);
    let usn = (le(b, usa, 2) as u16).wrapping_add(1).max(1).to_le_bytes();
    b[usa..usa + 2].copy_from_slice(&usn);
    for i in 1..count {
        let end = i * 512;
        b[usa + 2 * i] = b[end - 2];
        b[usa + 2 * i + 1] = b[end - 1];
        b[end - 2..end].copy_from_slice(&usn);
    }
}

fn record_of<R: Read + Seek>(v: &mut Volume<R>, name: &str) -> u64 {
    match v.find(name).unwrap().node {
        Node::Ntfs { record, .. } => record,
        n => panic!("{name}: {n:?}"),
    }
}

#[test]
fn ntfs_deleted_files_come_back() {
    let mkntfs = need!("mkntfs");
    let ntfscp = need!("ntfscp");
    for cluster in ["4096", "512"] {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("ntfs.img");
        File::create(&img).unwrap().set_len(24 << 20).unwrap();
        run(Command::new(&mkntfs)
            .args(["-F", "-Q", "-q", "-L", "NTFSDEL", "-c", cluster])
            .arg(&img));
        let file = |name: &str, bytes: &[u8]| {
            let src = dir.path().join("src.bin");
            std::fs::write(&src, bytes).unwrap();
            run(Command::new(&ntfscp).arg(&img).arg(&src).arg(name));
        };
        let big = noise(41, 300_000);
        let partly = noise(42, 65_536);
        let photo = noise(43, 20_000);
        file("keep.txt", b"kept\n");
        file("small.txt", b"a small file, kept inside its MFT record\n");
        file("big.bin", &big);
        file("partly.bin", &partly);
        file("Trip", b"becomes a folder");
        file("a photo.jpg", &photo);
        file("b.txt", b"b, in the deleted folder\n");
        file("orphan.txt", b"its folder is gone\n");
        let mut w = NtfsW::new(std::fs::read(&img).unwrap());
        let mut v = open(&w.img);
        let rec = |v: &mut Mem, n: &str| record_of(v, n);
        let [keep, small, bigr, partr, trip, a, b, orphan] = [
            "keep.txt",
            "small.txt",
            "big.bin",
            "partly.bin",
            "Trip",
            "a photo.jpg",
            "b.txt",
            "orphan.txt",
        ]
        .map(|n| rec(&mut v, n));
        // a deleted folder: "Trip" made a folder, the two files in it, then all three deleted
        let trip_seq = w.seq(trip);
        w.set_parent(a, trip, trip_seq);
        w.set_parent(b, trip, trip_seq);
        // a deleted file whose folder record now holds something else
        w.set_parent(orphan, keep, w.seq(keep));
        for n in [small, bigr, partr, a, b, orphan] {
            w.delete(n, false);
        }
        w.delete(trip, true);
        // partly.bin: a few of its clusters taken by another file since
        let cl = w.cluster;
        let prun = runs_of(&w.read(partr), 0x80)[0].2.unwrap();
        let at = ((prun + 2) * cl) as usize;
        w.img[at..at + 2 * cl as usize].fill(0xDD);
        // held as in use while ntfs-3g saves another file (only big.bin is left for it to reuse)
        let held: Vec<(u64, u64)> = [partr, a]
            .iter()
            .flat_map(|&n| runs_of(&w.read(n), 0x80))
            .map(|(_, len, lcn)| (lcn.unwrap(), len))
            .collect();
        for &(l, n) in &held {
            w.bits(6, 0x80, l, n, true);
        }
        std::fs::write(&img, &w.img).unwrap();
        // another file saved since, wherever ntfs-3g puts it (big.bin's clusters are free)
        let after = noise(44, 200_000);
        file("after.bin", &after);
        // then the held clusters freed (but partly.bin's taken two), and the deleted records freed
        // in the MFT's own bitmap too
        let mut w = NtfsW::new(std::fs::read(&img).unwrap());
        for &(l, n) in &held {
            w.bits(6, 0x80, l, n, false);
        }
        w.bits(6, 0x80, prun + 2, 2, true);
        for n in [small, bigr, partr, a, b, orphan, trip] {
            w.bits(0, 0xB0, n, 1, false);
        }
        std::fs::write(&img, &w.img).unwrap();
        if let Some(und) = tool("ntfsundelete") {
            let out = Command::new(und)
                .arg("-s")
                .arg("-f")
                .arg(&img)
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&out.stdout);
            for name in ["small.txt", "big.bin", "a photo.jpg", "orphan.txt"] {
                assert!(
                    text.contains(name),
                    "ntfsundelete doesn't list {name}:\n{text}"
                );
            }
        }
        let mut v = open(&w.img);
        let live = names(&mut v, "");
        assert_eq!(live, sorted(&["keep.txt", "after.bin"]), "{live:?}");
        let back = read(&mut v, "after.bin");
        assert!(
            back == after,
            "{cluster}: after.bin differs from byte {:?} (of {}), MFT bitmap attr: {:?}",
            back.iter().zip(&after).position(|(x, y)| x != y),
            back.len(),
            attrs_in(&w.read(0))
                .iter()
                .map(|&(a, k)| (k, w.read(0)[a + 8]))
                .collect::<Vec<_>>()
        );
        let gone = deleted_of(&mut v, "");
        assert_eq!(
            names_of(&gone),
            ["Trip", "big.bin", "orphan.txt", "partly.bin", "small.txt"]
        );
        let s = by_name(&gone, "small.txt");
        assert_eq!(state(s), ("recoverable", 99));
        assert_eq!(
            got(&mut v, s),
            b"a small file, kept inside its MFT record\n"
        );
        assert!(s.mtime.is_some_and(|t| t > 1_600_000_000));
        // big.bin: whole, or whatever after.bin took is empty and the rest is as it was
        let g = by_name(&gone, "big.bin");
        let data = got(&mut v, g);
        match state(g) {
            ("recoverable", 95) => assert_eq!(data, big),
            ("partly overwritten", _) => {
                for (x, y) in data.chunks(cl as usize).zip(big.chunks(cl as usize)) {
                    assert!(x == y || x.iter().all(|&z| z == 0));
                }
            }
            other => panic!("big.bin: {other:?}"),
        }
        let p = by_name(&gone, "partly.bin");
        assert_eq!(state(p).0, "partly overwritten");
        let mut want = partly.clone();
        want[2 * cl as usize..4 * cl as usize].fill(0);
        let back = got(&mut v, p);
        assert!(
            back == want,
            "{cluster}: partly.bin differs from byte {:?}; runs {:?}",
            back.iter().zip(&want).position(|(x, y)| x != y),
            runs_of(&w.read(partr), 0x80)
        );
        let o = by_name(&gone, "orphan.txt");
        assert!(
            o.deleted
                .as_ref()
                .unwrap()
                .why
                .contains("folder was deleted too")
        );
        assert_eq!(got(&mut v, o), b"its folder is gone\n");
        let t = by_name(&gone, "Trip");
        assert!(t.dir && state(t).0 == "recoverable");
        let seg = t.deleted.as_ref().unwrap().seg.clone();
        let inside = deleted_of(&mut v, &seg);
        assert_eq!(names_of(&inside), ["a photo.jpg", "b.txt"]);
        assert_eq!(state(&inside[0]), ("recoverable", 95));
        assert_eq!(got(&mut v, &inside[0]), photo);
        assert_eq!(got(&mut v, &inside[1]), b"b, in the deleted folder\n");
        let path = format!("{seg}/{}", inside[0].deleted.as_ref().unwrap().seg);
        assert_eq!(read(&mut v, &path), photo);
        // by record number only among a folder's deleted entries
        assert!(v.find(&undelete::seg("a photo.jpg", a)).is_err());
        if cluster == "4096" {
            let mft = le(&w.img, 48, 8) * 4096;
            survive_damage_deleted(&w.img, &[0, mft, mft + 16 * 1024, mft + 40 * 1024], 120);
        }
    }
}

// ---- ext: deleted files can't be followed

#[test]
fn ext_says_why_not() {
    let src = ext_source();
    let Some((_t, img)) = mke2fs(&src, 16 << 20, &["-t", "ext4"]) else {
        eprintln!("skipping: mke2fs isn't installed");
        return;
    };
    let mut v = open(&img);
    assert!(
        v.undelete_unsupported()
            .is_some_and(|w| w.contains("Files were deleted, or it was"))
    );
    let err = v.deleted_path("", 10).unwrap_err().to_string();
    assert!(err.contains("ext3 and ext4"), "{err}");
}
