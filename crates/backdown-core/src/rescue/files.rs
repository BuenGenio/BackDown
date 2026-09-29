//! Copy files out of a filesystem on a drive or an image of one, reading it through `fsread` (so
//! nothing is mounted and nothing on the source changes): the files and folders chosen in the
//! browser, live or deleted, from a partition Rescue found (or one in the table, or one in a disk
//! image), into a folder on another drive.
//!
//! What's chosen is named by paths through the filesystem's own listings (a deleted entry by its
//! segment, see `fsread::Deleted::seg`); what's written is named only from those listed names,
//! made safe for any destination (no separators, nothing Windows refuses), so nothing can land
//! outside the destination folder. Nothing is overwritten: a name that's taken gets " (2)". Each
//! copy is read back and checked against what was read from the source (SHA-256); a deleted file
//! whose space other files have taken since comes back with those parts empty, and says so. It
//! stops between pieces when asked, and never leaves a half-written file behind.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::fsread::{Entry, MAX_LIST, Volume};

/// Folders deeper than this aren't followed (a damaged filesystem can loop).
const MAX_DEPTH: usize = 64;
/// The most files one copy takes on.
const MAX_FILES: u64 = 1_000_000;
/// Problems noted, at most.
const MAX_PROBLEMS: usize = 200;

/// What to copy, from where.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Options {
    /// Where the filesystem is on the source: its first byte, and its length (to the end of the
    /// source when None).
    pub start: u64,
    pub len: Option<u64>,
    /// Paths through the filesystem's listings ("" is the root).
    pub items: Vec<String>,
    /// Also copy the deleted files and folders in chosen folders (a deleted folder's are always).
    pub with_deleted: bool,
    /// Carrying on after a stop: a file already there by its own name and size was copied (and
    /// checked) last time, so it's left as it is.
    pub resume: bool,
}

/// How far a copy is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Progress {
    pub files: u64,
    pub bytes: u64,
    pub total_files: u64,
    pub total_bytes: u64,
}

/// What a copy did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Summary {
    /// Files copied and checked.
    pub files: u64,
    pub bytes: u64,
    pub folders: u64,
    /// Deleted files among them.
    pub deleted: u64,
    /// Deleted files that came back with parts missing (other files had reused that space).
    pub partly: u64,
    /// Files saved under another name because theirs was taken.
    pub renamed: u64,
    /// Files that couldn't be copied (see `problems`).
    pub failed: u64,
    pub problems: Vec<String>,
    pub stopped: bool,
}

impl Summary {
    fn problem(&mut self, p: String) {
        if self.problems.len() < MAX_PROBLEMS {
            self.problems.push(p);
        }
    }
}

/// Copies `opts.items` from the filesystem at `opts.start` on `src` into `dest` (created if
/// needed), each item as `dest/<its name>`. Reports progress at most about four times a second.
pub fn copy_out(
    src: &Path,
    opts: &Options,
    dest: &Path,
    progress: &mut dyn FnMut(&Progress),
    stop: &AtomicBool,
) -> Result<Summary> {
    if super::access(src).device && super::carve::same_device(src, dest) {
        bail!(
            "{} is on the drive being read: saving there could overwrite what's being recovered. Choose a folder on another drive",
            dest.display()
        );
    }
    if opts.items.is_empty() {
        bail!("choose what to copy");
    }
    let f = super::open_at(src).with_context(|| format!("can't open {}", src.display()))?;
    let size = f.size();
    if opts.start >= size {
        bail!("the filesystem starts past the end of {}", src.display());
    }
    let len = opts.len.unwrap_or(size - opts.start);
    let mut v = Volume::open(f, opts.start, len)?;
    fs::create_dir_all(dest).with_context(|| format!("can't create {}", dest.display()))?;

    // first how much there is, then the copying
    let mut p = Progress::default();
    let mut sum = Summary::default();
    for item in &opts.items {
        let e = match v.find(item) {
            Ok(e) => e,
            Err(err) => {
                sum.problem(format!("{}: {err:#}", shown(item)));
                sum.failed += 1;
                continue;
            }
        };
        tally(&mut v, item, &e, opts.with_deleted, 0, &mut p);
    }
    progress(&p);
    let mut last = Instant::now();
    for item in &opts.items {
        if stop.load(Ordering::Relaxed) {
            sum.stopped = true;
            break;
        }
        let Ok(mut e) = v.find(item) else { continue };
        if e.name.is_empty() {
            // the whole filesystem: named after its label, or what it is
            e.name = v
                .info()
                .label
                .clone()
                .unwrap_or_else(|| v.info().kind.clone());
        }
        let mut ctx = Copy {
            v: &mut v,
            with_deleted: opts.with_deleted,
            resume: opts.resume,
            p: &mut p,
            sum: &mut sum,
            progress: &mut *progress,
            last: &mut last,
            stop,
        };
        ctx.item(item, &e, dest, 0)?;
    }
    progress(&p);
    Ok(sum)
}

/// A path for people, without deleted entries' tokens.
fn shown(path: &str) -> String {
    path.split('/')
        .map(|s| s.split_once('\0').map_or(s, |(n, _)| n))
        .collect::<Vec<_>>()
        .join("/")
}

/// The path to `e` inside `dir_path`, as `find` takes it.
fn child_path(dir_path: &str, e: &Entry) -> String {
    let seg = e
        .deleted
        .as_ref()
        .map_or_else(|| e.name.clone(), |d| d.seg.clone());
    if dir_path.is_empty() {
        seg
    } else {
        format!("{dir_path}/{seg}")
    }
}

/// A folder's entries to copy: its listing (a deleted folder's are all deleted), and its deleted
/// ones too when asked.
fn children<R: Read + std::io::Seek>(
    v: &mut Volume<R>,
    dir: &Entry,
    with_deleted: bool,
) -> Result<Vec<Entry>> {
    let mut all = v.list(dir, MAX_LIST)?.entries;
    if with_deleted && dir.deleted.is_none() && v.undelete_unsupported().is_none() {
        all.extend(v.deleted(dir, MAX_LIST)?.entries);
    }
    Ok(all)
}

fn tally<R: Read + std::io::Seek>(
    v: &mut Volume<R>,
    path: &str,
    e: &Entry,
    with_deleted: bool,
    depth: usize,
    p: &mut Progress,
) {
    if p.total_files >= MAX_FILES || depth > MAX_DEPTH {
        return;
    }
    if !e.dir {
        if e.link.is_none() && e.unreadable.is_none() {
            p.total_files += 1;
            p.total_bytes += e.size;
        }
        return;
    }
    for c in children(v, e, with_deleted).unwrap_or_default() {
        tally(v, &child_path(path, &c), &c, with_deleted, depth + 1, p);
    }
}

struct Copy<'a, R> {
    v: &'a mut Volume<R>,
    with_deleted: bool,
    resume: bool,
    p: &'a mut Progress,
    sum: &'a mut Summary,
    progress: &'a mut dyn FnMut(&Progress),
    last: &'a mut Instant,
    stop: &'a AtomicBool,
}

impl<R: Read + std::io::Seek> Copy<'_, R> {
    fn tick(&mut self) {
        tick(self.last, &mut *self.progress, self.p);
    }

    /// Copies `e` (at `path` in the filesystem) into folder `into`.
    fn item(&mut self, path: &str, e: &Entry, into: &Path, depth: usize) -> Result<()> {
        if self.stop.load(Ordering::Relaxed) {
            self.sum.stopped = true;
            return Ok(());
        }
        if self.sum.files + self.sum.failed >= MAX_FILES {
            if self
                .sum
                .problems
                .last()
                .is_none_or(|p| !p.contains("at most"))
            {
                self.sum.problem(format!(
                    "a copy takes at most {MAX_FILES} files: the rest weren't copied"
                ));
            }
            return Ok(());
        }
        let name = local_name(if e.name.is_empty() {
            "filesystem"
        } else {
            &e.name
        });
        if e.dir {
            if depth > MAX_DEPTH {
                self.sum.problem(format!(
                    "{}: folders nested too deep to follow",
                    shown(path)
                ));
                return Ok(());
            }
            let dir = into.join(&name);
            fs::create_dir_all(&dir).with_context(|| format!("can't create {}", dir.display()))?;
            self.sum.folders += 1;
            let kids = match children(self.v, e, self.with_deleted) {
                Ok(k) => k,
                Err(err) => {
                    self.sum.problem(format!("{}: {err:#}", shown(path)));
                    return Ok(());
                }
            };
            for c in kids {
                self.item(&child_path(path, &c), &c, &dir, depth + 1)?;
                if self.sum.stopped {
                    break;
                }
            }
            return Ok(());
        }
        if let Some(t) = &e.link {
            self.sum.problem(format!(
                "{}: a link to {t}; links aren't copied",
                shown(path)
            ));
            return Ok(());
        }
        if let Some(why) = &e.unreadable {
            self.sum.failed += 1;
            self.sum.problem(format!("{}: {why}", shown(path)));
            return Ok(());
        }
        if self.resume
            && fs::symlink_metadata(into.join(&name))
                .is_ok_and(|m| m.is_file() && m.len() == e.size)
        {
            // copied and checked before the stop
            self.sum.files += 1;
            self.sum.bytes += e.size;
            self.p.files += 1;
            self.p.bytes += e.size;
            return Ok(());
        }
        match self.file(e, into, &name) {
            Ok(true) => {
                self.sum.files += 1;
                self.sum.bytes += e.size;
                if let Some(d) = &e.deleted {
                    self.sum.deleted += 1;
                    if d.state != "recoverable" {
                        self.sum.partly += 1;
                    }
                }
            }
            Ok(false) => self.sum.stopped = true,
            Err(err) => {
                // a destination that can't be written to stops the copy; a source file that
                // can't be read is noted and skipped
                if err.downcast_ref::<Dest>().is_some() {
                    return Err(err);
                }
                self.sum.failed += 1;
                self.sum.problem(format!("{}: {err:#}", shown(path)));
            }
        }
        self.tick();
        Ok(())
    }

    /// One file: written under a free name, read back and checked. False when stopped (nothing
    /// left behind).
    fn file(&mut self, e: &Entry, into: &Path, name: &str) -> Result<bool> {
        let Copy {
            v,
            p,
            sum,
            progress,
            last,
            stop,
            ..
        } = self;
        let mut rd = v.reader(e)?;
        let (path, renamed) = fresh(into, name)?;
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|err| Dest(format!("can't write {}: {err}", path.display())))?;
        let mut h = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        let result = (|| -> Result<bool> {
            loop {
                if stop.load(Ordering::Relaxed) {
                    return Ok(false);
                }
                let n = match rd.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(err) => bail!("reading it failed: {err}"),
                };
                h.update(&buf[..n]);
                out.write_all(&buf[..n])
                    .map_err(|err| Dest(format!("can't write {}: {err}", path.display())))?;
                p.bytes += n as u64;
                tick(last, &mut **progress, p);
            }
            out.sync_all()
                .map_err(|err| Dest(format!("can't write {}: {err}", path.display())))?;
            Ok(true)
        })();
        drop(out);
        match result {
            Ok(true) => {}
            other => {
                let _ = fs::remove_file(&path);
                return other;
            }
        }
        // read it back from the destination: it must be exactly what was read from the source
        let want = h.finalize();
        let mut back = File::open(&path)?;
        let mut h = Sha256::new();
        loop {
            let n = back.read(&mut buf)?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
        if h.finalize() != want {
            let _ = fs::remove_file(&path);
            bail!("the copy didn't read back the same (a failing destination drive?)");
        }
        if let Some(t) = e.mtime.and_then(|t| u64::try_from(t).ok()) {
            let when = std::time::UNIX_EPOCH + Duration::from_secs(t);
            let _ = File::options()
                .write(true)
                .open(&path)
                .and_then(|f| f.set_modified(when));
        }
        if renamed {
            sum.renamed += 1;
        }
        p.files += 1;
        Ok(true)
    }
}

/// Progress, at most four times a second.
fn tick(last: &mut Instant, progress: &mut dyn FnMut(&Progress), p: &Progress) {
    if last.elapsed() >= Duration::from_millis(250) {
        *last = Instant::now();
        progress(p);
    }
}

/// A destination problem: it stops the whole copy.
#[derive(Debug)]
struct Dest(String);

impl std::fmt::Display for Dest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Dest {}

/// `name` in `dir`, or "name (2).ext", "name (3).ext"… if it's taken; and whether it was.
fn fresh(dir: &Path, name: &str) -> Result<(PathBuf, bool)> {
    let first = dir.join(name);
    if fs::symlink_metadata(&first).is_err() {
        return Ok((first, false));
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, format!(".{e}")),
        _ => (name, String::new()),
    };
    for k in 2..10_000 {
        let p = dir.join(format!("{stem} ({k}){ext}"));
        if fs::symlink_metadata(&p).is_err() {
            return Ok((p, true));
        }
    }
    bail!(Dest(format!(
        "{name}: too many files by that name in {}",
        dir.display()
    )))
}

/// A listed name as a file name any destination takes: no separators or characters Windows
/// refuses, no trailing dots or spaces, not a DOS device name, not empty.
pub fn local_name(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| {
            if c < ' ' || "\\/:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    while s.ends_with(['.', ' ']) {
        s.pop();
    }
    if s.is_empty() {
        return "_".into();
    }
    let stem = s.split('.').next().unwrap_or("").to_ascii_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit());
    if device {
        s.insert(0, '_');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_for_any_destination() {
        assert_eq!(local_name("photo.jpg"), "photo.jpg");
        assert_eq!(local_name("a/b\\c:d*e?f\"g<h>i|j"), "a_b_c_d_e_f_g_h_i_j");
        assert_eq!(local_name("trailing. . "), "trailing");
        assert_eq!(local_name(".."), "_");
        assert_eq!(local_name(""), "_");
        assert_eq!(local_name("con.txt"), "_con.txt");
        assert_eq!(local_name("COM1"), "_COM1");
        assert_eq!(local_name("COMPUTER.txt"), "COMPUTER.txt");
        assert_eq!(local_name("tab\there"), "tab_here");
    }

    #[test]
    fn taken_names_get_a_number() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            fresh(tmp.path(), "a.txt").unwrap(),
            (tmp.path().join("a.txt"), false)
        );
        fs::write(tmp.path().join("a.txt"), "x").unwrap();
        fs::write(tmp.path().join("a (2).txt"), "x").unwrap();
        assert_eq!(
            fresh(tmp.path(), "a.txt").unwrap(),
            (tmp.path().join("a (3).txt"), true)
        );
        fs::write(tmp.path().join(".hidden"), "x").unwrap();
        assert_eq!(
            fresh(tmp.path(), ".hidden").unwrap().0,
            tmp.path().join(".hidden (2)")
        );
    }
}
