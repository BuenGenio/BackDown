//! The other machine's end: answers to `backdown serve-stdio`'s requests, one JSON line each.
//!
//! Requests are `{"id": N, "op": "...", ...}` (optionally `"v": 1`), parsed strictly: an unknown
//! op, an unknown or mistyped field, a line over `MAX_LINE`, or a number out of range is refused
//! with an error answer, never guessed at. Answers are `{"id": N, "ok": true, "data": ...}` or
//! `{"id": N, "ok": false, "error": "..."}`.
//!
//! Everything here reads: the index through a read-only connection (the caller opens it so), and
//! files only by their index id, only while they're current and still as scanned (`hash` and
//! `stat`), the way `backdown hash` would. A file's bytes travel only for `get`, one file at a time, asked for by the person at the
//! other end to open, preview or save it there: the answer says what follows, and the process
//! (remote/stdio.rs in the CLI) writes exactly that many raw bytes after the answer line, as
//! `swarm-stdio` takes a `put`'s. Drive
//! health (`health`) is what the index holds: no drive is read for it. Its storage (`volumes`)
//! is the volume probe and each file system's size and free space (swarm.rs).
//!
//! A request this end doesn't know (one a newer BackDown added) is refused as an "unknown
//! request", so the asker can tell it apart from a mistake and do without it; versions before
//! that said "bad request: unknown variant".

use std::fs;
use std::io::Read;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{PROTO, SAMPLE};
use crate::{Db, browse, find, fmt, health, paths, recognise, report};

/// The longest request line accepted.
pub const MAX_LINE: usize = 64 * 1024;
/// Rows per page of `changes` and `gone`, at most.
pub const MAX_PAGE: u32 = 10_000;
/// Files per `hash` request, and per `stat` request.
pub const MAX_HASH_IDS: usize = 256;
pub const MAX_STAT_IDS: usize = 5000;
/// Seconds a `hash` request may spend reading.
pub const MAX_BUDGET: u64 = 3600;
/// Entries of one folder listing sent, at most.
pub const MAX_ENTRIES: usize = 10_000;
/// Drives in a `health` answer, at most (the worst first), its latest changes for each, and its
/// latest notifications about its own drives.
pub const MAX_HEALTH_DRIVES: usize = 100;
pub const MAX_HEALTH_CHANGES: usize = 5;
pub const MAX_HEALTH_NOTES: usize = 20;

/// What this end says about itself.
pub struct Here {
    pub db_path: String,
    pub host: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Request {
    Hello {},
    Status {},
    Roots {},
    Ls {
        dir: String,
        #[serde(default)]
        scan: Option<i64>,
    },
    File {
        /// the file's id in the index (`id` is the request's own)
        file: i64,
    },
    Find {
        q: String,
        #[serde(default)]
        within: Option<String>,
        #[serde(default)]
        earlier: bool,
        #[serde(default)]
        limit: Option<u32>,
    },
    Search {
        dir: String,
        q: String,
        #[serde(default)]
        limit: Option<u32>,
    },
    Dupes {
        #[serde(default)]
        min: Option<u64>,
        #[serde(default)]
        limit: Option<u32>,
    },
    Entities {},
    Changes {
        root: i64,
        since: i64,
        upto: i64,
        after: i64,
        #[serde(default)]
        limit: Option<u32>,
    },
    Gone {
        root: i64,
        since: i64,
        after: i64,
        #[serde(default)]
        limit: Option<u32>,
    },
    Hash {
        ids: Vec<i64>,
        mode: HashMode,
        /// read the file even if the index has the hash already
        #[serde(default)]
        fresh: bool,
        #[serde(default)]
        budget: Option<u64>,
    },
    Stat {
        ids: Vec<i64>,
    },
    /// Its drives' health as its index has it (no drive is read for this), and its own latest
    /// drive-health notifications.
    Health {},
    /// Its storage (swarm.rs): its drives and file systems, each mounted one's size and free
    /// space, and its indexed drives, each connected or not and the drive it was last scanned on.
    /// The same read-only probe as `backdown volumes`: nothing is mounted or written, and no
    /// drive's files are read.
    Volumes {},
    /// A file's bytes, by its index id, only while it's current and still as scanned: `len`
    /// bytes from `from` (the whole file unless said). The answer describes them; the raw bytes
    /// follow the answer line, exactly `len` of them.
    Get {
        file: i64,
        #[serde(default)]
        from: Option<u64>,
        #[serde(default)]
        len: Option<u64>,
    },
}

/// What a `get` answer promises follows it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct GetPlan {
    pub path: std::path::PathBuf,
    pub name: String,
    pub size: u64,
    pub mtime: i64,
    pub from: u64,
    pub len: u64,
    pub sha256: Option<String>,
}

/// The `get` request's plan: the file if it's current and unchanged, and the range asked for,
/// checked against its size.
pub fn get_plan(db: &Db, file: i64, from: Option<u64>, len: Option<u64>) -> Result<GetPlan> {
    let (row, why) = current(db, file)?;
    let Some(r) = row else {
        bail!(
            "that file {}",
            match why {
                Some("missing") => "isn't there any more",
                Some("changed") => "has changed since it was indexed",
                _ => "isn't in the index (or isn't current)",
            }
        );
    };
    let from = from.unwrap_or(0);
    if from > r.size {
        bail!("the range starts past the end of the file");
    }
    let len = len.unwrap_or(r.size - from);
    if from + len > r.size {
        bail!("the range runs past the end of the file");
    }
    let mtime: i64 = db
        .conn
        .query_row("SELECT mtime FROM files WHERE id = ?1", [file], |x| {
            x.get(0)
        })?;
    Ok(GetPlan {
        name: r
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        path: r.path,
        size: r.size,
        mtime,
        from,
        len,
        sha256: r.sha.as_deref().map(fmt::hex),
    })
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum HashMode {
    Sample,
    Full,
}

impl Request {
    /// How long answering may take (a `hash` request says; everything else is quick or wrong).
    pub fn budget_secs(&self) -> u64 {
        match self {
            Request::Hash { budget, .. } => budget.unwrap_or(60).clamp(1, MAX_BUDGET) + 5,
            // the answer line is quick; the bytes after it take as long as the link allows
            Request::Get { .. } => 30,
            // a search of a big index without its names index yet reads every name (name_index.rs)
            Request::Find { .. } => 115,
            _ => 30,
        }
    }
}

/// Parse one request line: its id (to answer with, even when the rest is refused) and the request.
pub fn parse(line: &str) -> (Value, Result<Request>) {
    if line.len() > MAX_LINE {
        return (Value::Null, Err(anyhow::anyhow!("request too long")));
    }
    let mut v: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return (Value::Null, Err(anyhow::anyhow!("not JSON: {e}"))),
    };
    let Some(obj) = v.as_object_mut() else {
        return (
            Value::Null,
            Err(anyhow::anyhow!("a request is a JSON object")),
        );
    };
    let id = obj.remove("id").unwrap_or(Value::Null);
    if !id.is_u64() {
        return (
            Value::Null,
            Err(anyhow::anyhow!("a request needs a numeric id")),
        );
    }
    if let Some(ver) = obj.remove("v")
        && ver != json!(1)
    {
        return (id, Err(anyhow::anyhow!("unknown request format {ver}")));
    }
    let op = obj.get("op").and_then(Value::as_str).map(str::to_string);
    let req = serde_json::from_value::<Request>(v).map_err(|e| {
        let e = e.to_string();
        match op {
            // a request a newer BackDown added: the asker can tell, and do without it
            Some(op) if e.starts_with(&format!("unknown variant `{op}`")) => anyhow::anyhow!(
                "unknown request {op:?}: BackDown {} here doesn't know it",
                env!("CARGO_PKG_VERSION")
            ),
            _ => anyhow::anyhow!("bad request: {e}"),
        }
    });
    (id, req)
}

/// Answer one request line (the whole answer line, without its newline).
pub fn answer_line(db: &Db, here: &Here, line: &str, deadline: Instant) -> String {
    let (id, req) = parse(line);
    let out = req.and_then(|r| answer(db, here, &r, deadline));
    let v = match out {
        Ok(data) => json!({ "id": id, "ok": true, "data": data }),
        Err(e) => json!({ "id": id, "ok": false, "error": format!("{e:#}") }),
    };
    v.to_string()
}

fn limit(l: Option<u32>, default: u32, max: u32) -> Result<u32> {
    let l = l.unwrap_or(default);
    if l == 0 || l > max {
        bail!("limit must be between 1 and {max}");
    }
    Ok(l)
}

fn key_path(key: &str) -> Result<std::path::PathBuf> {
    if key.len() > 8192 {
        bail!("path too long");
    }
    browse::path_of(key).context("not a path key")
}

/// The index's own identity: changes when it's replaced by another.
fn ident(db: &Db) -> Result<String> {
    let first: Option<(i64, i64)> = db
        .conn
        .query_row(
            "SELECT id, started_at FROM scans ORDER BY id LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(first.map_or_else(|| "empty".into(), |(id, t)| format!("{id}@{t}")))
}

pub fn answer(db: &Db, here: &Here, req: &Request, deadline: Instant) -> Result<Value> {
    Ok(match req {
        Request::Hello {} => json!({
            "proto": PROTO, "version": env!("CARGO_PKG_VERSION"), "sample": SAMPLE,
            "os": if cfg!(windows) { "windows" } else { "unix" },
            "host": here.host, "db": here.db_path, "now": super::now(), "ident": ident(db)?,
            "read_only": true,
            "limits": { "line": MAX_LINE, "page": MAX_PAGE, "hash_ids": MAX_HASH_IDS, "budget": MAX_BUDGET },
        }),
        Request::Status {} => json!(db.status()?),
        Request::Roots {} => json!({ "roots": browse::roots(db)? }),
        Request::Ls { dir, scan } => {
            let mut l = serde_json::to_value(browse::list(db, &key_path(dir)?, *scan)?)?;
            if let Some(e) = l["entries"].as_array_mut()
                && e.len() > MAX_ENTRIES
            {
                e.truncate(MAX_ENTRIES);
                l["truncated"] = json!(true);
            }
            l
        }
        Request::File { file } => json!(browse::file(db, *file)?),
        Request::Find {
            q,
            within,
            earlier,
            limit: l,
        } => {
            if q.len() > 200 {
                bail!("search text too long");
            }
            let within = within.as_deref().map(key_path).transpose()?;
            json!(find::find(
                db,
                &find::Query {
                    q,
                    within: within.as_deref(),
                    earlier: *earlier,
                    limit: limit(*l, 200, 500)? as usize,
                }
            )?)
        }
        Request::Search { dir, q, limit: l } => {
            if q.len() > 200 {
                bail!("search text too long");
            }
            json!(browse::search(
                db,
                &key_path(dir)?,
                q,
                limit(*l, 300, 300)? as usize
            )?)
        }
        Request::Dupes { min, limit: l } => json!(report::dupes(
            db,
            min.unwrap_or(1_000_000).max(1),
            limit(*l, 50, 200)? as usize
        )?),
        Request::Entities {} => entities(db)?,
        Request::Changes {
            root,
            since,
            upto,
            after,
            limit: l,
        } => {
            let l = limit(*l, 5000, MAX_PAGE)?;
            changes(db, [*root, *since, *upto, *after], l, WALK)?
        }
        Request::Gone {
            root,
            since,
            after,
            limit: l,
        } => {
            let l = limit(*l, 5000, MAX_PAGE)?;
            let mut stmt = db.conn.prepare_cached(
                "SELECT id FROM files WHERE root_id = ?1 AND current = 0 AND last_scan >= ?2 AND id > ?3
                 ORDER BY id LIMIT ?4",
            )?;
            let ids: Vec<i64> = stmt
                .query_map(params![root, since, after, l], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let next = (ids.len() == l as usize)
                .then(|| ids.last().copied())
                .flatten();
            json!({ "ids": ids, "next": next })
        }
        Request::Hash {
            ids,
            mode,
            fresh,
            budget,
        } => {
            if ids.is_empty() || ids.len() > MAX_HASH_IDS {
                bail!("ask about 1 to {MAX_HASH_IDS} files at a time");
            }
            let until = Instant::now()
                + std::time::Duration::from_secs(budget.unwrap_or(60).clamp(1, MAX_BUDGET));
            hash(db, ids, *mode, *fresh, until.min(deadline))?
        }
        Request::Stat { ids } => {
            if ids.is_empty() || ids.len() > MAX_STAT_IDS {
                bail!("ask about 1 to {MAX_STAT_IDS} files at a time");
            }
            let mut out = Vec::new();
            for id in ids {
                let (row, why) = current(db, *id)?;
                out.push(match (row, why) {
                    (Some(r), None) => {
                        json!({ "id": id, "ok": true, "sha256": r.sha.as_deref().map(fmt::hex) })
                    }
                    (_, why) => json!({ "id": id, "ok": false, "why": why }),
                });
            }
            json!({ "results": out })
        }
        Request::Health {} => drive_health(db)?,
        Request::Volumes {} => crate::swarm::storage(db)?,
        Request::Get { file, from, len } => json!(get_plan(db, *file, *from, *len)?),
    })
}

/// Each drive's latest verdict, readings and changes, as `backdown health --json` has them from
/// the index (nothing is read from a drive for this), the worst first; and this machine's latest
/// notifications about its own drives (not the ones it gathered from others: those stay there).
fn drive_health(db: &Db) -> Result<Value> {
    if !health::available(db) {
        // an index this version never opened for writing: no readings yet
        return Ok(json!({ "available": false, "drives": [], "notes": [], "truncated": false }));
    }
    let all = health::overview(Some(db), &[])?;
    let truncated = all.len() > MAX_HEALTH_DRIVES;
    let drives: Vec<Value> = all
        .into_iter()
        .take(MAX_HEALTH_DRIVES)
        .map(|mut s| {
            let n = s.changes.len();
            s.changes.drain(..n.saturating_sub(MAX_HEALTH_CHANGES));
            let mut v = json!(s);
            if let Some(o) = v.as_object_mut() {
                // "connected now" means nothing here (none was read), and the sparkline stays there
                o.remove("present");
                o.remove("series");
            }
            v
        })
        .collect();
    let mut stmt = db.conn.prepare(
        "SELECT at, level, title, body, drive FROM notifications
         WHERE kind = 'health' AND substr(key, 1, 7) = 'health:' ORDER BY id DESC LIMIT ?1",
    )?;
    let notes: Vec<Value> = stmt
        .query_map([MAX_HEALTH_NOTES as i64], |r| {
            Ok(json!({
                "at": r.get::<_, i64>(0)?, "level": r.get::<_, String>(1)?,
                "title": r.get::<_, String>(2)?, "body": r.get::<_, String>(3)?,
                "drive": r.get::<_, Option<String>>(4)?,
            }))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(json!({ "available": true, "drives": drives, "notes": notes, "truncated": truncated }))
}

/// How a drive's files are paged for `changes`, in id order.
#[derive(Clone, Copy)]
pub(super) struct Walk {
    /// A drive with more files than this (at its last scan) is walked in id order. Through
    /// files_root, which isn't in id order, every page would gather and sort all of the drive's
    /// rows: on a 16-million-file drive that alone ran past a request's 30 s, page after page.
    pub above: i64,
    /// How many ids one page of a walk may look at, so each answer stays quick where the drive's
    /// files are sparse among the ids; the page then ends early and says where to go on from.
    pub window: i64,
}

pub(super) const WALK: Walk = Walk {
    above: 100_000,
    window: 500_000,
};

/// One page of drive `root`'s current files first seen after scan `since`, up to scan `upto`,
/// with ids above `after`. `next` is where the following page starts; a walk's page can be short,
/// even empty, and still have one. A client only follows `next`, so older ones page through
/// either way.
pub(super) fn changes(
    db: &Db,
    [root, since, upto, after]: [i64; 4],
    l: u32,
    walk: Walk,
) -> Result<Value> {
    let files: i64 = db.conn.query_row(
        "SELECT COALESCE((SELECT s.files FROM roots r JOIN scans s ON s.id = r.last_scan WHERE r.id = ?1), 0)",
        [root],
        |r| r.get(0),
    )?;
    let row = |r: &rusqlite::Row| -> rusqlite::Result<Value> {
        Ok(json!([
            r.get::<_, i64>(0)?,
            browse::key_bytes(&r.get::<_, Vec<u8>>(1)?),
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, Option<Vec<u8>>>(4)?.as_deref().map(fmt::hex),
            r.get::<_, Option<Vec<u8>>>(5)?.as_deref().map(fmt::hex),
            r.get::<_, bool>(6)? as i64,
        ]))
    };
    let (rows, on): (Vec<Value>, Option<i64>) = if files > walk.above {
        let top: i64 = db
            .conn
            .query_row("SELECT COALESCE(MAX(id), 0) FROM files", [], |r| r.get(0))?;
        let hi = after.saturating_add(walk.window);
        // NOT INDEXED: the rowid range, in id order, never files_root
        let mut stmt = db.conn.prepare_cached(
            "SELECT id, path, size, mtime, sample, sha256, error IS NOT NULL FROM files NOT INDEXED
             WHERE id > ?4 AND id <= ?6 AND root_id = ?1 AND current = 1 AND first_scan > ?2 AND first_scan <= ?3
             ORDER BY id LIMIT ?5",
        )?;
        let rows = stmt
            .query_map(params![root, since, upto, after, l, hi], row)?
            .collect::<rusqlite::Result<_>>()?;
        (rows, (hi < top).then_some(hi))
    } else {
        let mut stmt = db.conn.prepare_cached(
            "SELECT id, path, size, mtime, sample, sha256, error IS NOT NULL FROM files
             WHERE root_id = ?1 AND current = 1 AND first_scan > ?2 AND first_scan <= ?3 AND id > ?4
             ORDER BY id LIMIT ?5",
        )?;
        let rows = stmt
            .query_map(params![root, since, upto, after, l], row)?
            .collect::<rusqlite::Result<_>>()?;
        (rows, None)
    };
    let next = if rows.len() == l as usize {
        rows.last().and_then(|r| r[0].as_i64())
    } else {
        on
    };
    Ok(json!({ "rows": rows, "next": next }))
}

/// What's recognised on each drive, in brief: the biggest things, without their contents.
fn entities(db: &Db) -> Result<Value> {
    let mut out = Vec::new();
    for id in recognise::root_ids(db)? {
        let Some((r, fresh)) = recognise::load(db, id)? else {
            continue;
        };
        let mut list: Vec<Value> = r
            .entities
            .iter()
            .map(|e| {
                let v = serde_json::to_value(e).unwrap_or(Value::Null);
                json!({ "kind": v["kind"], "label": v["label"], "title": v["title"], "path": v["path"],
                        "key": v["key"], "files": v["files"], "bytes": v["bytes"] })
            })
            .collect();
        list.sort_by_key(|v| std::cmp::Reverse(v["bytes"].as_u64().unwrap_or(0)));
        list.truncate(100);
        out.push(json!({ "root_id": id, "root": r.root, "scan": r.scan, "fresh": fresh, "entities": list }));
    }
    Ok(json!({ "roots": out }))
}

struct Row {
    path: std::path::PathBuf,
    size: u64,
    sample: Option<Vec<u8>>,
    sha: Option<Vec<u8>>,
}

/// A file by its id, if it's current and still as scanned; otherwise why not.
fn current(db: &Db, id: i64) -> Result<(Option<Row>, Option<&'static str>)> {
    type R = (Vec<u8>, i64, i64, Option<Vec<u8>>, Option<Vec<u8>>, bool);
    let row: Option<R> = db
        .conn
        .query_row(
            "SELECT path, size, mtime, sample, sha256, current FROM files WHERE id = ?1",
            [id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .optional()?;
    let Some((path, size, mtime, sample, sha, cur)) = row else {
        return Ok((None, Some("gone")));
    };
    if !cur {
        return Ok((None, Some("gone")));
    }
    let path = paths::from_bytes(&path);
    let meta = match fs::metadata(&path) {
        Ok(m) if m.is_file() => m,
        _ => return Ok((None, Some("missing"))),
    };
    let mt = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64);
    if meta.len() != size as u64 || mt != mtime {
        return Ok((None, Some("changed")));
    }
    Ok((
        Some(Row {
            path,
            size: size as u64,
            sample,
            sha,
        }),
        None,
    ))
}

fn hash(db: &Db, ids: &[i64], mode: HashMode, fresh: bool, until: Instant) -> Result<Value> {
    let mut out = Vec::new();
    let mut done = true;
    for &id in ids {
        if Instant::now() >= until {
            done = false;
            break;
        }
        let (row, why) = current(db, id)?;
        let Some(row) = row else {
            out.push(match why {
                Some("gone") => json!({ "id": id, "gone": true }),
                // still in the index there, not on the disk now (its drive unplugged): the file
                // isn't gone, so the asker keeps it and asks again another time
                Some("missing") => json!({ "id": id, "away": true }),
                _ => json!({ "id": id, "changed": true }),
            });
            continue;
        };
        // what the index knows already needs no reading (unless asked to read it again)
        if !fresh {
            match (mode, &row.sample, &row.sha) {
                (_, _, Some(h)) => {
                    out.push(json!({ "id": id, "sample": row.sample.as_deref().map(fmt::hex), "sha256": fmt::hex(h), "read": false }));
                    continue;
                }
                (HashMode::Sample, Some(s), None) => {
                    out.push(json!({ "id": id, "sample": fmt::hex(s), "read": false }));
                    continue;
                }
                _ => {}
            }
        }
        match mode {
            HashMode::Sample => match crate::hash::sample_of(&row.path, row.size) {
                Ok((s, whole)) => out.push(json!({
                    "id": id, "sample": fmt::hex(&s), "sha256": whole.as_deref().map(fmt::hex), "read": whole.is_some(),
                })),
                Err(e) => out.push(json!({ "id": id, "error": e })),
            },
            HashMode::Full => match full(&row.path, row.size, until) {
                Ok(Some(h)) => out.push(json!({ "id": id, "sha256": fmt::hex(&h), "read": true })),
                Ok(None) => {
                    done = false; // out of time partway: not an answer
                    break;
                }
                Err(e) => out.push(json!({ "id": id, "error": format!("{e:#}") })),
            },
        }
    }
    Ok(json!({ "results": out, "done": done }))
}

/// The whole file's SHA-256, unless time runs out first (None).
fn full(path: &std::path::Path, size: u64, until: Instant) -> Result<Option<Vec<u8>>> {
    let mut f = fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut read = 0u64;
    loop {
        let n = match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        };
        h.update(&buf[..n]);
        let before = read;
        read += n as u64;
        // a look at the clock every 64 MiB
        if before >> 26 != read >> 26 && Instant::now() >= until {
            return Ok(None);
        }
    }
    if read != size {
        bail!("changed since it was indexed");
    }
    Ok(Some(h.finalize().to_vec()))
}
