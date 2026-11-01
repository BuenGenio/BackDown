//! Tidying the archive: identical files inside one folder made to share their storage (opt-in).
//!
//! Within one folder (an archive BackDown filed into, or any folder a person names), files the
//! index knows to have the same SHA-256 become either
//!
//! - **hardlinks**: one file with several names. Saves the space of every extra copy, on any
//!   filesystem that has hardlinks, but editing one name edits them all; or
//! - **clones**: separate files that share blocks until one is changed (btrfs, XFS, bcachefs via
//!   the kernel's own compare-and-share, `FIDEDUPERANGE`; APFS via `clonefile`). Each keeps its
//!   own name, dates, owner and permissions, and stays independent.
//!
//! The rules:
//!
//! 1. Nothing outside the folder is touched, nothing is linked across filesystems, and a file that
//!    has another name outside the folder is left alone (linking it would save nothing, or share
//!    storage with something outside).
//! 2. A hardlink is one inode, so files are only hardlinked to files with the same permissions,
//!    owner, group, extended attributes and modification time (to the second): nothing a file
//!    carries is lost. Files that differ are grouped, and each group links among itself. Clones
//!    made in place (Linux) keep every file's own metadata, so only the filesystem must match.
//! 3. Before linking, both files are re-read and must still have the index's hash; a file that
//!    changed since the last scan is left alone. A hardlink replaces the duplicate atomically (a new
//!    name made first, then renamed over it), so the name never points at nothing; a clone is made
//!    by the kernel, which compares the bytes again under lock.
//! 4. Every link is journaled (which file now shares which, its size and hash), and recorded in
//!    the chain of custody (custody.rs) in the same transaction. A run that stops is carried on
//!    by running it again: files already sharing their keeper's storage are counted, not linked
//!    twice.
//! 5. A dry run (the default) reads only the index and the files' metadata, writing nothing.
//!
//! There's no undo for a hardlink, and none is needed: to make one name a separate file again,
//! copy it and replace it with the copy.

use std::collections::{BTreeMap, HashSet};
#[cfg(unix)]
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::apply::hash_file;
#[cfg(unix)]
use crate::apply::mtime;
use crate::clock::now;
use crate::custody::{self, Action, Event};
use crate::{Db, fastcopy, fmt, paths};

/// The tables (made by `Db::open`): each run, and each file it linked.
pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS dedups (
    id         INTEGER PRIMARY KEY,
    created_at INTEGER NOT NULL,
    folder     TEXT NOT NULL,
    link       TEXT NOT NULL,         -- hard | clone
    min_size   INTEGER NOT NULL,
    state      TEXT NOT NULL,         -- new | running | done | partial | failed | stopped
    summary    TEXT
);
CREATE TABLE IF NOT EXISTS dedup_links (
    id       INTEGER PRIMARY KEY,
    dedup_id INTEGER NOT NULL REFERENCES dedups(id),
    path     BLOB NOT NULL,           -- the duplicate, now sharing the keeper's storage
    keeper   BLOB NOT NULL,
    size     INTEGER NOT NULL,
    mtime    INTEGER NOT NULL,
    sha256   BLOB NOT NULL,           -- what both read back as, just before linking
    how      TEXT NOT NULL,           -- hardlink | clone
    at       INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS dedup_links_path ON dedup_links(path);
";

/// How duplicates share storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Link {
    Hard,
    Clone,
}

impl Link {
    pub fn parse(s: &str) -> Result<Link> {
        match s {
            "hard" | "hardlink" | "hardlinks" => Ok(Link::Hard),
            "clone" | "clones" | "reflink" => Ok(Link::Clone),
            _ => bail!("--link is hard or clone"),
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Link::Hard => "hard",
            Link::Clone => "clone",
        }
    }
}

/// What people are told before and after, whichever the link.
pub fn warning(link: Link) -> &'static str {
    match link {
        Link::Hard => {
            "Hardlinked files are one file with several names: editing any of them changes them all. To make one a separate file again, copy it and replace it with the copy (cp -p photo.jpg photo.tmp && mv photo.tmp photo.jpg)."
        }
        Link::Clone => {
            "Cloned files stay separate files: changing one leaves the others as they are, and only then does it take space of its own."
        }
    }
}

/// Up to this many of the groups that save the most are listed.
const EXAMPLES: usize = 40;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Group {
    pub size: u64,
    pub sha256: String,
    /// the file the others will share with
    pub keeper: String,
    pub others: Vec<String>,
    pub saves: u64,
}

/// What a dedup would do (the dry run), or did.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Report {
    pub folder: String,
    pub link: Option<Link>,
    /// the folder's filesystem, and whether it can share blocks between files
    pub filesystem: Option<String>,
    pub clones_possible: bool,
    /// sets of identical files with something to link
    pub groups: u64,
    /// files that were (or would be) linked to their keeper
    pub linked: u64,
    pub saved_bytes: u64,
    /// already sharing their keeper's storage (an earlier run, or linked before)
    pub already: u64,
    /// identical content, but different permissions, owner, extended attributes or (hardlinks)
    /// modification time from every other copy: kept apart
    pub kept_apart: u64,
    pub kept_apart_bytes: u64,
    /// with another name outside the folder (or not in the index): left alone
    pub elsewhere: u64,
    /// changed, gone or unreadable since the last scan (or read back differently): left alone
    pub changed: u64,
    pub failed: u64,
    /// files in the folder whose content isn't hashed yet (they might have duplicates too)
    pub unhashed: u64,
    pub examples: Vec<Group>,
    pub problems: Vec<String>,
    pub warning: String,
}

fn push(v: &mut Vec<String>, s: String) {
    if v.len() < 30 {
        v.push(s);
    }
}

fn range(dir: &Path) -> (Vec<u8>, Vec<u8>) {
    let lo = paths::dir_prefix(dir);
    let mut hi = lo.clone();
    if let Some(l) = hi.last_mut() {
        *l += 1; // a separator is never 0xFF
    }
    (lo, hi)
}

/// The folder as it will be worked on: absolute, existing, not the system or the index.
pub fn check_folder(folder: &Path, index: Option<&Path>) -> Result<PathBuf> {
    if !folder.is_absolute() {
        bail!("the folder must be a full path");
    }
    let f =
        paths::canonical(folder).with_context(|| format!("{} isn't there", folder.display()))?;
    if !f.is_dir() {
        bail!("{} isn't a folder", f.display());
    }
    if let Some(why) = crate::wipe::refusal(&f, index) {
        bail!("BackDown won't link files in {}: {why}", f.display());
    }
    if !cfg!(unix) {
        bail!(
            "tidying an archive isn't available on this system yet: its files' alternate data streams could be lost"
        );
    }
    Ok(f)
}

// ---------- what a file is now

/// What a link must not lose, and must not cross: files link only within one class.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Class {
    dev: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    /// seconds (one inode has one modification time)
    mtime: i64,
    /// BSD file flags (macOS)
    flags: u32,
    xattrs: Vec<(Vec<u8>, Vec<u8>)>,
}

#[derive(Debug, Clone)]
struct Now {
    path: PathBuf,
    size: u64,
    ino: u64,
    nlink: u64,
    mtime: i64,
    class: Class,
}

/// Does this link keep one inode's metadata for several files (so they must all agree on it)?
/// Hardlinks do; so do macOS clones (a new file cloned from the keeper). Linux clones are made in
/// place and keep each file's own.
#[cfg(unix)]
fn strict(link: Link) -> bool {
    link == Link::Hard || cfg!(target_os = "macos")
}

#[cfg(unix)]
fn stat(path: &Path, link: Link) -> std::io::Result<Option<Now>> {
    use std::os::unix::fs::MetadataExt;
    let m = fs::symlink_metadata(path)?;
    if !m.file_type().is_file() {
        return Ok(None);
    }
    let class = if strict(link) {
        #[cfg(target_os = "macos")]
        let flags = {
            use std::os::macos::fs::MetadataExt as _;
            m.st_flags()
        };
        #[cfg(not(target_os = "macos"))]
        let flags = 0;
        Class {
            dev: m.dev(),
            mode: m.mode() & 0o7777,
            uid: m.uid(),
            gid: m.gid(),
            mtime: if link == Link::Hard { m.mtime() } else { 0 },
            flags,
            xattrs: xattrs(path)?,
        }
    } else {
        Class {
            dev: m.dev(),
            mode: 0,
            uid: 0,
            gid: 0,
            mtime: 0,
            flags: 0,
            xattrs: Vec::new(),
        }
    };
    Ok(Some(Now {
        path: path.to_path_buf(),
        size: m.len(),
        ino: m.ino(),
        nlink: m.nlink(),
        mtime: mtime(&m),
        class,
    }))
}

#[cfg(not(unix))]
fn stat(_path: &Path, _link: Link) -> std::io::Result<Option<Now>> {
    Err(std::io::ErrorKind::Unsupported.into())
}

/// A file's extended attributes (names and values, sorted), not following a symbolic link.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn xattrs(path: &Path) -> std::io::Result<Vec<(Vec<u8>, Vec<u8>)>> {
    use std::ffi::{CStr, CString};
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY (the four closures): `c` and `name` are NUL-terminated strings alive for the call, and
    // each buffer is a live allocation whose length is passed with it (or null with length 0, to ask
    // the size); the kernel writes at most that many bytes.
    #[cfg(target_os = "linux")]
    let list = |buf: *mut libc::c_char, n: usize| unsafe { libc::llistxattr(c.as_ptr(), buf, n) };
    #[cfg(target_os = "macos")]
    let list = |buf: *mut libc::c_char, n: usize| unsafe {
        libc::listxattr(c.as_ptr(), buf, n, libc::XATTR_NOFOLLOW)
    };
    #[cfg(target_os = "linux")]
    let get = |name: &CStr, buf: *mut libc::c_void, n: usize| unsafe {
        libc::lgetxattr(c.as_ptr(), name.as_ptr(), buf, n)
    };
    #[cfg(target_os = "macos")]
    let get = |name: &CStr, buf: *mut libc::c_void, n: usize| unsafe {
        libc::getxattr(c.as_ptr(), name.as_ptr(), buf, n, 0, libc::XATTR_NOFOLLOW)
    };
    let mut names = Vec::new();
    for attempt in 0..4 {
        let need = list(std::ptr::null_mut(), 0);
        if need < 0 {
            let e = io::Error::last_os_error();
            // a filesystem without extended attributes has none to lose
            return if e.raw_os_error() == Some(libc::ENOTSUP) {
                Ok(Vec::new())
            } else {
                Err(e)
            };
        }
        if need == 0 {
            return Ok(Vec::new());
        }
        names = vec![0u8; need as usize];
        let got = list(names.as_mut_ptr().cast(), names.len());
        if got >= 0 {
            names.truncate(got as usize);
            break;
        }
        // they grew between the two calls (ERANGE): ask again
        if attempt == 3 {
            return Err(io::Error::last_os_error());
        }
    }
    let mut out = Vec::new();
    for name in names.split(|&b| b == 0).filter(|n| !n.is_empty()) {
        let cname = CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
        let need = get(&cname, std::ptr::null_mut(), 0);
        if need < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut val = vec![0u8; need as usize];
        let got = get(&cname, val.as_mut_ptr().cast(), val.len());
        if got < 0 {
            return Err(io::Error::last_os_error());
        }
        val.truncate(got as usize);
        out.push((name.to_vec(), val));
    }
    out.sort();
    Ok(out)
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn xattrs(_path: &Path) -> std::io::Result<Vec<(Vec<u8>, Vec<u8>)>> {
    // other Unixes: not read, so they can't be compared; refuse rather than lose them
    Err(std::io::ErrorKind::Unsupported.into())
}

// ---------- deciding what links to what

/// What becomes of one file of a set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// shares the keeper's storage already
    Already,
    /// another name of an inode that one of its names brings along (a clone shares the inode's
    /// storage for all its names)
    Along,
    /// to be linked; `frees` once its inode has no other name left to link
    Link { frees: bool },
}

/// One set of files that link among themselves: the same content and class.
struct Set {
    keeper: Now,
    others: Vec<(Now, Role)>,
}

/// Look at one group of index rows (the same content) as the files are now, and decide what links
/// to what. Counts what's left alone into `r`.
fn plan_group(size: u64, rows: Vec<(PathBuf, i64)>, link: Link, r: &mut Report) -> Vec<Set> {
    let mut by_class: BTreeMap<Class, Vec<Now>> = BTreeMap::new();
    for (path, indexed_mtime) in rows {
        match stat(&path, link) {
            Ok(Some(n)) if n.size == size && n.mtime == indexed_mtime => {
                by_class.entry(n.class.clone()).or_default().push(n);
            }
            Ok(_) => r.changed += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => r.changed += 1,
            Err(e) => {
                r.changed += 1;
                push(
                    &mut r.problems,
                    format!("can't read {}: {e}", path.display()),
                );
            }
        }
    }
    let mut sets = Vec::new();
    for (_, files) in by_class {
        if files.len() < 2 {
            r.kept_apart += files.len() as u64;
            r.kept_apart_bytes += size * files.len() as u64;
            continue;
        }
        // names per inode, here; an inode with a name elsewhere (outside the folder, or one the
        // index doesn't know) is left alone: relinking its names here wouldn't free it, and
        // linking to it would reach outside
        let mut inodes: BTreeMap<u64, Vec<Now>> = BTreeMap::new();
        for f in files {
            inodes.entry(f.ino).or_default().push(f);
        }
        let mut inside: Vec<Vec<Now>> = Vec::new();
        for (_, mut names) in inodes {
            names.sort_by(|a, b| a.path.cmp(&b.path));
            if names[0].nlink > names.len() as u64 {
                r.elsewhere += names.len() as u64;
            } else {
                inside.push(names);
            }
        }
        if inside.is_empty() {
            continue;
        }
        // the keeper: the inode with the most names already, then the first path (so a second run
        // picks the same one)
        inside.sort_by(|a, b| b.len().cmp(&a.len()).then(a[0].path.cmp(&b[0].path)));
        let mut it = inside.into_iter();
        let mut first = it.next().expect("not empty");
        let keeper = first.remove(0);
        let mut others: Vec<(Now, Role)> = first.into_iter().map(|n| (n, Role::Already)).collect();
        for names in it {
            let n = names.len();
            for (i, name) in names.into_iter().enumerate() {
                let role = match link {
                    Link::Hard => Role::Link { frees: i + 1 == n },
                    Link::Clone if i == 0 => Role::Link { frees: true },
                    Link::Clone => Role::Along,
                };
                others.push((name, role));
            }
        }
        if others.is_empty() {
            continue;
        }
        sets.push(Set { keeper, others });
    }
    sets
}

/// Groups of identical content in `folder`: (sha256, size).
fn groups(db: &Db, folder: &Path, min_size: u64) -> Result<Vec<(Vec<u8>, u64)>> {
    let (lo, hi) = range(folder);
    let mut stmt = db.conn.prepare(
        "SELECT sha256, MIN(size) FROM files WHERE current = 1 AND path >= ?1 AND path < ?2 AND size >= ?3 \
         AND sha256 IS NOT NULL AND error IS NULL GROUP BY sha256 HAVING COUNT(*) > 1",
    )?;
    let rows = stmt.query_map(params![lo, hi, min_size.max(1) as i64], |r| {
        Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)? as u64))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn members(db: &Db, folder: &Path, sha: &[u8]) -> Result<Vec<(PathBuf, i64)>> {
    let (lo, hi) = range(folder);
    let mut stmt = db.conn.prepare_cached(
        "SELECT path, mtime FROM files WHERE current = 1 AND sha256 = ?1 AND path >= ?2 AND path < ?3 AND error IS NULL ORDER BY path",
    )?;
    let rows = stmt.query_map(params![sha, lo, hi], |r| {
        Ok((
            paths::from_bytes(&r.get::<_, Vec<u8>>(0)?),
            r.get::<_, i64>(1)?,
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn unhashed(db: &Db, folder: &Path, min_size: u64) -> Result<u64> {
    // the same size as another file (not another name of the same one), yet no hash at all:
    // `hash` hasn't caught up with a scan
    let (lo, hi) = range(folder);
    Ok(db.conn.query_row(
        "SELECT COUNT(*) FROM files f WHERE f.current = 1 AND f.path >= ?1 AND f.path < ?2 AND f.size >= ?3 \
         AND f.sha256 IS NULL AND f.sample IS NULL AND f.error IS NULL \
         AND EXISTS (SELECT 1 FROM files g WHERE g.current = 1 AND g.size = f.size AND g.id != f.id \
                     AND (g.ino IS NULL OR f.ino IS NULL OR g.dev != f.dev OR g.ino != f.ino))",
        params![lo, hi, min_size.max(1) as i64],
        |r| r.get::<_, i64>(0),
    )? as u64)
}

fn base_report(db: &Db, folder: &Path, link: Link, min_size: u64) -> Result<Report> {
    let (lo, hi) = range(folder);
    let indexed = db
        .conn
        .query_row(
            "SELECT 1 FROM files WHERE current = 1 AND path >= ?1 AND path < ?2 LIMIT 1",
            params![lo, hi],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !indexed {
        bail!(
            "nothing in {} is in the index yet: index it first (backdown index \"{}\")",
            folder.display(),
            folder.display()
        );
    }
    Ok(Report {
        folder: folder.display().to_string(),
        link: Some(link),
        filesystem: fastcopy::fs_name(folder),
        clones_possible: fastcopy::clone_capable(folder).is_some(),
        unhashed: unhashed(db, folder, min_size)?,
        warning: warning(link).to_string(),
        ..Report::default()
    })
}

/// Keep the sets that save the most as examples.
fn example(r: &mut Report, size: u64, sha: &[u8], sets: &[Set]) {
    for s in sets {
        let others: Vec<String> = s
            .others
            .iter()
            .filter(|(_, role)| matches!(role, Role::Link { .. }))
            .map(|(n, _)| n.path.display().to_string())
            .collect();
        let frees = s
            .others
            .iter()
            .filter(|(_, role)| *role == Role::Link { frees: true })
            .count() as u64;
        if others.is_empty() {
            continue;
        }
        let saves = size * frees;
        if r.examples.len() >= EXAMPLES && r.examples.last().is_some_and(|g| g.saves >= saves) {
            continue;
        }
        r.examples.push(Group {
            size,
            sha256: fmt::hex(sha),
            keeper: s.keeper.path.display().to_string(),
            others,
            saves,
        });
        r.examples.sort_by_key(|g| std::cmp::Reverse(g.saves));
        r.examples.truncate(EXAMPLES);
    }
}

/// What linking duplicates in `folder` would do: from the index and the files' metadata only
/// (nothing is read or written).
pub fn assess(db: &Db, folder: &Path, link: Link, min_size: u64) -> Result<Report> {
    // as the index spells it (a name typed in the other Unicode form than the drive's)
    let folder = &crate::names::as_indexed_or_same(db, folder);
    let mut r = base_report(db, folder, link, min_size)?;
    for (sha, size) in groups(db, folder, min_size)? {
        let sets = plan_group(size, members(db, folder, &sha)?, link, &mut r);
        let mut any = false;
        for s in &sets {
            for (_, role) in &s.others {
                match role {
                    Role::Already => r.already += 1,
                    Role::Along => {}
                    Role::Link { frees } => {
                        any = true;
                        r.linked += 1;
                        if *frees {
                            r.saved_bytes += size;
                        }
                    }
                }
            }
        }
        if any {
            r.groups += 1;
        }
        example(&mut r, size, &sha, &sets);
    }
    Ok(r)
}

// ---------- doing it

/// Record a dedup run before starting it (the daemon runs it as a job).
pub fn record(db: &Db, folder: &Path, link: Link, min_size: u64) -> Result<i64> {
    db.conn.execute(
        "INSERT INTO dedups(created_at, folder, link, min_size, state) VALUES (?1, ?2, ?3, ?4, 'new')",
        params![now(), folder.display().to_string(), link.name(), min_size as i64],
    )?;
    Ok(db.conn.last_insert_rowid())
}

/// A recorded run: (folder, link, min_size, state).
pub fn load(db: &Db, id: i64) -> Result<(PathBuf, Link, u64, String)> {
    let (folder, link, min, state): (String, String, i64, String) = db
        .conn
        .query_row(
            "SELECT folder, link, min_size, state FROM dedups WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?
        .with_context(|| format!("there's no dedup {id}"))?;
    Ok((
        PathBuf::from(folder),
        Link::parse(&link)?,
        min as u64,
        state,
    ))
}

/// Dedup runs on record, newest first.
pub fn history(db: &Db) -> Result<Vec<serde_json::Value>> {
    let mut stmt = db.conn.prepare(
        "SELECT d.id, d.created_at, d.folder, d.link, d.state, d.summary, \
         (SELECT COUNT(*) FROM dedup_links l WHERE l.dedup_id = d.id) \
         FROM dedups d ORDER BY d.id DESC LIMIT 30",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(serde_json::json!({
            "id": r.get::<_, i64>(0)?, "created_at": r.get::<_, i64>(1)?, "folder": r.get::<_, String>(2)?,
            "link": r.get::<_, String>(3)?, "state": r.get::<_, String>(4)?, "summary": r.get::<_, Option<String>>(5)?,
            "linked": r.get::<_, i64>(6)?,
        }))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// One line about a run (or a dry run).
pub fn summary(r: &Report) -> String {
    let how = match r.link {
        Some(Link::Clone) => "cloned",
        _ => "hardlinked",
    };
    let mut s = format!(
        "{how} {} files, saving {}",
        fmt::count(r.linked),
        fmt::bytes(r.saved_bytes)
    );
    if r.already > 0 {
        s += &format!("; {} already shared", fmt::count(r.already));
    }
    if r.kept_apart > 0 {
        s += &format!(
            "; {} kept apart (different permissions, owner or attributes{})",
            fmt::count(r.kept_apart),
            if r.link == Some(Link::Hard) {
                ", or dates"
            } else {
                ""
            }
        );
    }
    if r.elsewhere > 0 {
        s += &format!(
            "; {} with other names outside, left alone",
            fmt::count(r.elsewhere)
        );
    }
    if r.changed > 0 {
        s += &format!(
            "; {} changed since the last scan, left alone",
            fmt::count(r.changed)
        );
    }
    if r.failed > 0 {
        s += &format!("; {} failed", fmt::count(r.failed));
    }
    s
}

/// Carry out dedup `id` (made by `record`). See the module doc for the rules. `stop` is checked
/// between files; a stopped run is carried on by running it again.
pub fn run(
    db: &Db,
    id: i64,
    index: Option<&Path>,
    stop: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Report> {
    let r = run_inner(db, id, index, stop, progress);
    match &r {
        Ok(rep) => {
            let state = if stop.load(Ordering::Relaxed) {
                "stopped"
            } else if rep.failed + rep.changed > 0 {
                "partial"
            } else {
                "done"
            };
            db.conn.execute(
                "UPDATE dedups SET state = ?2, summary = ?3 WHERE id = ?1",
                params![id, state, summary(rep)],
            )?;
        }
        Err(e) => {
            let _ = db.conn.execute(
                "UPDATE dedups SET state = 'failed', summary = ?2 WHERE id = ?1",
                params![id, format!("{e:#}")],
            );
        }
    }
    r
}

fn run_inner(
    db: &Db,
    id: i64,
    index: Option<&Path>,
    stop: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Report> {
    let (folder, link, min_size, _) = load(db, id)?;
    let folder = crate::names::as_indexed_or_same(db, &check_folder(&folder, index)?);
    let mut r = base_report(db, &folder, link, min_size)?;
    if link == Link::Clone && !r.clones_possible {
        bail!(
            "{} is on {}, which can't share blocks between files: use hardlinks (--link hard), or keep the archive on btrfs, XFS or APFS",
            folder.display(),
            r.filesystem.as_deref().unwrap_or("a filesystem")
        );
    }
    db.conn.execute(
        "UPDATE dedups SET state = 'running' WHERE id = ?1",
        params![id],
    )?;
    let mut journal = db.conn.prepare_cached(
        "INSERT INTO dedup_links(dedup_id, path, keeper, size, mtime, sha256, how, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    // a clone leaves no trace in the inode; an earlier run's journal says what's shared already
    let mut cloned_before = db.conn.prepare_cached(
        "SELECT 1 FROM dedup_links WHERE path = ?1 AND keeper = ?2 AND size = ?3 AND mtime = ?4 AND how = 'clone' LIMIT 1",
    )?;
    let inside = |p: &Path| {
        p.parent()
            .and_then(|d| paths::canonical(d).ok())
            .is_some_and(|d| d.starts_with(&folder))
    };
    let (mut seen, mut seen_bytes, mut tick) = (0u64, 0u64, Instant::now());
    'groups: for (sha, size) in groups(db, &folder, min_size)? {
        let sets = plan_group(size, members(db, &folder, &sha)?, link, &mut r);
        let mut any = false;
        for set in &sets {
            // the keeper is re-read once for its set
            let mut keeper_ok: Option<bool> = None;
            // inodes some name of which wasn't linked: not freed
            let mut kept_alive: HashSet<u64> = HashSet::new();
            for (dup, role) in &set.others {
                let frees = match role {
                    Role::Already => {
                        r.already += 1;
                        continue;
                    }
                    Role::Along => continue,
                    Role::Link { frees } => *frees,
                };
                if stop.load(Ordering::Relaxed) {
                    break 'groups;
                }
                seen += 1;
                seen_bytes += size;
                if tick.elapsed().as_millis() >= 250 {
                    tick = Instant::now();
                    progress(seen, seen_bytes);
                }
                let fail = |kept: &mut HashSet<u64>| {
                    kept.insert(dup.ino);
                };
                if link == Link::Clone
                    && cloned_before
                        .query_row(
                            params![
                                paths::to_bytes(&dup.path),
                                paths::to_bytes(&set.keeper.path),
                                size as i64,
                                dup.mtime
                            ],
                            |_| Ok(()),
                        )
                        .optional()?
                        .is_some()
                {
                    r.already += 1;
                    continue;
                }
                if !inside(&dup.path) || !inside(&set.keeper.path) {
                    r.elsewhere += 1;
                    fail(&mut kept_alive);
                    push(
                        &mut r.problems,
                        format!(
                            "{} is reached through a link out of the folder: left alone",
                            dup.path.display()
                        ),
                    );
                    continue;
                }
                let k_ok = *keeper_ok.get_or_insert_with(|| {
                    hash_file(&set.keeper.path).is_ok_and(|h| h.as_slice() == sha.as_slice())
                });
                if !k_ok {
                    r.changed += 1;
                    fail(&mut kept_alive);
                    push(
                        &mut r.problems,
                        format!(
                            "{} doesn't read back as the index has it: its copies are left alone",
                            set.keeper.path.display()
                        ),
                    );
                    continue;
                }
                match hash_file(&dup.path) {
                    Ok(h) if h.as_slice() == sha.as_slice() => {}
                    Ok(_) => {
                        r.changed += 1;
                        fail(&mut kept_alive);
                        push(
                            &mut r.problems,
                            format!(
                                "{} changed since the last scan: left alone",
                                dup.path.display()
                            ),
                        );
                        continue;
                    }
                    Err(e) => {
                        r.changed += 1;
                        fail(&mut kept_alive);
                        push(
                            &mut r.problems,
                            format!("can't read {}: {e}", dup.path.display()),
                        );
                        continue;
                    }
                }
                let done = match link {
                    Link::Hard => hardlink(&set.keeper, dup),
                    Link::Clone => clone(&set.keeper, dup, size, &sha),
                };
                match done {
                    Ok(true) => {
                        any = true;
                        r.linked += 1;
                        if frees && !kept_alive.contains(&dup.ino) {
                            r.saved_bytes += size;
                        }
                        // and in the chain of custody, in the same transaction
                        let (dup_b, keeper_b) = (
                            paths::to_bytes(&dup.path),
                            paths::to_bytes(&set.keeper.path),
                        );
                        let ev =
                            Event::new(Action::Link, ("dedup", id), &dup_b, Some(&sha[..]), size)
                                .to(&keeper_b, Some(&sha[..]));
                        custody::journaled(&db.conn, &ev, || {
                            journal.execute(params![
                                id,
                                dup_b,
                                keeper_b,
                                size as i64,
                                dup.mtime,
                                sha,
                                if link == Link::Hard {
                                    "hardlink"
                                } else {
                                    "clone"
                                },
                                now()
                            ])
                        })?;
                    }
                    Ok(false) => {
                        r.changed += 1;
                        fail(&mut kept_alive);
                        push(
                            &mut r.problems,
                            format!(
                                "{} changed while it was being linked: left alone",
                                dup.path.display()
                            ),
                        );
                    }
                    Err(e) => {
                        r.failed += 1;
                        fail(&mut kept_alive);
                        push(
                            &mut r.problems,
                            format!("couldn't link {}: {e:#}", dup.path.display()),
                        );
                    }
                }
            }
        }
        if any {
            r.groups += 1;
        }
        example(&mut r, size, &sha, &sets);
    }
    progress(seen, seen_bytes);
    Ok(r)
}

/// A name for our own temporary file next to `path`.
#[cfg(unix)]
fn temp_beside(path: &Path, what: &str) -> Result<PathBuf> {
    let dir = path.parent().context("a file without a folder")?;
    Ok(dir.join(format!(
        ".{}.backdown-{what}",
        path.file_name().unwrap_or_default().to_string_lossy()
    )))
}

/// Keep a folder's modification time across a change to its entries.
#[cfg(unix)]
fn keep_dir_time(dir: &Path) -> impl FnOnce() + use<> {
    let before = fs::metadata(dir).and_then(|m| m.modified()).ok();
    let dir = dir.to_path_buf();
    move || {
        if let (Some(t), Ok(f)) = (before, File::open(&dir)) {
            let _ = f.set_times(fs::FileTimes::new().set_modified(t));
        }
    }
}

/// Replace `dup` with a hardlink to `keeper`: a new name for the keeper first, checked, then
/// renamed over the duplicate. `Ok(false)` if the duplicate changed meanwhile.
#[cfg(unix)]
fn hardlink(keeper: &Now, dup: &Now) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let tmp = temp_beside(&dup.path, "link")?;
    let _ = fs::remove_file(&tmp); // ours, from an interrupted run
    let restore = keep_dir_time(dup.path.parent().context("no folder")?);
    fs::hard_link(&keeper.path, &tmp).context("making the link")?;
    let t = fs::symlink_metadata(&tmp)?;
    let still = fs::symlink_metadata(&dup.path)
        .is_ok_and(|d| d.ino() == dup.ino && d.len() == dup.size && mtime(&d) == dup.mtime);
    if t.ino() != keeper.ino || t.dev() != keeper.class.dev || !still {
        let _ = fs::remove_file(&tmp);
        restore();
        return Ok(false);
    }
    if let Err(e) = fs::rename(&tmp, &dup.path) {
        let _ = fs::remove_file(&tmp);
        restore();
        return Err(e).context("putting the link in place");
    }
    restore();
    Ok(true)
}

#[cfg(not(unix))]
fn hardlink(_keeper: &Now, _dup: &Now) -> Result<bool> {
    bail!("not available on this system")
}

/// Make `dup` share `keeper`'s blocks, in place: the kernel compares the bytes itself under lock
/// and shares them only if identical. The file keeps its inode, names and all its metadata
/// (sharing blocks isn't a change to its contents, so not even its dates move). `Ok(false)` if the
/// contents turned out different.
#[cfg(target_os = "linux")]
fn clone(keeper: &Now, dup: &Now, size: u64, _sha: &[u8]) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let k = File::open(&keeper.path)?;
    // writable if we may; the owner of a read-only file may still share its blocks (Linux 5.0+)
    let d = match fs::OpenOptions::new().write(true).open(&dup.path) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => File::open(&dup.path)?,
        Err(e) => return Err(e).context("opening the duplicate"),
    };
    let m = d.metadata()?;
    if m.ino() != dup.ino || m.len() != size {
        return Ok(false);
    }
    match fastcopy::dedupe_range(&k, &d, size).context("sharing blocks")? {
        fastcopy::Dedupe::Shared => Ok(true),
        fastcopy::Dedupe::Differs => Ok(false),
    }
}

/// macOS has no in-place sharing: a clone of the keeper is made beside the duplicate, given the
/// duplicate's dates, checked to carry exactly the duplicate's permissions, owner, flags and
/// attributes (the keeper's: they're in the same class) and content, then renamed over it.
#[cfg(target_os = "macos")]
fn clone(keeper: &Now, dup: &Now, _size: u64, sha: &[u8]) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;
    if dup.nlink > 1 {
        return Ok(false); // a new file would part it from its other names
    }
    let tmp = temp_beside(&dup.path, "clone")?;
    let _ = fs::remove_file(&tmp);
    let restore = keep_dir_time(dup.path.parent().context("no folder")?);
    let k = File::open(&keeper.path)?;
    let out = fastcopy::clone_new(&k, &tmp).context("cloning")?;
    let dm = fs::symlink_metadata(&dup.path)?;
    out.set_times(
        fs::FileTimes::new()
            .set_modified(dm.modified()?)
            .set_accessed(dm.accessed()?),
    )?;
    drop(out);
    let same = stat(&tmp, Link::Clone)?.is_some_and(|t| t.class == dup.class)
        && dm.ino() == dup.ino
        && dm.len() == dup.size
        && hash_file(&tmp).is_ok_and(|h| h.as_slice() == sha);
    if !same {
        let _ = fs::remove_file(&tmp);
        restore();
        return Ok(false);
    }
    if let Err(e) = fs::rename(&tmp, &dup.path) {
        let _ = fs::remove_file(&tmp);
        restore();
        return Err(e).context("putting the clone in place");
    }
    restore();
    Ok(true)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn clone(_keeper: &Now, _dup: &Now, _size: u64, _sha: &[u8]) -> Result<bool> {
    bail!("clones aren't available on this system")
}
