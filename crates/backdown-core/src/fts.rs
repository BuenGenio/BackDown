//! Full-text search over what's inside the recognised things: mail (subjects, senders,
//! recipients), the text in database dumps and SQLite files, table and column names.
//!
//! Names are searched by `find.rs`; this is about contents. Documents are small: one per
//! message (its headers; the body only when asked for), one per table or chunk of a table's
//! text, one per SQLite full-text table that can be searched in place. They live in `content_docs`
//! with where they came from and how the text was got (`source`: Dovecot's own cache, the
//! message file, the dump, the SQLite file), and in `content_fts`, an SQLite FTS5 index that keeps
//! no copy of the text (contentless): a hit is the document's title, who and when, and where to
//! open it, not a snippet, which is what keeps millions of messages affordable.
//!
//! Filling it is the CLI's job (content.rs there: it knows mail and dump formats); this module is
//! the table, the writer and the query.

use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use crate::db::{Lock, Tx};
use crate::{Db, browse, paths};

pub const SCHEMA: &str = "
-- what has been indexed for content search (fts.rs): one row per document, the text itself only
-- in the FTS index. Added in 1.1 without a schema bump
CREATE TABLE IF NOT EXISTS content_docs (
    id         INTEGER PRIMARY KEY,
    root_id    INTEGER NOT NULL,
    entity_key TEXT NOT NULL,          -- the recognised thing it belongs to
    kind       TEXT NOT NULL,          -- message | table | sqlite-fts | file
    path       TEXT NOT NULL,          -- the file (message, dump, database) or folder (mailbox)
    key        TEXT NOT NULL,          -- the browser's key for path
    locator    TEXT,                   -- within it: a UID, a table name, a chunk number
    title      TEXT NOT NULL,
    who        TEXT NOT NULL DEFAULT '',
    at         INTEGER,
    bytes      INTEGER NOT NULL DEFAULT 0,
    source     TEXT NOT NULL,          -- dovecot-cache | message-file | dump | sqlite | sqlite-fts
    scan       INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS content_docs_entity ON content_docs(root_id, entity_key);
CREATE VIRTUAL TABLE IF NOT EXISTS content_fts USING fts5(
    title, who, body,
    content='', contentless_delete=1,
    tokenize='unicode61 remove_diacritics 2'
);
";

/// What's known about one document when it's indexed.
pub struct Doc<'a> {
    pub root_id: i64,
    pub entity_key: &'a str,
    pub kind: &'a str,
    pub path: &'a Path,
    pub locator: Option<&'a str>,
    pub title: &'a str,
    pub who: &'a str,
    pub at: Option<i64>,
    pub bytes: u64,
    pub source: &'a str,
    pub scan: i64,
    /// the searchable text beyond title and who (not kept)
    pub body: &'a str,
}

/// Adds documents in one transaction (`db::Tx`). Drop without `commit` to roll back.
pub struct Writer<'a> {
    db: &'a Db,
    tx: Tx<'a>,
    pub added: u64,
    /// since the last commit: how many documents, and since when
    pending: u64,
    since: Instant,
}

/// A writer commits at least this often, so reading a big file for a few documents never keeps
/// other writers (scans, syncs, the command line) waiting on the index.
const COMMIT_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

impl<'a> Writer<'a> {
    pub fn begin(db: &'a Db) -> Result<Self> {
        Ok(Writer {
            db,
            tx: Tx::begin(&db.conn, Lock::Deferred)?,
            added: 0,
            pending: 0,
            since: Instant::now(),
        })
    }

    /// Commit if `batch` documents or a couple of seconds have gone by since the last commit.
    pub fn tick(&mut self, batch: u64) -> Result<()> {
        if self.pending >= batch || (self.pending > 0 && self.since.elapsed() >= COMMIT_EVERY) {
            self.checkpoint()?;
        }
        Ok(())
    }

    pub fn put(&mut self, d: &Doc) -> Result<i64> {
        let mut ins = self.db.conn.prepare_cached(
            "INSERT INTO content_docs(root_id, entity_key, kind, path, key, locator, title, who, at, bytes, source, scan)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        )?;
        ins.execute(params![
            d.root_id,
            d.entity_key,
            d.kind,
            d.path.display().to_string(),
            browse::key(d.path),
            d.locator,
            d.title,
            d.who,
            d.at,
            d.bytes as i64,
            d.source,
            d.scan
        ])?;
        let id = self.db.conn.last_insert_rowid();
        let mut fts = self.db.conn.prepare_cached(
            "INSERT INTO content_fts(rowid, title, who, body) VALUES (?1, ?2, ?3, ?4)",
        )?;
        fts.execute(params![id, d.title, d.who, d.body])?;
        self.added += 1;
        self.pending += 1;
        Ok(id)
    }

    /// Commit now and start another transaction (so a long build doesn't hold one for hours).
    pub fn checkpoint(&mut self) -> Result<()> {
        self.tx.checkpoint()?;
        self.pending = 0;
        self.since = Instant::now();
        Ok(())
    }

    pub fn commit(self) -> Result<u64> {
        self.tx.commit()?;
        Ok(self.added)
    }
}

/// Drop what was indexed for one entity (before indexing it again), or for a whole root.
pub fn forget(db: &Db, root_id: i64, entity_key: Option<&str>) -> Result<u64> {
    let ids: Vec<i64> = {
        let mut stmt = match entity_key {
            Some(_) => db
                .conn
                .prepare("SELECT id FROM content_docs WHERE root_id = ?1 AND entity_key = ?2")?,
            None => db
                .conn
                .prepare("SELECT id FROM content_docs WHERE root_id = ?1 AND ?2 IS NULL")?,
        };
        let rows = stmt.query_map(params![root_id, entity_key], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    if ids.is_empty() {
        return Ok(0);
    }
    // inside the caller's transaction if there is one, else its own (rolled back on any error)
    let tx = Tx::join(&db.conn, Lock::Deferred)?;
    {
        // a contentless_delete table takes an ordinary DELETE by rowid
        let mut del_fts = db
            .conn
            .prepare_cached("DELETE FROM content_fts WHERE rowid = ?1")?;
        let mut del = db
            .conn
            .prepare_cached("DELETE FROM content_docs WHERE id = ?1")?;
        for id in &ids {
            del_fts.execute(params![id])?;
            del.execute(params![id])?;
        }
    }
    tx.commit()?;
    Ok(ids.len() as u64)
}

/// The scan of documents written while their thing is still being read: a build stopped part way
/// leaves them so, and the next build sees the thing isn't done.
pub const PENDING: i64 = 0;

/// Stamp an entity's documents with the scan they describe, now that all of it has been read.
pub fn settle(db: &Db, root_id: i64, entity_key: &str, scan: i64) -> Result<()> {
    db.conn.execute(
        "UPDATE content_docs SET scan = ?3 WHERE root_id = ?1 AND entity_key = ?2",
        params![root_id, entity_key, scan],
    )?;
    Ok(())
}

/// The scan an entity's documents describe, if any (so a rebuild can skip what's current).
pub fn indexed_scan(db: &Db, root_id: i64, entity_key: &str) -> Result<Option<i64>> {
    Ok(db
        .conn
        .query_row(
            "SELECT MAX(scan) FROM content_docs WHERE root_id = ?1 AND entity_key = ?2",
            params![root_id, entity_key],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten())
}

#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    pub id: i64,
    pub root_id: i64,
    pub root: String,
    pub entity_key: String,
    pub kind: String,
    pub path: String,
    pub key: String,
    pub locator: Option<String>,
    pub title: String,
    pub who: String,
    pub at: Option<i64>,
    pub bytes: u64,
    pub source: String,
}

#[derive(Debug, Serialize)]
pub struct Found {
    pub hits: Vec<Hit>,
    /// all matches (hits are the best `limit`), counted up to 10,000
    pub total: u64,
    pub ms: u64,
    /// the FTS5 query the words became
    pub query: String,
}

/// Words typed by a person as an FTS5 query: each word a prefix match, all required; a quoted
/// phrase stays a phrase. FTS5 syntax characters are neutralised, so nothing a person types is
/// a syntax error.
pub fn query_of(q: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut rest = q.trim();
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix('"') {
            let end = r.find('"').unwrap_or(r.len());
            let phrase = r[..end].replace('"', " ");
            if !phrase.trim().is_empty() {
                out.push(format!("\"{}\"", phrase.trim()));
            }
            rest = r[end..].strip_prefix('"').unwrap_or(&r[end..]).trim_start();
            continue;
        }
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let word: String = rest[..end]
            .chars()
            .filter(|c| {
                c.is_alphanumeric()
                    || *c == '@'
                    || *c == '.'
                    || *c == '-'
                    || *c == '_'
                    || *c == '\''
            })
            .collect();
        // an address or a dotted name is several tokens to FTS5: a phrase keeps them together
        let tokens: Vec<&str> = word
            .split(|c: char| !c.is_alphanumeric() && c != '\'')
            .filter(|t| !t.is_empty())
            .collect();
        match tokens.len() {
            0 => {}
            1 => out.push(format!("\"{}\"*", tokens[0])),
            _ => out.push(format!("\"{}\"*", tokens.join(" "))),
        }
        rest = rest[end..].trim_start();
    }
    out.join(" ")
}

pub struct Query<'a> {
    pub q: &'a str,
    /// "message", "table", …
    pub kind: Option<&'a str>,
    pub root_id: Option<i64>,
    pub entity_key: Option<&'a str>,
    pub limit: usize,
}

pub fn search(db: &Db, q: &Query) -> Result<Found> {
    let t0 = Instant::now();
    let fq = query_of(q.q);
    if fq.is_empty() {
        return Ok(Found {
            hits: Vec::new(),
            total: 0,
            ms: 0,
            query: fq,
        });
    }
    let limit = q.limit.clamp(1, 2000) as i64;
    let mut stmt = db.conn.prepare_cached(
        "SELECT d.id, d.root_id, r.path, d.entity_key, d.kind, d.path, d.key, d.locator, d.title, d.who, d.at, d.bytes, d.source
         FROM content_fts f JOIN content_docs d ON d.id = f.rowid JOIN roots r ON r.id = d.root_id
         WHERE content_fts MATCH ?1
           AND (?2 IS NULL OR d.kind = ?2) AND (?3 IS NULL OR d.root_id = ?3) AND (?4 IS NULL OR d.entity_key = ?4)
         ORDER BY bm25(content_fts, 4.0, 2.0, 1.0), d.at DESC
         LIMIT ?5",
    )?;
    let hits = stmt
        .query_map(params![fq, q.kind, q.root_id, q.entity_key, limit], |r| {
            Ok(Hit {
                id: r.get(0)?,
                root_id: r.get(1)?,
                root: paths::from_bytes(&r.get::<_, Vec<u8>>(2)?)
                    .display()
                    .to_string(),
                entity_key: r.get(3)?,
                kind: r.get(4)?,
                path: r.get(5)?,
                key: r.get(6)?,
                locator: r.get(7)?,
                title: r.get(8)?,
                who: r.get(9)?,
                at: r.get(10)?,
                bytes: r.get::<_, i64>(11)? as u64,
                source: r.get(12)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("searching contents")?;
    let total: i64 = if (hits.len() as i64) < limit {
        hits.len() as i64
    } else {
        db.conn.query_row(
            "SELECT COUNT(*) FROM (SELECT f.rowid FROM content_fts f JOIN content_docs d ON d.id = f.rowid
             WHERE content_fts MATCH ?1 AND (?2 IS NULL OR d.kind = ?2) AND (?3 IS NULL OR d.root_id = ?3) AND (?4 IS NULL OR d.entity_key = ?4)
             LIMIT 10000)",
            params![fq, q.kind, q.root_id, q.entity_key],
            |r| r.get(0),
        )?
    };
    Ok(Found {
        hits,
        total: total as u64,
        ms: t0.elapsed().as_millis() as u64,
        query: fq,
    })
}

/// How much is indexed, per drive and source.
#[derive(Debug, Clone, Serialize)]
pub struct Coverage {
    pub root_id: i64,
    pub root: String,
    pub kind: String,
    pub source: String,
    pub docs: u64,
    pub entities: u64,
}

pub fn coverage(db: &Db) -> Result<Vec<Coverage>> {
    let mut stmt = db.conn.prepare(
        "SELECT d.root_id, r.path, d.kind, d.source, COUNT(*), COUNT(DISTINCT d.entity_key)
         FROM content_docs d JOIN roots r ON r.id = d.root_id
         GROUP BY d.root_id, d.kind, d.source ORDER BY r.path, d.kind, d.source",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(Coverage {
            root_id: r.get(0)?,
            root: paths::from_bytes(&r.get::<_, Vec<u8>>(1)?)
                .display()
                .to_string(),
            kind: r.get(2)?,
            source: r.get(3)?,
            docs: r.get::<_, i64>(4)? as u64,
            entities: r.get::<_, i64>(5)? as u64,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_become_a_safe_query() {
        assert_eq!(query_of("hello"), "\"hello\"*");
        assert_eq!(query_of("hello world"), "\"hello\"* \"world\"*");
        assert_eq!(
            query_of("\"exact phrase\" more"),
            "\"exact phrase\" \"more\"*"
        );
        assert_eq!(query_of("alice@example.org"), "\"alice example org\"*");
        assert_eq!(query_of("AND OR NOT ( ) *"), "\"AND\"* \"OR\"* \"NOT\"*");
        assert_eq!(query_of("   "), "");
    }

    #[test]
    fn indexes_and_finds() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("i.db")).unwrap();
        db.conn
            .execute(
                "INSERT INTO roots(id, path, last_scan) VALUES (1, ?1, 5)",
                params![paths::to_bytes(Path::new("/d"))],
            )
            .unwrap();
        let mut w = Writer::begin(&db).unwrap();
        for (i, (t, who, body)) in [
            ("Invoice 2021-03", "Alice <alice@example.org>", ""),
            ("Re: the garden", "bob@example.org", "roses and tulips"),
            ("orders", "shop", "id customer_name total created_at"),
        ]
        .iter()
        .enumerate()
        {
            w.put(&Doc {
                root_id: 1,
                entity_key: if i < 2 { "mail" } else { "dump" },
                kind: if i < 2 { "message" } else { "table" },
                path: Path::new("/d/x"),
                locator: Some(&i.to_string()),
                title: t,
                who,
                at: Some(1_600_000_000 + i as i64),
                bytes: 10,
                source: "test",
                scan: 5,
                body,
            })
            .unwrap();
        }
        assert_eq!(w.commit().unwrap(), 3);
        let find = |q: &str, kind: Option<&str>| {
            search(
                &db,
                &Query {
                    q,
                    kind,
                    root_id: None,
                    entity_key: None,
                    limit: 10,
                },
            )
            .unwrap()
        };
        assert_eq!(find("invoice", None).hits[0].title, "Invoice 2021-03");
        assert_eq!(find("alice@example.org", None).total, 1);
        assert_eq!(find("tulip", None).hits[0].title, "Re: the garden");
        assert_eq!(find("customer", None).hits[0].who, "shop");
        assert_eq!(find("customer", Some("message")).total, 0);
        assert_eq!(find("example", None).total, 2);
        assert_eq!(find("nothing-like-this", None).total, 0);
        assert_eq!(indexed_scan(&db, 1, "mail").unwrap(), Some(5));
        let cov = coverage(&db).unwrap();
        assert_eq!(cov.len(), 2);
        assert_eq!(forget(&db, 1, Some("mail")).unwrap(), 2);
        assert_eq!(find("example", None).total, 0);
        assert_eq!(find("customer", None).total, 1);
        assert_eq!(forget(&db, 1, None).unwrap(), 1);
        assert_eq!(find("customer", None).total, 0);
    }
}
