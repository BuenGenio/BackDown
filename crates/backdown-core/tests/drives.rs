//! End-to-end: two "drives" as temp directories, indexed, hashed, and checked.

use std::fs;
use std::path::Path;

use backdown_core::{Db, hash, index, report};

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn big(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

fn scan(db: &mut Db, root: &Path) -> index::Stats {
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
}

fn hash_all(db: &mut Db) -> hash::Stats {
    hash::hash(
        db,
        &hash::Options {
            jobs_per_device: 2,
            retry_errors: false,
        },
        &mut |_| {},
    )
    .unwrap()
}

#[test]
fn old_drive_is_safe_once_everything_is_copied() {
    let tmp = tempfile::tempdir().unwrap();
    let (old, archive) = (tmp.path().join("old"), tmp.path().join("archive"));
    let photo = big(1, 400_000); // larger than a sample: exercises all three tiers
    write(&old.join("Photos/2019/a.jpg"), &photo);
    write(&old.join("notes.txt"), b"small but duplicated");
    write(&old.join("only-here/thesis.pdf"), &big(2, 300_000));
    write(&old.join("empty.lock"), b"");
    write(&archive.join("Photos/a.jpg"), &photo);
    write(&archive.join("misc/notes.txt"), b"small but duplicated");
    // same size and same head/middle/tail as the thesis, different somewhere else: only a full hash can tell
    let mut near = big(2, 300_000);
    near[100_000] ^= 0xff;
    write(&archive.join("misc/near-thesis.pdf"), &near);

    let mut db = Db::open(&tmp.path().join("idx.db")).unwrap();
    scan(&mut db, &old);
    scan(&mut db, &archive);
    let st = hash_all(&mut db);
    assert!(
        st.hashed >= 2,
        "the near-identical pair needs full hashes: {st:?}"
    );

    let c = report::check(&db, &old).unwrap();
    assert!(!c.safe);
    assert_eq!(c.verified.files, 2);
    assert_eq!(c.unique.files, 1);
    assert_eq!(c.unique.bytes, 300_000);
    assert_eq!(c.empty, 1);
    assert_eq!(c.unique_by_dir[0].dir, "only-here");

    // copy the thesis over; after a re-index the old drive is safe
    fs::copy(old.join("only-here/thesis.pdf"), archive.join("thesis.pdf")).unwrap();
    scan(&mut db, &archive);
    hash_all(&mut db);
    let c = report::check(&db, &old).unwrap();
    assert!(c.safe, "{c:?}");
}

#[test]
fn duplicates_ignore_hardlinks_and_count_reclaimable_space() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join("drive");
    let data = big(7, 250_000);
    write(&d.join("a/x.bin"), &data);
    write(&d.join("b/x.bin"), &data);
    write(&d.join("c/x.bin"), &data);
    write(&d.join("solo.bin"), &big(8, 250_000));

    #[cfg(unix)]
    {
        write(&d.join("linked/one.bin"), &big(9, 250_000));
        fs::hard_link(d.join("linked/one.bin"), d.join("linked/two.bin")).unwrap();
    }

    let mut db = Db::open(&tmp.path().join("idx.db")).unwrap();
    scan(&mut db, &d);
    hash_all(&mut db);
    let dupes = report::dupes(&db, 1, 10).unwrap();
    assert_eq!(
        dupes.groups, 1,
        "hardlinks are one file, not a duplicate: {dupes:?}"
    );
    assert_eq!(dupes.largest[0].copies, 3);
    assert_eq!(dupes.reclaimable, 2 * 250_000);
}

#[cfg(unix)]
#[test]
fn a_hardlink_is_not_a_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join("drive");
    write(&d.join("src/data.bin"), &big(3, 250_000));
    fs::create_dir_all(d.join("backup")).unwrap();
    fs::hard_link(d.join("src/data.bin"), d.join("backup/data.bin")).unwrap();

    let mut db = Db::open(&tmp.path().join("idx.db")).unwrap();
    scan(&mut db, &d);
    hash_all(&mut db);
    let c = report::check(&db, &d.join("src")).unwrap();
    assert!(!c.safe, "the only other copy is the same inode");
    assert_eq!(c.unique.files, 1);
}

#[test]
fn rescans_keep_history_and_hashes() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join("drive");
    write(&d.join("keep.txt"), b"unchanged");
    write(&d.join("edit.txt"), b"version one");
    write(&d.join("gone.txt"), b"deleted later");

    let mut db = Db::open(&tmp.path().join("idx.db")).unwrap();
    let first = scan(&mut db, &d);
    assert_eq!(first.new, 3);

    write(&d.join("edit.txt"), b"version two, longer");
    fs::remove_file(d.join("gone.txt")).unwrap();
    let second = scan(&mut db, &d);
    assert_eq!((second.new, second.changed, second.removed), (0, 1, 1));

    let status = db.status().unwrap();
    assert_eq!(status.files, 2);
    assert_eq!(
        status.versions, 2,
        "the old edit.txt and gone.txt are kept as history"
    );
    assert_eq!(report::scans(&db).unwrap().len(), 2);

    // an index of an overlapping root is refused
    assert!(
        index::index(
            &mut db,
            tmp.path(),
            &index::Options {
                one_file_system: true,
                allow_empty: false
            },
            &mut |_| {}
        )
        .is_err()
    );
}

#[test]
fn an_emptied_root_is_refused_unless_allowed() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join("mnt/usb");
    write(&d.join("photos/a.jpg"), &big(5, 1000));
    let mut db = Db::open(&tmp.path().join("idx.db")).unwrap();
    scan(&mut db, &d);

    // the drive is unplugged: its mount point is still there, but empty
    fs::remove_dir_all(d.join("photos")).unwrap();
    let err = index::index(
        &mut db,
        &d,
        &index::Options {
            one_file_system: true,
            allow_empty: false,
        },
        &mut |_| {},
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("unplugged"), "{err}");
    assert_eq!(
        db.status().unwrap().files,
        1,
        "nothing was recorded as deleted"
    );

    // told explicitly that it really was emptied
    let st = index::index(
        &mut db,
        &d,
        &index::Options {
            one_file_system: true,
            allow_empty: true,
        },
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(st.removed, 1);
}

#[test]
fn an_unplugged_drive_waits_instead_of_turning_unreadable() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join("usb");
    write(&d.join("a.bin"), &big(1, 70_000));
    write(&d.join("b.bin"), &big(2, 70_000)); // same size: needs hashing
    let mut db = Db::open(&tmp.path().join("i.db")).unwrap();
    scan(&mut db, &d);
    fs::rename(&d, tmp.path().join("elsewhere")).unwrap();
    let st = hash_all(&mut db);
    assert_eq!((st.sampled, st.errors, st.absent), (0, 0, 2), "{st:?}");
    let unreadable: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM files WHERE error IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        unreadable, 0,
        "nothing is marked unreadable while the drive is away"
    );
    fs::rename(tmp.path().join("elsewhere"), &d).unwrap();
    let st = hash_all(&mut db);
    assert_eq!((st.sampled, st.absent), (2, 0), "{st:?}");
}
