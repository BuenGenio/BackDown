//! What things are: recognisers over the index.
//!
//! A drive isn't a heap of files to the person who owns it: it's "the mail server's maildir backup",
//! "my old MacBook's Time Machine", "Dad's iPhone backup from 2016". This module finds those
//! *entities* (APP-PLAN §6) with rules over the index, adds what the drive itself says about where
//! they came from (a backup script's rsync line, an iPhone backup's Info.plist, a system's
//! /etc/hostname, a repository's remote), and marks the parts that are regenerable junk (caches,
//! previews, trash) so they don't count as data that must be kept.
//!
//! It scales with the index, not with memory: two streaming passes collect only "marker" names
//! (`cur`, `Manifest.db`, `.git`, …), per-folder media counts and a few file lists; rules then decide
//! what the marked folders are, and each entity's dates come from a range scan of the path index.
//! Provenance needs the drive to be connected (it reads small text files, read-only); without it,
//! entities are still found, just with less said about them.
//!
//! Results are stored per root (table `entities`, one JSON document per root) with the scan they
//! describe, so the browser and reports show them at once and know when they're out of date.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::clock::now;
use crate::{Db, browse, fmt, paths};

mod collect;
mod markers;
mod provenance;
mod rules;

pub(crate) use collect::*;
pub(crate) use markers::*;
pub use provenance::*;
pub use rules::*;
// what other modules and the CLI build on: dump names and headers
pub use collect::dump_stem;
pub use markers::is_sql_dump;

/// A recognised thing: a folder (or, for disk images and dumps, a file) with what it is.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entity {
    /// stable id of the kind: "mail-store", "timemachine", "git", … (see `KINDS`)
    pub kind: String,
    /// the kind for people: "Mail server store"
    pub label: String,
    /// one line about this one: "3 domains, 142 mailboxes"
    pub title: String,
    pub path: String,
    /// the browser's key for `path`
    pub key: String,
    pub is_file: bool,
    /// 0..1: how sure the rule is
    pub confidence: f32,
    pub files: u64,
    pub bytes: u64,
    /// modification times of the files inside (not when they were copied here)
    pub oldest: Option<i64>,
    pub newest: Option<i64>,
    /// the facts behind the title, in order ("Mailboxes" → "142")
    pub facts: Vec<(String, String)>,
    /// where it came from, as sentences ("Copied from mail.example.com:/var/vmail with rsync (backup-mail.sh).")
    pub provenance: Vec<String>,
    /// regenerable parts inside it
    pub junk: Vec<Junk>,
    /// the entity it sits in, if any (by key)
    pub inside: Option<String>,
    /// probably not worth keeping as a unit (a downloads pile, recovered fragments)
    pub low_value: bool,
}

/// A regenerable subtree: caches, thumbnails, previews, build output, trash.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Junk {
    pub path: String,
    pub key: String,
    pub why: String,
    pub files: u64,
    pub bytes: u64,
}

/// Everything recognised on one root, as of one scan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RootEntities {
    pub root_id: i64,
    pub root: String,
    pub scan: i64,
    pub computed_at: i64,
    /// the drive was connected, so provenance files could be read
    pub read_drive: bool,
    pub entities: Vec<Entity>,
    /// junk that isn't inside any entity
    pub junk: Vec<Junk>,
}

/// Kinds, their labels, and the order they're listed in.
pub const KINDS: &[(&str, &str)] = &[
    ("mail-store", "Mail server store"),
    ("maildir", "Mailbox (Maildir)"),
    ("mdbox", "Mail store (Dovecot mdbox)"),
    ("thunderbird", "Thunderbird profile"),
    ("nextcloud", "Nextcloud data"),
    ("nextcloud-server", "Nextcloud server"),
    ("timemachine", "Time Machine backup"),
    ("ios-backup", "iPhone or iPad backup"),
    ("android-backup", "Android backup"),
    ("system", "Operating system"),
    ("home-mac", "Mac home folder"),
    ("home-linux", "Linux home folder"),
    ("home-windows", "Windows user profile"),
    ("website", "Website"),
    ("git", "Git repository"),
    ("sql-dump", "Database dump"),
    ("mongo-dump", "MongoDB dump"),
    ("sqlite-db", "SQLite database"),
    ("mysql-data", "MySQL or MariaDB data directory"),
    ("postgres-data", "PostgreSQL data directory"),
    ("mongo-data", "MongoDB data directory"),
    ("disk-image", "Disk image"),
    ("photos", "Photo library"),
    ("music", "Music library"),
    ("videos", "Video library"),
    ("export", "Data export"),
    ("downloads", "Downloads pile"),
    ("recovery", "Recovered files"),
];

fn label(kind: &str) -> String {
    KINDS
        .iter()
        .find(|(k, _)| *k == kind)
        .map_or(kind, |(_, l)| l)
        .to_string()
}

// ---------- putting it together

/// Recognise everything on one root, as of its latest scan. `read_drive` lets it open small files
/// on the drive for provenance (skipped anyway when the drive isn't there).
pub fn recognise(
    db: &Db,
    root_id: i64,
    read_drive: bool,
    progress: &mut dyn FnMut(&str),
) -> Result<RootEntities> {
    let (root_bytes, scan): (Vec<u8>, Option<i64>) = db.conn.query_row(
        "SELECT path, last_scan FROM roots WHERE id = ?1",
        params![root_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let scan = scan.context("this root has no finished scan yet")?;
    let root_path = paths::from_bytes(&root_bytes);
    let read_drive = read_drive && root_path.is_dir();
    let m = collect(db, root_id, progress)?;
    progress("recognising");
    let mut cands = rules(db, &root_bytes, &m)?;
    cands.extend(media_libraries(db, &root_bytes, &m)?);
    let junk_list = junk_dirs(&m);
    let junk_prefixes: Vec<Vec<u8>> = junk_list
        .iter()
        .map(|(p, _)| paths::dir_prefix(&paths::from_bytes(p)))
        .collect();
    let in_junk = |p: &[u8]| junk_prefixes.iter().any(|j| p.starts_with(j));

    // one entity per path (the more certain rule wins), none inside junk
    cands.retain(|c| !in_junk(&c.path));
    cands.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then(b.confidence.total_cmp(&a.confidence))
    });
    cands.dedup_by(|b, a| a.path == b.path);
    // containers whose insides aren't separate things: snapshots in a Time Machine set, mailboxes
    // in a store, libraries inside backups, submodules inside repositories
    let swallows = |outer: &str, inner: &str| match outer {
        "timemachine" | "ios-backup" | "android-backup" => true,
        // programs keep their own SQLite files everywhere: only a loose one is a thing of its own
        _ if inner == "sqlite-db" => !matches!(outer, "sql-dump" | "mongo-dump"),
        "mail-store" | "mdbox" => inner == "maildir",
        "git" => inner == "git",
        "system" => matches!(
            inner,
            "photos" | "music" | "videos" | "sql-dump" | "downloads"
        ),
        "nextcloud" => matches!(inner, "photos" | "music" | "videos" | "maildir" | "git"),
        "website" => matches!(inner, "photos" | "git" | "website"),
        "export" => matches!(inner, "photos" | "videos" | "music"),
        "mongo-dump" => inner == "mongo-dump",
        _ => false,
    };
    let dir_prefix_of = |c: &Candidate| {
        if c.is_file {
            c.path.clone()
        } else {
            paths::dir_prefix(&paths::from_bytes(&c.path))
        }
    };
    let prefixes: Vec<(Vec<u8>, &'static str, bool)> = cands
        .iter()
        .map(|c| (dir_prefix_of(c), c.kind, c.is_file))
        .collect();
    // (a folder's prefix ends with a separator, so starting with it means being strictly inside)
    let keep: Vec<bool> = cands
        .iter()
        .map(|c| {
            !prefixes
                .iter()
                .any(|(p, k, is_file)| !is_file && c.path.starts_with(p) && swallows(k, c.kind))
        })
        .collect();
    let cands: Vec<Candidate> = cands
        .into_iter()
        .zip(keep)
        .filter_map(|(c, k)| k.then_some(c))
        .collect();

    // scripts and sync markers, for provenance
    let mut scripts: Vec<(PathBuf, Vec<Transfer>)> = Vec::new();
    let mut sync_markers: Vec<(Vec<u8>, &'static str)> = Vec::new();
    if read_drive {
        progress("reading backup scripts");
        for s in &m.scripts {
            let p = paths::from_bytes(s);
            if let Some(t) = read_small(&p, 256 * 1024) {
                let ts = parse_script(&t);
                if !ts.is_empty() {
                    scripts.push((p, ts));
                }
            }
        }
    }
    for (dir, names) in &m.dirs {
        if names.contains(".stfolder") {
            sync_markers.push((dir.clone(), "Syncthing"));
        }
        if names.contains(".sync") {
            sync_markers.push((dir.clone(), "Resilio Sync"));
        }
        if names.contains(".dropbox.cache") {
            sync_markers.push((dir.clone(), "Dropbox"));
        }
    }
    for (dir, files) in &m.files {
        if files.contains(".dropbox") {
            sync_markers.push((dir.clone(), "Dropbox"));
        }
        if files.contains(".nextcloudsync.log") || files.iter().any(|f| f.starts_with("._sync_")) {
            sync_markers.push((dir.clone(), "the Nextcloud desktop client"));
        }
        if files.contains(".owncloudsync.log") {
            sync_markers.push((dir.clone(), "the ownCloud desktop client"));
        }
    }

    // sizes, dates and the rest, per entity
    progress("measuring");
    let sep = sep();
    let mut totals = db
        .conn
        .prepare_cached("SELECT files, bytes FROM dirs WHERE path = ?1 AND current = 1")?;
    let mut file_row = db
        .conn
        .prepare_cached("SELECT size, mtime FROM files WHERE path = ?1 AND current = 1")?;
    let mut range = db.conn.prepare_cached(
        "SELECT dir, mtime FROM files WHERE current = 1 AND path >= ?1 AND path < ?2",
    )?;
    let cur = [&sep[..], &paths::to_bytes(Path::new("cur"))[..]].concat();
    let new = [&sep[..], &paths::to_bytes(Path::new("new"))[..]].concat();
    let mut entities = Vec::new();
    for c in &cands {
        let p = paths::from_bytes(&c.path);
        let mut e = Entity {
            kind: c.kind.to_string(),
            label: label(c.kind),
            title: c.title.clone(),
            path: p.display().to_string(),
            key: browse::key(&p),
            is_file: c.is_file,
            confidence: c.confidence,
            files: 0,
            bytes: 0,
            oldest: None,
            newest: None,
            facts: c.facts.clone(),
            provenance: Vec::new(),
            junk: Vec::new(),
            inside: None,
            low_value: c.low_value,
        };
        if c.is_file {
            if let Some((size, mtime)) = file_row
                .query_row(params![c.path], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
                })
                .optional()?
            {
                e.files = 1;
                e.bytes = size as u64;
                e.oldest = Some(mtime);
                e.newest = Some(mtime);
            }
        } else {
            if let Some((files, bytes)) = totals
                .query_row(params![c.path], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
                })
                .optional()?
            {
                e.files = files as u64;
                e.bytes = bytes as u64;
            }
            let lo = paths::dir_prefix(&p);
            let mut hi = lo.clone();
            if let Some(last) = hi.last_mut() {
                *last += 1; // a separator is never 0xFF
            }
            let mail = matches!(c.kind, "mail-store" | "maildir");
            let mut messages = 0u64;
            let rows = range.query_map(params![lo, hi], |r| {
                Ok((r.get::<_, Option<Vec<u8>>>(0)?, r.get::<_, i64>(1)?))
            })?;
            for row in rows {
                let (dir, mtime) = row?;
                if mtime > 0 {
                    e.oldest = Some(e.oldest.map_or(mtime, |o| o.min(mtime)));
                    e.newest = Some(e.newest.map_or(mtime, |n| n.max(mtime)));
                }
                if mail && dir.is_some_and(|d| d.ends_with(&cur) || d.ends_with(&new)) {
                    messages += 1;
                }
            }
            if mail {
                e.facts.push(("Messages".into(), fmt::count(messages)));
            }
        }
        // mail stores that say more about themselves (mailstores.rs)
        crate::mailstores::describe(db, &mut e, &p, read_drive)?;
        if read_drive {
            provenance(&mut e, &root_path, &p, &scripts, &sync_markers);
        }
        entities.push(e);
    }

    // nesting: each entity's closest container
    let dirs_by_len: Vec<(Vec<u8>, String)> = entities
        .iter()
        .filter(|e| !e.is_file)
        .map(|e| (paths::dir_prefix(Path::new(&e.path)), e.key.clone()))
        .collect();
    for e in &mut entities {
        let me = paths::to_bytes(Path::new(&e.path));
        e.inside = dirs_by_len
            .iter()
            .filter(|(p, k)| me.starts_with(p) && *k != e.key)
            .max_by_key(|(p, _)| p.len())
            .map(|(_, k)| k.clone());
    }

    // junk: its size, and the entity it belongs to
    let mut loose = Vec::new();
    for (path, why) in &junk_list {
        let Some((files, bytes)) = totals
            .query_row(params![path], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
            })
            .optional()?
        else {
            continue;
        };
        if bytes == 0 {
            continue;
        }
        let p = paths::from_bytes(path);
        let j = Junk {
            path: p.display().to_string(),
            key: browse::key(&p),
            why: (*why).to_string(),
            files: files as u64,
            bytes: bytes as u64,
        };
        let owner = entities
            .iter_mut()
            .filter(|e| !e.is_file && path.starts_with(&paths::dir_prefix(Path::new(&e.path))))
            .max_by_key(|e| e.path.len());
        match owner {
            Some(e) => e.junk.push(j),
            None => loose.push(j),
        }
    }
    for e in &mut entities {
        e.junk.sort_by_key(|j| std::cmp::Reverse(j.bytes));
    }
    loose.sort_by_key(|j| std::cmp::Reverse(j.bytes));
    let order = |k: &str| {
        KINDS
            .iter()
            .position(|(x, _)| *x == k)
            .unwrap_or(usize::MAX)
    };
    entities.sort_by(|a, b| {
        order(&a.kind)
            .cmp(&order(&b.kind))
            .then(b.bytes.cmp(&a.bytes))
            .then(a.path.cmp(&b.path))
    });
    Ok(RootEntities {
        root_id,
        root: root_path.display().to_string(),
        scan,
        computed_at: now(),
        read_drive,
        entities,
        junk: loose,
    })
}

// ---------- storage

/// Keep the result for the browser and reports. Needs a writable connection.
pub fn store(db: &Db, r: &RootEntities) -> Result<()> {
    db.conn.execute(
        "INSERT INTO entities(root_id, scan, computed_at, data) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(root_id) DO UPDATE SET scan = excluded.scan, computed_at = excluded.computed_at, data = excluded.data",
        params![r.root_id, r.scan, r.computed_at, serde_json::to_string(r)?],
    )?;
    Ok(())
}

/// What's stored for a root, and whether it describes the root's latest scan.
pub fn load(db: &Db, root_id: i64) -> Result<Option<(RootEntities, bool)>> {
    let row: Option<(String, i64)> = db
        .conn
        .query_row(
            "SELECT e.data, r.last_scan FROM entities e JOIN roots r ON r.id = e.root_id WHERE e.root_id = ?1",
            params![root_id],
            |r| Ok((r.get(0)?, r.get::<_, Option<i64>>(1)?.unwrap_or(0))),
        )
        .optional()?;
    Ok(match row {
        Some((data, last)) => {
            let r: RootEntities = serde_json::from_str(&data)?;
            let fresh = r.scan == last;
            Some((r, fresh))
        }
        None => None,
    })
}

/// Ids of roots with a finished scan.
pub fn root_ids(db: &Db) -> Result<Vec<i64>> {
    let mut stmt = db
        .conn
        .prepare("SELECT id FROM roots WHERE last_scan IS NOT NULL ORDER BY path")?;
    let ids = stmt
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

#[cfg(test)]
mod tests;
