//! Removing a drive (a root) from the index: everything recorded of it goes, as if it had never
//! been added. Nothing on the drive itself is touched.
//!
//! Its files, folders and scans, what was recognised on it (things, people, databases), what was
//! indexed inside them for search, its NTFS streams, the physical drive it was last seen on, a
//! legacy import's resume point and pool plans for it that never started. What stays, on purpose:
//! signed manifests (a signed statement outlives its drive's record, manifest.rs), the chain of
//! custody, and the history of plans, wipes, mirrors and journals, which name paths, not roots.
//!
//! A big drive holds millions of rows, so they go in batches, each its own transaction, and other
//! writers (the health thread, a machine's sync) get their turn in between. The root is marked
//! first (`roots_removing`), so an interrupted removal is finished later rather than left half
//! done: a marked root isn't scanned again (`index::scan` refuses it), and `pending` lists it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use crate::db::{Db, Lock, Tx};
use crate::{clock, fts, paths};

/// Rows deleted per transaction.
const BATCH: usize = 20_000;

#[derive(Debug, Default, Clone, Serialize)]
pub struct Forgotten {
    pub root: PathBuf,
    /// file versions (current and earlier ones)
    pub files: u64,
    /// folder versions
    pub dirs: u64,
    pub scans: u64,
}

/// Where a removal is, for progress.
#[derive(Debug, Clone, Copy)]
pub struct Progress {
    pub stage: &'static str,
    pub done: u64,
    pub total: u64,
}

fn has_table(db: &Db, name: &str) -> Result<bool> {
    Ok(db.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [name],
        |r| r.get(0),
    )?)
}

/// The id of the root whose path is `root` (as it was indexed: canonical, in its own bytes).
pub fn root_id(db: &Db, root: &Path) -> Result<Option<i64>> {
    let id = db
        .conn
        .query_row(
            "SELECT id FROM roots WHERE path = ?1",
            params![paths::to_bytes(root)],
            |r| r.get(0),
        )
        .optional()?;
    Ok(id)
}

/// Mark `root` for removal: from now on it isn't scanned again, and the drives list leaves it out.
/// Returns its id. Marking it twice is harmless.
pub fn mark(db: &Db, root: &Path) -> Result<i64> {
    let Some(id) = root_id(db, root)? else {
        bail!("{} isn't an indexed drive", root.display());
    };
    db.conn.execute(
        "INSERT OR IGNORE INTO roots_removing(root_id, at) VALUES (?1, ?2)",
        params![id, clock::now()],
    )?;
    Ok(id)
}

/// Is this root being removed? (Only on an index opened read-write, which has the table.)
pub fn is_marked(db: &Db, root_id: i64) -> Result<bool> {
    Ok(db.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM roots_removing WHERE root_id = ?1)",
        [root_id],
        |r| r.get(0),
    )?)
}

/// Roots whose removal was started and not finished (the daemon carries on with them).
pub fn pending(db: &Db) -> Result<Vec<PathBuf>> {
    let mut stmt = db.conn.prepare(
        "SELECT r.path FROM roots_removing m JOIN roots r ON r.id = m.root_id ORDER BY m.at",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))?;
    rows.map(|r| Ok(paths::from_bytes(&r?))).collect()
}

/// Delete, `BATCH` rows at a time, the rows `select` names (`?1` is the root's id).
fn in_batches(
    db: &Db,
    root_id: i64,
    select: &str,
    deletes: &[&str],
    stage: &'static str,
    total: u64,
    progress: &mut dyn FnMut(Progress),
) -> Result<u64> {
    let mut done = 0u64;
    loop {
        let ids: Vec<i64> = {
            let mut stmt = db.conn.prepare_cached(select)?;
            let rows = stmt.query_map(params![root_id, BATCH as i64], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        if ids.is_empty() {
            return Ok(done);
        }
        let tx = Tx::begin(&db.conn, Lock::Immediate)?;
        for sql in deletes {
            let mut stmt = db.conn.prepare_cached(sql)?;
            for id in &ids {
                stmt.execute([id])?;
            }
        }
        tx.commit()?;
        done += ids.len() as u64;
        progress(Progress {
            stage,
            done,
            total: total.max(done),
        });
    }
}

/// Remove `root` and everything recorded of it (see the module notes). Marks it first, so if
/// this is interrupted, calling it again finishes the job.
pub fn forget(db: &Db, root: &Path, progress: &mut dyn FnMut(Progress)) -> Result<Forgotten> {
    let id = mark(db, root)?;
    let count = |sql: &str| -> Result<u64> {
        Ok(db.conn.query_row(sql, [id], |r| r.get::<_, i64>(0))? as u64)
    };
    let files_total = count("SELECT COUNT(*) FROM files WHERE root_id = ?1")?;
    let dirs_total = count("SELECT COUNT(*) FROM dirs WHERE root_id = ?1")?;
    let scans = count("SELECT COUNT(*) FROM scans WHERE root_id = ?1")?;
    progress(Progress {
        stage: "files",
        done: 0,
        total: files_total,
    });

    // search entries first: they name the root's things, and are cheap
    if has_table(db, "content_docs")? {
        fts::forget(db, id, None).context("dropping what was indexed for search")?;
    }
    // NTFS streams hang off file rows, so they go with them
    let streams = db
        .conn
        .query_row("SELECT EXISTS(SELECT 1 FROM streams)", [], |r| {
            r.get::<_, bool>(0)
        })?;
    let mut file_deletes = vec!["DELETE FROM files WHERE id = ?1"];
    if streams {
        file_deletes.insert(0, "DELETE FROM streams WHERE file_id = ?1");
    }
    let files = in_batches(
        db,
        id,
        "SELECT id FROM files WHERE root_id = ?1 LIMIT ?2",
        &file_deletes,
        "files",
        files_total,
        progress,
    )?;
    let dirs = in_batches(
        db,
        id,
        "SELECT id FROM dirs WHERE root_id = ?1 LIMIT ?2",
        &["DELETE FROM dirs WHERE id = ?1"],
        "folders",
        dirs_total,
        progress,
    )?;

    // the rest is small: one transaction, with the root itself last (foreign keys point at it)
    let tx = Tx::begin(&db.conn, Lock::Immediate)?;
    for sql in [
        "DELETE FROM entities WHERE root_id = ?1",
        "DELETE FROM subjects WHERE root_id = ?1",
        "DELETE FROM streams_roots WHERE root_id = ?1",
        "DELETE FROM root_drives WHERE root_id = ?1",
    ] {
        db.conn.execute(sql, [id])?;
    }
    if has_table(db, "legacy_imports")? {
        db.conn
            .execute("DELETE FROM legacy_imports WHERE root_id = ?1", [id])?;
    }
    if has_table(db, "pool_plans")? {
        // one that sent anything is history; one that never started is only a proposal
        db.conn.execute(
            "DELETE FROM pool_plans WHERE root_id = ?1 AND state = 'new'",
            [id],
        )?;
    }
    db.conn
        .execute("UPDATE roots SET last_scan = NULL WHERE id = ?1", [id])?;
    db.conn
        .execute("DELETE FROM scans WHERE root_id = ?1", [id])?;
    db.conn
        .execute("DELETE FROM roots_removing WHERE root_id = ?1", [id])?;
    db.conn.execute("DELETE FROM roots WHERE id = ?1", [id])?;
    tx.commit()?;
    progress(Progress {
        stage: "done",
        done: files,
        total: files,
    });
    Ok(Forgotten {
        root: root.to_path_buf(),
        files,
        dirs,
        scans,
    })
}
