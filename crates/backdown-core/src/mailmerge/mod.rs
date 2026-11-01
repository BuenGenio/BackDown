//! Merging mailboxes: copies of the same mailbox taken at different times (a Maildir, Maildir++
//! folders, mbox files, a Thunderbird folder of mbox files) made into one Maildir, every message
//! once. And the way back out: a Maildir folder exported as one mbox file (mboxrd).
//!
//! **Which messages are the same.** By Message-ID, together with the message's body (line endings
//! and mbox quoting evened out): two copies of one message that a server or a mail program gave
//! different headers to (Status, X-UID, an extra Received line) are one message; two different
//! messages that happen to share an ID (a mailer that reuses them, drafts saved over and over) are
//! both kept. A message without a Message-ID is matched by its main headers and body instead.
//!
//! **Where each goes.** The newest snapshot that has a message decides its folder(s): a message
//! filed from INBOX into Archive between two backups ends up in Archive only, and one deleted
//! since the older backup is kept, in the folder it was in then. Snapshots are ordered by their
//! newest message. Folders that only one snapshot has are kept.
//!
//! **Flags.** Seen, replied, flagged and forwarded are combined from every copy (read anywhere
//! means read); draft and deleted come from the copy that's kept. Mbox flags are read from
//! Status / X-Status (mutt, Pine) and X-Mozilla-Status (Thunderbird).
//!
//! **What's written.** Each message's bytes exactly as its source holds them (an mbox message
//! without its "From " line and with its ">From " quoting undone), into `cur/` of the right folder
//! under a name made from its delivery time and content hash, so running again finds what's
//! there. Each is written to `tmp/`, flushed, renamed into place, read back and compared, and
//! journaled (and recorded in the chain of custody, custody.rs). Nothing is ever written to a
//! source, and nothing at the destination is overwritten.
//!
//! Mail stores (a server's folder of domains and users) merge mailbox by mailbox: each mailbox is
//! matched with the same one (the same path inside the store) in the other snapshots.

use std::fs::{self};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, params};

use crate::clock::now;
use crate::{Db, fmt};

mod deciding;
mod export;
mod message;
mod sources;
mod writing;

pub use deciding::*;
pub use export::*;
pub use message::*;
use sources::*;
use writing::*;

/// The tables (made by `Db::open`): each merge (or export), and each message it wrote.
pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS mail_merges (
    id         INTEGER PRIMARY KEY,
    created_at INTEGER NOT NULL,
    kind       TEXT NOT NULL,         -- merge | export
    sources    TEXT NOT NULL,         -- JSON list of paths
    dest       TEXT NOT NULL,         -- the Maildir (or the mbox file) written
    plan_id    INTEGER,               -- a filing plan's merge item, if it came from one
    item       INTEGER,
    state      TEXT NOT NULL,         -- new | running | done | partial | failed | stopped
    summary    TEXT,
    report     TEXT                   -- JSON: Report
);
CREATE TABLE IF NOT EXISTS mail_merge_log (
    id       INTEGER PRIMARY KEY,
    merge_id INTEGER NOT NULL REFERENCES mail_merges(id),
    dest     BLOB NOT NULL,           -- the message file written
    folder   TEXT NOT NULL,
    source   BLOB NOT NULL,           -- where its bytes came from: a Maildir file, or an mbox
    offset   INTEGER,                 -- in an mbox: where the message starts
    size     INTEGER NOT NULL,
    sha256   BLOB NOT NULL,           -- of the bytes written, as read back
    flags    TEXT NOT NULL,
    at       INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS mail_merge_log_dest ON mail_merge_log(merge_id, dest);
";

pub const SEEN: u8 = 1;
pub const REPLIED: u8 = 2;
pub const FLAGGED: u8 = 4;
pub const TRASHED: u8 = 8;
pub const DRAFT: u8 = 16;
pub const PASSED: u8 = 32;
/// combined from every copy
const UNION: u8 = SEEN | REPLIED | FLAGGED | PASSED;

/// Maildir's info separator: ':' where file names may hold it, '!' on Windows.
const INFO: &str = if cfg!(windows) { "!2," } else { ":2," };

fn push(v: &mut Vec<String>, s: String) {
    if v.len() < 30 {
        v.push(s);
    }
}

// ---------- the public face

/// What merging `sources` into `dest` would do. Reads every message of every source; writes
/// nothing, anywhere.
pub fn dry_run(
    sources: &[PathBuf],
    dest: &Path,
    stop: &AtomicBool,
    progress: &mut dyn FnMut(&str, u64, u64),
) -> Result<Report> {
    let dest = resolve_dest(dest)?;
    check_dest(None, None, &dest)?;
    let m = plan(sources, &dest, stop, progress)?;
    let mut r = m.report;
    r.dry_run = true;
    Ok(r)
}

/// Record a merge before running it (the daemon runs it as a job).
pub fn record(
    db: &Db,
    sources: &[PathBuf],
    dest: &Path,
    plan_item: Option<(i64, i64)>,
) -> Result<i64> {
    let dest = resolve_dest(dest)?;
    db.conn.execute(
        "INSERT INTO mail_merges(created_at, kind, sources, dest, plan_id, item, state) VALUES (?1, 'merge', ?2, ?3, ?4, ?5, 'new')",
        params![
            now(),
            source_list(sources)?,
            dest.display().to_string(),
            plan_item.map(|p| p.0),
            plan_item.map(|p| p.1)
        ],
    )?;
    Ok(db.conn.last_insert_rowid())
}

fn source_list(sources: &[PathBuf]) -> Result<String> {
    let list: Vec<String> = sources
        .iter()
        .map(|s| {
            dunce::canonicalize(s)
                .unwrap_or_else(|_| s.clone())
                .display()
                .to_string()
        })
        .collect();
    Ok(serde_json::to_string(&list)?)
}

/// An unfinished merge of the same sources into the same place, to carry on with.
pub fn unfinished(db: &Db, sources: &[PathBuf], dest: &Path) -> Result<Option<i64>> {
    let dest = resolve_dest(dest)?;
    Ok(db
        .conn
        .query_row(
            "SELECT id FROM mail_merges WHERE kind = 'merge' AND sources = ?1 AND dest = ?2 AND state != 'done' ORDER BY id DESC LIMIT 1",
            params![source_list(sources)?, dest.display().to_string()],
            |r| r.get(0),
        )
        .optional()?)
}

/// The merge a filing plan's item made, if it made one.
pub fn for_plan_item(db: &Db, plan: i64, item: i64) -> Result<Option<i64>> {
    Ok(db
        .conn
        .query_row(
            "SELECT id FROM mail_merges WHERE plan_id = ?1 AND item = ?2 ORDER BY id DESC LIMIT 1",
            params![plan, item],
            |r| r.get(0),
        )
        .optional()?)
}

/// A recorded merge: (sources, dest, state).
pub fn load(db: &Db, id: i64) -> Result<(Vec<PathBuf>, PathBuf, String)> {
    let (sources, dest, state): (String, String, String) = db
        .conn
        .query_row(
            "SELECT sources, dest, state FROM mail_merges WHERE id = ?1 AND kind = 'merge'",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .with_context(|| format!("there's no mail merge {id}"))?;
    let sources: Vec<String> = serde_json::from_str(&sources)?;
    Ok((
        sources.into_iter().map(PathBuf::from).collect(),
        PathBuf::from(dest),
        state,
    ))
}

/// Merges and exports on record, newest first.
pub fn history(db: &Db) -> Result<Vec<serde_json::Value>> {
    let mut stmt = db.conn.prepare(
        "SELECT id, created_at, kind, sources, dest, state, summary, plan_id FROM mail_merges ORDER BY id DESC LIMIT 30",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(serde_json::json!({
            "id": r.get::<_, i64>(0)?, "created_at": r.get::<_, i64>(1)?, "kind": r.get::<_, String>(2)?,
            "sources": serde_json::from_str::<serde_json::Value>(&r.get::<_, String>(3)?).unwrap_or_default(),
            "dest": r.get::<_, String>(4)?, "state": r.get::<_, String>(5)?, "summary": r.get::<_, Option<String>>(6)?,
            "plan": r.get::<_, Option<i64>>(7)?,
        }))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The report a finished merge stored.
pub fn report_of(db: &Db, id: i64) -> Result<Option<Report>> {
    let r: Option<Option<String>> = db
        .conn
        .query_row(
            "SELECT report FROM mail_merges WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(match r.flatten() {
        Some(s) => Some(serde_json::from_str(&s)?),
        None => None,
    })
}

/// One line about a merge.
pub fn summary(r: &Report) -> String {
    let mut s = format!(
        "{} {} distinct messages{} ({}) from {} snapshots; {} duplicate copies left out",
        if r.dry_run { "would merge" } else { "merged" },
        fmt::count(r.distinct),
        if r.mailboxes > 1 {
            format!(" in {} mailboxes", fmt::count(r.mailboxes))
        } else {
            String::new()
        },
        fmt::bytes(r.bytes),
        r.sources.len(),
        fmt::count(r.duplicates)
    );
    if !r.dry_run {
        s += &format!(
            "; {} written and verified{}",
            fmt::count(r.written),
            if r.resumed > 0 {
                format!(", {} already there", fmt::count(r.resumed))
            } else {
                String::new()
            }
        );
    }
    if r.failed + r.unreadable > 0 {
        s += &format!(
            "; {} failed or unreadable",
            fmt::count(r.failed + r.unreadable)
        );
    }
    s
}

/// Carry out merge `id` (made by `record`): read every source, write the merged Maildir, journal
/// each message. Running it again carries on where it stopped.
pub fn run(
    db: &Db,
    id: i64,
    stop: &AtomicBool,
    progress: &mut dyn FnMut(&str, u64, u64),
) -> Result<Report> {
    let r = (|| -> Result<Report> {
        let (sources, dest, _) = load(db, id)?;
        check_dest(Some(db), Some(id), &dest)?;
        db.conn.execute(
            "UPDATE mail_merges SET state = 'running' WHERE id = ?1",
            params![id],
        )?;
        let m = plan(&sources, &dest, stop, progress)?;
        fs::create_dir_all(&dest).with_context(|| format!("can't create {}", dest.display()))?;
        execute(&m, &dest, Some((db, id)), stop, progress)
    })();
    match &r {
        Ok(rep) => {
            let state = if stop.load(Ordering::Relaxed) {
                "stopped"
            } else if rep.failed + rep.unreadable > 0 {
                "partial"
            } else {
                "done"
            };
            db.conn.execute(
                "UPDATE mail_merges SET state = ?2, summary = ?3, report = ?4 WHERE id = ?1",
                params![id, state, summary(rep), serde_json::to_string(rep)?],
            )?;
        }
        Err(e) => {
            let _ = db.conn.execute(
                "UPDATE mail_merges SET state = ?2, summary = ?3 WHERE id = ?1",
                params![
                    id,
                    if stop.load(Ordering::Relaxed) {
                        "stopped"
                    } else {
                        "failed"
                    },
                    format!("{e:#}")
                ],
            );
        }
    }
    r
}

#[cfg(test)]
mod tests;
