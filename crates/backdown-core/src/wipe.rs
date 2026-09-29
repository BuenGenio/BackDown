//! Wiping: freeing drives (or folders on them) once everything on them is provably kept elsewhere.
//!
//! This is the one thing BackDown deletes, and only when asked (APP-PLAN §8). The rules:
//!
//! 1. Never the running system: nothing on it outside home folders (and never BackDown's index).
//! 2. A file is deleted only when it still is what the index says (re-read: same SHA-256), and an
//!    identical copy *outside the selection* has just been re-read and matches. Copies inside the
//!    selection don't count: they're going too. A copy on another machine (remote/) counts only
//!    if it's hashed there, was confirmed there within the proof window, and that machine re-reads
//!    it (over ssh) just before and it matches.
//! 3. What exists only in the selection is first copied to the chosen place (verified by reading it
//!    back), and only then deleted: a move.
//! 4. Anything else is left where it is: changed or new since the last scan, unreadable, or with
//!    no copy and no place to move it to.
//! 5. Every deletion is journaled with its proof: which copy, its hash, how it was verified.
//! 6. Folders emptied by the wipe are removed (a drive's own top folder stays).
//!
//! `--trust-index` skips the re-reads and relies on the index's hashes (much faster; right after a
//! fresh scan and hash of both sides it's nearly as safe, but not quite: a file can rot quietly).
//! The copy must still be there, with the size and modification time the index has for it: an
//! index that hasn't seen a drive emptied yet mustn't vouch for copies that are gone.

use std::fs;
use std::path::{MAIN_SEPARATOR_STR, Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::apply::{Outcome, copy_verified, hash_file, mtime};
use crate::{Db, fmt, paths};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Place {
    pub path: String,
    pub bytes: u64,
}

/// One selected path, as the index sees it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Item {
    pub path: String,
    pub drive: String,
    pub files: u64,
    pub bytes: u64,
    /// an identical copy outside the selection
    pub kept_files: u64,
    pub kept_bytes: u64,
    /// only here: moved first (with a destination), otherwise left
    pub only_files: u64,
    pub only_bytes: u64,
    pub unreadable_files: u64,
    /// where the copies are (drive / top folder), most first
    pub copies: Vec<Place>,
    /// why this path can't be wiped at all
    pub refused: Option<String>,
    /// of `kept`: files whose only copy is on another machine (remote/), hashed there and confirmed
    /// within the proof window; that machine re-reads each one before it's deleted here
    #[serde(default)]
    pub remote_files: u64,
    #[serde(default)]
    pub remote_bytes: u64,
    /// the oldest confirmation among those copies
    #[serde(default)]
    pub remote_confirmed: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Assessment {
    pub items: Vec<Item>,
    pub files: u64,
    pub bytes: u64,
    pub kept_bytes: u64,
    pub only_files: u64,
    pub only_bytes: u64,
    /// the drive names a person types to confirm
    pub confirm: String,
    /// copies on other machines count when confirmed there within this many days (0: no machines)
    #[serde(default)]
    pub proof_days: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Stats {
    pub deleted: u64,
    pub deleted_bytes: u64,
    pub moved: u64,
    pub moved_bytes: u64,
    /// changed or new since the last scan
    pub left_changed: u64,
    pub left_no_copy: u64,
    pub left_unreadable: u64,
    pub left_links: u64,
    pub failed: u64,
    pub dirs_removed: u64,
    pub problems: Vec<String>,
    /// of `deleted`: proved by a copy on another machine, re-read there just before
    #[serde(default)]
    pub remote: u64,
}

#[derive(Debug, Clone)]
pub struct Options {
    /// where what exists only in the selection goes first (else it's left)
    pub move_to: Option<PathBuf>,
    /// rely on the index's hashes instead of re-reading both sides
    pub trust_index: bool,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Why `path` must never be wiped, if it mustn't: BackDown's own index, the operating system's
/// folders wherever they're mounted, a home folder as a whole, and anything else on the running
/// system's own filesystem that isn't inside a home folder. Other drives are fine wherever they're
/// mounted (/media, /mnt, /data, …).
pub fn refusal(path: &Path, index: Option<&Path>) -> Option<String> {
    if let Some(ix) = index {
        // a folder holding the index, the index and its -wal/-shm files, and BackDown's own data
        // folder (where the index usually lives); not whatever else sits beside an index kept elsewhere
        let dir = ix.parent().unwrap_or(ix);
        let sidecar = path.parent() == Some(dir)
            && path
                .file_name()
                .zip(ix.file_name())
                .is_some_and(|(n, i)| n.to_string_lossy().starts_with(&*i.to_string_lossy()));
        let own_dir = dir
            .file_name()
            .is_some_and(|n| n.eq_ignore_ascii_case("backdown"))
            && path.starts_with(dir);
        if ix.starts_with(path) || sidecar || own_dir {
            return Some("it holds BackDown's own index".into());
        }
    }
    let s = path.to_string_lossy().replace('\\', "/");
    let lower = s.to_lowercase();
    if lower.len() >= 2 && lower.as_bytes()[1] == b':' {
        let sys = std::env::var("SystemDrive")
            .unwrap_or_else(|_| "C:".into())
            .to_lowercase();
        let rest = lower.strip_prefix(&sys).unwrap_or("-");
        if rest == "/" || rest.is_empty() {
            return Some("it's the system drive".into());
        }
        for p in [
            "/windows",
            "/program files",
            "/program files (x86)",
            "/programdata",
            "/recovery",
            "/$recycle.bin",
        ] {
            if rest.starts_with(p) {
                return Some("it's part of Windows".into());
            }
        }
        if rest.trim_end_matches('/') == "/users"
            || rest
                .strip_prefix("/users/")
                .is_some_and(|u| !u.trim_end_matches('/').contains('/'))
        {
            return Some("it's a whole home folder (pick folders inside it)".into());
        }
        return None;
    }
    let under = |p: &str| s == p || s.starts_with(&format!("{p}/"));
    const SYSTEM: &[&str] = &[
        "/bin",
        "/boot",
        "/dev",
        "/efi",
        "/etc",
        "/lib",
        "/lib32",
        "/lib64",
        "/libx32",
        "/opt",
        "/proc",
        "/root",
        "/sbin",
        "/snap",
        "/sys",
        "/usr",
        "/var",
        "/System",
        "/Library",
        "/Applications",
        "/private",
        "/cores",
    ];
    if s == "/" || SYSTEM.iter().any(|p| under(p)) || (under("/run") && !under("/run/media")) {
        return Some("it's part of the operating system".into());
    }
    let home_of = |base: &str| {
        s.strip_prefix(&format!("{base}/"))
            .map(|r| r.trim_end_matches('/'))
    };
    if s == "/home"
        || s == "/Users"
        || home_of("/home")
            .or(home_of("/Users"))
            .is_some_and(|r| !r.contains('/'))
    {
        return Some("it's a whole home folder (pick folders inside it)".into());
    }
    if home_of("/home").or(home_of("/Users")).is_some() {
        return None;
    }
    // anything else on the running system's own filesystem is part of the system
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mut p = path;
        while !p.exists() {
            match p.parent() {
                Some(q) => p = q,
                None => break,
            }
        }
        if let (Ok(a), Ok(r)) = (fs::metadata(p), fs::metadata("/"))
            && a.dev() == r.dev()
        {
            return Some("it's on the running system's own disk, outside the home folders".into());
        }
    }
    None
}

fn drive_of(db: &Db, path: &[u8]) -> Result<Option<(i64, PathBuf)>> {
    let mut stmt = db.conn.prepare_cached("SELECT id, path FROM roots")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))?;
    let mut best: Option<(i64, Vec<u8>)> = None;
    for row in rows {
        let (id, root) = row?;
        let pre = paths::dir_prefix(&paths::from_bytes(&root));
        if (path.starts_with(&pre) || path == root.as_slice())
            && best.as_ref().is_none_or(|(_, b)| root.len() > b.len())
        {
            best = Some((id, root));
        }
    }
    Ok(best.map(|(id, r)| (id, paths::from_bytes(&r))))
}

fn range(path: &Path) -> (Vec<u8>, Vec<u8>) {
    let lo = paths::dir_prefix(path);
    let mut hi = lo.clone();
    if let Some(l) = hi.last_mut() {
        *l += 1; // a separator is never 0xFF
    }
    (lo, hi)
}

struct Selection {
    prefixes: Vec<Vec<u8>>,
    files: Vec<Vec<u8>>,
}

impl Selection {
    fn new(paths: &[PathBuf]) -> Self {
        Selection {
            prefixes: paths.iter().map(|p| paths::dir_prefix(p)).collect(),
            files: paths.iter().map(|p| paths::to_bytes(p)).collect(),
        }
    }
    fn contains(&self, p: &[u8]) -> bool {
        self.prefixes.iter().any(|x| p.starts_with(x)) || self.files.iter().any(|f| f == p)
    }
}

/// Where a copy is, for people: its drive and top folder.
fn place_of(db: &Db, p: &[u8]) -> String {
    match drive_of(db, p).ok().flatten() {
        Some((_, root)) => {
            let path = paths::from_bytes(p);
            let top = path
                .strip_prefix(&root)
                .ok()
                .and_then(|r| r.components().next())
                .map(|c| c.as_os_str().to_string_lossy().into_owned());
            let name = root.file_name().map_or_else(
                || root.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            match top {
                Some(t)
                    if path
                        .strip_prefix(&root)
                        .is_ok_and(|r| r.components().count() > 1) =>
                {
                    format!("{name}/{t}")
                }
                _ => name,
            }
        }
        None => paths::from_bytes(p)
            .parent()
            .map(|x| x.display().to_string())
            .unwrap_or_default(),
    }
}

/// Free space where things would be moved to: at `move_to`, or the nearest folder up that exists.
pub fn room(move_to: &Path) -> Option<u64> {
    crate::apply::free_space(move_to)
}

/// What wiping `paths` would do, from the index (nothing is read or written).
pub fn assess(db: &Db, paths: &[PathBuf], index: Option<&Path>) -> Result<Assessment> {
    if paths.is_empty() {
        bail!("nothing selected");
    }
    let sel = Selection::new(paths);
    let mut a = Assessment::default();
    // copies on other machines, by hash and confirmed within the proof window (remote/)
    let mut prover = crate::remote::Prover::new(db)?;
    if crate::remote::cutoff(db)?.is_some() {
        a.proof_days = prover.proof_days;
    }
    let mut copies_q = db.conn.prepare_cached(
        "SELECT path, dev, ino FROM files WHERE current = 1 AND sha256 = ?1 AND error IS NULL LIMIT 50",
    )?;
    let mut drives: Vec<String> = Vec::new();
    for p in paths {
        let pb = paths::to_bytes(p);
        let mut it = Item {
            path: p.display().to_string(),
            ..Item::default()
        };
        let drive = drive_of(db, &pb)?;
        it.drive = drive
            .as_ref()
            .map(|(_, r)| {
                r.file_name().map_or_else(
                    || r.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                )
            })
            .unwrap_or_default();
        it.refused = refusal(p, index);
        if drive.is_none() && it.refused.is_none() {
            it.refused = Some("it isn't on an indexed drive".into());
        }
        let (lo, hi) = range(p);
        let mut rows = db.conn.prepare_cached(
            "SELECT path, size, sha256, error, dev, ino FROM files WHERE current = 1 AND ((path >= ?1 AND path < ?2) OR path = ?3)",
        )?;
        let files = rows.query_map(params![lo, hi, pb], |r| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get::<_, i64>(1)? as u64,
                r.get::<_, Option<Vec<u8>>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<i64>>(4)?,
                r.get::<_, Option<i64>>(5)?,
            ))
        })?;
        let mut places: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        for f in files {
            let (_path, size, sha, err, dev, ino) = f?;
            it.files += 1;
            it.bytes += size;
            if err.is_some() {
                it.unreadable_files += 1;
                continue;
            }
            let copy = match &sha {
                Some(h) if size > 0 => copies_q
                    .query_map(params![h], |r| {
                        Ok((
                            r.get::<_, Vec<u8>>(0)?,
                            r.get::<_, Option<i64>>(1)?,
                            r.get::<_, Option<i64>>(2)?,
                        ))
                    })?
                    .filter_map(Result::ok)
                    .find(|(g, gd, gi)| {
                        !sel.contains(g) && !(ino.is_some() && *gd == dev && *gi == ino)
                    })
                    .map(|(g, _, _)| g),
                _ => None,
            };
            let there = match (&copy, &sha) {
                (None, Some(h)) if size > 0 => prover.find(h, size)?,
                _ => None,
            };
            match copy {
                Some(g) => {
                    it.kept_files += 1;
                    it.kept_bytes += size;
                    *places.entry(place_of(db, &g)).or_default() += size;
                }
                None if size == 0 => {
                    it.kept_files += 1; // an empty file carries nothing to keep
                }
                None if there.is_some() => {
                    let (machine, confirmed) = there.expect("matched");
                    it.kept_files += 1;
                    it.kept_bytes += size;
                    it.remote_files += 1;
                    it.remote_bytes += size;
                    it.remote_confirmed =
                        Some(it.remote_confirmed.map_or(confirmed, |c| c.min(confirmed)));
                    *places.entry(format!("on {machine}")).or_default() += size;
                }
                None => {
                    it.only_files += 1;
                    it.only_bytes += size;
                }
            }
        }
        let mut pl: Vec<Place> = places
            .into_iter()
            .map(|(path, bytes)| Place { path, bytes })
            .collect();
        pl.sort_by_key(|x| std::cmp::Reverse(x.bytes));
        pl.truncate(6);
        it.copies = pl;
        if it.refused.is_none() {
            a.files += it.files;
            a.bytes += it.bytes;
            a.kept_bytes += it.kept_bytes;
            a.only_files += it.only_files;
            a.only_bytes += it.only_bytes;
            if !it.drive.is_empty() && !drives.contains(&it.drive) {
                drives.push(it.drive.clone());
            }
        }
        a.items.push(it);
    }
    a.confirm = drives.join(", ");
    Ok(a)
}

// ---------- doing it

/// Record a wipe before starting it (the daemon runs it as a job).
pub fn record(db: &Db, paths: &[PathBuf], move_to: Option<&Path>) -> Result<i64> {
    db.conn.execute(
        "INSERT INTO wipes(created_at, paths, move_to, state) VALUES (?1, ?2, ?3, 'new')",
        params![
            now(),
            serde_json::to_string(
                &paths
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
            )?,
            move_to.map(|m| m.display().to_string())
        ],
    )?;
    Ok(db.conn.last_insert_rowid())
}

pub fn load(db: &Db, id: i64) -> Result<(Vec<PathBuf>, Option<PathBuf>, String)> {
    let (paths, move_to, state): (String, Option<String>, String) = db
        .conn
        .query_row(
            "SELECT paths, move_to, state FROM wipes WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .with_context(|| format!("there's no wipe {id}"))?;
    let paths: Vec<String> = serde_json::from_str(&paths)?;
    Ok((
        paths.into_iter().map(PathBuf::from).collect(),
        move_to.map(PathBuf::from),
        state,
    ))
}

/// Wipes on record, newest first: (id, created_at, paths, move_to, state, summary).
pub fn history(db: &Db) -> Result<Vec<serde_json::Value>> {
    let mut stmt = db.conn.prepare("SELECT id, created_at, paths, move_to, state, summary FROM wipes ORDER BY id DESC LIMIT 50")?;
    let rows = stmt.query_map([], |r| {
        Ok(serde_json::json!({
            "id": r.get::<_, i64>(0)?, "created_at": r.get::<_, i64>(1)?,
            "paths": serde_json::from_str::<serde_json::Value>(&r.get::<_, String>(2)?).unwrap_or_default(),
            "move_to": r.get::<_, Option<String>>(3)?, "state": r.get::<_, String>(4)?, "summary": r.get::<_, Option<String>>(5)?,
        }))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Carry out wipe `id` (made by `record`). See the module doc for what is and isn't deleted.
/// A wipe that stops with an error is recorded as failed, with the reason.
pub fn wipe(
    db: &mut Db,
    id: i64,
    trust_index: bool,
    index: Option<&Path>,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Stats> {
    wipe_with(db, id, trust_index, index, None, progress)
}

/// `wipe`, with copies on other machines as proof too (remote/): a file whose only copy is there
/// is deleted only once that machine has re-read the copy just then and it matched (trusting the
/// index: checked it's there as scanned), and only if the copy was confirmed within the proof
/// window before that.
pub fn wipe_with(
    db: &mut Db,
    id: i64,
    trust_index: bool,
    index: Option<&Path>,
    remote: Option<&mut crate::remote::Verifier>,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Stats> {
    let r = run(db, id, trust_index, index, remote, progress);
    if let Err(e) = &r {
        let _ = db.conn.execute(
            "UPDATE wipes SET state = 'failed', summary = ?2 WHERE id = ?1",
            params![id, format!("{e:#}")],
        );
    }
    r
}

fn run(
    db: &mut Db,
    id: i64,
    trust_index: bool,
    index: Option<&Path>,
    mut remote: Option<&mut crate::remote::Verifier>,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Stats> {
    let (paths, move_to, _) = load(db, id)?;
    for p in &paths {
        if let Some(why) = refusal(p, index) {
            bail!("{} can't be wiped: {why}", p.display());
        }
        if !p.exists() {
            bail!("{} isn't there: connect its drive", p.display());
        }
    }
    if let Some(m) = &move_to
        && paths.iter().any(|p| m.starts_with(p))
    {
        bail!("the place to move things to is inside what's being wiped");
    }
    let sel = Selection::new(&paths);
    db.conn.execute(
        "UPDATE wipes SET state = 'running' WHERE id = ?1",
        params![id],
    )?;
    let mut st = Stats::default();
    let mut row_q = db.conn.prepare_cached(
        "SELECT size, mtime, sha256, dev, ino FROM files WHERE path = ?1 AND current = 1",
    )?;
    let mut copies_q = db.conn.prepare_cached("SELECT path, dev, ino, mtime FROM files WHERE current = 1 AND sha256 = ?1 AND error IS NULL LIMIT 20")?;
    let mut journal = db.conn.prepare_cached(
        "INSERT INTO deletions(wipe_id, path, size, sha256, proof, proof_how, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    let (mut seen, mut seen_bytes, mut tick) = (0u64, 0u64, Instant::now());
    for root in &paths {
        let drive_root = drive_of(db, &paths::to_bytes(root))?.map(|(_, r)| r);
        for entry in WalkDir::new(root).follow_links(false) {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    st.left_unreadable += 1;
                    push(&mut st.problems, format!("can't read: {e}"));
                    continue;
                }
            };
            if entry.file_type().is_symlink() {
                st.left_links += 1;
                continue;
            }
            if !entry.file_type().is_file() {
                continue;
            }
            let f = entry.path();
            let fb = paths::to_bytes(f);
            let meta = entry.metadata()?;
            seen += 1;
            seen_bytes += meta.len();
            if tick.elapsed().as_millis() >= 250 {
                tick = Instant::now();
                progress(seen, seen_bytes);
            }
            // the index must know this file as it is now
            let known: Option<IndexRow> = row_q
                .query_row(params![fb], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })
                .optional()?;
            let Some((size, _mtime, sha, dev, ino)) =
                known.filter(|k| k.0 as u64 == meta.len() && k.1 == mtime(&meta))
            else {
                st.left_changed += 1;
                push(
                    &mut st.problems,
                    format!(
                        "changed or new since the last scan, left alone: {}",
                        f.display()
                    ),
                );
                continue;
            };
            let size = size as u64;
            // what it is now: re-read (or the index's hash when trusted)
            let actual: Vec<u8> = match (&sha, trust_index) {
                (Some(h), true) => h.clone(),
                _ => match hash_file(f) {
                    Ok(h) => h.to_vec(),
                    Err(e) => {
                        st.left_unreadable += 1;
                        push(
                            &mut st.problems,
                            format!("can't read {}, left alone: {e}", f.display()),
                        );
                        continue;
                    }
                },
            };
            if sha.as_ref().is_some_and(|h| *h != actual) {
                st.left_changed += 1;
                push(
                    &mut st.problems,
                    format!(
                        "its content isn't what was scanned, left alone: {}",
                        f.display()
                    ),
                );
                continue;
            }
            // a copy outside the selection, re-read now
            let mut proof: Option<(Vec<u8>, &str)> = None;
            if size == 0 {
                proof = Some((Vec::new(), "empty"));
            } else {
                let cands: Vec<CopyRow> = copies_q
                    .query_map(params![actual], |r| {
                        Ok((
                            r.get::<_, Vec<u8>>(0)?,
                            r.get::<_, Option<i64>>(1)?,
                            r.get::<_, Option<i64>>(2)?,
                            r.get::<_, i64>(3)?,
                        ))
                    })?
                    .filter_map(Result::ok)
                    .filter(|(g, gd, gi, _)| {
                        !sel.contains(g) && !(ino.is_some() && *gd == dev && *gi == ino)
                    })
                    .collect();
                for (g, _, _, gm) in cands {
                    let gp = paths::from_bytes(&g);
                    let Ok(gmeta) = fs::metadata(&gp) else {
                        continue; // gone, or its drive isn't connected
                    };
                    if trust_index {
                        // not re-read, but still there as the index knows it
                        if gmeta.len() == size && mtime(&gmeta) == gm {
                            proof = Some((g, "index"));
                            break;
                        }
                        continue;
                    }
                    if gmeta.len() == size
                        && hash_file(&gp).is_ok_and(|h| h.as_slice() == actual.as_slice())
                    {
                        proof = Some((g, "re-read"));
                        break;
                    }
                }
                // none here: a copy on another machine, confirmed there now
                if proof.is_none()
                    && let Some(v) = remote.as_deref_mut()
                    && let Some((there, how)) = v.confirm(db, &actual, size, trust_index)?
                {
                    proof = Some((there.into_bytes(), how));
                }
            }
            // none: move it to the chosen place first
            if proof.is_none() {
                let Some(dest_root) = &move_to else {
                    st.left_no_copy += 1;
                    continue;
                };
                let rel = drive_root
                    .as_ref()
                    .and_then(|r| f.strip_prefix(r).ok())
                    .unwrap_or(f);
                let drive_name = drive_root
                    .as_ref()
                    .and_then(|r| r.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "drive".into());
                let dest = dest_root
                    .join(drive_name)
                    .join(rel.strip_prefix(MAIN_SEPARATOR_STR).unwrap_or(rel));
                match copy_verified(f, &dest, &meta) {
                    Ok(Outcome::Copied(h)) | Ok(Outcome::AlreadyThere(h))
                        if h.as_slice() == actual.as_slice() =>
                    {
                        st.moved += 1;
                        st.moved_bytes += size;
                        proof = Some((paths::to_bytes(&dest), "moved"));
                    }
                    Ok(Outcome::Conflict(msg)) => {
                        st.left_no_copy += 1;
                        push(
                            &mut st.problems,
                            format!("couldn't move it, left alone: {msg}"),
                        );
                        continue;
                    }
                    Ok(_) => {
                        st.failed += 1;
                        push(
                            &mut st.problems,
                            format!("the moved copy of {} didn't match, left alone", f.display()),
                        );
                        continue;
                    }
                    Err(e) => {
                        st.failed += 1;
                        push(
                            &mut st.problems,
                            format!("couldn't move {}, left alone: {e:#}", f.display()),
                        );
                        continue;
                    }
                }
            }
            let (proof, how) = proof.expect("set above");
            match fs::remove_file(f) {
                Ok(()) => {
                    st.deleted += 1;
                    st.deleted_bytes += size;
                    if how.starts_with("remote") {
                        st.remote += 1;
                    }
                    journal.execute(params![id, fb, size as i64, actual, proof, how, now()])?;
                }
                Err(e) => {
                    st.failed += 1;
                    let hint = if e.raw_os_error() == Some(30) {
                        " (the drive is mounted read-only: remount it read-write to wipe)"
                    } else {
                        ""
                    };
                    push(
                        &mut st.problems,
                        format!("couldn't delete {}: {e}{hint}", f.display()),
                    );
                }
            }
        }
        // folders the wipe emptied (the drive's own top folder stays)
        for entry in WalkDir::new(root)
            .contents_first(true)
            .into_iter()
            .filter_map(Result::ok)
        {
            if entry.file_type().is_dir()
                && drive_root.as_deref() != Some(entry.path())
                && fs::remove_dir(entry.path()).is_ok()
            {
                st.dirs_removed += 1;
            }
        }
    }
    if let Some(v) = remote {
        for p in v.problems.drain(..) {
            push(&mut st.problems, p);
        }
    }
    let summary = format!(
        "deleted {} files ({}), {} of them moved to the chosen place first{}; left {} changed or new, {} with no copy, {} unreadable{}",
        fmt::count(st.deleted),
        fmt::bytes(st.deleted_bytes),
        fmt::count(st.moved),
        if st.remote > 0 {
            format!(
                " and {} kept only on another machine (re-read there first)",
                fmt::count(st.remote)
            )
        } else {
            String::new()
        },
        fmt::count(st.left_changed),
        fmt::count(st.left_no_copy),
        fmt::count(st.left_unreadable),
        if st.failed > 0 {
            format!("; {} failed", st.failed)
        } else {
            String::new()
        }
    );
    let state = if st.failed == 0 && st.left_changed + st.left_no_copy + st.left_unreadable == 0 {
        "done"
    } else {
        "partial"
    };
    db.conn.execute(
        "UPDATE wipes SET state = ?2, summary = ?3 WHERE id = ?1",
        params![id, state, summary],
    )?;
    Ok(st)
}

/// path, dev, ino, mtime of a possible copy
type CopyRow = (Vec<u8>, Option<i64>, Option<i64>, i64);

/// size, mtime, sha256, dev, ino of a file's current row
type IndexRow = (i64, i64, Option<Vec<u8>>, Option<i64>, Option<i64>);

fn push(v: &mut Vec<String>, s: String) {
    if v.len() < 30 {
        v.push(s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn never_the_system() {
        for p in [
            "/",
            "/usr/share",
            "/etc",
            "/boot",
            "/var/lib",
            "/home",
            "/root/x",
        ] {
            assert!(refusal(Path::new(p), None).is_some(), "{p}");
        }
        for p in ["/home/alex/Downloads", "/home/alex/Old Photos/2009"] {
            assert!(refusal(Path::new(p), None).is_none(), "{p}");
        }
        assert!(
            refusal(Path::new("/home/alex"), None).is_some(),
            "a whole home folder"
        );
        assert!(refusal(Path::new("/run/user/1000"), None).is_some());
        assert!(
            refusal(
                Path::new("/home/alex/.local/share/backdown"),
                Some(Path::new("/home/alex/.local/share/backdown/index.db"))
            )
            .is_some()
        );
    }

    #[test]
    fn never_the_index() {
        let ix = Path::new("/home/alex/.local/share/backdown/index.db");
        for p in [
            "/home/alex/.local",
            "/home/alex/.local/share/backdown/index.db",
            "/home/alex/.local/share/backdown/index.db-wal",
            "/home/alex/.local/share/backdown/ui.json",
        ] {
            assert!(refusal(Path::new(p), Some(ix)).is_some(), "{p}");
        }
        // an index kept beside other things (`--db /home/alex/work/index.db`): only it is off-limits
        let ix = Path::new("/home/alex/work/index.db");
        assert!(refusal(Path::new("/home/alex/work"), Some(ix)).is_some());
        assert!(refusal(Path::new("/home/alex/work/index.db-shm"), Some(ix)).is_some());
        assert!(refusal(Path::new("/home/alex/work/Old Stick"), Some(ix)).is_none());
    }

    #[test]
    fn windows_paths() {
        assert!(refusal(Path::new("C:\\Windows\\System32"), None).is_some());
        assert!(refusal(Path::new("C:\\"), None).is_some());
        assert!(refusal(Path::new("E:\\Old stuff"), None).is_none());
        assert!(refusal(Path::new("C:\\Users\\Alex\\Downloads"), None).is_none());
    }
}
