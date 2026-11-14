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
            allow_other_drive: false,
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

/// Three duplicated groups of different sizes, and one file that isn't duplicated.
fn three_groups(d: &Path) {
    for (seed, len, copies) in [(11u8, 300_000, 3), (12, 200_000, 2), (13, 100_000, 2)] {
        for c in 0..copies {
            write(&d.join(format!("g{seed}/copy{c}.bin")), &big(seed, len));
        }
    }
    write(&d.join("solo.bin"), &big(14, 150_000));
}

#[test]
fn the_totals_cover_every_group_when_only_the_largest_are_listed() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join("drive");
    three_groups(&d);
    let mut db = Db::open(&tmp.path().join("idx.db")).unwrap();
    scan(&mut db, &d);
    hash_all(&mut db);
    let all = 2 * 300_000 + 200_000 + 100_000;
    for limit in [0, 1, 2, 10] {
        let r = report::dupes(&db, 1, limit).unwrap();
        assert_eq!(r.groups, 3, "limit {limit}");
        assert_eq!(r.reclaimable, all, "limit {limit}");
        assert_eq!(r.largest.len(), limit.min(3), "limit {limit}");
        let sizes: Vec<u64> = r.largest.iter().map(|g| g.size).collect();
        assert_eq!(
            sizes,
            [300_000, 200_000, 100_000][..limit.min(3)],
            "limit {limit}"
        );
    }
    // the size floor drops whole groups
    let r = report::dupes(&db, 150_000, 10).unwrap();
    assert_eq!((r.groups, r.reclaimable), (2, 2 * 300_000 + 200_000));
}

#[test]
fn duplicates_are_the_same_with_or_without_the_covering_index() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join("drive");
    three_groups(&d);
    let path = tmp.path().join("idx.db");
    let mut db = Db::open(&path).unwrap();
    scan(&mut db, &d);
    hash_all(&mut db);
    // opening doesn't build it (minutes on a big index): the daemon does, as a job
    assert!(!db.has_dupes_index().unwrap());
    let key = |r: &report::Dupes| {
        let groups: Vec<_> = r
            .largest
            .iter()
            .map(|g| (g.sha256.clone(), g.copies, g.reclaimable, g.paths.clone()))
            .collect();
        (r.groups, r.reclaimable, groups)
    };
    let without = key(&report::dupes(&db, 1, 10).unwrap());
    // read-only without it, like another machine's index through serve-stdio: still answers
    let ro = key(&report::dupes(&Db::open_read_only(&path).unwrap(), 1, 10).unwrap());
    assert_eq!(ro, without);
    assert!(db.ensure_dupes_index().unwrap());
    assert!(!db.ensure_dupes_index().unwrap(), "only built once");
    let with = key(&report::dupes(&db, 1, 10).unwrap());
    assert_eq!(with, without);
    assert_eq!(with.0, 3);
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
                allow_empty: false,
                allow_other_drive: false,
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
            allow_other_drive: false,
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
            allow_other_drive: false,
        },
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(st.removed, 1);
}

fn opts(allow_empty: bool) -> index::Options {
    index::Options {
        one_file_system: true,
        allow_empty,
        allow_other_drive: false,
    }
}

fn count(db: &Db, sql: &str) -> i64 {
    db.conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

const CURRENT: &str = "SELECT COUNT(*) FROM files WHERE current = 1";
const SCANS: &str = "SELECT COUNT(*) FROM scans";

#[test]
fn a_root_holding_folders_but_no_files_is_refused_unless_allowed() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join("media/me/BIG");
    write(&d.join("Archive/2019/a.jpg"), &big(5, 1000));
    write(&d.join("Archive/2019/b.jpg"), &big(6, 1000));
    let mut db = Db::open(&tmp.path().join("idx.db")).unwrap();
    scan(&mut db, &d);

    // the drive isn't mounted, and something made empty folders where it should be (a container
    // runtime making its bind mounts, say): not empty, but no files at all
    fs::remove_dir_all(d.join("Archive")).unwrap();
    for sub in ["etc", "data", "certs"] {
        fs::create_dir_all(d.join("Archive/_stalwart").join(sub)).unwrap();
    }
    let scans = count(&db, SCANS);
    for method in [index::Method::Walk, index::Method::Auto] {
        let err = index::scan(&mut db, &d, &opts(false), method, &mut |_| {}).unwrap_err();
        assert_eq!(
            err.downcast_ref::<index::Refused>().map(|r| r.why),
            Some(index::Refusal::Empty),
            "{err:#}"
        );
        let msg = err.to_string();
        assert!(msg.contains("holds no files at all") && msg.contains("--allow-empty"));
    }
    assert_eq!(count(&db, CURRENT), 2, "nothing was recorded as deleted");
    assert_eq!(count(&db, SCANS), scans, "nor any scan begun");
    assert!(db.conn.is_autocommit());

    // told explicitly that it really was emptied
    let st = index::index(&mut db, &d, &opts(true), &mut |_| {}).unwrap();
    assert_eq!((st.files, st.removed), (0, 2));
}

/// Makes `p` unreadable until dropped. None when that doesn't keep this process out (running as
/// root): the test has nothing to show then.
#[cfg(unix)]
struct Locked(std::path::PathBuf);

#[cfg(unix)]
impl Locked {
    fn new(p: &Path) -> Option<Locked> {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(p, fs::Permissions::from_mode(0o000)).unwrap();
        let l = Locked(p.to_path_buf());
        if fs::read_dir(p).is_ok() {
            eprintln!("skipped: permissions don't keep this user out (root?)");
            return None;
        }
        Some(l)
    }
}

#[cfg(unix)]
impl Drop for Locked {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
    }
}

#[cfg(unix)]
#[test]
fn a_root_that_cant_be_listed_records_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join("usb");
    write(&d.join("photos/a.jpg"), &big(5, 1000));
    write(&d.join("b.txt"), b"b");
    let mut db = Db::open(&tmp.path().join("idx.db")).unwrap();
    scan(&mut db, &d);
    let scans = count(&db, SCANS);
    let Some(_locked) = Locked::new(&d) else {
        return;
    };
    // not even when told it may be empty: a folder that can't be read says nothing of what's in it
    for allow_empty in [false, true] {
        let err = index::index(&mut db, &d, &opts(allow_empty), &mut |_| {}).unwrap_err();
        assert_eq!(
            err.downcast_ref::<index::Refused>().map(|r| r.why),
            Some(index::Refusal::Unreadable),
            "{err:#}"
        );
        assert!(err.to_string().contains("can't list"), "{err:#}");
    }
    assert_eq!(count(&db, CURRENT), 2);
    assert_eq!(count(&db, SCANS), scans);
}

#[cfg(unix)]
#[test]
fn an_unreadable_folder_keeps_what_it_held() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join("usb");
    write(&d.join("ok/a.txt"), b"a");
    write(&d.join("ok/gone.txt"), b"deleted later");
    write(&d.join("locked/b.txt"), b"bb");
    write(&d.join("locked/sub/c.txt"), b"ccc");
    let mut db = Db::open(&tmp.path().join("idx.db")).unwrap();
    let first = scan(&mut db, &d);
    assert_eq!(first.files, 4);

    fs::remove_file(d.join("ok/gone.txt")).unwrap();
    let Some(locked) = Locked::new(&d.join("locked")) else {
        return;
    };
    let st = scan(&mut db, &d);
    assert_eq!(
        (st.files, st.kept, st.removed),
        (3, 2, 1),
        "only what really went is gone: {st:?}"
    );
    assert!(st.errors >= 1, "the folder that couldn't be read is noted");
    assert!(
        st.first_errors.iter().any(|e| e.contains("locked")),
        "{:?}",
        st.first_errors
    );
    let kept: Vec<String> = db
        .conn
        .prepare("SELECT path FROM files WHERE current = 1 AND last_scan = ?1 ORDER BY path")
        .unwrap()
        .query_map([st.scan], |r| r.get::<_, Vec<u8>>(0))
        .unwrap()
        .map(|p| String::from_utf8(p.unwrap()).unwrap())
        .filter(|p| p.contains("/locked/"))
        .collect();
    assert_eq!(kept.len(), 2, "{kept:?}");
    // the folders' totals still count what's in it
    let files_in = |p: &Path| -> i64 {
        let p = fs::canonicalize(p).unwrap();
        db.conn
            .query_row(
                "SELECT files FROM dirs WHERE path = ?1 AND current = 1",
                [p.to_string_lossy().as_bytes()],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert_eq!(files_in(&d.join("locked")), 2);
    assert_eq!(files_in(&d), 3);

    // readable again: the next scan sees it all, and nothing changed
    drop(locked);
    let st = scan(&mut db, &d);
    assert_eq!((st.files, st.kept, st.new, st.removed), (3, 0, 0, 0));
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

#[test]
fn the_duplicates_view_filters_by_size_type_date_and_hidden_names() {
    use backdown_core::browse;
    use report::DupesFilter;
    use std::collections::HashSet;
    use std::time::{Duration, SystemTime};

    // the temp folder is itself named ".tmp…": only names below the drive's top make a path hidden
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join("drive");
    write(&d.join("photos/a.jpg"), &big(21, 300_000));
    write(&d.join(".cache/thumbs/a.jpg"), &big(21, 300_000));
    // only in hidden places
    write(&d.join(".trash/v.mp4"), &big(22, 200_000));
    write(&d.join("x/.old/v.mp4"), &big(22, 200_000));
    // each path's own name counts: "report.pdf~" is hidden, and its extension is "pdf~"
    write(&d.join("docs/report.pdf"), &big(23, 100_000));
    write(&d.join("docs/report.pdf~"), &big(23, 100_000));
    // a name without a dot is its own extension, as the browser's icons.js has it
    write(&d.join("docs/README"), &big(24, 120_000));
    write(&d.join("old/README"), &big(24, 120_000));
    let long_ago = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    fs::File::options()
        .write(true)
        .open(d.join("old/README"))
        .unwrap()
        .set_modified(long_ago)
        .unwrap();
    #[cfg(unix)]
    {
        // two names of one inode and a real copy: two copies, three paths
        write(&d.join("links/one.bin"), &big(25, 150_000));
        fs::hard_link(d.join("links/one.bin"), d.join("links/two.bin")).unwrap();
        write(&d.join("links/three.bin"), &big(25, 150_000));
    }
    let mut db = Db::open(&tmp.path().join("idx.db")).unwrap();
    scan(&mut db, &d);
    hash_all(&mut db);
    let groups = if cfg!(unix) { 5 } else { 4 };
    // as the index has it (no `\\?\` on Windows), and read with `/` between names
    let top = backdown_core::paths::canonical(&d)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let listed = |r: &report::Dupes| -> HashSet<String> {
        r.largest
            .iter()
            .flat_map(|g| g.files.iter().filter(|f| f.listed))
            .map(|f| {
                f.path
                    .strip_prefix(top.as_str())
                    .unwrap()
                    .replace('\\', "/")
            })
            .collect()
    };
    let run = |f: &DupesFilter| report::dupes_listed(&db, 1, f, 10).unwrap();

    // no filter: every group and path, with no second set of totals
    let all = run(&DupesFilter::default());
    assert_eq!(all.groups, groups);
    assert!(all.unfiltered.is_none());
    assert!(all.largest.iter().all(|g| g.files.iter().all(|f| f.listed)));
    let total = all.reclaimable;

    // hidden names left out: a group shows when one path is visible, and lists only those
    let r = run(&DupesFilter {
        no_hidden: true,
        ..Default::default()
    });
    assert_eq!(r.groups, groups - 1, "the video is only in hidden folders");
    assert_eq!(r.only_hidden, 1);
    let u = r.unfiltered.as_ref().unwrap();
    assert_eq!((u.groups, u.reclaimable), (groups, total));
    let shown = listed(&r);
    assert!(shown.contains("/photos/a.jpg") && !shown.contains("/.cache/thumbs/a.jpg"));
    assert!(shown.contains("/docs/report.pdf") && !shown.contains("/docs/report.pdf~"));
    let photo = r.largest.iter().find(|g| g.size == 300_000).unwrap();
    assert_eq!(
        (photo.copies, photo.files.len()),
        (2, 2),
        "hidden copies still count"
    );

    // a type, as extensions
    let exts = |list: &[&str], except: bool| DupesFilter {
        exts: Some(list.iter().map(|e| e.to_string()).collect()),
        except,
        ..Default::default()
    };
    let r = run(&exts(&["jpg", "png"], false));
    assert_eq!((r.groups, r.reclaimable), (1, 300_000));
    let r = run(&exts(&["readme"], false));
    assert_eq!(
        listed(&r),
        HashSet::from(["/docs/README".into(), "/old/README".into()])
    );
    // "other files": none of the known extensions
    let r = run(&exts(&["jpg", "mp4", "pdf", "bin"], true));
    assert_eq!(
        listed(&r),
        HashSet::from([
            "/docs/README".into(),
            "/old/README".into(),
            "/docs/report.pdf~".into()
        ])
    );

    // dates are each path's own
    let r = run(&DupesFilter {
        modified_before: Some(1_000_000_001),
        ..Default::default()
    });
    assert_eq!(listed(&r), HashSet::from(["/old/README".into()]));
    assert_eq!(
        r.largest[0].files.len(),
        2,
        "the newer copy is there, not listed"
    );

    // sizes keep or drop a group whole
    let r = run(&DupesFilter {
        size_from: 150_001,
        size_below: Some(300_000),
        ..Default::default()
    });
    assert_eq!((r.groups, r.largest.len()), (1, 1));
    assert_eq!(r.largest[0].size, 200_000);

    // what the browser picks a path by: its key, its folder's, and which distinct copy it is
    for f in all.largest.iter().flat_map(|g| &g.files) {
        let p = browse::path_of(&f.key).unwrap();
        assert_eq!(p.display().to_string(), f.path);
        assert_eq!(browse::path_of(&f.dir).unwrap(), p.parent().unwrap());
    }
    #[cfg(unix)]
    {
        let links = all.largest.iter().find(|g| g.size == 150_000).unwrap();
        let copy = |name: &str| {
            links
                .files
                .iter()
                .find(|f| f.path.ends_with(name))
                .unwrap()
                .copy
        };
        assert_eq!((links.copies, links.files.len()), (2, 3));
        assert_eq!(copy("one.bin"), copy("two.bin"));
        assert_ne!(copy("one.bin"), copy("three.bin"));
    }
}
