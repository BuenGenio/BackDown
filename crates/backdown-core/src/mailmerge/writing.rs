//! Writing the merged Maildir: its folders and file names, each message through tmp/ (flushed,
//! dated, renamed into cur/, read back), and where a merge may write at all.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Instant, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use rusqlite::{OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::clock::now;
use crate::custody::{self, Action, Event};
use crate::{Db, fastcopy, fmt, paths};

use super::*;

// ---------- writing

/// The folder's directory in the merged Maildir: the mailbox itself for INBOX, ".Name" otherwise.
pub(super) fn folder_dir(dest: &Path, key: &str, folder: &str) -> PathBuf {
    let mb = if key.is_empty() {
        dest.to_path_buf()
    } else {
        key.split('/').fold(dest.to_path_buf(), |p, c| p.join(c))
    };
    if folder == "INBOX" {
        mb
    } else {
        mb.join(format!(".{folder}"))
    }
}

/// A message's file name: its delivery time and content hash (so running again finds it), and
/// its flags.
pub(super) fn message_name(occ: &Occ, flags: u8) -> String {
    format!(
        "{}.M{}.backdown{INFO}{}",
        occ.time.max(0),
        &fmt::hex(&occ.sha)[..20],
        letters(flags)
    )
}

pub(super) fn make_maildir(dir: &Path, sub: bool) -> io::Result<()> {
    for d in ["cur", "new", "tmp"] {
        fs::create_dir_all(dir.join(d))?;
    }
    if sub && !dir.join("maildirfolder").exists() {
        File::create(dir.join("maildirfolder"))?;
    }
    Ok(())
}

pub(super) fn content_of(cat: &Catalog, occ: &Occ) -> io::Result<Vec<u8>> {
    match &occ.at {
        At::File(p) => fs::read(p),
        At::Mbox { file, start, len } => mbox_content(&cat.mboxes[*file as usize], *start, *len),
    }
}

pub(super) fn source_of(cat: &Catalog, occ: &Occ) -> (Vec<u8>, Option<i64>) {
    match &occ.at {
        At::File(p) => (paths::to_bytes(p), None),
        At::Mbox { file, start, .. } => (
            paths::to_bytes(&cat.mboxes[*file as usize]),
            Some(*start as i64),
        ),
    }
}

/// Where the merged mailbox may go: new, empty, or holding only what this merge wrote.
pub(super) fn check_dest(db: Option<&Db>, id: Option<i64>, dest: &Path) -> Result<()> {
    if !dest.is_absolute() {
        bail!("the merged mailbox must be a full path");
    }
    if !dest.exists() {
        return Ok(());
    }
    if !dest.is_dir() {
        bail!("{} exists and isn't a folder", dest.display());
    }
    let mut foreign = 0u64;
    let mut example = None;
    let mut q = match (db, id) {
        (Some(db), Some(_)) => Some(
            db.conn
                .prepare("SELECT 1 FROM mail_merge_log WHERE merge_id = ?1 AND dest = ?2")?,
        ),
        _ => None,
    };
    for e in walkdir::WalkDir::new(dest)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !e.file_type().is_file() {
            continue;
        }
        let in_mail = e
            .path()
            .parent()
            .and_then(|p| p.file_name())
            .is_some_and(|n| n == "cur" || n == "new");
        if !in_mail {
            continue;
        }
        let ours = match (&mut q, id) {
            (Some(q), Some(id)) => q
                .query_row(params![id, paths::to_bytes(e.path())], |_| Ok(()))
                .optional()?
                .is_some(),
            _ => false,
        } || file_name(e.path()).contains(".backdown");
        if !ours {
            foreign += 1;
            example.get_or_insert_with(|| e.path().display().to_string());
        }
    }
    if foreign > 0 {
        bail!(
            "{} already holds {} messages that aren't from this merge (e.g. {}): merge into a new folder",
            dest.display(),
            fmt::count(foreign),
            example.unwrap_or_default()
        );
    }
    Ok(())
}

/// Write one message: tmp/, flushed, dated, renamed into cur/, read back. `Ok(false)` if it was
/// there already (an earlier run), verified.
pub(super) fn write_message(
    dir: &Path,
    name: &str,
    content: &[u8],
    sha: &[u8; 32],
    time: i64,
) -> Result<bool> {
    let dest = dir.join("cur").join(name);
    if dest.exists() {
        let back = crate::apply::hash_file(&dest)?;
        if &back == sha {
            return Ok(false);
        }
        bail!("{} exists and differs: left as it is", dest.display());
    }
    let base = name.split(INFO).next().unwrap_or(name);
    let tmp = dir.join("tmp").join(base);
    let result = (|| -> Result<()> {
        let mut out = File::create(&tmp)?;
        out.write_all(content)?;
        out.sync_all()?;
        if time > 0 {
            let t = UNIX_EPOCH + std::time::Duration::from_secs(time as u64);
            out.set_times(fs::FileTimes::new().set_modified(t).set_accessed(t))?;
        }
        fastcopy::forget_cached(&out);
        drop(out);
        if dest.exists() {
            bail!("{} appeared while writing", dest.display());
        }
        fs::rename(&tmp, &dest)?;
        let back = crate::apply::hash_file(&dest)?;
        if &back != sha {
            let _ = fs::remove_file(&dest); // our own bad copy
            bail!(
                "{} read back differently: removed; try again (failing disk?)",
                dest.display()
            );
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map(|_| true)
}

#[cfg(unix)]
pub(super) fn sync_dir(dir: &Path) {
    if let Ok(f) = File::open(dir) {
        let _ = f.sync_all();
    }
}

#[cfg(not(unix))]
pub(super) fn sync_dir(_dir: &Path) {}

pub(super) fn execute(
    m: &Merge,
    dest: &Path,
    journal: Option<(&Db, i64)>,
    stop: &AtomicBool,
    progress: &mut dyn FnMut(&str, u64, u64),
) -> Result<Report> {
    let mut r = m.report.clone();
    let mut made: BTreeSet<PathBuf> = BTreeSet::new();
    let mut log = match journal {
        Some((db, _)) => Some(db.conn.prepare_cached(
            "INSERT INTO mail_merge_log(merge_id, dest, folder, source, offset, size, sha256, flags, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?),
        None => None,
    };
    let mut logged = match journal {
        Some((db, _)) => Some(
            db.conn
                .prepare_cached("SELECT 1 FROM mail_merge_log WHERE merge_id = ?1 AND dest = ?2")?,
        ),
        None => None,
    };
    let (mut n, mut last) = (0u64, Instant::now());
    for p in &m.placements {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let occ = &m.cat.occs[p.occ as usize];
        let folder = &m.cat.folders[p.folder as usize];
        let dir = folder_dir(dest, &p.key, folder);
        if !made.contains(&dir) {
            make_maildir(&dir, folder != "INBOX")
                .with_context(|| format!("can't create {}", dir.display()))?;
            made.insert(dir.clone());
        }
        n += 1;
        if last.elapsed().as_millis() >= 250 {
            last = Instant::now();
            progress(folder, n, r.written_bytes);
        }
        let name = message_name(occ, p.flags);
        let content = match content_of(&m.cat, occ) {
            Ok(c) if Sha256::digest(&c)[..] == occ.sha[..] => c,
            Ok(_) => {
                r.failed += 1;
                push(
                    &mut r.problems,
                    format!(
                        "a message in {} changed since it was read: skipped",
                        m.sources[occ.src as usize].path.display()
                    ),
                );
                continue;
            }
            Err(e) => {
                r.failed += 1;
                push(&mut r.problems, format!("can't read a message again: {e}"));
                continue;
            }
        };
        let path = dir.join("cur").join(&name);
        match write_message(&dir, &name, &content, &occ.sha, occ.time) {
            Ok(fresh) => {
                if fresh {
                    r.written += 1;
                    r.written_bytes += occ.size;
                } else {
                    r.resumed += 1;
                }
                if let (Some((db, id)), Some(log), Some(logged)) = (journal, &mut log, &mut logged)
                {
                    let key = paths::to_bytes(&path);
                    if logged
                        .query_row(params![id, key], |_| Ok(()))
                        .optional()?
                        .is_none()
                    {
                        let (src, offset) = source_of(&m.cat, occ);
                        // and in the chain of custody, in the same transaction
                        let ev = Event::new(
                            Action::Merge,
                            ("merge", id),
                            &src,
                            Some(&occ.sha[..]),
                            occ.size,
                        )
                        .to(&key, Some(&occ.sha[..]));
                        custody::journaled(&db.conn, &ev, || {
                            log.execute(params![
                                id,
                                key,
                                if p.key.is_empty() {
                                    folder.clone()
                                } else {
                                    format!("{}: {folder}", p.key)
                                },
                                src,
                                offset,
                                occ.size as i64,
                                occ.sha.to_vec(),
                                letters(p.flags),
                                now()
                            ])
                        })?;
                    }
                }
            }
            Err(e) => {
                r.failed += 1;
                push(&mut r.problems, format!("{e:#}"));
            }
        }
    }
    for d in &made {
        sync_dir(&d.join("cur"));
    }
    progress("", n, r.written_bytes);
    Ok(r)
}
