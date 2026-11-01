//! What has been read of each mailbox, kept between requests.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Instant, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use backdown_core::{Db, browse, paths};
use rusqlite::params;
use serde_json::{Value, json};

use super::*;

// ---- what has been read of each mailbox, kept between requests

pub(super) enum Source {
    Mbox(PathBuf),
    Maildir(Vec<MailFile>),
}

pub(super) struct Scan {
    pub(super) src: Source,
    pub(super) heads: Vec<Head>,
    /// Where reading carries on: a byte offset (mbox) or a file number (Maildir).
    pub(super) pos: u64,
    /// Bytes (mbox) or files (Maildir) in all.
    pub(super) size: u64,
    pub(super) done: bool,
    /// Stopped at `MAX_MESSAGES`.
    pub(super) capped: bool,
    /// Maildir messages whose bytes couldn't be read.
    pub(super) unreadable: u64,
    pub(super) threads: Option<(usize, Arc<Vec<Thread>>)>,
}

impl Scan {
    pub(super) fn new(src: Source, size: u64) -> Scan {
        Scan {
            src,
            heads: Vec::new(),
            pos: 0,
            size,
            done: false,
            capped: false,
            unreadable: 0,
            threads: None,
        }
    }

    /// Reads on until `until` (always at least one message), the end, or `MAX_MESSAGES`.
    pub(super) fn step(&mut self, db: &Db, until: Instant) -> Result<()> {
        if self.done || self.capped {
            return Ok(());
        }
        let room = MAX_MESSAGES.saturating_sub(self.heads.len());
        match &self.src {
            Source::Mbox(path) => {
                let (pos, stop) = scan_mbox(path, self.pos, until, room, &mut self.heads)?;
                self.pos = pos;
                match stop {
                    Stop::Done => self.done = true,
                    Stop::Full => self.capped = true,
                    Stop::Budget => {}
                }
            }
            Source::Maildir(files) => {
                let start = self.pos;
                while let Some(f) = files.get(self.pos as usize) {
                    if self.heads.len() >= MAX_MESSAGES {
                        self.capped = true;
                        return Ok(());
                    }
                    if self.pos > start && Instant::now() > until {
                        return Ok(());
                    }
                    match open_indexed(db, f).map(read_head) {
                        Some(Ok(raw)) => {
                            self.heads
                                .push(head_of(&raw, f.id, f.size, Some(f.time), f.flags))
                        }
                        _ => self.unreadable += 1,
                    }
                    self.pos += 1;
                }
                self.done = true;
            }
        }
        Ok(())
    }

    pub(super) fn threads(&mut self) -> Arc<Vec<Thread>> {
        if let Some((n, t)) = &self.threads
            && *n == self.heads.len()
        {
            return Arc::clone(t);
        }
        let t = Arc::new(thread(&self.heads));
        self.threads = Some((self.heads.len(), Arc::clone(&t)));
        t
    }

    pub(super) fn status(&self) -> serde_json::Map<String, Value> {
        let v = json!({
            "kind": match self.src { Source::Mbox(_) => "mbox", Source::Maildir(_) => "maildir" },
            "messages": self.heads.len(),
            "read": self.pos,
            "size": self.size,
            "done": self.done,
            "capped": self.capped,
            "unreadable": self.unreadable,
        });
        match v {
            Value::Object(m) => m,
            _ => unreachable!(),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) enum Key {
    Mbox(PathBuf, u64, i64),
    Maildir(PathBuf, i64),
}

pub(super) struct Cached {
    pub(super) key: Key,
    pub(super) scan: Arc<Mutex<Scan>>,
    pub(super) messages: Arc<AtomicUsize>,
    pub(super) used: Instant,
}

pub(super) static CACHE: Mutex<Vec<Cached>> = Mutex::new(Vec::new());

pub(super) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub(super) type Entry = (Arc<Mutex<Scan>>, Arc<AtomicUsize>, bool);

/// The mailbox's read-so-far state, and whether it is new (nothing read yet).
pub(super) fn cached(key: Key, make: impl FnOnce() -> Result<Scan>) -> Result<Entry> {
    let find = |c: &mut Vec<Cached>| {
        c.iter_mut().find(|e| e.key == key).map(|e| {
            e.used = Instant::now();
            (Arc::clone(&e.scan), Arc::clone(&e.messages), false)
        })
    };
    if let Some(hit) = find(&mut lock(&CACHE)) {
        return Ok(hit);
    }
    let scan = make()?; // outside the lock: listing a big Maildir takes a moment
    let mut c = lock(&CACHE);
    if let Some(hit) = find(&mut c) {
        return Ok(hit);
    }
    let entry = Cached {
        key,
        scan: Arc::new(Mutex::new(scan)),
        messages: Arc::new(AtomicUsize::new(0)),
        used: Instant::now(),
    };
    let out = (Arc::clone(&entry.scan), Arc::clone(&entry.messages), true);
    c.push(entry);
    // the least recently used go first, down to the limits (never the one just added)
    while c.len() > 1
        && (c.len() > CACHE_MAILBOXES
            || c.iter()
                .map(|e| e.messages.load(Ordering::Relaxed))
                .sum::<usize>()
                > CACHE_MESSAGES)
    {
        let (i, _) = c[..c.len() - 1]
            .iter()
            .enumerate()
            .min_by_key(|(_, e)| e.used)
            .expect("more than one entry");
        c.remove(i);
    }
    Ok(out)
}

pub(super) fn source(db: &Db, id: i64) -> Result<(PathBuf, u64)> {
    browse::resolve_content(db, id)?
        .context("this version's bytes aren't on any indexed drive any more")
}

pub(super) fn mbox_entry(db: &Db, id: i64) -> Result<Entry> {
    let (path, size) = source(db, id)?;
    let mtime = std::fs::metadata(&path)?
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64);
    cached(Key::Mbox(path.clone(), size, mtime), || {
        Ok(Scan::new(Source::Mbox(path), size))
    })
}

/// Whether the index has a folder (or, from an index without folder rows, files in it).
pub(super) fn has_dir(db: &Db, dir: &Path) -> Result<bool> {
    let b = paths::to_bytes(dir);
    Ok(db.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM dirs WHERE path = ?1 AND current = 1)
             OR EXISTS(SELECT 1 FROM files WHERE dir = ?1 AND current = 1)",
        params![b],
        |r| r.get(0),
    )?)
}

pub(super) fn is_maildir(db: &Db, dir: &Path) -> Result<bool> {
    Ok(has_dir(db, &dir.join("cur"))? || has_dir(db, &dir.join("new"))?)
}

pub(super) fn maildir_entry(db: &Db, dir: &Path) -> Result<Entry> {
    let listing = browse::list(db, dir, None).context("not an indexed folder")?;
    if !is_maildir(db, dir)? {
        bail!(
            "{} isn't a Maildir (it has no cur/ or new/ folder)",
            dir.display()
        );
    }
    cached(Key::Maildir(dir.to_path_buf(), listing.latest_scan), || {
        let files = maildir_files(db, dir)?;
        let n = files.len() as u64;
        Ok(Scan::new(Source::Maildir(files), n))
    })
}

pub(super) fn leaf_name(p: &Path) -> String {
    p.file_name().map_or_else(
        || p.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// The Maildirs directly inside `dir` (its cur/, new/ and tmp/ aside).
pub(super) fn maildirs_in(db: &Db, dir: &Path) -> Result<Vec<PathBuf>> {
    let kids: Vec<PathBuf> = {
        let mut stmt = db
            .conn
            .prepare_cached("SELECT path FROM dirs WHERE parent = ?1 AND current = 1")?;
        let rows = stmt.query_map(params![paths::to_bytes(dir)], |r| r.get::<_, Vec<u8>>(0))?;
        rows.map(|r| r.map(|b| paths::from_bytes(&b)))
            .collect::<rusqlite::Result<_>>()?
    };
    let mut out = Vec::new();
    for k in kids {
        if !matches!(leaf_name(&k).as_str(), "cur" | "new" | "tmp") && is_maildir(db, &k)? {
            out.push(k);
        }
    }
    Ok(out)
}

/// The Maildir's family, for a side list: an inbox and the Maildirs inside it (Maildir++'s
/// ".Sent", ".Drafts", …, or Dovecot's "fs" layout), or Maildirs side by side in a folder that
/// isn't one (mail/INBOX, mail/Sent); empty when it's alone. And the family's top folder, whose
/// name is the mailbox's.
pub(super) fn folders(db: &Db, dir: &Path) -> Result<(Vec<Value>, PathBuf)> {
    let parent = dir.parent().filter(|p| !p.as_os_str().is_empty());
    let in_maildir = match parent {
        Some(p) => is_maildir(db, p)?,
        None => false,
    };
    let (top, mut family) = match parent {
        // a sub-folder (Maildir++'s ".Sent", or "fs" layout's "Sent"): its inbox's family
        Some(p) if in_maildir => (p.to_path_buf(), maildirs_in(db, p)?),
        _ => {
            let below = maildirs_in(db, dir)?;
            let beside = match parent {
                Some(p) if below.is_empty() => maildirs_in(db, p)?,
                _ => Vec::new(),
            };
            match parent {
                Some(p) if beside.len() > 1 => (p.to_path_buf(), beside),
                _ => (dir.to_path_buf(), below),
            }
        }
    };
    if family.is_empty() {
        return Ok((Vec::new(), top));
    }
    family.sort_by_key(|k| leaf_name(k).trim_start_matches('.').to_lowercase());
    let count = |d: &Path| -> Result<i64> {
        Ok(db.conn.query_row(
            "SELECT COUNT(*) FROM files WHERE dir = ?1 AND current = 1",
            params![paths::to_bytes(d)],
            |r| r.get(0),
        )?)
    };
    let mut out = Vec::new();
    for (i, p) in std::iter::once(&top).chain(&family).enumerate() {
        if i == 0 && !is_maildir(db, p)? {
            continue; // a folder of mailboxes, not one itself
        }
        let n = leaf_name(p);
        let shown = if i == 0 {
            "Inbox".to_string()
        } else if let Some(sub) = n.strip_prefix('.') {
            sub.replace('.', " / ")
        } else {
            n
        };
        let new = count(&p.join("new"))?;
        out.push(json!({
            "name": shown,
            "key": browse::key(p),
            "count": count(&p.join("cur"))? + new,
            "new": new,
            "here": p == dir,
        }));
    }
    Ok((out, top))
}
