//! Importing the pre-Rust scripts' index. The legacy files are written here the way bd-index.sh
//! (`find -printf '%s\t%T@\t%p\n'`), bd-plan.py and bd-hash.py wrote them, for a real directory
//! tree "crawled" while it was mounted somewhere else; after the import, a real `index` of the tree
//! must find nothing new or changed and keep the imported hashes.
#![cfg(unix)]

use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use backdown_core::{Db, browse, fmt, hash, index, legacy, report};
use rusqlite::params;
use sha2::{Digest, Sha256};

/// Where the drive was mounted when the scripts crawled it (invented; it doesn't exist here).
const OLD_MOUNT: &str = "/mnt/old-usb-disk";
const CRAWL_START: u64 = 1_780_000_000;
const CRAWL_END: u64 = CRAWL_START + 3_600;

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn big(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

fn touch(path: &Path, t: SystemTime) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

fn at(secs: u64, nanos: u32) -> SystemTime {
    UNIX_EPOCH + Duration::new(secs, nanos)
}

/// `%T@` as GNU find prints it: seconds, a dot, nanoseconds and a trailing 0. Before 1970 it
/// prints the (negative) whole seconds and the positive nanoseconds: -1.5 s is "-2.5000000000".
fn find_mtime(m: &fs::Metadata) -> String {
    format!("{}.{:09}0", m.mtime(), m.mtime_nsec())
}

/// One line of `find DIR -xdev -type f -printf '%s\t%T@\t%p\n'`, as if `drive` were mounted at
/// `mount`: raw bytes, nothing escaped.
fn find_line(drive: &Path, mount: &str, file: &Path) -> Vec<u8> {
    let m = fs::symlink_metadata(file).unwrap();
    let mut line = format!("{}\t{}\t{mount}/", m.len(), find_mtime(&m)).into_bytes();
    line.extend_from_slice(file.strip_prefix(drive).unwrap().as_os_str().as_bytes());
    line.push(b'\n');
    line
}

/// Regular files (`find -type f`: no symlinks) in `dir`, or below it.
fn files_in(dir: &Path, recursive: bool) -> Vec<PathBuf> {
    walkdir::WalkDir::new(dir)
        .min_depth(1)
        .max_depth(if recursive { usize::MAX } else { 1 })
        .sort_by_file_name()
        .into_iter()
        .map(|e| e.unwrap())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .collect()
}

/// Leave `drive` in `index/<vol>` as bd-index.sh would have, crawled while mounted at `mount`: the
/// plan (`F` the root's own files, then `R` each top folder), a finished chunk per plan line
/// (`.path`, `.tsv`, `.done`) and the merged `<vol>.tsv`, all dated from the crawl.
fn crawl(index: &Path, vol: &str, drive: &Path, mount: &str) {
    let chunks = index.join(vol).join("chunks");
    fs::create_dir_all(&chunks).unwrap();
    let mut plan = vec![("F", drive.to_path_buf())];
    let mut tops: Vec<PathBuf> = fs::read_dir(drive)
        .unwrap()
        .map(|e| e.unwrap())
        .filter(|e| e.file_type().unwrap().is_dir())
        .map(|e| e.path())
        .collect();
    tops.sort();
    plan.extend(tops.into_iter().map(|d| ("R", d)));

    let (mut list, mut merged) = (Vec::new(), Vec::new());
    for (n, (kind, dir)) in plan.iter().enumerate() {
        let mut line = format!("{kind}\t{mount}").into_bytes();
        if dir != drive {
            line.push(b'/');
            line.extend_from_slice(dir.strip_prefix(drive).unwrap().as_os_str().as_bytes());
        }
        line.push(b'\n');
        list.extend_from_slice(&line);
        let tsv: Vec<u8> = files_in(dir, *kind == "R")
            .iter()
            .flat_map(|f| find_line(drive, mount, f))
            .collect();
        let id = chunks.join(format!("{n:016x}"));
        for (ext, body, t) in [
            ("path", &line, CRAWL_START),
            ("tsv", &tsv, CRAWL_END - 60),
            ("done", &Vec::new(), CRAWL_END),
        ] {
            let f = id.with_extension(ext);
            fs::write(&f, body).unwrap();
            touch(&f, at(t, 0));
        }
        merged.extend_from_slice(&tsv);
    }
    fs::write(index.join(vol).join("chunks.list"), list).unwrap();
    let m = index.join(format!("{vol}.tsv"));
    fs::write(&m, merged).unwrap();
    touch(&m, at(CRAWL_END + 60, 0));
}

/// bd-hash.py's hashes.db, with its schema.
fn hashes_db(path: &Path, rows: &[(String, u64, f64, String)]) {
    let c = rusqlite::Connection::open(path).unwrap();
    c.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE IF NOT EXISTS h(path TEXT PRIMARY KEY, size INTEGER, mtime REAL, sha256 TEXT, hashed_at REAL);
         CREATE INDEX IF NOT EXISTS h_sha ON h(sha256);
         CREATE INDEX IF NOT EXISTS h_size ON h(size);",
    )
    .unwrap();
    for (p, s, m, h) in rows {
        c.execute(
            "INSERT OR REPLACE INTO h VALUES (?1, ?2, ?3, ?4, ?5)",
            params![p, *s as i64, m, h, 1.78e9],
        )
        .unwrap();
    }
}

/// The row bd-hash.py wrote for `rel` (`--from-tsv`: mtime is `float()` of the TSV's text).
fn hashed(drive: &Path, rel: &str) -> (String, u64, f64, String) {
    let f = drive.join(rel);
    let m = fs::metadata(&f).unwrap();
    (
        format!("{OLD_MOUNT}/{rel}"),
        m.len(),
        find_mtime(&m).parse().unwrap(),
        fmt::hex(&Sha256::digest(fs::read(&f).unwrap())),
    )
}

fn options(drive: &Path, hashes: Option<PathBuf>) -> legacy::Options {
    legacy::Options {
        hashes,
        maps: vec![(PathBuf::from(OLD_MOUNT), drive.to_path_buf())],
        volumes: Vec::new(),
        replace: false,
        dry_run: false,
    }
}

fn import(db: &mut Db, index: &Path, opts: &legacy::Options) -> Result<legacy::Report, String> {
    legacy::import(db, index, opts, &mut |_| {}).map_err(|e| format!("{e:#}"))
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

fn count(db: &Db, sql: &str) -> i64 {
    db.conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

/// A drive with duplicates, near-duplicates, awkward names and awkward times.
fn make_drive(root: &Path) -> PathBuf {
    let copy = big(1, 300_000); // larger than a sample, so only a full hash can match it
    write(&root.join("a/dup-1.bin"), &copy);
    write(&root.join("b/dup-2.bin"), &copy);
    let mut near = copy.clone();
    near[100_000] ^= 0xff;
    write(&root.join("c/near.bin"), &near);
    write(&root.join("c/twin-new.bin"), &big(2, 250_000));
    write(&root.join("d/twin-old.bin"), &big(2, 250_000));
    write(
        &root.join("notes/small.txt"),
        b"a small file, hashed by bd-hash.py",
    );
    write(&root.join("notes/stale.txt"), b"hashed, then edited");
    write(&root.join("weird/tab\there.txt"), b"a tab in the name");
    write(&root.join("weird/new\nline.txt"), b"a newline in the name");
    #[cfg(target_os = "linux")]
    write(
        &root.join(std::ffi::OsStr::from_bytes(b"weird/caf\xe9.txt")),
        b"not UTF-8",
    );
    write(&root.join("old/1969.txt"), b"from before the epoch");
    write(&root.join("top.txt"), b"directly in the root");
    write(&root.join("empty.lock"), b"");
    fs::create_dir_all(root.join("emptydir")).unwrap();
    std::os::unix::fs::symlink("top.txt", root.join("link-to-top")).unwrap();

    for (i, f) in files_in(root, true).iter().enumerate() {
        touch(
            f,
            at(
                1_700_000_000 + 1_000 * i as u64,
                123_456_789 * (i as u32 % 8),
            ),
        );
    }
    touch(
        &root.join("old/1969.txt"),
        UNIX_EPOCH - Duration::from_millis(1_500),
    );
    dunce::canonicalize(root).unwrap()
}

#[test]
fn an_import_is_indistinguishable_from_a_scan() {
    let tmp = tempfile::tempdir().unwrap();
    let drive = make_drive(&tmp.path().join("drive"));
    let idx = tmp.path().join("index");
    crawl(&idx, "usbdisk", &drive, OLD_MOUNT);
    let files = files_in(&drive, true).len() as u64;

    // the real world: a line that isn't a record, a record from elsewhere, stray TSVs
    let merged = idx.join("usbdisk.tsv");
    let mut tsv = b"a line that is not a record\n".to_vec();
    tsv.extend(fs::read(&merged).unwrap());
    tsv.extend(b"7\t1600000000.0000000000\t/somewhere/else.txt\n");
    fs::write(&merged, tsv).unwrap();
    touch(&merged, at(CRAWL_END + 60, 0));
    fs::write(idx.join("verified.tsv"), b"/x\t/y\n").unwrap();

    // hashed: both copies, one twin, a small file; a file edited since; files not in the crawl
    let mut stale = hashed(&drive, "notes/stale.txt");
    stale.2 -= 100.0;
    hashes_db(
        &idx.join("hashes.db"),
        &[
            hashed(&drive, "a/dup-1.bin"),
            hashed(&drive, "b/dup-2.bin"),
            hashed(&drive, "d/twin-old.bin"),
            hashed(&drive, "notes/small.txt"),
            stale,
            (format!("{OLD_MOUNT}/gone.bin"), 5, 1.7e9, "ab".repeat(32)),
            ("/mnt/another-disk/x.bin".into(), 5, 1.7e9, "cd".repeat(32)),
        ],
    );

    let mut db = Db::open(&tmp.path().join("idx.db")).unwrap();
    let opts = options(&drive, Some(idx.join("hashes.db")));
    let r = import(&mut db, &idx, &opts).unwrap();
    assert_eq!(r.ignored, ["verified.tsv"]);
    let [v] = &r.volumes[..] else { panic!("{r:?}") };
    assert_eq!(v.outcome, legacy::Outcome::Imported);
    assert_eq!(v.source, "usbdisk.tsv");
    assert_eq!((v.root.as_path(), v.mounted), (drive.as_path(), true));
    assert_eq!((v.chunks_planned, v.chunks_unfinished), (9, 0));
    let t = &v.tally;
    assert_eq!(t.files, files);
    assert_eq!(
        (
            t.malformed,
            t.outside_root,
            t.rejoined,
            t.duplicates,
            t.truncated
        ),
        (1, 1, 1, 0, 0)
    );
    let h = r.hashes.as_ref().unwrap();
    assert_eq!(
        (h.rows, h.attached, h.stale, h.unmatched, h.conflicting),
        (7, 4, 1, 2, 0)
    );
    // the scan carries the crawl's own dates
    let s = &report::scans(&db).unwrap()[0];
    assert_eq!(
        (s.started_at, s.finished_at, s.files),
        (CRAWL_START as i64, CRAWL_END as i64, files)
    );

    // the duplicate is known before anything is read: only the imported hashes reveal it
    let d = report::dupes(&db, 1, 10).unwrap();
    assert_eq!(d.groups, 1, "{d:?}");
    assert_eq!((d.largest[0].copies, d.largest[0].size), (2, 300_000));
    assert!(report::check(&db, &drive.join("a")).unwrap().safe);
    let small: Option<Vec<u8>> = db
        .conn
        .query_row(
            "SELECT sample FROM files WHERE CAST(path AS TEXT) LIKE '%/notes/small.txt'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(small.is_some(), "a small file's sample is its full hash");

    // now the real thing: nothing is new or changed, no folder's totals differ
    let st = scan(&mut db, &drive);
    assert_eq!(
        (st.files, st.new, st.changed, st.removed, st.errors),
        (files, 0, 0, 0, 0)
    );
    assert_eq!(count(&db, "SELECT COUNT(*) FROM dirs WHERE current = 0"), 0);
    let new_dirs: Vec<Vec<u8>> = db
        .conn
        .prepare("SELECT path FROM dirs WHERE first_scan = ?1")
        .unwrap()
        .query_map([st.scan], |r| r.get(0))
        .unwrap()
        .map(|p| p.unwrap())
        .collect();
    assert_eq!(
        new_dirs,
        [drive.join("emptydir").as_os_str().as_bytes()],
        "find never listed empty folders"
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM files WHERE current = 1 AND sha256 IS NOT NULL"
        ),
        4,
        "the imported hashes survive the scan"
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM files WHERE current = 1 AND ino IS NULL"
        ),
        0
    );

    // hashing reads only what's missing: never a file whose full hash is known
    let hs = hash::hash(
        &mut db,
        &hash::Options {
            jobs_per_device: 1,
            retry_errors: false,
        },
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(hs.errors, 0);
    assert!(hs.hashed >= 2, "near.bin and twin-new.bin: {hs:?}");
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM files WHERE current = 1 AND sample IS NULL AND sha256 IS NOT NULL"
        ),
        3,
        "dup-1, dup-2 and twin-old were never sampled"
    );
    let d = report::dupes(&db, 1, 10).unwrap();
    assert_eq!(d.groups, 2, "{d:?}");
    assert!(
        d.largest
            .iter()
            .all(|g| g.paths.iter().all(|p| !p.ends_with("near.bin")))
    );

    // the same data again: nothing duplicated, nothing rescanned
    let r = import(&mut db, &idx, &opts).unwrap();
    assert_eq!(r.volumes[0].outcome, legacy::Outcome::AlreadyImported);
    let h = r.hashes.unwrap();
    assert_eq!((h.attached, h.already, h.stale), (0, 4, 1));
    let status = db.status().unwrap();
    assert_eq!((status.files, status.scans, status.versions), (files, 2, 0));
}

#[test]
fn an_interrupted_import_resumes() {
    let tmp = tempfile::tempdir().unwrap();
    let idx = tmp.path().join("index");
    let mount = "/mnt/legacy-archive";
    let chunks = idx.join("archive/chunks");
    fs::create_dir_all(&chunks).unwrap();
    let mut plan = format!("F\t{mount}\n");
    // two finished chunks of 30 and 15 folders, and one the crawl never finished
    for (n, folders) in [(0u32, 0..30), (1, 30..45)] {
        let mut tsv = String::new();
        for d in folders.clone() {
            for f in 0..1_000 {
                tsv += &format!("{}\t1700000000.5000000000\t{mount}/d{d}/f{f}\n", f + 1);
            }
        }
        let line = format!("R\t{mount}/part{n}\n");
        plan += &line;
        fs::write(chunks.join(format!("{n:016x}.tsv")), tsv).unwrap();
        fs::write(chunks.join(format!("{n:016x}.path")), line).unwrap();
        fs::write(chunks.join(format!("{n:016x}.done")), "").unwrap();
    }
    plan += &format!("R\t{mount}/never-finished\n");
    fs::write(
        chunks.join("00000000000000ff.tsv.tmp"),
        "1\t1.0\t/partial\n",
    )
    .unwrap();
    fs::write(idx.join("archive/chunks.list"), plan).unwrap();

    let db_path = tmp.path().join("idx.db");
    let opts = legacy::Options {
        hashes: None,
        maps: Vec::new(),
        volumes: Vec::new(),
        replace: false,
        dry_run: false,
    };
    let mut db = Db::open(&db_path).unwrap();
    let killed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        legacy::import(&mut db, &idx, &opts, &mut |p| {
            assert!(p.files < 20_000, "simulated kill after the first batch");
        })
    }));
    assert!(killed.is_err());
    drop(db);

    let mut db = Db::open(&db_path).unwrap();
    assert_eq!(
        db.status().unwrap().files,
        20_000,
        "the first batch was kept"
    );
    assert!(report::scans(&db).unwrap().is_empty(), "no scan shows yet");
    let r = import(&mut db, &idx, &opts).unwrap();
    let v = &r.volumes[0];
    assert_eq!(v.outcome, legacy::Outcome::Resumed { from_files: 20_000 });
    assert_eq!(v.source, "2 finished chunks");
    assert_eq!((v.chunks_planned, v.chunks_unfinished), (4, 2));
    assert_eq!((v.tally.files, v.tally.duplicates), (45_000, 0));
    assert!(!v.mounted);
    assert_eq!(v.root, Path::new(mount));

    // browsable as it was: one folder per d<N>, each with its own totals
    let l = browse::list(&db, Path::new(mount), None).unwrap();
    assert_eq!((l.files, l.entries.len()), (45_000, 45));
    let d7 = l.entries.iter().find(|e| e.name == "d7").unwrap();
    assert_eq!(
        (d7.files, d7.size, d7.mtime),
        (Some(1_000), 500_500, 1_700_000_000)
    );

    let r = import(&mut db, &idx, &opts).unwrap();
    assert_eq!(r.volumes[0].outcome, legacy::Outcome::AlreadyImported);
    assert_eq!(db.status().unwrap().files, 45_000);
}

#[test]
fn refuses_to_mix_with_an_indexed_root_unless_replaced() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("drive");
    write(&root.join("sub/a.txt"), b"one");
    write(&root.join("b.txt"), b"two");
    let drive = dunce::canonicalize(&root).unwrap();
    let idx = tmp.path().join("index");
    crawl(&idx, "disk", &drive, OLD_MOUNT);

    let mut db = Db::open(&tmp.path().join("idx.db")).unwrap();
    scan(&mut db, &drive.join("sub"));
    let mut opts = options(&drive, None);
    let err = import(&mut db, &idx, &opts).unwrap_err();
    assert!(err.contains("overlaps"), "{err}");
    assert_eq!(db.status().unwrap().roots.len(), 1, "nothing was written");

    opts.replace = true;
    let r = import(&mut db, &idx, &opts).unwrap();
    assert_eq!(r.volumes[0].replaced, [drive.join("sub")]);
    assert_eq!(db.status().unwrap().roots, [drive.display().to_string()]);

    // the drive is scanned for real since; the same legacy data is still recognised...
    opts.replace = false;
    scan(&mut db, &drive);
    let r = import(&mut db, &idx, &opts).unwrap();
    assert_eq!(r.volumes[0].outcome, legacy::Outcome::AlreadyImported);
    // ...but other legacy data for it is refused
    let merged = idx.join("disk.tsv");
    let mut tsv = fs::read(&merged).unwrap();
    tsv.extend(format!("3\t1700000000.0\t{OLD_MOUNT}/c.txt\n").as_bytes());
    fs::write(&merged, tsv).unwrap();
    let err = import(&mut db, &idx, &opts).unwrap_err();
    assert!(err.contains("already indexed"), "{err}");
    assert_eq!(report::scans(&db).unwrap().len(), 2);
}

#[test]
fn a_dry_run_reads_everything_and_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let drive = make_drive(&tmp.path().join("drive"));
    let idx = tmp.path().join("index");
    crawl(&idx, "usbdisk", &drive, OLD_MOUNT);
    hashes_db(
        &idx.join("hashes.db"),
        &[
            hashed(&drive, "a/dup-1.bin"),
            ("/mnt/another-disk/x.bin".into(), 5, 1.7e9, "cd".repeat(32)),
        ],
    );
    let db_path = tmp.path().join("idx.db");
    let mut db = Db::open(&db_path).unwrap();
    let mut opts = options(&drive, None);
    opts.dry_run = true;
    let r = import(&mut db, &idx, &opts).unwrap();
    assert_eq!(
        r.volumes[0].tally.files,
        files_in(&drive, true).len() as u64
    );
    assert_eq!(r.hashes_unused, Some(idx.join("hashes.db")));

    // a read-only index proves it
    drop(db);
    let mut db = Db::open_read_only(&db_path).unwrap();
    opts.hashes = Some(idx.join("hashes.db"));
    let r = import(&mut db, &idx, &opts).unwrap();
    let h = r.hashes.unwrap();
    assert_eq!((h.rows, h.for_volumes, h.attached), (2, 1, 0));
    assert_eq!(db.status().unwrap().files, 0);
}

#[test]
fn chunks_are_read_when_the_merge_is_missing_or_older() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("drive");
    write(&root.join("x/1.txt"), b"one");
    write(&root.join("y/2.txt"), b"two");
    let drive = dunce::canonicalize(&root).unwrap();
    let idx = tmp.path().join("index");
    crawl(&idx, "disk", &drive, OLD_MOUNT);
    // a chunk finished after the merge
    touch(
        &idx.join("disk/chunks/0000000000000002.done"),
        at(CRAWL_END + 120, 0),
    );
    let mut db = Db::open(&tmp.path().join("idx.db")).unwrap();
    let mut opts = options(&drive, None);
    opts.dry_run = true;
    let r = import(&mut db, &idx, &opts).unwrap();
    assert_eq!(r.volumes[0].source, "3 finished chunks");
    assert!(r.volumes[0].merge_outdated);

    fs::remove_file(idx.join("disk.tsv")).unwrap();
    opts.dry_run = false;
    let r = import(&mut db, &idx, &opts).unwrap();
    assert_eq!(r.volumes[0].source, "3 finished chunks");
    assert!(!r.volumes[0].merge_outdated);
    assert_eq!(r.volumes[0].tally.files, 2);
    assert_eq!(scan(&mut db, &drive).new, 0);
}
