//! Searching every drive at once: ranking, accents, scope, files that are gone, and indexes made
//! before names were stored.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use backdown_core::find::{self, Found, Query};
use backdown_core::name_index;
use backdown_core::{Db, index};

fn write(path: &Path, bytes: &[u8], age_days: u64) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
    let t = SystemTime::now() - Duration::from_secs(age_days * 86_400);
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

fn scan(db: &mut Db, root: &Path) -> i64 {
    index::index(
        db,
        root,
        &index::Options {
            one_file_system: true,
            allow_empty: false,
            allow_other_drive: false,
        },
        &mut |_| {},
    )
    .unwrap()
    .scan
}

fn search(db: &Db, q: &str, within: Option<&Path>, earlier: bool, limit: usize) -> Found {
    find::find(
        db,
        &Query {
            q,
            within,
            earlier,
            limit,
        },
    )
    .unwrap()
}

fn names(f: &Found) -> Vec<String> {
    f.hits
        .iter()
        .map(|h| format!("{}/{}", h.drive, h.name))
        .collect()
}

/// Two drives: a laptop and an archive, the archive scanned twice with a file deleted in between.
fn drives() -> (tempfile::TempDir, Db) {
    let tmp = tempfile::tempdir().unwrap();
    let laptop = tmp.path().join("Laptop");
    let archive = tmp.path().join("Archive");
    write(&laptop.join("docs/report.pdf"), b"r1", 30);
    write(&laptop.join("docs/Annual report 2019.docx"), b"annual", 20);
    write(&laptop.join("docs/unreported.txt"), b"u", 10);
    write(&laptop.join("Reports/q1.xlsx"), b"q1", 5);
    write(&laptop.join("photos/Résumé.pdf"), b"nfc", 40);
    write(
        &laptop.join("photos/Re\u{301}sume\u{301} old.txt"),
        b"nfd",
        50,
    );
    write(&archive.join("old/report.pdf"), b"r2", 2);
    write(&archive.join("old/gone.txt"), b"soon deleted", 100);
    let mut db = Db::open(&tmp.path().join("idx.db")).unwrap();
    scan(&mut db, &laptop);
    scan(&mut db, &archive);
    fs::remove_file(archive.join("old/gone.txt")).unwrap();
    scan(&mut db, &archive);
    (tmp, db)
}

#[test]
fn everywhere_ranked_exact_then_prefix_then_word_then_anywhere() {
    let (tmp, db) = drives();
    let f = search(&db, "REPORT", None, false, 50);
    assert_eq!(
        names(&f),
        [
            "Archive/report.pdf", // exact (extension aside), the newer one first
            "Laptop/report.pdf",
            "Laptop/Reports",                 // starts with it (a folder)
            "Laptop/Annual report 2019.docx", // a word starts with it
            "Laptop/unreported.txt",          // somewhere inside
        ]
    );
    assert_eq!(f.total, 5);
    let top = &f.hits[0];
    assert_eq!(
        (top.kind, top.folder.as_str(), top.rank),
        ("file", "old", 4)
    );
    assert!(!top.gone);
    assert_eq!(f.hits[2].kind, "dir");
    assert_eq!(
        find::lname(&tmp.path().join("Laptop/Reports")),
        "reports",
        "folder names are folded too"
    );

    // several words: all of them, in any order
    assert_eq!(
        names(&search(&db, "2019 annual", None, false, 50)),
        ["Laptop/Annual report 2019.docx"]
    );

    // the best few, and how many there are
    let f = search(&db, "report", None, false, 2);
    assert_eq!(f.hits.len(), 2);
    assert_eq!(f.total, 5);
}

#[test]
fn accents_match_whichever_way_they_were_written() {
    let (_tmp, db) = drives();
    let mut found = names(&search(&db, "resume", None, false, 50));
    found.sort();
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(found.iter().all(|n| n.starts_with("Laptop/R")));
    assert_eq!(search(&db, "RÉSUMÉ", None, false, 50).total, 2);
}

#[test]
fn this_folder_only() {
    let (tmp, db) = drives();
    // the index resolves the scanned root's symlinks (on macOS /var → /private/var), so a scope
    // built from the raw tempdir path would match nothing — canonicalise it the same way
    let root = tmp.path().canonicalize().unwrap();
    let f = search(&db, "report", Some(&root.join("Laptop/docs")), false, 50);
    assert_eq!(
        names(&f),
        [
            "Laptop/report.pdf",
            "Laptop/Annual report 2019.docx",
            "Laptop/unreported.txt"
        ]
    );
    assert_eq!(
        search(&db, "report", Some(&root.join("Laptop")), false, 50).total,
        4
    );
}

#[test]
fn earlier_versions_say_when_they_were_last_seen() {
    let (_tmp, db) = drives();
    assert_eq!(search(&db, "gone", None, false, 50).total, 0);
    let f = search(&db, "gone", None, true, 50);
    assert_eq!(f.total, 1);
    assert_eq!(f.gone, 1);
    let h = &f.hits[0];
    assert!(h.gone);
    assert_eq!(h.drive, "Archive");
    let scan = h.scan.expect("the scan it was last seen at");
    let finished: i64 = db
        .conn
        .query_row("SELECT finished_at FROM scans WHERE id = ?1", [scan], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(h.seen, Some(finished));
    // what's still there comes first
    let f = search(&db, "o", None, true, 50);
    let first_gone = f.hits.iter().position(|h| h.gone).unwrap();
    assert!(
        f.hits[first_gone..]
            .iter()
            .all(|h| h.gone || h.rank < f.hits[first_gone].rank)
    );
}

#[test]
fn indexes_made_before_names_are_named_when_opened() {
    let (tmp, db) = drives();
    let path = tmp.path().join("idx.db");
    // an index as 0.2.0-alpha.4 left it: no names, no index on them, no names index
    db.conn
        .execute_batch(
            "DROP TRIGGER files_name_add; DROP TRIGGER files_name_set;
             DROP TRIGGER dirs_name_add; DROP TRIGGER dirs_name_set;
             DROP TRIGGER remote_files_name_add; DROP TRIGGER remote_files_name_set;
             DROP TRIGGER remote_dirs_name_add; DROP TRIGGER names_fts_add;
             DROP TABLE names_fts; DROP TABLE names; DROP TABLE names_filled;
             DROP INDEX files_lname; DROP INDEX dirs_lname;
             ALTER TABLE files DROP COLUMN lname; ALTER TABLE dirs DROP COLUMN lname;",
        )
        .unwrap();
    drop(db);
    // read-only (nothing added yet): found the slow way, from the paths
    let ro = Db::open_read_only(&path).unwrap();
    assert_eq!(
        names(&search(&ro, "report", None, false, 50))[0],
        "Archive/report.pdf"
    );
    assert_eq!(search(&ro, "report", None, false, 50).total, 5);
    drop(ro);
    // opened for writing: names added and indexed
    let db = Db::open(&path).unwrap();
    let unnamed: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM files WHERE lname IS NULL", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(unnamed, 0);
    // the names index isn't filled yet (that's the daemon's job): found through the names column
    assert!(!name_index::ready(&db.conn, name_index::LOCAL).unwrap());
    assert_eq!(search(&db, "report", None, false, 50).total, 5);
    // filled: the same, through the names index
    assert!(db.missing_later_indexes().unwrap().contains(&"names"));
    db.build_later_indexes(&mut |_| {}).unwrap();
    assert!(name_index::ready(&db.conn, name_index::LOCAL).unwrap());
    assert!(db.missing_later_indexes().unwrap().is_empty());
    assert_eq!(search(&db, "report", None, false, 50).total, 5);

    // rows an older BackDown writes after that (no name) are still found
    db.conn
        .execute(
            "UPDATE files SET lname = NULL WHERE lname = 'unreported.txt'",
            [],
        )
        .unwrap();
    let f = search(&db, "report", None, false, 50);
    assert_eq!(f.total, 5);
    assert!(names(&f).contains(&"Laptop/unreported.txt".to_string()));
    drop(db);
    let db = Db::open(&path).unwrap();
    let unnamed: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM files WHERE lname IS NULL", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(unnamed, 0, "filled in on the next open");
}

#[test]
fn the_names_index_answers_as_reading_every_name_does() {
    let (tmp, mut db) = drives();
    // a file added after the names index was ready: named by the trigger as it's written
    write(&tmp.path().join("Laptop/new/report-final.txt"), b"new", 1);
    scan(&mut db, &tmp.path().join("Laptop"));
    assert!(name_index::ready(&db.conn, name_index::LOCAL).unwrap());
    let laptop = tmp.path().join("Laptop");
    let queries = [
        "report",
        "REPORT",
        "rep",
        "re",
        "o",
        "2019 annual",
        "résumé",
        "gone",
        "pdf",
        "final",
        "nothing like this",
        "q1",
    ];
    let scopes = [(None, false), (None, true), (Some(laptop.as_path()), false)];
    let answers = |db: &Db| {
        let mut v = Vec::new();
        for q in queries {
            for (within, earlier) in scopes {
                let f = search(db, q, within, earlier, 50);
                assert!(!f.more);
                v.push((q, within.is_some(), earlier, names(&f), f.total, f.gone));
            }
        }
        v
    };
    let through_names = answers(&db);
    assert!(
        through_names
            .iter()
            .any(|a| a.0 == "final" && a.3 == ["Laptop/report-final.txt"])
    );
    // the same searches reading every name, as before the names index
    db.conn
        .execute("UPDATE names_filled SET done = 0", [])
        .unwrap();
    assert!(!name_index::ready(&db.conn, name_index::LOCAL).unwrap());
    assert_eq!(answers(&db), through_names);
}
