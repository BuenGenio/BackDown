//! The browser's view of a drive across two scans.

use std::fs;
use std::path::Path;

use backdown_core::{Db, browse, hash, index};

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn hash_all(db: &mut Db) {
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

fn scan(db: &mut Db, root: &Path) -> i64 {
    index::index(
        db,
        root,
        &index::Options {
            one_file_system: true,
            allow_empty: false,
        },
        &mut |_| {},
    )
    .unwrap()
    .scan
}

#[test]
fn folders_as_they_were() {
    let tmp = tempfile::tempdir().unwrap();
    let drive = tmp.path().join("drive");
    let other = tmp.path().join("other");
    write(&drive.join("docs/report.txt"), b"first draft");
    write(
        &drive.join("docs/old.txt"),
        b"to be deleted, but copied elsewhere",
    );
    write(&drive.join("photo.jpg"), &[7u8; 5000]);
    write(
        &other.join("backup-of-old.txt"),
        b"to be deleted, but copied elsewhere",
    );

    let mut db = Db::open(&tmp.path().join("idx.db")).unwrap();
    let first = scan(&mut db, &drive);
    scan(&mut db, &other);
    hash_all(&mut db); // each scan is hashed while its files exist, so old versions keep provable content

    write(&drive.join("docs/report.txt"), b"second draft, longer");
    fs::remove_file(drive.join("docs/old.txt")).unwrap();
    write(&drive.join("docs/new.txt"), b"brand new");
    let second = scan(&mut db, &drive);
    hash_all(&mut db);

    let drive = dunce::canonicalize(&drive).unwrap();
    let docs = drive.join("docs");

    // now
    let now = browse::list(&db, &docs, None).unwrap();
    assert_eq!(now.scan, second);
    let names: Vec<_> = now.entries.iter().map(|e| e.name.as_str()).collect();
    assert!(
        names.contains(&"new.txt") && names.contains(&"report.txt") && !names.contains(&"old.txt")
    );
    let new = now.entries.iter().find(|e| e.name == "new.txt").unwrap();
    assert!(new.new_here);
    let report = now.entries.iter().find(|e| e.name == "report.txt").unwrap();
    assert_eq!(report.versions, 2);
    assert!(report.new_here, "changed at this scan");

    // as it was at the first scan
    let then = browse::list(&db, &docs, Some(first)).unwrap();
    let old = then.entries.iter().find(|e| e.name == "old.txt").unwrap();
    assert_eq!(old.later, "deleted");
    let old_report = then
        .entries
        .iter()
        .find(|e| e.name == "report.txt")
        .unwrap();
    assert_eq!(old_report.later, "changed");
    assert!(!then.entries.iter().any(|e| e.name == "new.txt"));

    // folder totals, then and now
    let root_then = browse::list(&db, &drive, Some(first)).unwrap();
    let docs_then = root_then.entries.iter().find(|e| e.name == "docs").unwrap();
    assert_eq!(docs_then.files, Some(2));
    assert_eq!(docs_then.size, 11 + 35);
    let root_now = browse::list(&db, &drive, None).unwrap();
    assert_eq!(root_now.files, 3);
    assert_eq!(root_now.bytes, 20 + 9 + 5000);
    assert_eq!(root_now.crumbs.last().unwrap().name, "drive");

    // the deleted file's bytes are still readable from its copy on the other drive
    let gone = browse::file(&db, old.id.unwrap()).unwrap();
    assert!(!gone.current);
    let from = gone.content_from.expect("a copy exists");
    assert!(from.ends_with("backup-of-old.txt"), "{from}");
    // the first draft is gone for good: nothing kept its bytes
    let draft = browse::file(&db, old_report.id.unwrap()).unwrap();
    assert!(draft.content_from.is_none());
    assert_eq!(draft.versions.len(), 2);
    assert!(draft.versions.iter().any(|v| v.current && v.readable));

    // keys round-trip
    assert_eq!(browse::path_of(&browse::key(&docs)).unwrap(), docs);
    let hits = browse::search(&db, &drive, "REPORT", 10).unwrap();
    assert_eq!(hits.len(), 1);
}

#[test]
fn folders_list_even_without_folder_totals() {
    // an index written before folder totals existed (0.2.0-alpha.1) has files but no `dirs` rows
    let tmp = tempfile::tempdir().unwrap();
    let drive = tmp.path().join("drive");
    write(&drive.join("a/b/deep.txt"), b"deep");
    write(&drive.join("a/top.txt"), b"top!");
    write(&drive.join("root.txt"), b"r");
    let mut db = Db::open(&tmp.path().join("idx.db")).unwrap();
    scan(&mut db, &drive);
    db.conn.execute("DELETE FROM dirs", []).unwrap();

    let drive = dunce::canonicalize(&drive).unwrap();
    let root = browse::list(&db, &drive, None).unwrap();
    assert!(root.needs_reindex);
    let a = root
        .entries
        .iter()
        .find(|e| e.name == "a")
        .expect("subfolder derived from its files");
    assert_eq!((a.kind, a.files, a.size), ("dir", Some(2), 8));
    assert!(root.entries.iter().any(|e| e.name == "root.txt"));
    assert_eq!((root.files, root.bytes), (3, 9));
    let inner = browse::list(&db, &drive.join("a"), None).unwrap();
    assert!(
        inner
            .entries
            .iter()
            .any(|e| e.name == "b" && e.kind == "dir")
    );
    assert!(inner.entries.iter().any(|e| e.name == "top.txt"));
}
