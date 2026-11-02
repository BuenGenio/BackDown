//! Removing a drive from the index (forget.rs), and stopping a scan part way (index::stop_when).

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use backdown_core::{Db, browse, forget, hash, index};

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn opts() -> index::Options {
    index::Options {
        one_file_system: true,
        allow_empty: false,
    }
}

fn scan(db: &mut Db, root: &Path) -> anyhow::Result<index::Stats> {
    index::index(db, root, &opts(), &mut |_| {})
}

fn count(db: &Db, sql: &str, id: i64) -> i64 {
    db.conn.query_row(sql, [id], |r| r.get(0)).unwrap()
}

/// What's recorded of one root, in every table that holds rows of it.
fn rows_of(db: &Db, id: i64) -> [i64; 4] {
    [
        count(db, "SELECT COUNT(*) FROM files WHERE root_id = ?1", id),
        count(db, "SELECT COUNT(*) FROM dirs WHERE root_id = ?1", id),
        count(db, "SELECT COUNT(*) FROM scans WHERE root_id = ?1", id),
        count(db, "SELECT COUNT(*) FROM roots WHERE id = ?1", id),
    ]
}

#[test]
fn a_drive_added_by_mistake_goes_and_its_folder_can_be_added_instead() {
    let tmp = tempfile::tempdir().unwrap();
    let disk = tmp.path().join("disk");
    let home = disk.join("home/me");
    let other = tmp.path().join("other");
    write(&disk.join("etc/hosts"), b"127.0.0.1 localhost");
    write(&home.join("notes.txt"), b"the same words");
    write(&home.join("Photos/a.jpg"), &[7u8; 5000]);
    write(&other.join("copy-of-notes.txt"), b"the same words");

    let mut db = Db::open(&tmp.path().join("idx.db")).unwrap();
    let disk = index::scan(&mut db, &disk, &opts(), index::Method::Walk, &mut |_| {})
        .unwrap()
        .root;
    // a second scan, so there's history (an earlier version) to remove too
    write(&home.join("notes.txt"), b"the same words, edited");
    scan(&mut db, &disk).unwrap();
    let other = scan(&mut db, &other).unwrap().root;
    hash::hash(
        &mut db,
        &hash::Options {
            jobs_per_device: 1,
            retry_errors: false,
        },
        &mut |_| {},
    )
    .unwrap();

    // the folder wanted can't be added while the whole disk is
    let err = scan(&mut db, &home).unwrap_err().to_string();
    assert!(err.contains("overlaps"), "{err}");

    let disk_id = forget::root_id(&db, &disk).unwrap().unwrap();
    let other_id = forget::root_id(&db, &other).unwrap().unwrap();
    let other_before = rows_of(&db, other_id);
    let mut stages = Vec::new();
    let gone = forget::forget(&db, &disk, &mut |p| stages.push(p.stage)).unwrap();
    assert_eq!(gone.scans, 2);
    assert!(gone.files >= 4, "current and earlier versions: {gone:?}");
    assert!(gone.dirs >= 4, "{gone:?}");
    assert!(
        stages.contains(&"files") && stages.last() == Some(&"done"),
        "{stages:?}"
    );

    assert_eq!(rows_of(&db, disk_id), [0, 0, 0, 0]);
    assert_eq!(
        rows_of(&db, other_id),
        other_before,
        "the other drive is untouched"
    );
    assert!(forget::pending(&db).unwrap().is_empty());
    let roots: Vec<String> = browse::roots(&db)
        .unwrap()
        .into_iter()
        .map(|r| r.path)
        .collect();
    assert_eq!(roots.len(), 1, "{roots:?}");

    // nothing on the drive was touched, and the folder can be added now
    assert!(disk.join("etc/hosts").is_file());
    let st = scan(&mut db, &home).unwrap();
    assert_eq!(st.files, 2);
    // a drive that isn't indexed can't be removed
    assert!(forget::forget(&db, &tmp.path().join("nowhere"), &mut |_| {}).is_err());
}

#[test]
fn a_removal_that_was_interrupted_is_finished_and_blocks_scans_meanwhile() {
    let tmp = tempfile::tempdir().unwrap();
    let drive = tmp.path().join("drive");
    write(&drive.join("a.txt"), b"a");
    let mut db = Db::open(&tmp.path().join("idx.db")).unwrap();
    let drive = scan(&mut db, &drive).unwrap().root;

    // marked (the daemon was stopped right after): hidden, not scanned, and listed as pending
    let id = forget::mark(&db, &drive).unwrap();
    assert!(browse::roots(&db).unwrap().is_empty());
    let err = scan(&mut db, &drive).unwrap_err().to_string();
    assert!(err.contains("being removed"), "{err}");
    assert_eq!(forget::pending(&db).unwrap(), vec![drive.clone()]);

    forget::forget(&db, &drive, &mut |_| {}).unwrap();
    assert_eq!(rows_of(&db, id), [0, 0, 0, 0]);
    assert!(forget::pending(&db).unwrap().is_empty());
    // and it can be indexed again from scratch
    assert_eq!(scan(&mut db, &drive).unwrap().files, 1);
}

#[test]
fn a_stopped_scan_keeps_what_it_read_and_the_next_one_finishes() {
    let tmp = tempfile::tempdir().unwrap();
    let drive = tmp.path().join("drive");
    for i in 0..50 {
        write(
            &drive.join(format!("d{}/f{i}.txt", i % 5)),
            format!("{i}").as_bytes(),
        );
    }
    let mut db = Db::open(&tmp.path().join("idx.db")).unwrap();

    let stop = Arc::new(AtomicBool::new(true));
    let err = {
        let _g = index::stop_when(&stop);
        scan(&mut db, &drive).unwrap_err()
    };
    assert!(err.is::<index::Stopped>(), "{err:#}");
    // unfinished, so not on the timeline; the connection is usable (no transaction left open)
    let unfinished: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM scans WHERE finished_at IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(unfinished, 1);
    assert!(db.conn.is_autocommit());

    // the guard is gone: the next scan isn't stopped, and finishes the job
    let st = scan(&mut db, &drive).unwrap();
    assert_eq!(st.files, 50);
    let root = forget::root_id(&db, &st.root).unwrap().unwrap();
    assert_eq!(browse::timeline(&db, root).unwrap().len(), 1);
}
