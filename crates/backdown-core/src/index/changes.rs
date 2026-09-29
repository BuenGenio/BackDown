//! A scan from a change set: the previous scan, with only the paths a change log named looked at
//! again. The result is, row for row, what a walk would have written (the tests check exactly
//! that, over random trees and random edits).
//!
//! A change log names what changed, not always everything that changed: a folder renamed or moved
//! is one entry, though every path below it changed. So each named path is looked at as it is now:
//!
//! - gone: its rows, and everything below it, are closed;
//! - a file: recorded as it is (and, if it has other names, those are looked at too: changing a
//!   file through one hard link changes them all);
//! - a folder that's still the one on record (same inode, not born since the previous scan
//!   began): listed again, its files looked at, what's no longer there closed, and new folders in
//!   it walked;
//! - a folder that isn't (new, moved here, put in place of another, or asked for whole): walked;
//! - anything else (a symlink, a pipe): not recorded, as a walk doesn't record it.
//!
//! Then the totals of every folder above something that changed are added up again from what's
//! directly in them, deepest first, and everything else carries over to the new scan unchanged.
//!
//! Anything that doesn't add up means the change log missed something, so a walk is made
//! instead: a path whose folder is gone or new although the log never said so, a folder replaced
//! behind the log's back, the root itself replaced. All of that is worked out before anything is
//! written.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

use super::{
    Fallback, Seen, Stats, Totals, Writer, dir_identity, inode, mtime_of, walk_items, write_items,
};
use crate::paths;

/// What changed, by path: what a change log says, turned into paths under the scan's root.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangeSet {
    /// What's at these paths may have changed: created, modified, deleted, renamed away or here,
    /// replaced.
    pub paths: BTreeSet<PathBuf>,
    /// Anything at or below these folders may have changed: they're walked whole.
    pub subtrees: BTreeSet<PathBuf>,
    /// Files (by inode number) the log named: every name they have on record is looked at, as
    /// changing a file through one hard link changes them all.
    pub inodes: BTreeSet<u64>,
    /// The change log can't say what changed (events were dropped, its history was reset…): why.
    pub full: Option<String>,
}

impl ChangeSet {
    pub fn new() -> ChangeSet {
        ChangeSet::default()
    }

    pub fn path(&mut self, p: impl Into<PathBuf>) {
        self.paths.insert(p.into());
    }

    pub fn subtree(&mut self, p: impl Into<PathBuf>) {
        self.subtrees.insert(p.into());
    }

    pub fn inode(&mut self, ino: u64) {
        self.inodes.insert(ino);
    }

    /// Everything must be read again (the first reason given is kept).
    pub fn everything(&mut self, why: impl Into<String>) {
        if self.full.is_none() {
            self.full = Some(why.into());
        }
    }

    /// How many paths it names.
    pub fn len(&self) -> usize {
        self.paths.len() + self.subtrees.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
            && self.subtrees.is_empty()
            && self.inodes.is_empty()
            && self.full.is_none()
    }

    /// Only what concerns `root` (a change log covers a whole volume): paths at or below it. A
    /// path above it may have put another folder in its place, so the root is looked at; a
    /// subtree at or above it is the whole root.
    pub fn within(self, root: &Path) -> ChangeSet {
        let mut out = ChangeSet {
            full: self.full,
            inodes: self.inodes,
            ..ChangeSet::default()
        };
        for p in self.paths {
            if p.starts_with(root) {
                out.paths.insert(p);
            } else if root.starts_with(&p) {
                out.paths.insert(root.to_path_buf());
            }
        }
        for s in self.subtrees {
            if root.starts_with(&s) {
                out.everything("the change log asks for the whole drive to be read again");
            } else if s.starts_with(root) {
                out.subtrees.insert(s);
            }
        }
        out
    }
}

/// What applying a change set needs to know about the scan.
pub(crate) struct Ctx {
    /// the scan stays on the root's filesystem
    pub one_fs: bool,
    /// the root's device now, and in the scan the rows come from (they differ when a drive was
    /// plugged in again)
    pub root_dev: Option<u64>,
    pub old_dev: Option<u64>,
    /// when the scan the rows come from began (unix seconds): a folder born since then can't be
    /// the one on record, whatever its inode number
    pub since: i64,
}

/// A file as it is now.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FileStat {
    pub size: u64,
    pub mtime: i64,
    pub dev: Option<i64>,
    pub ino: Option<i64>,
}

/// A folder's identity now.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DirStat {
    pub dev: Option<i64>,
    pub ino: Option<i64>,
    /// its birth time, where the filesystem keeps one
    pub born: Option<i64>,
}

/// What's at a path now (not following symlinks, as a walk doesn't).
pub(crate) enum Found {
    /// Nothing, or it can't be looked at (why).
    Gone(Option<String>),
    File(FileStat, u64),
    Dir(DirStat),
    /// A symlink, a pipe, a device: not recorded.
    Other,
}

pub(crate) fn look(path: &Path) -> Found {
    let m = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return Found::Gone(None);
        }
        Err(e) => return Found::Gone(Some(format!("{}: {e}", path.display()))),
    };
    if m.is_file() {
        let (dev, ino) = inode(&m);
        Found::File(
            FileStat {
                size: m.len(),
                mtime: mtime_of(&m),
                dev,
                ino,
            },
            links(&m, path),
        )
    } else if m.is_dir() {
        let (dev, ino) = dir_identity(path);
        let born = m
            .created()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64);
        Found::Dir(DirStat { dev, ino, born })
    } else {
        Found::Other
    }
}

/// How many names (hard links) a file has.
#[cfg(unix)]
fn links(m: &std::fs::Metadata, _: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    m.nlink()
}

#[cfg(windows)]
fn links(_: &std::fs::Metadata, path: &Path) -> u64 {
    super::usn::sys::link_count(path).unwrap_or(1)
}

#[cfg(not(any(unix, windows)))]
fn links(_: &std::fs::Metadata, _: &Path) -> u64 {
    1
}

/// What a change set comes to, worked out before anything is written.
#[derive(Debug, Default)]
pub(crate) struct Plan {
    root: PathBuf,
    /// nothing here any more: its file row, its folder row and everything below are closed
    gone: BTreeSet<PathBuf>,
    /// no folder here any more (a file or a link took its place): its folder row and all below
    not_dir: BTreeSet<PathBuf>,
    /// no file here any more (a folder or a link took its place)
    not_file: BTreeSet<PathBuf>,
    /// a folder that can't be listed, or has another filesystem mounted on it: whatever was
    /// below it is closed (it stays, empty, as a walk records it)
    emptied: BTreeSet<PathBuf>,
    /// files to record as they are now
    files: BTreeMap<PathBuf, FileStat>,
    /// folders to walk whole
    walks: BTreeSet<PathBuf>,
    /// folders listed again
    relisted: BTreeSet<PathBuf>,
    /// what couldn't be looked at
    errors: Vec<String>,
}

/// A folder on record.
#[derive(Clone, Copy)]
struct DirRow {
    ino: Option<i64>,
}

struct Resolver<'a> {
    conn: &'a Connection,
    root: &'a Path,
    changes: &'a ChangeSet,
    ctx: &'a Ctx,
    plan: Plan,
    queue: BTreeSet<PathBuf>,
    done: HashSet<PathBuf>,
    /// parents already checked
    parents: HashMap<PathBuf, Parent>,
}

/// Whether a folder is the one on record.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Same {
    Yes,
    No,
    Unsure,
}

/// What a folder means for what's in it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Parent {
    /// on record (still the same folder, or looked at again): what's in it is seen by a walk
    Fine,
    /// another filesystem is mounted on it: a walk that stays on one filesystem sees nothing in it
    Foreign,
    /// new, gone or replaced, though the change log never said so
    Missed,
}

fn odd(msg: String) -> Fallback {
    Fallback::Odd(msg)
}

/// Works out what the change set means for the rows, reading only.
pub(crate) fn resolve(
    conn: &Connection,
    root: &Path,
    changes: &ChangeSet,
    ctx: &Ctx,
) -> std::result::Result<Plan, Fallback> {
    if let Some(why) = &changes.full {
        return Err(odd(why.clone()));
    }
    if dir_row(conn, root)?.is_none() {
        return Err(odd(
            "the previous scan has no folder totals for the root".into()
        ));
    }
    let mut r = Resolver {
        conn,
        root,
        changes,
        ctx,
        plan: Plan {
            root: root.to_path_buf(),
            ..Plan::default()
        },
        queue: changes
            .paths
            .iter()
            .chain(&changes.subtrees)
            .filter(|p| p.starts_with(root))
            .cloned()
            .collect(),
        done: HashSet::new(),
        parents: HashMap::new(),
    };
    // files the log named by inode: every name they have on record
    for ino in &changes.inodes {
        for p in r.on_record("files", None, *ino as i64)? {
            r.queue.insert(p);
        }
    }
    // parents before children: a folder's fate (walked, listed again) is known before what's in it
    while let Some(p) = r.queue.pop_first() {
        if !r.done.insert(p.clone()) || r.walked(&p) {
            continue;
        }
        // what was here had other names (hard links)? Whatever changed it changed them too
        if let Some((dev, ino)) = identity_on_record(conn, "files", &p)? {
            for o in r.on_record("files", dev, ino)? {
                if !r.done.contains(&o) {
                    r.queue.insert(o);
                }
            }
        }
        r.entry(&p)?;
    }
    let mut plan = r.plan;
    plan.prune();
    Ok(plan)
}

impl Resolver<'_> {
    /// At or below a folder that's walked whole.
    fn walked(&self, p: &Path) -> bool {
        p.ancestors().any(|a| self.plan.walks.contains(a))
    }

    /// Did the change log name this path?
    fn reported(&self, p: &Path) -> bool {
        self.changes.paths.contains(p) || self.changes.subtrees.contains(p)
    }

    fn entry(&mut self, p: &Path) -> std::result::Result<(), Fallback> {
        let root = self.root;
        let force_walk = self.changes.subtrees.contains(p);
        let found = look(p);
        if p == root {
            return match found {
                Found::Dir(d)
                    if !self.other_fs(&d) && !force_walk && self.same_dir(p, &d)? == Same::Yes =>
                {
                    self.relist(p)
                }
                Found::Dir(_) => Err(odd("the root folder isn't the one scanned before".into())),
                Found::Gone(why) => Err(odd(format!(
                    "the root folder can't be read{}",
                    why.map(|w| format!(": {w}")).unwrap_or_default()
                ))),
                _ => Err(odd("the root is no longer a folder".into())),
            };
        }
        if let Found::Gone(why) = found {
            self.plan.errors.extend(why);
            // what's no longer a folder on the way down must be something the log named
            let mut top = p.to_path_buf();
            while let Some(parent) = top.parent()
                && parent != root
                && parent.starts_with(root)
                && !std::fs::symlink_metadata(parent).is_ok_and(|m| m.is_dir())
            {
                top = parent.to_path_buf();
            }
            if top != p && !self.reported(&top) && !self.walked(&top) {
                return Err(odd(format!(
                    "the change log didn't say that {} changed",
                    top.display()
                )));
            }
            if top == p {
                self.parent(p)?;
            }
            self.plan.gone.insert(p.to_path_buf());
            return Ok(());
        }
        if self.parent(p)? == Parent::Foreign {
            // inside another filesystem mounted here: a walk doesn't see it
            self.plan.gone.insert(p.to_path_buf());
            return Ok(());
        }
        match found {
            Found::Gone(_) | Found::Other => {
                self.plan.gone.insert(p.to_path_buf());
            }
            Found::File(st, links) => self.file(p, st, links)?,
            Found::Dir(d) => {
                if file_row(self.conn, p)? {
                    self.plan.not_file.insert(p.to_path_buf());
                }
                if self.other_fs(&d) {
                    self.plan.emptied.insert(p.to_path_buf());
                } else if !force_walk && self.same_dir(p, &d)? == Same::Yes {
                    self.relist(p)?;
                } else {
                    self.plan.walks.insert(p.to_path_buf());
                }
            }
        }
        Ok(())
    }

    /// A file as it is now; its other names (hard links) are looked at too.
    fn file(&mut self, p: &Path, st: FileStat, links: u64) -> std::result::Result<(), Fallback> {
        if dir_row(self.conn, p)?.is_some() {
            self.plan.not_dir.insert(p.to_path_buf());
        }
        self.plan.files.insert(p.to_path_buf(), st);
        self.done.insert(p.to_path_buf());
        if links > 1 {
            for other in self.other_names(p, &st)? {
                if other.starts_with(self.root) && !self.done.contains(&other) {
                    self.queue.insert(other);
                }
            }
        }
        Ok(())
    }

    fn other_names(&self, p: &Path, st: &FileStat) -> Result<Vec<PathBuf>> {
        other_names(self.conn, self.ctx, p, st.dev, st.ino)
    }

    /// Every current row of `table` ("files" or "dirs") under this root with this inode (on
    /// the root's device then or now, or `dev`).
    fn on_record(&self, table: &str, dev: Option<i64>, ino: i64) -> Result<Vec<PathBuf>> {
        let devs: BTreeSet<i64> = [
            self.ctx.old_dev.map(|d| d as i64),
            self.ctx.root_dev.map(|d| d as i64),
            dev,
        ]
        .into_iter()
        .flatten()
        .collect();
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT path FROM {table} WHERE dev = ?1 AND ino = ?2 AND current = 1"
        ))?;
        let mut out = Vec::new();
        for d in devs {
            for p in stmt.query_map(params![d, ino], |r| r.get::<_, Vec<u8>>(0))? {
                let p = paths::from_bytes(&p?);
                if p.starts_with(self.root) {
                    out.push(p);
                }
            }
        }
        Ok(out)
    }

    /// Another filesystem is mounted here (a walk that stays on one filesystem records the folder
    /// but not what's in it).
    fn other_fs(&self, d: &DirStat) -> bool {
        self.ctx.one_fs && matches!((self.ctx.root_dev, d.dev), (Some(r), Some(x)) if r != x as u64)
    }

    /// Still the folder on record? The same inode, and born before the previous scan began: then
    /// it existed alongside the one recorded, so it is that one. Born since, it may be, or may be
    /// a new folder that got a freed inode number: unsure.
    fn same_dir(&self, p: &Path, d: &DirStat) -> Result<Same> {
        let Some(row) = dir_row(self.conn, p)? else {
            return Ok(Same::No);
        };
        Ok(match (row.ino, d.ino, d.born) {
            (Some(a), Some(b), _) if a != b => Same::No,
            (None, _, _) | (_, None, _) => Same::No,
            (_, _, Some(born)) if born >= self.ctx.since => Same::Unsure,
            _ => Same::Yes,
        })
    }

    /// The folder `p` is in: it must be on record and still the same one, unless the log named
    /// it (then it was looked at first: parents come before children).
    fn parent(&mut self, p: &Path) -> std::result::Result<Parent, Fallback> {
        let Some(parent) = p.parent() else {
            return Ok(Parent::Fine);
        };
        if !parent.starts_with(self.root) || self.walked(parent) {
            return Ok(Parent::Fine);
        }
        let verdict = match self.parents.get(parent) {
            Some(v) => *v,
            None => {
                // named by the log but not looked at yet can't be (parents come first)
                let unseen = self.reported(parent) && !self.done.contains(parent);
                let v = match look(parent) {
                    Found::Dir(d) if self.other_fs(&d) => Parent::Foreign,
                    Found::Dir(_) if self.plan.relisted.contains(parent) => Parent::Fine,
                    Found::Dir(d) if !unseen => match self.same_dir(parent, &d)? {
                        Same::Yes => Parent::Fine,
                        // it can't be told from a new folder: walk it, which is right either way
                        Same::Unsure if parent != self.root => {
                            self.plan.walks.insert(parent.to_path_buf());
                            Parent::Fine
                        }
                        _ => Parent::Missed,
                    },
                    _ => Parent::Missed,
                };
                self.parents.insert(parent.to_path_buf(), v);
                v
            }
        };
        if verdict == Parent::Missed {
            return Err(odd(format!(
                "the change log didn't say that {} changed",
                parent.display()
            )));
        }
        Ok(verdict)
    }

    /// Lists a folder on record again: its files as they are now, new folders walked, what's no
    /// longer there closed.
    fn relist(&mut self, d: &Path) -> std::result::Result<(), Fallback> {
        self.plan.relisted.insert(d.to_path_buf());
        // a folder with other names (HFS+ folder hard links, as old Time Machine backups have):
        // what changed in it changed there too
        if let Some((dev, ino)) = identity_on_record(self.conn, "dirs", d)? {
            for o in self.on_record("dirs", dev, ino)? {
                if o != d && !self.done.contains(&o) {
                    self.queue.insert(o);
                }
            }
        }
        let listing = match std::fs::read_dir(d) {
            Ok(l) => l,
            Err(e) => {
                self.plan.errors.push(format!("{}: {e}", d.display()));
                self.plan.emptied.insert(d.to_path_buf());
                return Ok(());
            }
        };
        let mut here: HashSet<PathBuf> = HashSet::new();
        for entry in listing {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    self.plan.errors.push(format!("{}: {e}", d.display()));
                    continue;
                }
            };
            let c = entry.path();
            here.insert(c.clone());
            if self.done.contains(&c) {
                continue;
            }
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(e) => {
                    self.plan.errors.push(format!("{}: {e}", c.display()));
                    self.plan.gone.insert(c);
                    continue;
                }
            };
            if ft.is_file() {
                match look(&c) {
                    Found::File(st, links) => self.file(&c, st, links)?,
                    Found::Gone(why) => {
                        self.plan.errors.extend(why);
                        self.plan.gone.insert(c);
                    }
                    _ => {
                        self.plan.gone.insert(c);
                    }
                }
            } else if ft.is_dir() {
                match look(&c) {
                    Found::Dir(cd) => {
                        if file_row(self.conn, &c)? {
                            self.plan.not_file.insert(c.clone());
                        }
                        if self.other_fs(&cd) {
                            self.plan.emptied.insert(c);
                        } else if self.same_dir(&c, &cd)? != Same::Yes {
                            self.plan.walks.insert(c);
                        }
                        // the same folder: what's in it is as recorded, unless the log named it
                    }
                    Found::Gone(why) => {
                        self.plan.errors.extend(why);
                        self.plan.gone.insert(c);
                    }
                    _ => {
                        self.plan.gone.insert(c);
                    }
                }
            } else {
                // a symlink or something special: a walk doesn't record it
                self.plan.gone.insert(c);
            }
        }
        // what the index had in it that isn't there now
        for c in children(self.conn, d)? {
            if !here.contains(&c) {
                self.plan.gone.insert(c);
            }
        }
        Ok(())
    }
}

impl Plan {
    /// Whether `p` is inside something that's no longer a folder (so there's nothing to add up).
    fn inside_removed(&self, p: &Path) -> bool {
        p.ancestors()
            .skip(1)
            .any(|a| self.gone.contains(a) || self.not_dir.contains(a) || self.emptied.contains(a))
    }

    /// Drops what a walk covers anyway, and what's inside something removed whole.
    fn prune(&mut self) {
        // by ancestors, so a folder deleted with its 100,000 files named one by one stays cheap
        let removed: HashSet<PathBuf> = self.gone.iter().chain(&self.not_dir).cloned().collect();
        let inside = |p: &Path| p.ancestors().skip(1).any(|a| removed.contains(a));
        self.gone.retain(|p| !inside(p));
        self.not_dir.retain(|p| !inside(p));
        self.not_file.retain(|p| !inside(p));
        self.files.retain(|p, _| !inside(p));
        self.relisted.retain(|p| !inside(p));
        self.emptied.retain(|p| !inside(p));
        self.walks.retain(|p| !inside(p));
        let walks: HashSet<PathBuf> = self.walks.iter().cloned().collect();
        let below = |p: &Path| p.ancestors().skip(1).any(|a| walks.contains(a));
        self.walks.retain(|p| !below(p));
        self.gone.retain(|p| !below(p));
        self.not_dir.retain(|p| !below(p));
        self.not_file.retain(|p| !below(p));
        self.emptied.retain(|p| !below(p) && !walks.contains(p));
        self.files.retain(|p, _| !below(p));
        self.relisted.retain(|p| !below(p) && !walks.contains(p));
    }
}

/// Writes what a plan says, then carries everything else over to this scan.
pub(crate) fn execute(
    w: &mut Writer,
    plan: Plan,
    ctx: &Ctx,
    progress: &mut dyn FnMut(&Stats),
) -> Result<()> {
    for e in &plan.errors {
        w.note_error(e.clone());
    }
    let conn = w.conn;
    // what's gone
    for p in &plan.gone {
        w.st.removed += close_file(conn, p)?;
        w.st.removed += close_below(conn, p, true)?;
    }
    for p in plan.not_dir.iter().chain(&plan.emptied) {
        w.st.removed += close_below(conn, p, plan.not_dir.contains(p))?;
    }
    for p in &plan.not_file {
        w.st.removed += close_file(conn, p)?;
    }
    // files as they are now
    for (p, st) in &plan.files {
        w.file(p, st.size, st.mtime, st.dev, st.ino)?;
        w.tick(progress)?;
    }
    // folders walked whole
    let mut linked = Vec::new();
    for d in &plan.walks {
        let mut seen = Seen::default();
        write_items(
            w,
            walk_items(d, ctx.one_fs),
            false,
            Some(&mut seen),
            progress,
        )?;
        w.st.removed += close_unseen_below(conn, d, &seen)?;
        linked.append(&mut seen.linked);
    }
    // a file met in a walk that has other names elsewhere: changing it through one name changed
    // them all, and only that one may have been named in the log
    let mut relinked: BTreeSet<PathBuf> = BTreeSet::new();
    for (dev, ino, p) in &linked {
        for o in other_names(conn, ctx, p, *dev, Some(*ino))? {
            let covered = plan.files.contains_key(&o)
                || o.ancestors().any(|a| plan.walks.contains(a))
                || plan.inside_removed(&o)
                || !o.starts_with(&plan.root);
            if !covered
                && relinked.insert(o.clone())
                && let Found::File(st, _) = look(&o)
            {
                w.file(&o, st.size, st.mtime, st.dev, st.ino)?;
            }
        }
    }
    // the totals of every folder above something that changed, deepest first
    let root = plan.root.as_path();
    let mut up: BTreeSet<(Reverse<usize>, PathBuf)> = BTreeSet::new();
    let changed = plan
        .gone
        .iter()
        .chain(&plan.not_dir)
        .chain(&plan.not_file)
        .chain(plan.files.keys())
        .chain(&plan.walks)
        .chain(&plan.emptied)
        .chain(&plan.relisted)
        .chain(&relinked);
    for c in changed {
        let own = plan.relisted.contains(c) || plan.emptied.contains(c);
        for a in c.ancestors().skip(usize::from(!own)) {
            if !a.starts_with(root) {
                break;
            }
            if plan.gone.contains(a) || plan.not_dir.contains(a) || plan.inside_removed(a) {
                continue; // not a folder any more: nothing to add up
            }
            if !up.insert((Reverse(a.components().count()), a.to_path_buf())) {
                break; // it and everything above it are in already
            }
        }
    }
    for (_, d) in &up {
        if d != root && !std::fs::symlink_metadata(d).is_ok_and(|m| m.is_dir()) {
            continue; // something named below it was a folder once, but it's a file now
        }
        let t = totals_of(conn, d)?;
        let (dev, ino) = dir_identity(d);
        w.dir(d, d == root, t, dev, ino)?;
        w.tick(progress)?;
    }
    // everything else is as it was
    if let (Some(old), Some(new)) = (ctx.old_dev, ctx.root_dev)
        && old != new
    {
        for table in ["files", "dirs"] {
            conn.execute(
                &format!(
                    "UPDATE {table} SET dev = ?3 WHERE root_id = ?1 AND current = 1 AND dev = ?2"
                ),
                params![w.root_id, old as i64, new as i64],
            )?;
        }
    }
    for table in ["files", "dirs"] {
        conn.execute(
            &format!(
                "UPDATE {table} SET last_scan = ?2 WHERE root_id = ?1 AND current = 1 AND last_scan < ?2"
            ),
            params![w.root_id, w.scan],
        )?;
    }
    let (files, bytes): (i64, i64) = conn.query_row(
        "SELECT files, bytes FROM dirs WHERE path = ?1 AND current = 1",
        params![paths::to_bytes(root)],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    w.st.files = files as u64;
    w.st.bytes = bytes as u64;
    Ok(())
}

/// The other names of a file with several (hard links): on record by inode (unix), or asked of
/// the system (Windows).
#[cfg(unix)]
fn other_names(
    conn: &Connection,
    ctx: &Ctx,
    p: &Path,
    dev: Option<i64>,
    ino: Option<i64>,
) -> Result<Vec<PathBuf>> {
    let Some(ino) = ino else {
        return Ok(Vec::new());
    };
    let devs = [ctx.old_dev, ctx.root_dev, dev.map(|d| d as u64)];
    let mut stmt =
        conn.prepare_cached("SELECT path FROM files WHERE dev = ?1 AND ino = ?2 AND current = 1")?;
    let mut out = Vec::new();
    for dev in devs.iter().flatten().collect::<BTreeSet<_>>() {
        for path in stmt.query_map(params![*dev as i64, ino], |r| r.get::<_, Vec<u8>>(0))? {
            let path = paths::from_bytes(&path?);
            if path != p {
                out.push(path);
            }
        }
    }
    Ok(out)
}

#[cfg(windows)]
fn other_names(
    _: &Connection,
    _: &Ctx,
    p: &Path,
    _: Option<i64>,
    _: Option<i64>,
) -> Result<Vec<PathBuf>> {
    Ok(super::usn::sys::link_names(p)
        .into_iter()
        .filter(|o| o != p)
        .collect())
}

#[cfg(not(any(unix, windows)))]
fn other_names(
    _: &Connection,
    _: &Ctx,
    _: &Path,
    _: Option<i64>,
    _: Option<i64>,
) -> Result<Vec<PathBuf>> {
    Ok(Vec::new())
}

// ---------- the rows

/// The device and inode on record for the current row at `p` in `table`, if it has them.
fn identity_on_record(
    conn: &Connection,
    table: &str,
    p: &Path,
) -> Result<Option<(Option<i64>, i64)>> {
    Ok(conn
        .prepare_cached(&format!(
            "SELECT dev, ino FROM {table} WHERE path = ?1 AND current = 1 AND ino IS NOT NULL"
        ))?
        .query_row(params![paths::to_bytes(p)], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?)
}

fn dir_row(conn: &Connection, p: &Path) -> Result<Option<DirRow>> {
    Ok(conn
        .prepare_cached("SELECT ino FROM dirs WHERE path = ?1 AND current = 1")?
        .query_row(params![paths::to_bytes(p)], |r| {
            Ok(DirRow { ino: r.get(0)? })
        })
        .optional()?)
}

fn file_row(conn: &Connection, p: &Path) -> Result<bool> {
    Ok(conn
        .prepare_cached("SELECT 1 FROM files WHERE path = ?1 AND current = 1")?
        .query_row(params![paths::to_bytes(p)], |_| Ok(()))
        .optional()?
        .is_some())
}

/// What the index has directly in a folder.
fn children(conn: &Connection, d: &Path) -> Result<Vec<PathBuf>> {
    let db = paths::to_bytes(d);
    let mut out = Vec::new();
    for sql in [
        "SELECT path FROM files WHERE dir = ?1 AND current = 1",
        "SELECT path FROM dirs WHERE parent = ?1 AND current = 1",
    ] {
        let mut stmt = conn.prepare_cached(sql)?;
        for p in stmt.query_map(params![db], |r| r.get::<_, Vec<u8>>(0))? {
            out.push(paths::from_bytes(&p?));
        }
    }
    Ok(out)
}

/// The byte range of every path strictly below `d`.
fn below(d: &Path) -> (Vec<u8>, Vec<u8>) {
    let lo = paths::dir_prefix(d);
    let mut hi = lo.clone();
    // the prefix ends in a separator byte (never 0xFF), so one more than it bounds everything below
    *hi.last_mut().expect("a prefix is never empty") += 1;
    (lo, hi)
}

fn close_file(conn: &Connection, p: &Path) -> Result<u64> {
    Ok(conn
        .prepare_cached("UPDATE files SET current = 0 WHERE path = ?1 AND current = 1")?
        .execute(params![paths::to_bytes(p)])? as u64)
}

/// Closes every row strictly below `d`, and its own folder row if `itself`. Returns the files closed.
fn close_below(conn: &Connection, d: &Path, itself: bool) -> Result<u64> {
    let (lo, hi) = below(d);
    let n = conn
        .prepare_cached(
            "UPDATE files SET current = 0 WHERE current = 1 AND path >= ?1 AND path < ?2",
        )?
        .execute(params![lo, hi])? as u64;
    conn.prepare_cached(
        "UPDATE dirs SET current = 0 WHERE current = 1 AND path >= ?1 AND path < ?2",
    )?
    .execute(params![lo, hi])?;
    if itself {
        conn.prepare_cached("UPDATE dirs SET current = 0 WHERE current = 1 AND path = ?1")?
            .execute(params![paths::to_bytes(d)])?;
    }
    Ok(n)
}

/// After a walk of `d`: closes what the index had at or below it that the walk didn't see.
fn close_unseen_below(conn: &Connection, d: &Path, seen: &Seen) -> Result<u64> {
    let (lo, hi) = below(d);
    let ids = |sql: &str, extra: Option<Vec<u8>>| -> Result<Vec<i64>> {
        let mut stmt = conn.prepare_cached(sql)?;
        let rows = match extra {
            Some(own) => stmt
                .query_map(params![lo, hi, own], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<i64>>>()?,
            None => stmt
                .query_map(params![lo, hi], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<i64>>>()?,
        };
        Ok(rows)
    };
    let files = ids(
        "SELECT id FROM files WHERE current = 1 AND path >= ?1 AND path < ?2",
        None,
    )?;
    let dirs = ids(
        "SELECT id FROM dirs WHERE current = 1 AND ((path >= ?1 AND path < ?2) OR path = ?3)",
        Some(paths::to_bytes(d)),
    )?;
    let mut n = 0;
    let mut close_file = conn.prepare_cached("UPDATE files SET current = 0 WHERE id = ?1")?;
    for id in files.into_iter().filter(|i| !seen.files.contains(i)) {
        close_file.execute(params![id])?;
        n += 1;
    }
    let mut close_dir = conn.prepare_cached("UPDATE dirs SET current = 0 WHERE id = ?1")?;
    for id in dirs.into_iter().filter(|i| !seen.dirs.contains(i)) {
        close_dir.execute(params![id])?;
    }
    Ok(n)
}

/// A folder's totals from what's directly in it, as they now stand in the index.
fn totals_of(conn: &Connection, d: &Path) -> Result<Totals> {
    let db = paths::to_bytes(d);
    let (f, b, n): (i64, i64, i64) = conn
        .prepare_cached(
            "SELECT COUNT(*), COALESCE(SUM(size), 0), COALESCE(MAX(mtime), 0) FROM files
             WHERE dir = ?1 AND current = 1",
        )?
        .query_row(params![db], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    let (df, dbytes, dn): (i64, i64, i64) = conn
        .prepare_cached(
            "SELECT COALESCE(SUM(files), 0), COALESCE(SUM(bytes), 0), COALESCE(MAX(newest), 0)
             FROM dirs WHERE parent = ?1 AND current = 1",
        )?
        .query_row(params![db], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    Ok(Totals {
        files: (f + df) as u64,
        bytes: (b + dbytes) as u64,
        newest: n.max(dn).max(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn within_a_root() {
        let root = Path::new("/v/photos");
        let mut c = ChangeSet::new();
        c.path("/v/photos/a.jpg");
        c.path("/v/music/b.mp3");
        c.path("/v"); // above the root: the root may be another folder now
        c.subtree("/v/photos/2019");
        c.subtree("/v/music");
        let w = c.within(root);
        assert_eq!(
            w.paths
                .iter()
                .map(|p| p.to_str().unwrap())
                .collect::<Vec<_>>(),
            ["/v/photos", "/v/photos/a.jpg"]
        );
        assert_eq!(w.subtrees.len(), 1);
        assert!(w.full.is_none());
        let mut c = ChangeSet::new();
        c.subtree("/v");
        assert!(c.within(root).full.is_some());
    }

    #[test]
    fn below_bounds_only_what_is_below() {
        let (lo, hi) = below(Path::new("/a/b"));
        let inside = |p: &str| {
            let b = paths::to_bytes(Path::new(p));
            b >= lo && b < hi
        };
        assert!(inside("/a/b/c"));
        assert!(inside("/a/b/c/d"));
        assert!(!inside("/a/b"));
        assert!(!inside("/a/bc"));
        assert!(!inside("/a/b0"));
        assert!(!inside("/a/c"));
    }
}
