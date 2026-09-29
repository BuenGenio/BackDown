//! Rescans from a change log must write exactly what a walk writes. Random trees get random edits
//! (files created, modified, touched, deleted, renamed and moved; folders made, deleted, renamed,
//! moved across the tree, out of the root and in from outside; a file replaced by a folder and a
//! folder by a file; hard links, symlinks, renames over existing files and empty folders), each
//! recorded by three fake change logs: the touched paths themselves, FSEvents-style events (with
//! whole folders sometimes coalesced into "look at all of this"), and USN-style records (file ids,
//! the folder each is in, names, reasons). Each log's change set is applied as a scan, round after
//! round, and every row (files and folders, current and past versions) is compared with a walk of
//! the same tree. Change logs that miss something the result would depend on must be refused.
//!
//! Test data lives under the target directory (CARGO_TARGET_TMPDIR), never in /tmp. More cases:
//! BACKDOWN_RESCAN_CASES=200 cargo test --test rescan

use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use backdown_core::index::{self, ChangeSet, fsevents, usn};
use backdown_core::{Db, paths};

// ---------- a deterministic random source

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // splitmix64
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.next() % 100 < pct
    }
    fn pick<'a, T>(&mut self, v: &'a [T]) -> Option<&'a T> {
        (!v.is_empty()).then(|| &v[self.below(v.len())])
    }
}

const NAMES: &[&str] = &[
    "a",
    "b",
    "c",
    "d",
    "notes.txt",
    "Photo 1.jpg",
    "ünï",
    "z.bin",
    "x",
    "y",
];

// ---------- the tree and its fake change logs

struct Harness {
    root: PathBuf,
    out: PathBuf,
    rng: Rng,
    /// every file and folder (under the root and outside it) by path: its id, as a file
    /// reference number would be (hard links share one)
    ids: HashMap<PathBuf, u64>,
    next_id: u64,
    raw: ChangeSet,
    fse: Vec<fsevents::Event>,
    usn: Vec<usn::Record>,
    log: Vec<String>,
    /// the folders (with ids) as the previous scan saw them
    folders_at_scan: Vec<(PathBuf, u64)>,
    /// each id's real inode number (FSEvents gives those)
    inodes: HashMap<u64, u64>,
}

fn mtime(rng: &mut Rng) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_500_000_000 + rng.next() % 200_000_000)
}

fn write_file(path: &Path, rng: &mut Rng) {
    let len = match rng.below(4) {
        0 => 0,
        1 => rng.below(64),
        _ => rng.below(3000),
    };
    let bytes: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
    fs::write(path, bytes).unwrap();
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(mtime(rng))
        .unwrap();
}

impl Harness {
    fn new(base: &Path, seed: u64) -> Harness {
        let root = base.join("root");
        let out = base.join("out");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&out).unwrap();
        let mut h = Harness {
            root: root.clone(),
            out: out.clone(),
            rng: Rng(seed),
            ids: HashMap::new(),
            next_id: 100,
            raw: ChangeSet::new(),
            fse: Vec::new(),
            usn: Vec::new(),
            log: Vec::new(),
            folders_at_scan: Vec::new(),
            inodes: HashMap::new(),
        };
        h.ids.insert(base.to_path_buf(), 2);
        h.ids.insert(root.clone(), 3);
        h.ids.insert(out, 4);
        h.grow(&root, 3);
        h.clear_logs();
        h
    }

    fn new_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// A random subtree under `dir` (recorded as creations).
    fn grow(&mut self, dir: &Path, depth: u32) {
        for _ in 0..self.rng.below(6) + 1 {
            let name = self.rng.pick(NAMES).unwrap().to_string();
            let p = dir.join(&name);
            if p.symlink_metadata().is_ok() {
                continue;
            }
            if depth > 0 && self.rng.chance(35) {
                fs::create_dir(&p).unwrap();
                self.created(&p, true);
                self.grow(&p, depth - 1);
            } else {
                write_file(&p, &mut self.rng);
                self.created(&p, false);
            }
        }
    }

    fn clear_logs(&mut self) {
        self.raw = ChangeSet::new();
        self.fse.clear();
        self.usn.clear();
    }

    /// Notes the folders under the root as a scan now sees them.
    fn scanned(&mut self) {
        self.folders_at_scan = self
            .ids
            .iter()
            .filter(|(p, _)| p.starts_with(&self.root) && p.is_dir() && !p.is_symlink())
            .map(|(p, i)| (p.clone(), *i))
            .collect();
    }

    /// Records one change in all three logs.
    fn emit(&mut self, p: &Path, id: u64, fse: u32, reason: u32, dir: bool) {
        self.raw.path(p);
        #[cfg(unix)]
        if let Ok(m) = p.symlink_metadata() {
            use std::os::unix::fs::MetadataExt;
            self.inodes.insert(id, m.ino());
        }
        self.fse.push(fsevents::Event {
            path: p.to_path_buf(),
            flags: fse
                | if dir {
                    fsevents::ITEM_IS_DIR
                } else {
                    fsevents::ITEM_IS_FILE
                },
            id: self.fse.len() as u64,
            inode: self.inodes.get(&id).copied(),
        });
        let parent = self.ids[p.parent().unwrap()];
        self.usn.push(usn::Record {
            usn: self.usn.len() as i64,
            frn: id,
            parent,
            name: p.file_name().unwrap().to_os_string(),
            reason,
            attributes: if dir { usn::ATTRIBUTE_DIRECTORY } else { 0x20 },
        });
    }

    fn created(&mut self, p: &Path, dir: bool) {
        let id = self.new_id();
        self.ids.insert(p.to_path_buf(), id);
        self.emit(p, id, fsevents::ITEM_CREATED, usn::REASON_FILE_CREATE, dir);
    }

    fn removed(&mut self, p: &Path, dir: bool) {
        let id = self.ids.remove(p).expect("known");
        self.emit(p, id, fsevents::ITEM_REMOVED, usn::REASON_FILE_DELETE, dir);
    }

    /// Everything under `base` (itself included): folders, files, others.
    fn entries(&self, base: &Path) -> (Vec<PathBuf>, Vec<PathBuf>, Vec<PathBuf>) {
        let (mut d, mut f, mut o) = (Vec::new(), Vec::new(), Vec::new());
        for e in walkdir::WalkDir::new(base)
            .follow_links(false)
            .sort_by_file_name()
        {
            let e = e.unwrap();
            let t = e.file_type();
            if t.is_dir() {
                d.push(e.into_path());
            } else if t.is_file() {
                f.push(e.into_path());
            } else {
                o.push(e.into_path());
            }
        }
        (d, f, o)
    }

    /// Deletes a folder and everything in it, deepest first, recording each.
    fn rm_tree(&mut self, dir: &Path) {
        let (dirs, files, others) = self.entries(dir);
        for f in files.iter().chain(&others) {
            fs::remove_file(f).unwrap();
            self.removed(f, false);
        }
        for d in dirs.iter().rev() {
            fs::remove_dir(d).unwrap();
            self.removed(d, true);
        }
    }

    /// Moves `from` to `to`, recording both names (ids of what's below move along).
    fn rename(&mut self, from: &Path, to: &Path, dir: bool) {
        let id = self.ids[from];
        self.emit(
            from,
            id,
            fsevents::ITEM_RENAMED,
            usn::REASON_RENAME_OLD_NAME,
            dir,
        );
        fs::rename(from, to).unwrap();
        let moved: Vec<(PathBuf, u64)> = self
            .ids
            .iter()
            .filter(|(p, _)| p.starts_with(from))
            .map(|(p, i)| (p.clone(), *i))
            .collect();
        for (p, i) in moved {
            self.ids.remove(&p);
            self.ids.insert(to.join(p.strip_prefix(from).unwrap()), i);
        }
        self.emit(
            to,
            id,
            fsevents::ITEM_RENAMED,
            usn::REASON_RENAME_NEW_NAME,
            dir,
        );
    }

    fn free_name(&mut self, dir: &Path) -> Option<PathBuf> {
        for _ in 0..5 {
            let p = dir.join(self.rng.pick(NAMES).unwrap());
            if p.symlink_metadata().is_err() {
                return Some(p);
            }
        }
        None
    }

    /// One random edit.
    fn edit(&mut self) {
        let (dirs, files, _) = self.entries(&self.root.clone());
        let inner: Vec<PathBuf> = dirs.iter().skip(1).cloned().collect(); // not the root
        let op = self.rng.below(17);
        let rng = &mut self.rng;
        let dir = rng.pick(&dirs).cloned().unwrap();
        let file = rng.pick(&files).cloned();
        let sub = rng.pick(&inner).cloned();
        match op {
            0 | 1 => {
                if let Some(p) = self.free_name(&dir) {
                    self.log.push(format!("create {}", p.display()));
                    write_file(&p, &mut self.rng);
                    self.created(&p, false);
                }
            }
            2 | 3 => {
                if let Some(f) = file {
                    // through this name only: other names (hard links) change too, unreported
                    self.log.push(format!("modify {}", f.display()));
                    write_file(&f, &mut self.rng);
                    let id = self.ids[&f];
                    self.emit(
                        &f,
                        id,
                        fsevents::ITEM_MODIFIED,
                        usn::REASON_DATA_OVERWRITE,
                        false,
                    );
                }
            }
            4 => {
                if let Some(f) = file {
                    self.log.push(format!("touch {}", f.display()));
                    let t = mtime(&mut self.rng);
                    File::options()
                        .write(true)
                        .open(&f)
                        .unwrap()
                        .set_modified(t)
                        .unwrap();
                    let id = self.ids[&f];
                    self.emit(
                        &f,
                        id,
                        fsevents::ITEM_INODE_META_MOD,
                        usn::REASON_BASIC_INFO_CHANGE,
                        false,
                    );
                }
            }
            5 => {
                if let Some(f) = file {
                    self.log.push(format!("delete {}", f.display()));
                    fs::remove_file(&f).unwrap();
                    self.removed(&f, false);
                }
            }
            6 => {
                // rename or move a file, sometimes over another file
                if let Some(f) = file {
                    let to = dir.join(self.rng.pick(NAMES).unwrap());
                    let over = to.symlink_metadata().ok();
                    if to == f || over.as_ref().is_some_and(|m| !m.is_file()) {
                        return;
                    }
                    #[cfg(unix)]
                    {
                        // two names of one file: rename(2) does nothing then
                        use std::os::unix::fs::MetadataExt;
                        let ino = f.symlink_metadata().unwrap().ino();
                        if over.as_ref().is_some_and(|m| m.ino() == ino) {
                            return;
                        }
                    }
                    self.log
                        .push(format!("move {} -> {}", f.display(), to.display()));
                    if over.is_some() {
                        // what's replaced goes away (a journal says so: the old file is deleted)
                        let id = self.ids.remove(&to).unwrap();
                        self.emit(
                            &to,
                            id,
                            fsevents::ITEM_REMOVED,
                            usn::REASON_FILE_DELETE,
                            false,
                        );
                    }
                    self.rename(&f, &to, false);
                }
            }
            7 => {
                if let Some(p) = self.free_name(&dir) {
                    self.log.push(format!("mkdir {}", p.display()));
                    fs::create_dir(&p).unwrap();
                    self.created(&p, true);
                    if self.rng.chance(60) {
                        self.grow(&p, 1);
                    }
                }
            }
            8 => {
                if let Some(d) = sub {
                    self.log.push(format!("rm -r {}", d.display()));
                    self.rm_tree(&d);
                }
            }
            9 | 10 => {
                // rename or move a folder across the tree (not into itself), sometimes over an
                // empty folder
                if let Some(d) = sub {
                    let target_dir = self.rng.pick(&dirs).cloned().unwrap();
                    if target_dir.starts_with(&d) {
                        return;
                    }
                    let to = target_dir.join(self.rng.pick(NAMES).unwrap());
                    if to == d {
                        return;
                    }
                    match to.symlink_metadata() {
                        Err(_) => {}
                        Ok(m) if m.is_dir() && fs::read_dir(&to).unwrap().next().is_none() => {
                            fs::remove_dir(&to).unwrap(); // rename(2) would replace it; same thing
                            let id = self.ids.remove(&to).unwrap();
                            self.emit(
                                &to,
                                id,
                                fsevents::ITEM_REMOVED,
                                usn::REASON_FILE_DELETE,
                                true,
                            );
                        }
                        Ok(_) => return,
                    }
                    self.log
                        .push(format!("move dir {} -> {}", d.display(), to.display()));
                    self.rename(&d, &to, true);
                }
            }
            11 => {
                // a file replaced by a folder
                if let Some(f) = file {
                    self.log.push(format!("file -> folder {}", f.display()));
                    fs::remove_file(&f).unwrap();
                    self.removed(&f, false);
                    fs::create_dir(&f).unwrap();
                    self.created(&f, true);
                    if self.rng.chance(70) {
                        self.grow(&f, 1);
                    }
                }
            }
            12 => {
                // a folder replaced by a file
                if let Some(d) = sub {
                    self.log.push(format!("folder -> file {}", d.display()));
                    self.rm_tree(&d);
                    write_file(&d, &mut self.rng);
                    self.created(&d, false);
                }
            }
            13 => {
                // a folder moved out of the root
                if let Some(d) = sub {
                    let to = self.out.join(format!("gone{}", self.rng.next() % 100_000));
                    self.log.push(format!("move out {}", d.display()));
                    self.rename(&d, &to, true);
                }
            }
            14 => {
                // a folder made outside the root, then moved in
                let tmp = self.out.join(format!("new{}", self.rng.next() % 100_000));
                if let (Ok(()), Some(to)) = (fs::create_dir(&tmp), self.free_name(&dir)) {
                    self.created(&tmp, true);
                    self.grow(&tmp, 2);
                    self.log.push(format!("move in {}", to.display()));
                    self.rename(&tmp, &to, true);
                }
            }
            15 => {
                // a hard link to a file, elsewhere in the tree
                if let (Some(f), Some(to)) = (file, self.free_name(&dir)) {
                    self.log
                        .push(format!("link {} -> {}", to.display(), f.display()));
                    fs::hard_link(&f, &to).unwrap();
                    let id = self.ids[&f];
                    self.ids.insert(to.clone(), id);
                    self.emit(
                        &to,
                        id,
                        fsevents::ITEM_CREATED,
                        usn::REASON_HARD_LINK_CHANGE,
                        false,
                    );
                }
            }
            _ => {
                // a symlink (walks don't record them); Windows needs rights to make one
                if let (true, Some(to)) = (cfg!(unix), self.free_name(&dir)) {
                    self.log.push(format!("symlink {}", to.display()));
                    #[cfg(unix)]
                    std::os::unix::fs::symlink("elsewhere", &to).unwrap();
                    let id = self.new_id();
                    self.ids.insert(to.clone(), id);
                    self.emit(
                        &to,
                        id,
                        fsevents::ITEM_CREATED | fsevents::ITEM_IS_SYMLINK,
                        usn::REASON_FILE_CREATE,
                        false,
                    );
                }
            }
        }
    }

    /// The FSEvents-style log, with the events inside some folders coalesced into one "look at
    /// everything in here" (as FSEvents does when it can't keep up).
    fn fse_changes(&mut self) -> ChangeSet {
        let mut events = self.fse.clone();
        if self.rng.chance(30) {
            let (dirs, _, _) = self.entries(&self.root.clone());
            if let Some(d) = self.rng.pick(&dirs[1..]).cloned() {
                events.retain(|e| !e.path.starts_with(&d));
                events.push(fsevents::Event {
                    path: d,
                    flags: fsevents::MUST_SCAN_SUBDIRS,
                    id: 0,
                    inode: None,
                });
            }
        }
        fsevents::changes(&self.root, &events)
    }

    fn usn_changes(&self) -> ChangeSet {
        let now: HashMap<u64, PathBuf> = self
            .ids
            .iter()
            .filter(|(p, _)| p.is_dir() && !p.is_symlink())
            .map(|(p, i)| (*i, p.clone()))
            .collect();
        let mut locate = |frn: u64| now.get(&frn).cloned();
        // every name a file has now (as Windows lists a file's hard links)
        let mut names = |frn: u64| {
            self.ids
                .iter()
                .filter(|(p, i)| **i == frn && p.symlink_metadata().is_ok_and(|m| !m.is_dir()))
                .map(|(p, _)| p.clone())
                .collect()
        };
        usn::changes(
            &self.root,
            self.folders_at_scan.clone(),
            &self.usn,
            &mut locate,
            &mut names,
        )
        .unwrap()
    }
}

// ---------- comparing indexes

fn opts() -> index::Options {
    index::Options {
        one_file_system: true,
        allow_empty: true,
    }
}

/// Every row of the index that a scan decides, in a comparable form.
fn dump(db: &Db) -> Vec<String> {
    let mut out = Vec::new();
    let mut q = |sql: &str, cols: usize| {
        let mut stmt = db.conn.prepare(sql).unwrap();
        let rows = stmt
            .query_map([], |r| {
                let mut s = String::new();
                for i in 0..cols {
                    let v: rusqlite::types::Value = r.get(i)?;
                    let text = match v {
                        rusqlite::types::Value::Blob(b) => {
                            paths::from_bytes(&b).display().to_string()
                        }
                        other => format!("{other:?}"),
                    };
                    s.push_str(&text);
                    s.push_str(" | ");
                }
                Ok(s)
            })
            .unwrap();
        for r in rows {
            out.push(r.unwrap());
        }
    };
    q(
        "SELECT 'file', path, dir, size, mtime, dev, ino, first_scan, last_scan, current, lname
         FROM files ORDER BY path, first_scan, current",
        11,
    );
    q(
        "SELECT 'dir', path, parent, files, bytes, newest, first_scan, last_scan, current, lname, dev, ino
         FROM dirs ORDER BY path, first_scan, current",
        12,
    );
    q(
        "SELECT 'scan', id, root_id, files, bytes, finished_at IS NOT NULL FROM scans ORDER BY id",
        6,
    );
    q(
        "SELECT 'root', id, path, last_scan FROM roots ORDER BY id",
        4,
    );
    out
}

fn same(label: &str, want: &[String], got: &[String], log: &[String], seed: u64) {
    if want == got {
        return;
    }
    let w: BTreeSet<&String> = want.iter().collect();
    let g: BTreeSet<&String> = got.iter().collect();
    let missing: Vec<_> = w.difference(&g).take(12).collect();
    let extra: Vec<_> = g.difference(&w).take(12).collect();
    panic!(
        "{label} differs from a walk (seed {seed})\nonly in the walk: {missing:#?}\nonly in the {label} scan: {extra:#?}\nedits:\n{}",
        log.join("\n")
    );
}

fn copy(db: &Db, to: &Path) -> Db {
    let _ = fs::remove_file(to);
    db.conn
        .execute("VACUUM INTO ?1", [to.to_str().unwrap()])
        .unwrap();
    Db::open(to).unwrap()
}

fn base_dir(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("rescan")
        .join(name);
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// Waits for the next second: folders made before a scan then have birth times before it, so
/// rescans list them again rather than walking them (the path worth testing).
fn next_second() {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    std::thread::sleep(Duration::from_millis(
        1000 - u64::from(now.subsec_millis()) + 20,
    ));
}

fn one_case(seed: u64, rounds: usize) {
    let base = base_dir(&format!("case{seed}"));
    let mut h = Harness::new(&base, seed);
    next_second();
    let db_dir = base.join("db");
    fs::create_dir_all(&db_dir).unwrap();
    let mut walk = Db::open(&db_dir.join("walk.db")).unwrap();
    index::index(&mut walk, &h.root, &opts(), &mut |_| {}).unwrap();
    h.scanned();
    let mut raw = copy(&walk, &db_dir.join("raw.db"));
    let mut fse = copy(&walk, &db_dir.join("fse.db"));
    let mut usn = copy(&walk, &db_dir.join("usn.db"));
    for round in 0..rounds {
        let edits = h.rng.below(8);
        h.log.push(format!("-- round {round}: {edits} edits"));
        for _ in 0..edits {
            h.edit();
        }
        let raw_changes = h.raw.clone();
        let fse_changes = h.fse_changes();
        let usn_changes = h.usn_changes();
        index::index(&mut walk, &h.root, &opts(), &mut |_| {}).unwrap();
        let want = dump(&walk);
        for (label, db, changes) in [
            ("raw", &mut raw, &raw_changes),
            ("fsevents", &mut fse, &fse_changes),
            ("usn", &mut usn, &usn_changes),
        ] {
            let st = index::scan_changes(db, &h.root, &opts(), changes, &mut |_| {})
                .unwrap_or_else(|e| {
                    panic!(
                        "{label} scan refused (seed {seed}): {e:#}\nedits:\n{}",
                        h.log.join("\n")
                    )
                });
            assert_eq!(st.method, "journal");
            same(label, &want, &dump(db), &h.log, seed);
        }
        h.clear_logs();
        h.scanned();
    }
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn journal_scans_equal_walks() {
    let var = |name: &str, default: u64| {
        std::env::var(name)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    };
    // BACKDOWN_RESCAN_FIRST=31 BACKDOWN_RESCAN_CASES=1: just the case with seed 31
    let first = var("BACKDOWN_RESCAN_FIRST", 1);
    for seed in first..first + var("BACKDOWN_RESCAN_CASES", 12) {
        one_case(seed, 5);
    }
}

// ---------- change logs that miss something are refused

fn setup(name: &str) -> (PathBuf, Db) {
    let base = base_dir(name);
    let root = base.join("root");
    for p in ["a/b/f.txt", "a/g.txt", "c/h.txt", "top.txt"] {
        let p = root.join(p);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, p.display().to_string()).unwrap();
    }
    next_second();
    let mut db = Db::open(&base.join("idx.db")).unwrap();
    index::index(&mut db, &root, &opts(), &mut |_| {}).unwrap();
    (root, db)
}

fn refused(db: &mut Db, root: &Path, c: &ChangeSet) -> String {
    let before = dump(db);
    let err = index::scan_changes(db, root, &opts(), c, &mut |_| {})
        .expect_err("a change set that misses something must be refused");
    // nothing was recorded
    let after = dump(db);
    assert_eq!(
        before
            .iter()
            .filter(|r| !r.starts_with("'scan'"))
            .collect::<Vec<_>>(),
        after
            .iter()
            .filter(|r| !r.starts_with("'scan'"))
            .collect::<Vec<_>>()
    );
    format!("{err:#}")
}

#[test]
fn unreported_changes_are_refused() {
    let (root, mut db) = setup("refused");
    // a folder moved away, but only a file inside it reported
    fs::rename(root.join("a"), root.join("a2")).unwrap();
    let mut c = ChangeSet::new();
    c.path(root.join("a/b/f.txt"));
    let why = refused(&mut db, &root, &c);
    assert!(why.contains("didn't say"), "{why}");
    // a new folder whose creation wasn't reported, only a file in it
    fs::rename(root.join("a2"), root.join("a")).unwrap();
    fs::create_dir(root.join("new")).unwrap();
    fs::write(root.join("new/n.txt"), "n").unwrap();
    let mut c = ChangeSet::new();
    c.path(root.join("new/n.txt"));
    let why = refused(&mut db, &root, &c);
    assert!(why.contains("didn't say"), "{why}");
    // a folder replaced by another behind the log's back
    fs::remove_dir_all(root.join("new")).unwrap();
    fs::rename(root.join("c"), base_dir("refused-aside").join("c")).unwrap();
    fs::create_dir(root.join("c")).unwrap();
    fs::write(root.join("c/h.txt"), "other").unwrap();
    let mut c = ChangeSet::new();
    c.path(root.join("c/h.txt"));
    let why = refused(&mut db, &root, &c);
    assert!(why.contains("didn't say"), "{why}");
    // the log asking for everything
    let mut c = ChangeSet::new();
    c.everything("events were dropped");
    assert!(refused(&mut db, &root, &c).contains("dropped"));
    let mut c = ChangeSet::new();
    c.subtree(&root);
    refused(&mut db, &root, &c);
    // reported properly, the same change is fine
    let mut c = ChangeSet::new();
    c.path(root.join("c"));
    let st = index::scan_changes(&mut db, &root, &opts(), &c, &mut |_| {}).unwrap();
    assert_eq!((st.new, st.changed, st.removed), (0, 1, 0));
}

#[test]
fn nothing_changed_is_quick_and_complete() {
    let (root, mut db) = setup("unchanged");
    let before: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM files WHERE current = 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    let st = index::scan_changes(&mut db, &root, &opts(), &ChangeSet::new(), &mut |_| {}).unwrap();
    assert_eq!((st.files, st.new, st.changed, st.removed), (4, 0, 0, 0));
    let (current, in_scan): (i64, i64) = db
        .conn
        .query_row(
            "SELECT COUNT(*), SUM(last_scan = ?1) FROM files WHERE current = 1",
            [st.scan],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((current, in_scan), (before, before));
    let how = index::how(&db, &[st.scan]).unwrap();
    assert_eq!(how[0].method, "journal");
}

#[test]
fn auto_walks_where_there_is_no_change_log() {
    let (root, mut db) = setup("auto");
    let st = index::scan(&mut db, &root, &opts(), index::Method::Auto, &mut |_| {}).unwrap();
    assert_eq!(st.method, "walk");
    assert!(!st.note.is_empty());
    // asked for explicitly, the change log's absence is an error, not a silent walk
    #[cfg(target_os = "linux")]
    {
        let e =
            index::scan(&mut db, &root, &opts(), index::Method::Journal, &mut |_| {}).unwrap_err();
        assert!(format!("{e:#}").contains("change log"), "{e:#}");
        let e =
            index::scan(&mut db, &root, &opts(), index::Method::Table, &mut |_| {}).unwrap_err();
        assert!(format!("{e:#}").contains("file table"), "{e:#}");
    }
    // settings: off means walks, with the reason
    let mut s = index::settings(&db).unwrap();
    assert_eq!(s, index::Settings::default());
    s.fast = false;
    index::set_settings(&db, &s).unwrap();
    let st = index::scan(&mut db, &root, &opts(), index::Method::Auto, &mut |_| {}).unwrap();
    assert!(st.note.contains("turned off"), "{}", st.note);
    assert_eq!(index::settings(&db).unwrap(), s);
}

/// How long a rescan from a change set takes against a walk, on a big tree (500,000 files by
/// default, BACKDOWN_BENCH_FILES to change), with a few hundred changes. Release mode:
/// cargo test --release --test rescan -- --ignored --nocapture bench
#[test]
#[ignore]
fn bench_rescan_from_changes() {
    use std::time::Instant;
    let n: usize = std::env::var("BACKDOWN_BENCH_FILES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(500_000);
    let base = base_dir("bench");
    let root = base.join("root");
    let t0 = Instant::now();
    for i in 0..n {
        let dir = root.join(format!("d{}/e{}", i % 100, i % 5000 / 100));
        if i < 5000 {
            fs::create_dir_all(&dir).unwrap();
        }
        fs::write(dir.join(format!("file {i}.txt")), [b'x'; 16]).unwrap();
    }
    eprintln!("made {n} files in {:.1?}", t0.elapsed());
    next_second();
    let mut db = Db::open(&base.join("idx.db")).unwrap();
    let t0 = Instant::now();
    let first = index::index(&mut db, &root, &opts(), &mut |_| {}).unwrap();
    eprintln!("first walk: {:.2?} ({} files)", t0.elapsed(), first.files);
    let t0 = Instant::now();
    index::index(&mut db, &root, &opts(), &mut |_| {}).unwrap();
    eprintln!("rescan by walking, nothing changed: {:.2?}", t0.elapsed());
    // a few hundred changes: new files, modified, deleted, a folder of 100 files moved
    let mut c = ChangeSet::new();
    for i in 0..200 {
        let p = root.join(format!("d{}/e0/new {i}.txt", i % 100));
        fs::write(&p, b"new").unwrap();
        c.path(p);
        let m = root.join(format!("d{}/e{}/file {}.txt", i % 100, i % 50, i * 7));
        if m.exists() {
            fs::write(&m, b"modified, longer").unwrap();
            c.path(m);
        }
    }
    for i in 0..100 {
        let p = root.join(format!(
            "d{}/e{}/file {}.txt",
            i % 100,
            10 + i % 30,
            100_000 + i * 13
        ));
        if p.exists() {
            fs::remove_file(&p).unwrap();
            c.path(p);
        }
    }
    fs::rename(root.join("d7/e3"), root.join("d8/e3-moved")).unwrap();
    c.path(root.join("d7/e3"));
    c.path(root.join("d8/e3-moved"));
    let mut journal = copy(&db, &base.join("journal.db"));
    let t0 = Instant::now();
    let st = index::scan_changes(&mut journal, &root, &opts(), &c, &mut |_| {}).unwrap();
    let from_changes = t0.elapsed();
    let t0 = Instant::now();
    let w = index::index(&mut db, &root, &opts(), &mut |_| {}).unwrap();
    let walked = t0.elapsed();
    eprintln!(
        "{} changes: rescan from them {from_changes:.2?} ({} new, {} changed, {} gone); \
         the same by walking {walked:.2?}",
        c.len(),
        st.new,
        st.changed,
        st.removed
    );
    let t0 = Instant::now();
    index::scan_changes(&mut journal, &root, &opts(), &ChangeSet::new(), &mut |_| {}).unwrap();
    eprintln!("rescan from an empty change log: {:.2?}", t0.elapsed());
    assert_eq!((st.files, st.bytes), (w.files, w.bytes));
    let _ = fs::remove_dir_all(&base);
}
