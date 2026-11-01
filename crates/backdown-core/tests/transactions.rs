//! A job that fails part way leaves the index's connection usable. The daemon's worker keeps one
//! connection for every job, so a transaction a failed job left open would make every later job's
//! `BEGIN` fail, and hold what was written after it uncommitted until the daemon stopped.

use std::fs;
use std::path::Path;

use backdown_core::{Db, hash, index};

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
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
}

fn hash_all(db: &mut Db) -> anyhow::Result<hash::Stats> {
    hash::hash(
        db,
        &hash::Options {
            jobs_per_device: 2,
            retry_errors: false,
        },
        &mut |_| {},
    )
}

#[test]
fn hashing_that_fails_part_way_leaves_no_transaction_open() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join("drive");
    // same sizes, so each could be a duplicate and gets hashed
    write(&d.join("a.bin"), &[1u8; 70_000]);
    write(&d.join("b.bin"), &[1u8; 70_000]);
    write(&d.join("c.bin"), &[2u8; 70_000]);
    let mut db = Db::open(&tmp.path().join("idx.db")).unwrap();
    scan(&mut db, &d);

    // recording a hash fails, as a write does when the disk is full or the lock can't be had
    db.conn
        .execute_batch(
            "CREATE TEMP TRIGGER no_room BEFORE UPDATE OF sample, sha256 ON files
             BEGIN SELECT RAISE(ABORT, 'no room'); END;",
        )
        .unwrap();
    assert!(hash_all(&mut db).is_err());
    assert!(
        db.conn.is_autocommit(),
        "the failed hash left a transaction open on the connection"
    );

    // the same connection carries on: a rescan, then hashing once writes work again
    db.conn.execute_batch("DROP TRIGGER no_room").unwrap();
    scan(&mut db, &d);
    let st = hash_all(&mut db).unwrap();
    assert_eq!(st.errors, 0);
    let unhashed: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM files WHERE current = 1 AND sha256 IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(unhashed, 0);
}
