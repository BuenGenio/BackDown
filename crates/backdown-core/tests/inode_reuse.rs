//! An inode number can belong to two different files over time: a file deleted from a drive that
//! hasn't been rescanned since still has a current row, and the filesystem may hand its inode
//! number to a new file elsewhere on it. Hashes are shared between rows of the same inode only when
//! they're the same file (the same size and modification time too, as hardlinks always are), so the
//! new file never inherits the dead one's SHA-256.
#![cfg(unix)]

use std::fs;
use std::path::Path;

use backdown_core::{Db, hash, index};
use sha2::{Digest, Sha256};

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn blob(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

fn scan(db: &mut Db, root: &Path) {
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
    .unwrap();
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

fn row(db: &Db, path: &Path) -> (i64, i64, Option<Vec<u8>>) {
    db.conn
        .query_row(
            "SELECT dev, ino, sha256 FROM files WHERE current = 1 AND path = ?1",
            [path.as_os_str().as_encoded_bytes()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
}

#[test]
fn a_new_file_never_inherits_a_dead_files_hash_through_a_reused_inode() {
    let tmp = tempfile::tempdir().unwrap();
    let (old, new) = (tmp.path().join("old"), tmp.path().join("new"));
    // the old drive: two copies of X, hashed
    let x = blob(1, 40_000);
    write(&old.join("x.bin"), &x);
    write(&old.join("x2.bin"), &x);
    // the new drive: two copies of Y, a different size
    let y = blob(2, 50_000);
    write(&new.join("y.bin"), &y);
    write(&new.join("y2.bin"), &y);

    let mut db = Db::open(&tmp.path().join("idx.db")).unwrap();
    scan(&mut db, &old);
    hash_all(&mut db);
    scan(&mut db, &new);

    // x.bin was deleted and its inode number went to y.bin; the old drive hasn't been rescanned,
    // so x.bin's row is still current (the inode is set by hand: a test can't make the
    // filesystem reuse one)
    let (dev, ino, _) = row(&db, &new.join("y.bin"));
    db.conn
        .execute(
            "UPDATE files SET dev = ?1, ino = ?2 WHERE current = 1 AND path = ?3",
            rusqlite::params![dev, ino, old.join("x.bin").as_os_str().as_encoded_bytes()],
        )
        .unwrap();

    hash_all(&mut db);
    let y_sha = Sha256::digest(&y).to_vec();
    let x_sha = Sha256::digest(&x).to_vec();
    assert_eq!(
        row(&db, &new.join("y.bin")).2,
        Some(y_sha.clone()),
        "y.bin got another file's hash"
    );
    assert_eq!(row(&db, &new.join("y2.bin")).2, Some(y_sha));
    assert_eq!(
        row(&db, &old.join("x.bin")).2,
        Some(x_sha),
        "x.bin's own hash is kept"
    );
}

#[test]
fn hardlinks_still_share_one_hash() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join("drive");
    let a = blob(3, 60_000);
    write(&d.join("a.bin"), &a);
    write(&d.join("copy.bin"), &a);
    fs::hard_link(d.join("a.bin"), d.join("link.bin")).unwrap();
    let mut db = Db::open(&tmp.path().join("idx.db")).unwrap();
    scan(&mut db, &d);
    hash_all(&mut db);
    let sha = Some(Sha256::digest(&a).to_vec());
    for n in ["a.bin", "link.bin", "copy.bin"] {
        assert_eq!(row(&db, &d.join(n)).2, sha, "{n}");
    }
    let (dev, ino, _) = row(&db, &d.join("a.bin"));
    assert_eq!(row(&db, &d.join("link.bin")).0, dev);
    assert_eq!(row(&db, &d.join("link.bin")).1, ino);
}
