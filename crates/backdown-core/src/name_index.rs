//! The names index: every distinct folded name once, whoever has it (this machine's files and
//! folders, and the files and folders cached here from other machines), with an SQLite FTS5
//! trigram index over them, so that finding the names that contain what was typed takes
//! milliseconds instead of a read of every file's name.
//!
//! Why distinct names: an index holds far fewer names than rows (on 17.4M files and 2.6M folders,
//! 2.6M names; with 27.4M files cached from another machine, 8.2M), and every version of a file
//! keeps its name. Why trigrams: a search matches anywhere in a name ("report" finds "annual
//! report.docx"), which an ordinary index can't answer without reading all of it; a trigram index
//! answers a term of 3 characters or more directly. Shorter terms read the names table, which is
//! still a fraction of the rows.
//!
//! A search here gives names; the rows with a name come from each table's own name index
//! (`files_lname`, `dirs_lname`, `remote_files_lname`, `remote_dirs_lname`): see find.rs and
//! swarm.rs.
//!
//! Keeping it current: triggers add a row's name whenever a row with a name is written or
//! renamed. Names are never taken out: one nobody has any more just finds no rows. Filling it on
//! an index made before it existed is a job of the daemon's (`fill`, through
//! `Db::build_later_indexes`), in batches that are committed one by one, so an interrupted fill
//! carries on where it was; until a table's names are all in, searches read that table as before
//! (`ready`).

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

use crate::db::{Lock, Tx};
use crate::remote;

/// Created when the index opens, after the tables it watches and their `lname` columns.
pub const SCHEMA: &str = "
-- every distinct folded name (name_index.rs), and a trigram index over them. Added in 1.0 beta.7
-- without a schema bump: an older BackDown leaves them alone, and the triggers keep them current
-- whoever writes the rows
CREATE TABLE IF NOT EXISTS names (
    id    INTEGER PRIMARY KEY,
    lname TEXT NOT NULL UNIQUE,
    has   INTEGER NOT NULL DEFAULT 0     -- who has it: 1 files, 2 dirs, 4 remote_files, 8 remote_dirs
);
CREATE VIRTUAL TABLE IF NOT EXISTS names_fts USING fts5(
    lname, content='names', content_rowid='id', tokenize='trigram', detail='none'
);
CREATE TRIGGER IF NOT EXISTS names_fts_add AFTER INSERT ON names BEGIN
    INSERT INTO names_fts(rowid, lname) VALUES (new.id, new.lname);
END;
-- how far filling in the names of rows written before this existed has got (`fill`)
CREATE TABLE IF NOT EXISTS names_filled (
    what  TEXT PRIMARY KEY,
    after BLOB,
    done  INTEGER NOT NULL DEFAULT 0
);
-- the folders of other machines' cached files: their files are synced, their folders are worked
-- out from the files' paths (swarm.rs searches them; remote/mod.rs adds them as files arrive)
CREATE TABLE IF NOT EXISTS remote_dirs (
    remote_id INTEGER NOT NULL REFERENCES remotes(id) ON DELETE CASCADE,
    path      BLOB NOT NULL,            -- in that machine's raw bytes, like remote_files.path
    root_id   INTEGER NOT NULL,
    lname     TEXT NOT NULL,
    newest    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (remote_id, path)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS remote_dirs_lname ON remote_dirs(lname, remote_id);
CREATE TRIGGER IF NOT EXISTS files_name_add AFTER INSERT ON files WHEN new.lname IS NOT NULL BEGIN
    INSERT INTO names(lname, has) VALUES (new.lname, 1)
    ON CONFLICT(lname) DO UPDATE SET has = has | 1 WHERE (has & 1) = 0;
END;
CREATE TRIGGER IF NOT EXISTS files_name_set AFTER UPDATE OF lname ON files WHEN new.lname IS NOT NULL BEGIN
    INSERT INTO names(lname, has) VALUES (new.lname, 1)
    ON CONFLICT(lname) DO UPDATE SET has = has | 1 WHERE (has & 1) = 0;
END;
CREATE TRIGGER IF NOT EXISTS dirs_name_add AFTER INSERT ON dirs WHEN new.lname IS NOT NULL BEGIN
    INSERT INTO names(lname, has) VALUES (new.lname, 2)
    ON CONFLICT(lname) DO UPDATE SET has = has | 2 WHERE (has & 2) = 0;
END;
CREATE TRIGGER IF NOT EXISTS dirs_name_set AFTER UPDATE OF lname ON dirs WHEN new.lname IS NOT NULL BEGIN
    INSERT INTO names(lname, has) VALUES (new.lname, 2)
    ON CONFLICT(lname) DO UPDATE SET has = has | 2 WHERE (has & 2) = 0;
END;
CREATE TRIGGER IF NOT EXISTS remote_files_name_add AFTER INSERT ON remote_files WHEN new.lname IS NOT NULL BEGIN
    INSERT INTO names(lname, has) VALUES (new.lname, 4)
    ON CONFLICT(lname) DO UPDATE SET has = has | 4 WHERE (has & 4) = 0;
END;
CREATE TRIGGER IF NOT EXISTS remote_files_name_set AFTER UPDATE OF lname ON remote_files WHEN new.lname IS NOT NULL BEGIN
    INSERT INTO names(lname, has) VALUES (new.lname, 4)
    ON CONFLICT(lname) DO UPDATE SET has = has | 4 WHERE (has & 4) = 0;
END;
CREATE TRIGGER IF NOT EXISTS remote_dirs_name_add AFTER INSERT ON remote_dirs BEGIN
    INSERT INTO names(lname, has) VALUES (new.lname, 8)
    ON CONFLICT(lname) DO UPDATE SET has = has | 8 WHERE (has & 8) = 0;
END;
";

/// The index on cached files' names: built once their names are filled in (`fill`), since
/// building it afterwards is much quicker than keeping it up through 27 million updates.
const REMOTE_FILES_LNAME: &str =
    "CREATE INDEX IF NOT EXISTS remote_files_lname ON remote_files(lname, remote_id)";

/// This machine's own tables, and the ones cached from other machines.
pub const LOCAL: &[&str] = &["files", "dirs"];
pub const REMOTE: &[&str] = &["remote_files", "remote_dirs"];

/// Who has a name (`names.has`): a search only looks up the rows of names its tables have, so a
/// word that's mostly in another machine's files doesn't cost a lookup per name here.
pub const FILES: i64 = 1;
pub const DIRS: i64 = 2;
pub const REMOTE_FILES: i64 = 4;
pub const REMOTE_DIRS: i64 = 8;

fn bit(table: &str) -> i64 {
    match table {
        "files" => FILES,
        "dirs" => DIRS,
        "remote_files" => REMOTE_FILES,
        _ => REMOTE_DIRS,
    }
}
const ALL: &[&str] = &["files", "dirs", "remote_files", "remote_dirs"];

/// Rows read per committed batch while filling.
const BATCH: i64 = 20_000;
/// Terms shorter than this can't use the trigram index.
const TRIGRAM: usize = 3;

/// Whether every name of these tables is in (a search may use the names index for them). False on
/// an index that hasn't got the names index at all, which is what a reader sees before the first
/// writer of a newer BackDown opens it.
pub fn ready(conn: &Connection, tables: &[&str]) -> Result<bool> {
    if !exists(conn, "names_filled")? {
        return Ok(false);
    }
    let mut stmt = conn.prepare_cached("SELECT done FROM names_filled WHERE what = ?1")?;
    for t in tables {
        let done: Option<bool> = stmt.query_row([t], |r| r.get(0)).optional()?;
        if done != Some(true) {
            return Ok(false);
        }
    }
    if tables.contains(&"remote_files") && !index_exists(conn, "remote_files_lname")? {
        return Ok(false);
    }
    Ok(true)
}

/// When the index opens: a table with no rows at all has nothing to fill in (a new index, or no
/// other machines yet), so it's ready from the start, and so is the cached files' name index.
pub fn open(conn: &Connection) -> Result<()> {
    let mut known = conn.prepare("SELECT EXISTS (SELECT 1 FROM names_filled WHERE what = ?1)")?;
    for table in ALL {
        if known.query_row([table], |r| r.get::<_, bool>(0))? {
            continue;
        }
        let empty: bool = conn.query_row(
            &format!("SELECT NOT EXISTS (SELECT 1 FROM {table})"),
            [],
            |r| r.get(0),
        )?;
        if empty {
            if *table == "remote_files" {
                conn.execute_batch(REMOTE_FILES_LNAME)?;
            }
            save(conn, table, None, true)?;
        }
    }
    Ok(())
}

/// Whether filling still has work to do (for `Db::missing_later_indexes`).
pub fn unfilled(conn: &Connection) -> Result<bool> {
    Ok(!ready(conn, ALL)?)
}

fn exists(conn: &Connection, table: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE name = ?1)",
        [table],
        |r| r.get(0),
    )?)
}

fn index_exists(conn: &Connection, name: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = ?1)",
        [name],
        |r| r.get(0),
    )?)
}

/// The distinct names containing every term (already folded) that any of the tables in `has`
/// (the bits above) has, each with who has it. Terms of 3 characters or more are looked up in the
/// trigram index; when there are none, every name is read.
pub fn matching(conn: &Connection, terms: &[&str], has: i64) -> Result<Vec<(String, i64)>> {
    let keep = |name: &str| terms.iter().all(|t| name.contains(t));
    // each trigram of each term, all of them required (detail=none can't check they're next to
    // each other, so the names it gives are checked here)
    let mut grams: Vec<String> = terms
        .iter()
        .flat_map(|t| {
            let c: Vec<char> = t.chars().collect();
            c.windows(TRIGRAM)
                .map(|w| w.iter().collect::<String>())
                .collect::<Vec<_>>()
        })
        .collect();
    grams.sort();
    grams.dedup();
    let mut out = Vec::new();
    let mut push = |r: &rusqlite::Row| -> rusqlite::Result<()> {
        let (name, who): (String, i64) = (r.get(0)?, r.get(1)?);
        if keep(&name) {
            out.push((name, who));
        }
        Ok(())
    };
    if grams.is_empty() {
        let mut stmt = conn.prepare_cached("SELECT lname, has FROM names WHERE (has & ?1) != 0")?;
        let mut rows = stmt.query([has])?;
        while let Some(r) = rows.next()? {
            push(r)?;
        }
    } else {
        let expr = grams
            .iter()
            .map(|g| format!("\"{}\"", g.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" AND ");
        let mut stmt = conn.prepare_cached(
            "SELECT n.lname, n.has FROM names_fts JOIN names n ON n.id = names_fts.rowid
             WHERE names_fts MATCH ?1 AND (n.has & ?2) != 0",
        )?;
        let mut rows = stmt.query(params![expr, has])?;
        while let Some(r) = rows.next()? {
            push(r)?;
        }
    }
    Ok(out)
}

/// Fill in what isn't there yet, saying which part is under way: this machine's names, then the
/// names and folders of other machines' cached files. Each batch is committed, and where it got to
/// is kept, so an interrupted fill carries on the next time. What was filled now, for the job's
/// summary (empty when nothing was left).
pub fn fill(conn: &Connection, doing: &mut dyn FnMut(&str)) -> Result<Vec<&'static str>> {
    let mut filled = Vec::new();
    for table in LOCAL {
        if !is_done(conn, table)? {
            doing("searching names");
            fill_local(conn, table)?;
            filled.push("searching names");
        }
    }
    if !is_done(conn, "remote_files")? || !index_exists(conn, "remote_files_lname")? {
        doing("searching other machines' files");
        fill_remote_files(conn)?;
        conn.execute_batch(REMOTE_FILES_LNAME)?;
        filled.push("searching other machines' files");
    }
    if !is_done(conn, "remote_dirs")? {
        doing("searching other machines' folders");
        fill_remote_dirs(conn)?;
        filled.push("searching other machines' folders");
    }
    filled.dedup();
    Ok(filled)
}

fn is_done(conn: &Connection, what: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT done FROM names_filled WHERE what = ?1",
            [what],
            |r| r.get::<_, bool>(0),
        )
        .optional()?
        .unwrap_or(false))
}

fn cursor(conn: &Connection, what: &str) -> Result<Option<Vec<u8>>> {
    Ok(conn
        .query_row(
            "SELECT after FROM names_filled WHERE what = ?1",
            [what],
            |r| r.get::<_, Option<Vec<u8>>>(0),
        )
        .optional()?
        .flatten())
}

fn save(conn: &Connection, what: &str, after: Option<&[u8]>, done: bool) -> Result<()> {
    conn.execute(
        "INSERT INTO names_filled(what, after, done) VALUES (?1, ?2, ?3)
         ON CONFLICT(what) DO UPDATE SET after = excluded.after, done = excluded.done",
        params![what, after, done],
    )?;
    Ok(())
}

/// This machine's names, read in name order from the table's own name index.
fn fill_local(conn: &Connection, table: &str) -> Result<()> {
    let mut after = cursor(conn, table)?
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let mut read = conn.prepare(&format!(
        "SELECT DISTINCT lname FROM {table} INDEXED BY {table}_lname
         WHERE lname > ?1 ORDER BY lname LIMIT ?2"
    ))?;
    loop {
        let names: Vec<String> = read
            .query_map(params![after, BATCH], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let tx = Tx::begin(conn, Lock::Immediate)?;
        {
            let mut put = conn.prepare_cached(
                "INSERT INTO names(lname, has) VALUES (?1, ?2)
                 ON CONFLICT(lname) DO UPDATE SET has = has | ?2 WHERE (has & ?2) = 0",
            )?;
            for n in &names {
                put.execute(params![n, bit(table)])?;
            }
        }
        match names.last() {
            Some(last) => {
                after.clone_from(last);
                save(conn, table, Some(after.as_bytes()), false)?;
                tx.commit()?;
            }
            None => {
                save(conn, table, Some(after.as_bytes()), true)?;
                tx.commit()?;
                return Ok(());
            }
        }
    }
}

/// The machines whose files are cached here, and how their paths are written.
fn machines(conn: &Connection) -> Result<Vec<(i64, remote::Os)>> {
    let mut stmt = conn.prepare("SELECT id, info FROM remotes ORDER BY id")?;
    let rows = stmt.query_map([], |r| {
        let info: Option<String> = r.get(1)?;
        Ok((r.get::<_, i64>(0)?, info))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, info) = row?;
        let windows = info
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .is_some_and(|v| v["os"] == "windows");
        out.push((
            id,
            if windows {
                remote::Os::Windows
            } else {
                remote::Os::Unix
            },
        ));
    }
    Ok(out)
}

/// A cached file's or folder's name as search compares it.
pub fn remote_lname(os: remote::Os, raw: &[u8]) -> String {
    crate::find::fold(&remote::name_of(os, raw))
}

/// Names for the cached files that haven't got one, machine by machine in row order.
fn fill_remote_files(conn: &Connection) -> Result<()> {
    // where it got to: the machine and the row there, as 16 bytes
    let (mut m_at, mut rid_at) = match cursor(conn, "remote_files")? {
        Some(b) if b.len() == 16 => (
            i64::from_be_bytes(b[..8].try_into().unwrap()),
            i64::from_be_bytes(b[8..].try_into().unwrap()),
        ),
        _ => (i64::MIN, i64::MIN),
    };
    let oses = machines(conn)?;
    let mut read = conn.prepare(
        "SELECT rid, path FROM remote_files WHERE remote_id = ?1 AND rid > ?2 AND lname IS NULL
         ORDER BY rid LIMIT ?3",
    )?;
    for &(m, os) in &oses {
        if m < m_at {
            continue;
        }
        if m > m_at {
            (m_at, rid_at) = (m, i64::MIN);
        }
        loop {
            let rows: Vec<(i64, Vec<u8>)> = read
                .query_map(params![m, rid_at, BATCH], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let Some(&(last, _)) = rows.last() else {
                break;
            };
            let tx = Tx::begin(conn, Lock::Immediate)?;
            {
                let mut put = conn.prepare_cached(
                    "UPDATE remote_files SET lname = ?3 WHERE remote_id = ?1 AND rid = ?2",
                )?;
                for (rid, path) in &rows {
                    put.execute(params![m, rid, remote_lname(os, path)])?;
                }
            }
            rid_at = last;
            let mut at = m_at.to_be_bytes().to_vec();
            at.extend_from_slice(&rid_at.to_be_bytes());
            save(conn, "remote_files", Some(&at), false)?;
            tx.commit()?;
        }
    }
    save(conn, "remote_files", None, true)?;
    Ok(())
}

/// The folders of the cached files, worked out from their paths, machine by machine in path order.
fn fill_remote_dirs(conn: &Connection) -> Result<()> {
    // where it got to: the machine (8 bytes), then the last file's path there
    let (mut m_at, mut path_at) = match cursor(conn, "remote_dirs")? {
        Some(b) if b.len() >= 8 => (
            i64::from_be_bytes(b[..8].try_into().unwrap()),
            b[8..].to_vec(),
        ),
        _ => (i64::MIN, Vec::new()),
    };
    let oses = machines(conn)?;
    let mut read = conn.prepare(
        "SELECT f.root_id, f.path, f.mtime, r.path FROM remote_files f INDEXED BY remote_files_path
         LEFT JOIN remote_roots r ON r.remote_id = f.remote_id AND r.root_id = f.root_id
         WHERE f.remote_id = ?1 AND f.path > ?2 ORDER BY f.path LIMIT ?3",
    )?;
    for &(m, os) in &oses {
        if m < m_at {
            continue;
        }
        if m > m_at {
            (m_at, path_at) = (m, Vec::new());
        }
        let mut seen = HashSet::new();
        loop {
            type Row = (i64, Vec<u8>, i64, Option<Vec<u8>>);
            let rows: Vec<Row> = read
                .query_map(params![m, path_at, BATCH], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })?
                .collect::<rusqlite::Result<_>>()?;
            let Some(last) = rows.last().map(|r| r.1.clone()) else {
                break;
            };
            let tx = Tx::begin(conn, Lock::Immediate)?;
            add_folders(
                conn,
                m,
                os,
                rows.iter().map(|(root, path, mtime, top)| {
                    (*root, top.as_deref(), path.as_slice(), *mtime)
                }),
                &mut seen,
            )?;
            path_at = last;
            let mut at = m_at.to_be_bytes().to_vec();
            at.extend_from_slice(&path_at);
            save(conn, "remote_dirs", Some(&at), false)?;
            tx.commit()?;
            // folders already written stay written; only the recent ones are worth remembering
            if seen.len() > 100_000 {
                seen.clear();
            }
        }
    }
    save(conn, "remote_dirs", None, true)?;
    Ok(())
}

/// The folders holding these cached files (root id, its drive's top, the file's path, its time),
/// recorded once each: the folder a file is in gets the newest of its files' times, the ones
/// above it are added if they aren't there. `seen` saves asking again about folders just written.
pub fn add_folders<'a>(
    conn: &Connection,
    m: i64,
    os: remote::Os,
    files: impl IntoIterator<Item = (i64, Option<&'a [u8]>, &'a [u8], i64)>,
    seen: &mut HashSet<Vec<u8>>,
) -> Result<()> {
    // each folder's newest file, in the order they come (paths sorted: neighbours share folders):
    // (folder, root id, its drive's top, the time)
    type Folder<'t> = (Vec<u8>, i64, Option<&'t [u8]>, i64);
    let mut newest: Vec<Folder> = Vec::new();
    let mut at: HashMap<Vec<u8>, usize> = HashMap::new();
    for (root, top, file, mtime) in files {
        let dir = remote::parent(os, file);
        match at.get(dir) {
            Some(&i) => newest[i].3 = newest[i].3.max(mtime),
            None => {
                at.insert(dir.to_vec(), newest.len());
                newest.push((dir.to_vec(), root, top, mtime));
            }
        }
    }
    let mut put = conn.prepare_cached(
        "INSERT INTO remote_dirs(remote_id, path, root_id, lname, newest) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(remote_id, path) DO UPDATE SET newest = max(newest, excluded.newest)",
    )?;
    let mut add = conn.prepare_cached(
        "INSERT OR IGNORE INTO remote_dirs(remote_id, path, root_id, lname, newest)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    // a folder at or above the drive's top isn't one of its folders
    let inside = |dir: &[u8], top: Option<&[u8]>| match top {
        Some(t) => dir.len() > t.len(),
        None => remote::parent(os, dir) != dir,
    };
    for (dir, root, top, time) in &newest {
        if !inside(dir, *top) {
            continue;
        }
        put.execute(params![m, dir, root, remote_lname(os, dir), time])?;
        seen.insert(dir.clone());
        // the ones above it, up to the first that's there already
        let mut up = remote::parent(os, dir);
        while inside(up, *top) && up != dir.as_slice() && !seen.contains(up) {
            let added = add.execute(params![m, up, root, remote_lname(os, up), time])? > 0;
            seen.insert(up.to_vec());
            if !added {
                break; // there already, so the ones above it are too
            }
            let next = remote::parent(os, up);
            if next == up {
                break;
            }
            up = next;
        }
    }
    Ok(())
}
