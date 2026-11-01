//! End-to-end: wiping only what's provably kept, moving what isn't, leaving what changed.

use std::fs;
use std::path::Path;

use backdown_core::{Db, hash, index, wipe};

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn blob(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

fn scan_all(db: &mut Db, drives: &[&Path]) {
    for d in drives {
        index::index(
            db,
            d,
            &index::Options {
                one_file_system: true,
                allow_empty: false,
            },
            &mut |_| {},
        )
        .unwrap();
    }
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

fn count(db: &Db, sql: &str) -> i64 {
    db.conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn wipe_deletes_only_what_is_kept_and_moves_the_rest() {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let old = tmp.path().join("media/OLD-USB");
    let keep = tmp.path().join("media/ARCHIVE");
    let rescued = tmp.path().join("media/ARCHIVE/Rescued");
    // kept elsewhere
    write(&old.join("photos/a.jpg"), &blob(1, 40_000));
    write(&keep.join("Photos/a.jpg"), &blob(1, 40_000));
    write(&old.join("photos/b.jpg"), &blob(2, 40_000));
    write(&keep.join("Photos/b.jpg"), &blob(2, 40_000));
    // two copies, both inside the selection: that's no copy at all
    write(&old.join("docs/x.pdf"), &blob(3, 30_000));
    write(&old.join("docs/copy of x.pdf"), &blob(3, 30_000));
    // only here
    write(&old.join("docs/thesis.odt"), &blob(4, 25_000));
    write(&old.join("empty.txt"), b"");
    // changed after the scan
    write(&old.join("notes/todo.txt"), &blob(5, 1_000));
    let mut db = Db::open(&tmp.path().join("i.db")).unwrap();
    scan_all(&mut db, &[&old, &keep]);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    write(&old.join("notes/todo.txt"), &blob(6, 1_000));

    let sel = vec![
        old.join("photos"),
        old.join("docs"),
        old.join("notes"),
        old.join("empty.txt"),
    ];
    let a = wipe::assess(&db, &sel, None).unwrap();
    let photos = &a.items[0];
    assert_eq!((photos.kept_files, photos.only_files), (2, 0));
    assert!(
        photos.copies[0].path.ends_with("ARCHIVE/Photos"),
        "{:?}",
        photos.copies
    );
    let docs = &a.items[1];
    assert_eq!(
        (docs.kept_files, docs.only_files),
        (0, 3),
        "copies inside the selection don't count"
    );
    assert_eq!(a.confirm, "OLD-USB");
    assert!(
        wipe::assess(&db, &[Path::new("/usr").to_path_buf()], None)
            .unwrap()
            .items[0]
            .refused
            .is_some()
    );

    // first without a place to move things: only the kept photos (and the empty file) go
    let id = wipe::record(&db, &sel, None).unwrap();
    let st = wipe::wipe(&mut db, id, false, None, &mut |_, _| {}).unwrap();
    assert_eq!(
        (st.deleted, st.moved, st.left_no_copy, st.left_changed),
        (3, 0, 3, 1),
        "{st:?}"
    );
    assert!(!old.join("photos").exists(), "an emptied folder is removed");
    assert!(old.join("docs/thesis.odt").exists() && old.join("notes/todo.txt").exists());
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM deletions WHERE proof_how = 're-read'"
        ),
        2
    );

    // a copy that rotted since the scan is not a copy
    write(&old.join("more/c.jpg"), &blob(7, 40_000));
    write(&keep.join("Photos/c.jpg"), &blob(7, 40_000));
    scan_all(&mut db, &[&old, &keep]);
    let mut rotten = blob(7, 40_000);
    rotten[5] ^= 1;
    fs::write(keep.join("Photos/c.jpg"), &rotten).unwrap();
    let id = wipe::record(&db, &[old.join("more")], None).unwrap();
    let st = wipe::wipe(&mut db, id, false, None, &mut |_, _| {}).unwrap();
    assert_eq!((st.deleted, st.left_no_copy), (0, 1), "{st:?}");
    assert!(old.join("more/c.jpg").exists());

    // with a place to move what exists only here: those are moved, verified, then deleted
    scan_all(&mut db, &[&old, &keep]);
    let id = wipe::record(&db, &[old.join("docs"), old.join("more")], Some(&rescued)).unwrap();
    let st = wipe::wipe(&mut db, id, false, None, &mut |_, _| {}).unwrap();
    assert_eq!((st.deleted, st.moved, st.failed), (4, 4, 0), "{st:?}");
    assert_eq!(
        fs::read(rescued.join("OLD-USB/docs/thesis.odt")).unwrap(),
        blob(4, 25_000)
    );
    assert_eq!(
        fs::read(rescued.join("OLD-USB/docs/copy of x.pdf")).unwrap(),
        blob(3, 30_000)
    );
    assert!(!old.join("docs").exists());
    assert!(old.exists(), "the drive's own folder stays");
    let (state, summary): (String, String) = db
        .conn
        .query_row(
            "SELECT state, summary FROM wipes WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, "done", "{summary}");
}

#[test]
fn trusting_the_index_still_needs_the_copy_to_be_there() {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let a = tmp.path().join("media/STICK");
    let b = tmp.path().join("media/ARCHIVE");
    write(&a.join("p.jpg"), &blob(1, 20_000));
    write(&b.join("p.jpg"), &blob(1, 20_000));
    let mut db = Db::open(&tmp.path().join("i.db")).unwrap();
    scan_all(&mut db, &[&a, &b]);
    // the stick goes, and the index hasn't seen it yet: its row still says "a copy is here"
    fs::remove_file(a.join("p.jpg")).unwrap();
    let id = wipe::record(&db, std::slice::from_ref(&b), None).unwrap();
    let st = wipe::wipe(&mut db, id, true, None, &mut |_, _| {}).unwrap();
    assert_eq!((st.deleted, st.left_no_copy), (0, 1));
    assert!(b.join("p.jpg").exists(), "the last copy stays");
}
