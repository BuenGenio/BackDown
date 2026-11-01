//! The pool (docs/SWARM.md §3): folders on this machine's drives that belong to the swarm's
//! archive. Each machine keeps its own pool folders here, and only it can add, change or remove
//! them; the other machines learn them with its storage (`swarm::storage`, the `volumes`
//! request). Together, every machine's pool folders are one filed tree (the filing schema's
//! places: Mail/…, Media/Photos/…), each thing kept whole in one of them.
//!
//! A pool folder keeps some room free on its drive (its reserve) and may take only some top-level
//! places (a media drive takes Media; empty takes anything). Two pool folders on one file system
//! share its room: `report` says which file system each is on, so nothing counts that room twice.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{Db, browse, fmt, names, paths, volumes};

pub mod plan;

pub const SCHEMA: &str = "
-- the pool (pool.rs, docs/SWARM.md): this machine's folders that are part of the swarm's archive.
-- Added without a schema bump
CREATE TABLE IF NOT EXISTS pool_areas (
    id         INTEGER PRIMARY KEY,
    path       TEXT NOT NULL UNIQUE,      -- the folder, absolute
    reserve    INTEGER NOT NULL,          -- bytes left free on its drive
    takes      TEXT NOT NULL DEFAULT '[]',-- JSON: the top-level places it takes; empty: anything
    created_at INTEGER NOT NULL
);
";

/// How long a pool folder's drive may take to say how much room it has (a network share whose
/// server went away hangs whoever asks).
const ASK_PATH: Duration = Duration::from_secs(3);

/// Room kept free on a pool folder's drive unless said otherwise: 5 % of it, at least 20 GB, but
/// never more than a quarter of it.
pub fn default_reserve(total: u64) -> u64 {
    (total / 20).max(20_000_000_000).min(total / 4)
}

#[derive(Debug, Clone, Serialize)]
pub struct Area {
    pub id: i64,
    pub path: String,
    pub reserve: u64,
    pub takes: Vec<String>,
    pub created_at: i64,
}

fn now() -> i64 {
    crate::remote::now()
}

fn area_row(r: &rusqlite::Row) -> rusqlite::Result<Area> {
    let takes: String = r.get(3)?;
    Ok(Area {
        id: r.get(0)?,
        path: r.get(1)?,
        reserve: r.get::<_, i64>(2)?.max(0) as u64,
        takes: serde_json::from_str(&takes).unwrap_or_default(),
        created_at: r.get(4)?,
    })
}

const COLS: &str = "id, path, reserve, takes, created_at";

/// The pool table is there (an index this version has opened for writing at least once).
fn has_table(db: &Db) -> bool {
    db.conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'pool_areas')",
            [],
            |r| r.get::<_, bool>(0),
        )
        .unwrap_or(false)
}

pub fn list(db: &Db) -> Result<Vec<Area>> {
    if !has_table(db) {
        return Ok(Vec::new());
    }
    let mut stmt = db
        .conn
        .prepare(&format!("SELECT {COLS} FROM pool_areas ORDER BY path"))?;
    let rows = stmt.query_map([], area_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get(db: &Db, id: i64) -> Result<Area> {
    db.conn
        .query_row(
            &format!("SELECT {COLS} FROM pool_areas WHERE id = ?1"),
            [id],
            area_row,
        )
        .optional()?
        .with_context(|| format!("there's no pool folder {id} here"))
}

/// A pool folder by its id or its path.
pub fn find(db: &Db, which: &str) -> Result<Area> {
    let all = list(db)?;
    if let Ok(id) = which.parse::<i64>()
        && let Some(a) = all.iter().find(|a| a.id == id)
    {
        return Ok(a.clone());
    }
    let p = dunce::canonicalize(which).unwrap_or_else(|_| PathBuf::from(which));
    all.into_iter()
        .find(|a| Path::new(&a.path) == p || a.path == which)
        .with_context(|| format!("{which} isn't a pool folder here (`backdown pool list`)"))
}

/// How much to keep free on a pool folder's drive.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Reserve {
    Bytes(u64),
    /// a share of the drive, in percent
    Percent(f64),
}

/// A reserve as people say it: a size ("50G", "500 GB") or a share of the drive ("5%").
pub fn parse_reserve(s: &str) -> Result<Reserve> {
    let s = s.trim();
    if let Some(p) = s.strip_suffix('%') {
        let n: f64 = p
            .trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("not a share: {s:?} (like 5%)"))?;
        if !(0.0..=90.0).contains(&n) {
            bail!("keep between 0 % and 90 % of the drive free, not {n} %");
        }
        return Ok(Reserve::Percent(n));
    }
    Ok(Reserve::Bytes(fmt::parse_size(s)?))
}

fn reserve_bytes(r: Reserve, total: u64) -> u64 {
    match r {
        Reserve::Bytes(b) => b,
        Reserve::Percent(p) => (total as f64 * p / 100.0) as u64,
    }
}

/// The places a pool folder takes, as given: top-level place names (Mail, Media), each once.
fn clean_takes(takes: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for t in takes.iter().flat_map(|t| t.split(',')) {
        let t = t.trim().trim_matches('/');
        if t.is_empty() {
            continue;
        }
        if t.contains('/') || t.contains('\\') || t.len() > 64 || t.chars().any(char::is_control) {
            bail!("{t:?} isn't a top-level place (like Mail or Media)");
        }
        if !out.iter().any(|o| o.eq_ignore_ascii_case(t)) {
            out.push(t.to_string());
        }
    }
    Ok(out)
}

/// Make `path` a pool folder of this machine (created if it isn't there yet), keeping `reserve`
/// free on its drive (unless said, `default_reserve`), taking `takes` (unless said, anything).
/// A pool folder can't be inside another one, nor hold one.
pub fn add(db: &Db, path: &Path, reserve: Option<Reserve>, takes: &[String]) -> Result<Area> {
    if !path.is_absolute() {
        bail!("a pool folder is given by its full path, like /srv/pool");
    }
    if !path.exists() {
        std::fs::create_dir_all(path)
            .with_context(|| format!("can't create {}", path.display()))?;
    }
    let path = dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if !path.is_dir() {
        bail!("{} isn't a folder", path.display());
    }
    let space = volumes::space_at(&path)
        .with_context(|| format!("can't tell how big the drive under {} is", path.display()))?;
    for a in list(db)? {
        let other = Path::new(&a.path);
        if names::within(&path, other) {
            bail!("{} is inside the pool folder {}", path.display(), a.path);
        }
        if names::within(other, &path) {
            bail!("{} holds the pool folder {}", path.display(), a.path);
        }
    }
    let reserve = reserve.map_or_else(
        || default_reserve(space.total),
        |r| reserve_bytes(r, space.total),
    );
    if reserve >= space.total {
        bail!(
            "keeping {} free would leave no room on a drive of {}",
            fmt::bytes(reserve),
            fmt::bytes(space.total)
        );
    }
    let takes = clean_takes(takes)?;
    db.conn.execute(
        "INSERT INTO pool_areas(path, reserve, takes, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![
            path.display().to_string(),
            reserve as i64,
            serde_json::to_string(&takes)?,
            now()
        ],
    )?;
    get(db, db.conn.last_insert_rowid())
}

/// Change what a pool folder keeps free or takes.
pub fn set(db: &Db, id: i64, reserve: Option<Reserve>, takes: Option<&[String]>) -> Result<Area> {
    let a = get(db, id)?;
    if let Some(r) = reserve {
        let total = volumes::space_at(Path::new(&a.path))
            .map(|s| s.total)
            .context("can't tell how big its drive is just now")?;
        let b = reserve_bytes(r, total);
        if b >= total {
            bail!("keeping {} free would leave no room", fmt::bytes(b));
        }
        db.conn.execute(
            "UPDATE pool_areas SET reserve = ?2 WHERE id = ?1",
            params![id, b as i64],
        )?;
    }
    if let Some(t) = takes {
        db.conn.execute(
            "UPDATE pool_areas SET takes = ?2 WHERE id = ?1",
            params![id, serde_json::to_string(&clean_takes(t)?)?],
        )?;
    }
    get(db, id)
}

/// Stop treating a folder as part of the pool. Nothing in it is touched.
pub fn remove(db: &Db, id: i64) -> Result<()> {
    if db
        .conn
        .execute("DELETE FROM pool_areas WHERE id = ?1", [id])?
        == 0
    {
        bail!("there's no pool folder {id} here");
    }
    Ok(())
}

/// The indexed drive whose scans cover `path` (the deepest one holding it).
fn covering_root(db: &Db, path: &Path) -> Result<Option<browse::Root>> {
    Ok(browse::roots(db)?
        .into_iter()
        .filter(|r| names::within(path, Path::new(&r.path)))
        .max_by_key(|r| r.path.len()))
}

/// What the index holds below `path` now (its folder totals), when it's indexed.
fn held(db: &Db, path: &Path) -> Result<Option<(u64, u64)>> {
    Ok(db
        .conn
        .query_row(
            "SELECT files, bytes FROM dirs WHERE path = ?1 AND current = 1",
            [paths::to_bytes(path)],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?.max(0) as u64,
                    r.get::<_, i64>(1)?.max(0) as u64,
                ))
            },
        )
        .optional()?)
}

/// This machine's pool folders as the swarm sees them: each with its drive's size and room now
/// (`fs`: the file system it's on, by its mount point and size, so two folders on one drive share
/// its room), its reserve and places, what it holds (from the index) and the indexed drive whose
/// scans cover it (none: it needs indexing before anything can be filed into it).
pub fn report(db: &Db) -> Result<Value> {
    let mut out = Vec::new();
    for a in list(db)? {
        let p = PathBuf::from(&a.path);
        let q = p.clone();
        let asked = crate::swarm::within(ASK_PATH, move || (q.is_dir(), volumes::space_at(&q)));
        let (present, space) = asked.unwrap_or((false, None));
        let root = covering_root(db, &p)?;
        let (files, bytes) = held(db, &p)?.unwrap_or((0, 0));
        let room = space.map(|s| s.free.saturating_sub(a.reserve));
        out.push(json!({
            "id": a.id, "path": a.path, "key": browse::key(&p), "reserve": a.reserve, "takes": a.takes,
            "created_at": a.created_at, "present": present, "answering": asked.is_some(),
            "space": space, "room": room,
            "indexed": root.is_some(),
            "root": root.map(|r| json!({ "id": r.id, "path": r.path, "key": r.key, "last_scan": r.last_scan, "scanned_at": r.scanned_at })),
            "files": files, "bytes": bytes,
        }));
    }
    Ok(json!(out))
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn reserves_are_read_as_sizes_or_shares() {
        assert_eq!(
            parse_reserve("50G").unwrap(),
            Reserve::Bytes(50_000_000_000)
        );
        assert_eq!(parse_reserve(" 5 % ").unwrap(), Reserve::Percent(5.0));
        assert!(parse_reserve("95%").is_err());
        assert!(parse_reserve("lots").is_err());
        assert_eq!(reserve_bytes(Reserve::Percent(10.0), 1000), 100);
        // 5 %, at least 20 GB, at most a quarter
        assert_eq!(default_reserve(4_000_000_000_000), 200_000_000_000);
        assert_eq!(default_reserve(100_000_000_000), 20_000_000_000);
        assert_eq!(default_reserve(40_000_000_000), 10_000_000_000);
    }

    #[test]
    fn places_taken_are_top_level_names_once_each() {
        let t = |v: &[&str]| clean_takes(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(
            t(&["Mail, Media", "mail", " "]).unwrap(),
            vec!["Mail", "Media"]
        );
        assert_eq!(t(&["/Media/"]).unwrap(), vec!["Media"]);
        assert!(t(&["Media/Photos"]).is_err());
    }
}

#[cfg(test)]
mod tests;
