//! Queries behind the file browser: folders as they were at any scan, a file's versions and its
//! other copies, and where its bytes can still be read from.
//!
//! Paths travel as "keys": base64url of the raw path bytes, so names that aren't valid UTF-8 still
//! round-trip. `name`/`path` fields are display strings only: composed (NFC) whatever form the
//! drive stores names in, and on Windows without a verbatim prefix.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use crate::{Db, fmt, paths};

pub fn key(path: &Path) -> String {
    URL_SAFE_NO_PAD.encode(paths::to_bytes(path))
}

pub fn key_bytes(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn path_of(key: &str) -> Result<PathBuf> {
    Ok(paths::from_bytes(&URL_SAFE_NO_PAD.decode(key)?))
}

fn name_of(path: &Path) -> String {
    path.file_name().map_or_else(
        || paths::show(path),
        |n| crate::names::nfc(&n.to_string_lossy()).into_owned(),
    )
}

#[derive(Debug, Serialize)]
pub struct Root {
    pub id: i64,
    pub name: String,
    pub path: String,
    pub key: String,
    pub last_scan: Option<i64>,
    pub scanned_at: Option<i64>,
    pub files: u64,
    pub bytes: u64,
}

pub fn roots(db: &Db) -> Result<Vec<Root>> {
    // drives being removed (forget.rs) are gone as far as anyone looking is concerned; an index
    // opened read-only by an older version's schema may not have that table yet
    let removing: bool = db.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'roots_removing')",
        [],
        |r| r.get(0),
    )?;
    let mut stmt = db.conn.prepare(if removing {
        "SELECT r.id, r.path, r.last_scan, s.finished_at, COALESCE(s.files, 0), COALESCE(s.bytes, 0)
         FROM roots r LEFT JOIN scans s ON s.id = r.last_scan
         WHERE r.id NOT IN (SELECT root_id FROM roots_removing) ORDER BY r.path"
    } else {
        "SELECT r.id, r.path, r.last_scan, s.finished_at, COALESCE(s.files, 0), COALESCE(s.bytes, 0)
         FROM roots r LEFT JOIN scans s ON s.id = r.last_scan ORDER BY r.path"
    })?;
    let rows = stmt.query_map([], |r| {
        let path = paths::from_bytes(&r.get::<_, Vec<u8>>(1)?);
        Ok(Root {
            id: r.get(0)?,
            name: name_of(&path),
            key: key(&path),
            path: paths::show(&path),
            last_scan: r.get(2)?,
            scanned_at: r.get(3)?,
            files: r.get::<_, i64>(4)? as u64,
            bytes: r.get::<_, i64>(5)? as u64,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[derive(Debug, Serialize)]
pub struct ScanPoint {
    pub id: i64,
    pub finished_at: i64,
    pub files: u64,
    pub bytes: u64,
}

/// The finished scans of one root, oldest first: the stops on the time slider.
pub fn timeline(db: &Db, root_id: i64) -> Result<Vec<ScanPoint>> {
    let mut stmt = db.conn.prepare(
        "SELECT id, finished_at, files, bytes FROM scans
         WHERE root_id = ?1 AND finished_at IS NOT NULL ORDER BY id",
    )?;
    let rows = stmt.query_map(params![root_id], |r| {
        Ok(ScanPoint {
            id: r.get(0)?,
            finished_at: r.get(1)?,
            files: r.get::<_, i64>(2)? as u64,
            bytes: r.get::<_, i64>(3)? as u64,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[derive(Debug, Serialize)]
pub struct Crumb {
    pub name: String,
    pub key: String,
}

#[derive(Debug, Serialize)]
pub struct Entry {
    pub kind: &'static str, // "dir" | "file"
    pub name: String,
    pub key: String,
    pub id: Option<i64>, // file row
    pub size: u64,       // folders: recursive bytes
    pub files: Option<u64>,
    pub mtime: i64, // folders: newest file below
    pub sha256: Option<String>,
    /// other current files with identical content (not hardlinks of this one)
    pub copies: u64,
    /// versions of this path on record
    pub versions: u64,
    /// appeared or changed at the scan being viewed (and it isn't the root's first scan)
    pub new_here: bool,
    /// what happened to it after the viewed scan: "kept", "changed" or "deleted"
    pub later: &'static str,
}

#[derive(Debug, Serialize)]
pub struct Listing {
    pub root_id: i64,
    pub scan: i64,
    pub latest_scan: i64,
    pub first_scan: i64,
    pub scanned_at: i64,
    pub name: String,
    pub path: String,
    pub key: String,
    pub crumbs: Vec<Crumb>,
    pub files: u64,
    pub bytes: u64,
    pub entries: Vec<Entry>,
    /// the folder hasn't got totals yet (indexed before 0.2.0-alpha.2): re-index to browse sizes
    pub needs_reindex: bool,
}

struct RootRow {
    id: i64,
    path: PathBuf,
    last_scan: i64,
    first_scan: i64,
}

fn root_for(db: &Db, dir: &Path) -> Result<RootRow> {
    let want = paths::dir_prefix(dir);
    let mut stmt = db.conn.prepare(
        "SELECT r.id, r.path, r.last_scan, (SELECT MIN(id) FROM scans WHERE root_id = r.id AND finished_at IS NOT NULL)
         FROM roots r WHERE r.last_scan IS NOT NULL",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, Vec<u8>>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, Option<i64>>(3)?,
        ))
    })?;
    for row in rows {
        let (id, path, last_scan, first_scan) = row?;
        let path = paths::from_bytes(&path);
        if want.starts_with(&paths::dir_prefix(&path)) {
            return Ok(RootRow {
                id,
                path,
                last_scan,
                first_scan: first_scan.unwrap_or(last_scan),
            });
        }
    }
    bail!("{} isn't inside an indexed root", dir.display())
}

/// A folder as it was at `scan` (a finished scan of its root; default: the latest).
pub fn list(db: &Db, dir: &Path, scan: Option<i64>) -> Result<Listing> {
    let root = root_for(db, dir)?;
    let scan = scan.unwrap_or(root.last_scan);
    let scanned_at: i64 = db
        .conn
        .query_row(
            "SELECT finished_at FROM scans WHERE id = ?1 AND root_id = ?2 AND finished_at IS NOT NULL",
            params![scan, root.id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| anyhow::anyhow!("scan {scan} isn't a finished scan of {}", root.path.display()))?;
    let dir_bytes = paths::to_bytes(dir);

    let totals: Option<(i64, i64)> = db
        .conn
        .query_row(
            "SELECT files, bytes FROM dirs WHERE path = ?1 AND first_scan <= ?2 AND last_scan >= ?2",
            params![dir_bytes, scan],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let needs_reindex = totals.is_none()
        && db.conn.query_row(
            "SELECT NOT EXISTS (SELECT 1 FROM dirs WHERE root_id = ?1)",
            params![root.id],
            |r| r.get::<_, bool>(0),
        )?;

    let mut entries = Vec::new();
    let mut dirs = db.conn.prepare(
        "SELECT d.path, d.files, d.bytes, d.newest, d.first_scan, d.last_scan,
                (SELECT COUNT(*) FROM dirs v WHERE v.path = d.path),
                EXISTS (SELECT 1 FROM dirs c WHERE c.path = d.path AND c.current = 1)
         FROM dirs d WHERE d.parent = ?1 AND d.first_scan <= ?2 AND d.last_scan >= ?2",
    )?;
    let rows = dirs.query_map(params![dir_bytes, scan], |r| {
        Ok((
            r.get::<_, Vec<u8>>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, i64>(4)?,
            r.get::<_, i64>(5)?,
            r.get::<_, i64>(6)?,
            r.get::<_, bool>(7)?,
        ))
    })?;
    for row in rows {
        let (path, files, bytes, newest, first, last, versions, exists_now) = row?;
        let p = paths::from_bytes(&path);
        entries.push(Entry {
            kind: "dir",
            name: name_of(&p),
            key: key_bytes(&path),
            id: None,
            size: bytes as u64,
            files: Some(files as u64),
            mtime: newest,
            sha256: None,
            copies: 0,
            versions: versions as u64,
            new_here: first == scan && scan != root.first_scan && versions == 1,
            later: fate(last, root.last_scan, exists_now),
        });
    }

    // an index from before folder totals (0.2.0-alpha.1): derive the subfolders from the files
    // themselves, so the browser works before the next `index` fills in `dirs`
    let mut derived = (0u64, 0u64);
    if needs_reindex {
        let prefix = paths::dir_prefix(dir);
        let mut stmt = db.conn.prepare(
            "SELECT dir, COUNT(*), SUM(size), MAX(mtime) FROM files
             WHERE root_id = ?1 AND first_scan <= ?2 AND last_scan >= ?2 AND substr(dir, 1, ?3) = ?4
             GROUP BY dir",
        )?;
        let rows = stmt.query_map(params![root.id, scan, prefix.len() as i64, prefix], |r| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        let mut children: std::collections::BTreeMap<PathBuf, (u64, u64, i64)> = Default::default();
        for row in rows {
            let (sub, files, bytes, newest) = row?;
            let sub = paths::from_bytes(&sub);
            let Some(first) = sub
                .strip_prefix(dir)
                .ok()
                .and_then(|r| r.components().next())
            else {
                continue;
            };
            let e = children.entry(dir.join(first)).or_default();
            e.0 += files as u64;
            e.1 += bytes as u64;
            e.2 = e.2.max(newest);
            derived.0 += files as u64;
            derived.1 += bytes as u64;
        }
        for (child, (files, bytes, newest)) in children {
            entries.push(Entry {
                kind: "dir",
                name: name_of(&child),
                key: key(&child),
                id: None,
                size: bytes,
                files: Some(files),
                mtime: newest,
                sha256: None,
                copies: 0,
                versions: 1,
                new_here: false,
                later: "kept",
            });
        }
    }

    let mut files = db.conn.prepare(
        "SELECT f.id, f.path, f.size, f.mtime, f.sha256, f.first_scan, f.last_scan,
                (SELECT COUNT(*) FROM files g WHERE g.current = 1 AND f.sha256 IS NOT NULL AND g.sha256 = f.sha256
                   AND g.path <> f.path AND NOT (f.ino IS NOT NULL AND g.dev = f.dev AND g.ino = f.ino)),
                (SELECT COUNT(*) FROM files v WHERE v.path = f.path),
                EXISTS (SELECT 1 FROM files c WHERE c.path = f.path AND c.current = 1)
         FROM files f WHERE f.dir = ?1 AND f.first_scan <= ?2 AND f.last_scan >= ?2",
    )?;
    let rows = files.query_map(params![dir_bytes, scan], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, Vec<u8>>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, Option<Vec<u8>>>(4)?,
            r.get::<_, i64>(5)?,
            r.get::<_, i64>(6)?,
            r.get::<_, i64>(7)?,
            r.get::<_, i64>(8)?,
            r.get::<_, bool>(9)?,
        ))
    })?;
    for row in rows {
        let (id, path, size, mtime, sha, first, last, copies, versions, exists_now) = row?;
        let p = paths::from_bytes(&path);
        entries.push(Entry {
            kind: "file",
            name: name_of(&p),
            key: key_bytes(&path),
            id: Some(id),
            size: size as u64,
            files: None,
            mtime,
            sha256: sha.map(|h| fmt::hex(&h)),
            copies: copies as u64,
            versions: versions as u64,
            new_here: first == scan && scan != root.first_scan,
            later: fate(last, root.last_scan, exists_now),
        });
    }

    let mut crumbs = Vec::new();
    let mut at = Some(dir);
    while let Some(p) = at {
        crumbs.push(Crumb {
            name: name_of(p),
            key: key(p),
        });
        if p == root.path {
            break;
        }
        at = p.parent();
    }
    crumbs.reverse();

    let direct = entries.iter().filter(|e| e.kind == "file");
    let (files_total, bytes_total) = match totals {
        Some((f, b)) => (f, b),
        None if needs_reindex => (
            (derived.0 + direct.clone().count() as u64) as i64,
            (derived.1 + direct.map(|e| e.size).sum::<u64>()) as i64,
        ),
        None => (0, 0),
    };
    Ok(Listing {
        root_id: root.id,
        scan,
        latest_scan: root.last_scan,
        first_scan: root.first_scan,
        scanned_at,
        name: name_of(dir),
        path: paths::show(dir),
        key: key(dir),
        crumbs,
        files: files_total as u64,
        bytes: bytes_total as u64,
        entries,
        needs_reindex,
    })
}

fn fate(last_scan: i64, latest: i64, exists_now: bool) -> &'static str {
    match (last_scan >= latest, exists_now) {
        (true, _) => "kept",
        (false, true) => "changed",
        (false, false) => "deleted",
    }
}

#[derive(Debug, Serialize)]
pub struct Version {
    pub id: i64,
    pub size: u64,
    pub mtime: i64,
    pub sha256: Option<String>,
    pub first_scan: i64,
    pub last_scan: i64,
    pub from: i64, // when first seen (scan finished_at)
    pub to: i64,   // when last seen
    pub current: bool,
    pub readable: bool, // its bytes can still be read from somewhere
}

#[derive(Debug, Serialize)]
pub struct Copy {
    pub id: i64,
    pub path: String,
    pub key: String,
    pub dir_key: String,
}

#[derive(Debug, Serialize)]
pub struct FileInfo {
    pub id: i64,
    pub name: String,
    pub path: String,
    pub key: String,
    pub dir_key: String,
    pub size: u64,
    pub mtime: i64,
    pub sha256: Option<String>,
    pub error: Option<String>,
    pub current: bool,
    pub versions: Vec<Version>,
    pub copies: Vec<Copy>,
    /// where the bytes would be read from if this exact file is gone ("" when it's this file)
    pub content_from: Option<String>,
    /// NTFS alternate data streams, when the index noted some (Windows)
    pub streams: Option<crate::streams::Streams>,
}

// path, size, mtime, sha256, error, current, dev, ino
type FileRow = (
    Vec<u8>,
    i64,
    i64,
    Option<Vec<u8>>,
    Option<String>,
    bool,
    Option<i64>,
    Option<i64>,
);
// path, size, mtime, sha256
type ContentRow = (Vec<u8>, i64, i64, Option<Vec<u8>>);

pub fn file(db: &Db, id: i64) -> Result<FileInfo> {
    let (path, size, mtime, sha, error, current, dev, ino): FileRow = db
        .conn
        .query_row(
            "SELECT path, size, mtime, sha256, error, current, dev, ino FROM files WHERE id = ?1",
            params![id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| anyhow::anyhow!("no file {id} in the index"))?;
    let p = paths::from_bytes(&path);

    let mut stmt = db.conn.prepare(
        "SELECT v.id, v.size, v.mtime, v.sha256, v.first_scan, v.last_scan, v.current,
                (SELECT finished_at FROM scans WHERE id = v.first_scan), (SELECT finished_at FROM scans WHERE id = v.last_scan)
         FROM files v WHERE v.path = ?1 ORDER BY v.first_scan",
    )?;
    let rows = stmt.query_map(params![path], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, Option<Vec<u8>>>(3)?,
            r.get::<_, i64>(4)?,
            r.get::<_, i64>(5)?,
            r.get::<_, bool>(6)?,
            r.get::<_, Option<i64>>(7)?,
            r.get::<_, Option<i64>>(8)?,
        ))
    })?;
    let mut versions = Vec::new();
    for row in rows {
        let (vid, vsize, vmtime, vsha, first, last, vcur, from, to) = row?;
        versions.push(Version {
            id: vid,
            size: vsize as u64,
            mtime: vmtime,
            sha256: vsha.as_deref().map(fmt::hex),
            first_scan: first,
            last_scan: last,
            from: from.unwrap_or(0),
            to: to.unwrap_or(0),
            current: vcur,
            readable: resolve_content(db, vid)?.is_some(),
        });
    }

    let mut copies = Vec::new();
    if let Some(h) = &sha {
        let mut stmt = db.conn.prepare(
            "SELECT id, path FROM files WHERE current = 1 AND sha256 = ?1 AND path <> ?2
               AND NOT (?3 IS NOT NULL AND dev = ?4 AND ino = ?3) ORDER BY path LIMIT 200",
        )?;
        let rows = stmt.query_map(params![h, path, ino, dev], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        for row in rows {
            let (cid, cpath) = row?;
            let cp = paths::from_bytes(&cpath);
            copies.push(Copy {
                id: cid,
                path: paths::show(&cp),
                key: key(&cp),
                dir_key: key(cp.parent().unwrap_or(&cp)),
            });
        }
    }

    let content_from = resolve_content(db, id)?.map(|(from, _)| {
        if from == p {
            String::new()
        } else {
            paths::show(&from)
        }
    });
    Ok(FileInfo {
        id,
        name: name_of(&p),
        dir_key: key(p.parent().unwrap_or(&p)),
        key: key(&p),
        path: paths::show(&p),
        size: size as u64,
        mtime,
        sha256: sha.as_deref().map(fmt::hex),
        error,
        current,
        versions,
        copies,
        content_from,
        streams: crate::streams::get(db, id),
    })
}

/// Where this version's bytes can be read now: the file itself if it is still current and
/// unchanged on disk, otherwise any current file with the same SHA-256. None if nowhere.
pub fn resolve_content(db: &Db, id: i64) -> Result<Option<(PathBuf, u64)>> {
    let row: Option<ContentRow> = db
        .conn
        .query_row(
            "SELECT path, size, mtime, sha256 FROM files WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((path, size, mtime, sha)) = row else {
        return Ok(None);
    };
    let unchanged = |p: &Path, want_mtime: Option<i64>| {
        std::fs::metadata(p).is_ok_and(|m| {
            m.is_file()
                && m.len() == size as u64
                && want_mtime.is_none_or(|t| {
                    m.modified()
                        .ok()
                        .and_then(|x| x.duration_since(std::time::UNIX_EPOCH).ok())
                        .is_some_and(|d| d.as_secs() as i64 == t)
                })
        })
    };
    let own = paths::from_bytes(&path);
    if unchanged(&own, Some(mtime)) {
        return Ok(Some((own, size as u64)));
    }
    if let Some(h) = sha {
        let mut stmt = db.conn.prepare(
            "SELECT path FROM files WHERE current = 1 AND error IS NULL AND sha256 = ?1 AND size = ?2 LIMIT 20",
        )?;
        let rows = stmt.query_map(params![h, size], |r| r.get::<_, Vec<u8>>(0))?;
        for row in rows {
            let cand = paths::from_bytes(&row?);
            if unchanged(&cand, None) {
                return Ok(Some((cand, size as u64)));
            }
        }
    }
    Ok(None)
}

#[derive(Debug, Serialize)]
pub struct Hit {
    pub kind: &'static str,
    pub id: Option<i64>,
    pub name: String,
    pub path: String,
    pub key: String,
    pub dir_key: String,
    pub size: u64,
    pub mtime: i64,
}

/// Current files and folders under `within` whose own name contains `q` (case-insensitive).
pub fn search(db: &Db, within: &Path, q: &str, limit: usize) -> Result<Vec<Hit>> {
    let needle = q.trim().to_lowercase();
    if needle.is_empty() {
        return Ok(Vec::new());
    }
    let prefix = paths::dir_prefix(within);
    let n = prefix.len() as i64;
    // Unix paths are stored as their (usually UTF-8) bytes, so SQLite's LIKE can pre-filter in C.
    // Windows paths are UTF-16 bytes, which LIKE can't read: there every candidate is decoded and
    // matched below (`?3 IS NULL` keeps the parameter list the same).
    let like = cfg!(unix).then(|| {
        format!(
            "%{}%",
            q.trim()
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        )
    });
    let filter = if like.is_some() {
        "CAST(path AS TEXT) LIKE ?3 ESCAPE '\\'"
    } else {
        "?3 IS NULL"
    };
    let mut hits = Vec::new();
    for (kind, select) in [
        ("dir", "SELECT NULL, path, bytes, newest FROM dirs"),
        ("file", "SELECT id, path, size, mtime FROM files"),
    ] {
        let sql = format!("{select} WHERE current = 1 AND substr(path, 1, ?1) = ?2 AND {filter}");
        let mut stmt = db.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![n, prefix, like], |r| {
            Ok((
                r.get::<_, Option<i64>>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        for row in rows {
            let (id, path, size, mtime) = row?;
            let p = paths::from_bytes(&path);
            let name = name_of(&p);
            // LIKE matches anywhere in the path; a hit is a match in the item's own name
            if !name.to_lowercase().contains(&needle) {
                continue;
            }
            hits.push(Hit {
                kind,
                id,
                name,
                path: paths::show(&p),
                key: key(&p),
                dir_key: key(p.parent().unwrap_or(&p)),
                size: size as u64,
                mtime,
            });
            if hits.len() >= limit {
                return Ok(hits);
            }
        }
    }
    Ok(hits)
}
