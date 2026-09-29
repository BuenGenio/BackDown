//! Tidying an archive: duplicates inside one folder become hardlinks (or clones), and nothing
//! else changes.
#![cfg(unix)]

use std::fs::{self, File};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};

use backdown_core::dedup::{self, Link};
use backdown_core::{Db, hash, index};

fn write(path: &Path, bytes: &[u8], mtime: u64) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(mtime))
        .unwrap();
}

fn blob(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(13).wrapping_add(seed))
        .collect()
}

fn ino(p: &Path) -> u64 {
    fs::metadata(p).unwrap().ino()
}

fn scan(db: &mut Db, root: &Path) {
    index::index(
        db,
        root,
        &index::Options {
            one_file_system: true,
            allow_empty: false,
        },
        &mut |_| {},
    )
    .unwrap();
    hash::hash(
        db,
        &hash::Options {
            jobs_per_device: 1,
            retry_errors: false,
        },
        &mut |_| {},
    )
    .unwrap();
}

#[cfg(target_os = "linux")]
fn set_xattr(p: &Path, name: &str, value: &[u8]) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(p.as_os_str().as_bytes()).unwrap();
    let name = std::ffi::CString::new(name).unwrap();
    // SAFETY: NUL-terminated strings and a live buffer with its length, for the call only.
    unsafe {
        libc::setxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        ) == 0
    }
}

/// drive/Archive: what's filed; drive/elsewhere: outside the folder being tidied
fn tree(d: &Path) -> bool {
    let a = d.join("Archive");
    let photo = blob(1, 300_000);
    // three copies alike in every way: two become links to the first
    write(&a.join("Photos/2019/beach.jpg"), &photo, 1_560_000_000);
    write(
        &a.join("Photos/Also from Old Stick/beach.jpg"),
        &photo,
        1_560_000_000,
    );
    write(
        &a.join("Unsorted/stick/DCIM/beach.jpg"),
        &photo,
        1_560_000_000,
    );
    // the same bytes with another date: kept apart (a hardlink would lose one date)
    write(
        &a.join("Unsorted/laptop/beach (copy).jpg"),
        &photo,
        1_600_000_000,
    );
    // the same bytes outside the folder: never touched
    write(&d.join("elsewhere/beach.jpg"), &photo, 1_560_000_000);
    // different permissions: kept apart
    let doc = blob(2, 50_000);
    write(&a.join("Papers/tax.pdf"), &doc, 1_500_000_000);
    write(&a.join("Unsorted/stick/tax.pdf"), &doc, 1_500_000_000);
    fs::set_permissions(
        a.join("Unsorted/stick/tax.pdf"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    // a file with another name outside the folder: left alone
    let song = blob(3, 80_000);
    write(&a.join("Music/song.mp3"), &song, 1_400_000_000);
    write(&a.join("Unsorted/stick/song.mp3"), &song, 1_400_000_000);
    fs::hard_link(
        a.join("Unsorted/stick/song.mp3"),
        d.join("elsewhere/song.mp3"),
    )
    .unwrap();
    // too small to bother (below --min-size)
    write(&a.join("a.txt"), b"tiny", 1_400_000_000);
    write(&a.join("b.txt"), b"tiny", 1_400_000_000);
    // unique
    write(&a.join("Papers/cv.pdf"), &blob(4, 20_000), 1_400_000_000);
    // extended attributes that differ: kept apart (Linux; where user attributes are allowed)
    #[cfg(target_os = "linux")]
    {
        let note = blob(5, 30_000);
        write(&a.join("Notes/a.md"), &note, 1_450_000_000);
        write(&a.join("Unsorted/b.md"), &note, 1_450_000_000);
        set_xattr(&a.join("Notes/a.md"), "user.backdown-test", b"tag")
    }
    #[cfg(not(target_os = "linux"))]
    false
}

#[test]
fn duplicates_in_the_folder_become_hardlinks_and_nothing_else_changes() {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let d = tmp.path().join("drive");
    let has_xattrs = tree(&d);
    let a = d.join("Archive");
    let mut db = Db::open(&tmp.path().join("i.db")).unwrap();
    scan(&mut db, &d);
    let folder = dedup::check_folder(&a, None).unwrap();

    // the dry run: from the index and metadata only
    let before: Vec<u64> = [
        "Photos/2019/beach.jpg",
        "Photos/Also from Old Stick/beach.jpg",
        "Unsorted/stick/DCIM/beach.jpg",
    ]
    .iter()
    .map(|p| ino(&a.join(p)))
    .collect();
    let dir_time = fs::metadata(a.join("Unsorted/stick"))
        .unwrap()
        .modified()
        .unwrap();
    let r = dedup::assess(&db, &folder, Link::Hard, 1024).unwrap();
    assert_eq!(r.linked, 2, "{r:#?}");
    assert_eq!(r.saved_bytes, 600_000);
    assert_eq!(r.groups, 1);
    // the dated copy, the two tax.pdf, (and the two notes, whose attributes differ)
    assert_eq!(r.kept_apart, if has_xattrs { 5 } else { 3 }, "{r:#?}");
    assert_eq!(r.elsewhere, 1, "song.mp3 has a name outside");
    assert!(r.warning.contains("editing any of them changes them all"));
    assert_eq!(r.examples.len(), 1);
    assert!(r.examples[0].keeper.ends_with("Photos/2019/beach.jpg"));
    let after: Vec<u64> = [
        "Photos/2019/beach.jpg",
        "Photos/Also from Old Stick/beach.jpg",
        "Unsorted/stick/DCIM/beach.jpg",
    ]
    .iter()
    .map(|p| ino(&a.join(p)))
    .collect();
    assert_eq!(before, after, "a dry run changes nothing");

    // the real thing
    let id = dedup::record(&db, &folder, Link::Hard, 1024).unwrap();
    let r = dedup::run(&db, id, None, &AtomicBool::new(false), &mut |_, _| {}).unwrap();
    assert_eq!(
        (r.linked, r.saved_bytes, r.failed),
        (2, 600_000, 0),
        "{r:#?}"
    );
    let k = ino(&a.join("Photos/2019/beach.jpg"));
    assert_eq!(ino(&a.join("Photos/Also from Old Stick/beach.jpg")), k);
    assert_eq!(ino(&a.join("Unsorted/stick/DCIM/beach.jpg")), k);
    assert_eq!(
        fs::metadata(a.join("Photos/2019/beach.jpg"))
            .unwrap()
            .nlink(),
        3
    );
    assert_ne!(
        ino(&a.join("Unsorted/laptop/beach (copy).jpg")),
        k,
        "its date differs"
    );
    assert_ne!(ino(&d.join("elsewhere/beach.jpg")), k, "outside the folder");
    assert_eq!(
        fs::read(a.join("Unsorted/stick/DCIM/beach.jpg")).unwrap(),
        blob(1, 300_000)
    );
    assert_ne!(
        ino(&a.join("Papers/tax.pdf")),
        ino(&a.join("Unsorted/stick/tax.pdf"))
    );
    assert_eq!(
        fs::metadata(a.join("Unsorted/stick/tax.pdf"))
            .unwrap()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(d.join("elsewhere/song.mp3")).unwrap().nlink(),
        2,
        "the file with a name outside is as it was"
    );
    assert_ne!(ino(&a.join("a.txt")), ino(&a.join("b.txt")));
    if has_xattrs {
        assert_ne!(ino(&a.join("Notes/a.md")), ino(&a.join("Unsorted/b.md")));
    }
    assert_eq!(
        fs::metadata(a.join("Unsorted/stick"))
            .unwrap()
            .modified()
            .unwrap(),
        dir_time,
        "folders keep their dates"
    );
    // nothing of ours left behind
    for e in walk(&d) {
        assert!(!e.contains(".backdown-"), "{e}");
    }
    // journaled
    let n: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM dedup_links WHERE dedup_id = ?1 AND how = 'hardlink'",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 2);
    let h = dedup::history(&db).unwrap();
    assert_eq!(h[0]["state"], "done");
    assert!(
        h[0]["summary"]
            .as_str()
            .unwrap()
            .starts_with("hardlinked 2 files, saving")
    );

    // again (the index not rescanned yet): everything is already linked
    let id2 = dedup::record(&db, &folder, Link::Hard, 1024).unwrap();
    let r = dedup::run(&db, id2, None, &AtomicBool::new(false), &mut |_, _| {}).unwrap();
    assert_eq!((r.linked, r.already), (0, 2), "{r:#?}");
    // and after a rescan, the same
    scan(&mut db, &d);
    let r = dedup::assess(&db, &folder, Link::Hard, 1024).unwrap();
    assert_eq!((r.linked, r.already), (0, 2), "{r:#?}");
    // a fresh index doesn't hash names of one file (nothing to compare): nothing is "unhashed"
    let mut fresh = Db::open(&tmp.path().join("fresh.db")).unwrap();
    scan(&mut fresh, &d);
    let r = dedup::assess(&fresh, &folder, Link::Hard, 1024).unwrap();
    assert_eq!((r.linked, r.unhashed), (0, 0), "{r:#?}");
}

fn walk(d: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for e in fs::read_dir(d).unwrap() {
        let p = e.unwrap().path();
        out.push(p.display().to_string());
        if p.is_dir() && !p.is_symlink() {
            out.extend(walk(&p));
        }
    }
    out
}

#[test]
fn a_file_changed_since_the_scan_is_left_alone() {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let a = tmp.path().join("Archive");
    let v = blob(9, 10_000);
    write(&a.join("one/x.bin"), &v, 1_500_000_000);
    write(&a.join("two/x.bin"), &v, 1_500_000_000);
    write(&a.join("three/x.bin"), &v, 1_500_000_000);
    let mut db = Db::open(&tmp.path().join("i.db")).unwrap();
    scan(&mut db, &a);
    // changed in place, keeping its size and date: only a re-read notices
    let mut w = v.clone();
    w[5000] ^= 1;
    write(&a.join("three/x.bin"), &w, 1_500_000_000);
    let folder = dedup::check_folder(&a, None).unwrap();
    let id = dedup::record(&db, &folder, Link::Hard, 1).unwrap();
    let r = dedup::run(&db, id, None, &AtomicBool::new(false), &mut |_, _| {}).unwrap();
    assert_eq!((r.linked, r.changed), (1, 1), "{r:#?}");
    assert_eq!(fs::read(a.join("three/x.bin")).unwrap(), w, "left as it is");
    assert_eq!(ino(&a.join("one/x.bin")), ino(&a.join("two/x.bin")));
    assert_eq!(dedup::history(&db).unwrap()[0]["state"], "partial");
}

#[test]
fn stopping_links_nothing_more_and_a_rerun_carries_on() {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let a = tmp.path().join("Archive");
    for g in 0..3u8 {
        for c in ["a", "b"] {
            write(
                &a.join(c).join(format!("{g}.bin")),
                &blob(g, 5000),
                1_500_000_000,
            );
        }
    }
    let mut db = Db::open(&tmp.path().join("i.db")).unwrap();
    scan(&mut db, &a);
    let folder = dedup::check_folder(&a, None).unwrap();
    let id = dedup::record(&db, &folder, Link::Hard, 1).unwrap();
    let r = dedup::run(&db, id, None, &AtomicBool::new(true), &mut |_, _| {}).unwrap();
    assert_eq!(r.linked, 0);
    assert_eq!(dedup::history(&db).unwrap()[0]["state"], "stopped");
    let r = dedup::run(&db, id, None, &AtomicBool::new(false), &mut |_, _| {}).unwrap();
    assert_eq!((r.linked, r.saved_bytes), (3, 15_000));
    assert_eq!(dedup::history(&db).unwrap()[0]["state"], "done");
}

#[test]
fn clones_need_a_filesystem_that_shares_blocks() {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let a = tmp.path().join("Archive");
    write(&a.join("a/x.bin"), &blob(1, 9000), 1_500_000_000);
    write(&a.join("b/x.bin"), &blob(1, 9000), 1_600_000_000);
    let mut db = Db::open(&tmp.path().join("i.db")).unwrap();
    scan(&mut db, &a);
    let folder = dedup::check_folder(&a, None).unwrap();
    let r = dedup::assess(&db, &folder, Link::Clone, 1).unwrap();
    assert!(r.warning.contains("stay separate files"));
    if cfg!(target_os = "linux") {
        // in-place clones keep each file's own dates: nothing is kept apart for them
        assert_eq!((r.linked, r.kept_apart), (1, 0), "{r:#?}");
    }
    let id = dedup::record(&db, &folder, Link::Clone, 1).unwrap();
    let out = dedup::run(&db, id, None, &AtomicBool::new(false), &mut |_, _| {});
    if r.clones_possible {
        let r = out.unwrap();
        assert_eq!(r.linked, 1);
        assert_ne!(
            ino(&a.join("a/x.bin")),
            ino(&a.join("b/x.bin")),
            "still two files"
        );
        assert_eq!(
            fs::metadata(a.join("b/x.bin")).unwrap().modified().unwrap(),
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000),
            "each keeps its own date"
        );
    } else {
        let e = format!("{:#}", out.unwrap_err());
        assert!(e.contains("can't share blocks"), "{e}");
        assert_eq!(dedup::history(&db).unwrap()[0]["state"], "failed");
    }
}

#[test]
fn never_the_system_nor_the_index_nor_an_unindexed_folder() {
    assert!(dedup::check_folder(Path::new("/usr"), None).is_err());
    assert!(dedup::check_folder(Path::new("relative/path"), None).is_err());
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let data = tmp.path().join("backdown");
    fs::create_dir_all(&data).unwrap();
    let ix = data.join("index.db");
    let e = dedup::check_folder(&data, Some(&ix)).unwrap_err();
    assert!(format!("{e:#}").contains("index"), "{e:#}");
    let db = Db::open(&tmp.path().join("i.db")).unwrap();
    let other = tmp.path().join("not-indexed");
    fs::create_dir_all(&other).unwrap();
    let e = dedup::assess(&db, &other, Link::Hard, 1).unwrap_err();
    assert!(format!("{e:#}").contains("index it first"), "{e:#}");
}
