//! Tags, flags and relationship edges: the model layer over the index (see docs/MODEL.md,
//! docs/TAGGING.md).
//!
//! A **tag** is a fact about content ("this is a photo", "taken in Hong Kong"); a **flag** is a
//! decision about an item ("this is the canonical copy", "duplicate of X"); an **edge** is a
//! relationship ("this photo was taken at that place"). All three attach to a *target* named by
//! a `(target_type, target_key)` pair, where the key is stable across scans and file moves:
//! content by `hex(sha256)`, a person/database by its `Subject.key`, a place/collection by its
//! `Entity.key`. Never a bare path (paths are versions).
//!
//! The tables are additive (`CREATE TABLE IF NOT EXISTS`), stitched in by `Db::open` with no
//! schema-version bump, so an older BackDown simply ignores them. Each row carries its own
//! provenance (`source`/`added_at`, `decided_by`/`decided_at`); binding authored changes into the
//! hash-chained custody log is a deliberate follow-up (the custody chain is file-operation
//! centric today).

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::clock::now;

/// The kinds of thing a tag/flag/edge can point at. The key's meaning depends on the type.
pub const TARGET_CONTENT: &str = "content"; // key = hex(sha256)
pub const TARGET_SUBJECT: &str = "subject"; // key = Subject.key
pub const TARGET_ENTITY: &str = "entity"; // key = Entity.key
pub const TARGET_PATH: &str = "path"; // key = browse key of a path (a specific copy)

/// The tables. `content` is a thin optional cache (one row per sha256); tags attach to content by
/// `hex(sha256)` rather than by a foreign key, so it is never load-bearing.
pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS content (
    sha256 BLOB PRIMARY KEY,
    size   INTEGER NOT NULL,
    kind   TEXT                     -- photo|music|video|… cached from recognise/magic
);
CREATE TABLE IF NOT EXISTS tags (
    id        INTEGER PRIMARY KEY,
    kind      TEXT NOT NULL,        -- owner|kind|location|country|city|place|airport|event|date|memory
    value     TEXT NOT NULL,
    parent_id INTEGER REFERENCES tags(id),  -- hierarchy: country>city, trip>place
    data      TEXT,                 -- JSON: coords, codes, confidence hints
    UNIQUE(kind, value, parent_id)
);
CREATE TABLE IF NOT EXISTS taggings (
    target_type TEXT NOT NULL,      -- content|subject|entity|path
    target_key  TEXT NOT NULL,
    tag_id      INTEGER NOT NULL REFERENCES tags(id),
    source      TEXT NOT NULL,      -- exif|derived|reference|user
    confidence  REAL NOT NULL DEFAULT 1.0,
    added_at    INTEGER NOT NULL,
    PRIMARY KEY (target_type, target_key, tag_id)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS taggings_by_tag ON taggings(tag_id);
CREATE TABLE IF NOT EXISTS item_state (
    target_type   TEXT NOT NULL,    -- content|subject|entity|path
    target_key    TEXT NOT NULL,
    state         TEXT NOT NULL,    -- canonical|duplicate|keep|drop|reviewed|owner-confirmed|mine|not-mine
    canonical_key TEXT,             -- for duplicate → the canonical target_key
    decided_by    TEXT NOT NULL,    -- user|auto
    decided_at    INTEGER NOT NULL,
    note          TEXT,
    PRIMARY KEY (target_type, target_key, state)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS edges (
    src_type TEXT NOT NULL,
    src_key  TEXT NOT NULL,
    rel      TEXT NOT NULL,         -- taken_at|in|part_of|owns|came_from|copy_of|…
    dst_type TEXT NOT NULL,
    dst_key  TEXT NOT NULL,
    data     TEXT,
    at       INTEGER NOT NULL,
    PRIMARY KEY (src_type, src_key, rel, dst_type, dst_key)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS edges_dst ON edges(dst_type, dst_key, rel);
";

// --- tags -------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tag {
    pub id: i64,
    pub kind: String,
    pub value: String,
    pub parent_id: Option<i64>,
    pub data: Option<String>,
}

/// A tag the target carries, with how it got there.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tagging {
    pub tag: Tag,
    pub source: String,
    pub confidence: f64,
}

/// Find the tag `(kind, value, parent)`, or create it; its id. `data` updates it when given.
pub fn upsert_tag(
    conn: &Connection,
    kind: &str,
    value: &str,
    parent_id: Option<i64>,
    data: Option<&str>,
) -> Result<i64> {
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM tags WHERE kind = ?1 AND value = ?2 AND parent_id IS ?3",
            params![kind, value, parent_id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        if data.is_some() {
            conn.execute("UPDATE tags SET data = ?2 WHERE id = ?1", params![id, data])?;
        }
        return Ok(id);
    }
    conn.execute(
        "INSERT INTO tags(kind, value, parent_id, data) VALUES (?1, ?2, ?3, ?4)",
        params![kind, value, parent_id, data],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Every tag of a kind (or all), parents before children, for listing.
pub fn list_tags(conn: &Connection, kind: Option<&str>) -> Result<Vec<Tag>> {
    let mut st = conn.prepare(
        "SELECT id, kind, value, parent_id, data FROM tags
         WHERE (?1 IS NULL OR kind = ?1) ORDER BY kind, value",
    )?;
    let rows = st.query_map(params![kind], |r| {
        Ok(Tag {
            id: r.get(0)?,
            kind: r.get(1)?,
            value: r.get(2)?,
            parent_id: r.get(3)?,
            data: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Attach `tag_id` to a target (idempotent; re-attaching updates source/confidence).
pub fn attach(
    conn: &Connection,
    target_type: &str,
    target_key: &str,
    tag_id: i64,
    source: &str,
    confidence: f64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO taggings(target_type, target_key, tag_id, source, confidence, added_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(target_type, target_key, tag_id)
         DO UPDATE SET source = excluded.source, confidence = excluded.confidence,
                       added_at = excluded.added_at",
        params![target_type, target_key, tag_id, source, confidence, now()],
    )?;
    Ok(())
}

/// Remove one tag from a target.
pub fn detach(conn: &Connection, target_type: &str, target_key: &str, tag_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM taggings WHERE target_type = ?1 AND target_key = ?2 AND tag_id = ?3",
        params![target_type, target_key, tag_id],
    )?;
    Ok(())
}

/// Every tag a target carries.
pub fn tags_of(conn: &Connection, target_type: &str, target_key: &str) -> Result<Vec<Tagging>> {
    let mut st = conn.prepare(
        "SELECT t.id, t.kind, t.value, t.parent_id, t.data, g.source, g.confidence
         FROM taggings g JOIN tags t ON t.id = g.tag_id
         WHERE g.target_type = ?1 AND g.target_key = ?2
         ORDER BY t.kind, t.value",
    )?;
    let rows = st.query_map(params![target_type, target_key], |r| {
        Ok(Tagging {
            tag: Tag {
                id: r.get(0)?,
                kind: r.get(1)?,
                value: r.get(2)?,
                parent_id: r.get(3)?,
                data: r.get(4)?,
            },
            source: r.get(5)?,
            confidence: r.get(6)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Every target carrying a tag, as `(target_type, target_key)`.
pub fn targets_of_tag(conn: &Connection, tag_id: i64) -> Result<Vec<(String, String)>> {
    let mut st = conn.prepare(
        "SELECT target_type, target_key FROM taggings WHERE tag_id = ?1
         ORDER BY target_type, target_key",
    )?;
    let rows = st.query_map(params![tag_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// One tag by its id.
pub fn tag(conn: &Connection, id: i64) -> Result<Option<Tag>> {
    Ok(conn
        .query_row(
            "SELECT id, kind, value, parent_id, data FROM tags WHERE id = ?1",
            params![id],
            |r| {
                Ok(Tag {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    value: r.get(2)?,
                    parent_id: r.get(3)?,
                    data: r.get(4)?,
                })
            },
        )
        .optional()?)
}

/// A tag with how much carries it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagCount {
    #[serde(flatten)]
    pub tag: Tag,
    /// targets carrying it
    pub count: u64,
    /// some of those were tagged by hand (`source = 'user'`)
    pub by_hand: bool,
    /// all of them were: a tag of the person's own, theirs to rename or delete
    pub own: bool,
}

/// Every tag of a kind (or all but the per-photo `location` and `date` ones), with counts.
pub fn tag_counts(conn: &Connection, kind: Option<&str>) -> Result<Vec<TagCount>> {
    let mut st = conn.prepare(
        "SELECT t.id, t.kind, t.value, t.parent_id, t.data, COUNT(g.target_key),
                COALESCE(MAX(g.source = 'user'), 0), COALESCE(MIN(g.source = 'user'), 1)
         FROM tags t LEFT JOIN taggings g ON g.tag_id = t.id
         WHERE (?1 IS NULL AND t.kind NOT IN ('location', 'date')) OR t.kind = ?1
         GROUP BY t.id ORDER BY t.kind, t.value",
    )?;
    let rows = st.query_map(params![kind], |r| {
        Ok(TagCount {
            tag: Tag {
                id: r.get(0)?,
                kind: r.get(1)?,
                value: r.get(2)?,
                parent_id: r.get(3)?,
                data: r.get(4)?,
            },
            count: r.get::<_, i64>(5)?.max(0) as u64,
            by_hand: r.get::<_, i64>(6)? != 0,
            own: r.get::<_, i64>(7)? != 0,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Tags (but per-photo locations and dates) whose value has `text` in it, in any case, the most
/// carried first: what a search box suggests as someone types.
pub fn find_tags(conn: &Connection, text: &str, limit: usize) -> Result<Vec<TagCount>> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let like = format!(
        "%{}%",
        text.replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    );
    let mut st = conn.prepare_cached(
        "SELECT t.id, t.kind, t.value, t.parent_id, t.data,
                (SELECT COUNT(*) FROM taggings g WHERE g.tag_id = t.id) AS n,
                (SELECT COUNT(*) FROM taggings g WHERE g.tag_id = t.id AND g.source = 'user') AS u
         FROM tags t
         WHERE t.kind NOT IN ('location', 'date') AND t.value LIKE ?1 ESCAPE '\\'
         ORDER BY n DESC, t.value LIMIT ?2",
    )?;
    let rows = st.query_map(params![like, limit as i64], |r| {
        let n: i64 = r.get(5)?;
        let u: i64 = r.get(6)?;
        Ok(TagCount {
            tag: Tag {
                id: r.get(0)?,
                kind: r.get(1)?,
                value: r.get(2)?,
                parent_id: r.get(3)?,
                data: r.get(4)?,
            },
            count: n.max(0) as u64,
            by_hand: u > 0,
            own: u == n,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Give a tag a new value. When another tag of its kind under the same parent already has that
/// value, the two become one: everything carrying this tag carries that one, the tags under this
/// one move under it, and this one goes. The id the tag has afterwards.
pub fn rename_tag(conn: &Connection, id: i64, value: &str) -> Result<i64> {
    let value = value.trim();
    if value.is_empty() {
        anyhow::bail!("a tag needs a value");
    }
    let Some(t) = tag(conn, id)? else {
        anyhow::bail!("there's no tag {id}");
    };
    let tx = crate::db::Tx::join(conn, crate::db::Lock::Immediate)?;
    let other: Option<i64> = conn
        .query_row(
            "SELECT id FROM tags WHERE kind = ?1 AND value = ?2 AND parent_id IS ?3 AND id != ?4",
            params![t.kind, value, t.parent_id, id],
            |r| r.get(0),
        )
        .optional()?;
    let to = match other {
        None => {
            conn.execute(
                "UPDATE tags SET value = ?2 WHERE id = ?1",
                params![id, value],
            )?;
            id
        }
        Some(to) => {
            conn.execute(
                "INSERT OR IGNORE INTO taggings(target_type, target_key, tag_id, source, confidence, added_at)
                 SELECT target_type, target_key, ?2, source, confidence, added_at FROM taggings WHERE tag_id = ?1",
                params![id, to],
            )?;
            conn.execute("DELETE FROM taggings WHERE tag_id = ?1", params![id])?;
            merge_children(conn, id, Some(to))?;
            conn.execute("DELETE FROM tags WHERE id = ?1", params![id])?;
            to
        }
    };
    tx.commit()?;
    Ok(to)
}

/// Move the tags under `from` to under `to` (or to the top); one already there with the same
/// kind and value becomes one with it, the same way.
fn merge_children(conn: &Connection, from: i64, to: Option<i64>) -> Result<()> {
    let kids: Vec<(i64, String, String)> = conn
        .prepare("SELECT id, kind, value FROM tags WHERE parent_id = ?1")?
        .query_map(params![from], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (kid, kind, value) in kids {
        let twin: Option<i64> = conn
            .query_row(
                "SELECT id FROM tags WHERE kind = ?1 AND value = ?2 AND parent_id IS ?3 AND id != ?4",
                params![kind, value, to, kid],
                |r| r.get(0),
            )
            .optional()?;
        match twin {
            None => {
                conn.execute(
                    "UPDATE tags SET parent_id = ?2 WHERE id = ?1",
                    params![kid, to],
                )?;
            }
            Some(twin) => {
                conn.execute(
                    "INSERT OR IGNORE INTO taggings(target_type, target_key, tag_id, source, confidence, added_at)
                     SELECT target_type, target_key, ?2, source, confidence, added_at FROM taggings WHERE tag_id = ?1",
                    params![kid, twin],
                )?;
                conn.execute("DELETE FROM taggings WHERE tag_id = ?1", params![kid])?;
                merge_children(conn, kid, Some(twin))?;
                conn.execute("DELETE FROM tags WHERE id = ?1", params![kid])?;
            }
        }
    }
    Ok(())
}

/// Delete a tag: it comes off everything carrying it, and the tags under it move up to its
/// parent (or to the top).
pub fn delete_tag(conn: &Connection, id: i64) -> Result<()> {
    let Some(t) = tag(conn, id)? else {
        anyhow::bail!("there's no tag {id}");
    };
    let tx = crate::db::Tx::join(conn, crate::db::Lock::Immediate)?;
    conn.execute("DELETE FROM taggings WHERE tag_id = ?1", params![id])?;
    merge_children(conn, id, t.parent_id)?;
    conn.execute("DELETE FROM tags WHERE id = ?1", params![id])?;
    tx.commit()?;
    Ok(())
}

// --- flags ------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Flag {
    pub target_type: String,
    pub target_key: String,
    pub state: String,
    pub canonical_key: Option<String>,
    pub decided_by: String,
    pub decided_at: i64,
    pub note: Option<String>,
}

/// Set (or update) a flag on an item. `canonical_key` points a `duplicate` at its canonical copy.
pub fn set_flag(
    conn: &Connection,
    target_type: &str,
    target_key: &str,
    state: &str,
    canonical_key: Option<&str>,
    decided_by: &str,
    note: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO item_state(target_type, target_key, state, canonical_key, decided_by, decided_at, note)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(target_type, target_key, state)
         DO UPDATE SET canonical_key = excluded.canonical_key, decided_by = excluded.decided_by,
                       decided_at = excluded.decided_at, note = excluded.note",
        params![target_type, target_key, state, canonical_key, decided_by, now(), note],
    )?;
    Ok(())
}

/// Clear one flag from an item.
pub fn clear_flag(
    conn: &Connection,
    target_type: &str,
    target_key: &str,
    state: &str,
) -> Result<()> {
    conn.execute(
        "DELETE FROM item_state WHERE target_type = ?1 AND target_key = ?2 AND state = ?3",
        params![target_type, target_key, state],
    )?;
    Ok(())
}

/// Every flag on an item.
pub fn flags_of(conn: &Connection, target_type: &str, target_key: &str) -> Result<Vec<Flag>> {
    let mut st = conn.prepare(
        "SELECT target_type, target_key, state, canonical_key, decided_by, decided_at, note
         FROM item_state WHERE target_type = ?1 AND target_key = ?2 ORDER BY state",
    )?;
    let rows = st.query_map(params![target_type, target_key], |r| {
        Ok(Flag {
            target_type: r.get(0)?,
            target_key: r.get(1)?,
            state: r.get(2)?,
            canonical_key: r.get(3)?,
            decided_by: r.get(4)?,
            decided_at: r.get(5)?,
            note: r.get(6)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

// --- edges ------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub src_type: String,
    pub src_key: String,
    pub rel: String,
    pub dst_type: String,
    pub dst_key: String,
    pub data: Option<String>,
    pub at: i64,
}

/// Add (or update) a relationship edge.
pub fn add_edge(
    conn: &Connection,
    src_type: &str,
    src_key: &str,
    rel: &str,
    dst_type: &str,
    dst_key: &str,
    data: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO edges(src_type, src_key, rel, dst_type, dst_key, data, at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(src_type, src_key, rel, dst_type, dst_key)
         DO UPDATE SET data = excluded.data, at = excluded.at",
        params![src_type, src_key, rel, dst_type, dst_key, data, now()],
    )?;
    Ok(())
}

fn read_edge(r: &rusqlite::Row) -> rusqlite::Result<Edge> {
    Ok(Edge {
        src_type: r.get(0)?,
        src_key: r.get(1)?,
        rel: r.get(2)?,
        dst_type: r.get(3)?,
        dst_key: r.get(4)?,
        data: r.get(5)?,
        at: r.get(6)?,
    })
}

const EDGE_COLS: &str = "src_type, src_key, rel, dst_type, dst_key, data, at";

/// Edges out of a node.
pub fn edges_from(conn: &Connection, src_type: &str, src_key: &str) -> Result<Vec<Edge>> {
    let mut st = conn.prepare(&format!(
        "SELECT {EDGE_COLS} FROM edges WHERE src_type = ?1 AND src_key = ?2 ORDER BY rel, dst_type, dst_key"
    ))?;
    let rows = st.query_map(params![src_type, src_key], read_edge)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Edges into a node.
pub fn edges_to(conn: &Connection, dst_type: &str, dst_key: &str) -> Result<Vec<Edge>> {
    let mut st = conn.prepare(&format!(
        "SELECT {EDGE_COLS} FROM edges WHERE dst_type = ?1 AND dst_key = ?2 ORDER BY rel, src_type, src_key"
    ))?;
    let rows = st.query_map(params![dst_type, dst_key], read_edge)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

// --- content cache ----------------------------------------------------------

/// Remember a piece of content (size, and optionally what kind it is). Idempotent.
pub fn remember_content(
    conn: &Connection,
    sha256: &[u8],
    size: i64,
    kind: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO content(sha256, size, kind) VALUES (?1, ?2, ?3)
         ON CONFLICT(sha256) DO UPDATE SET size = excluded.size,
                       kind = COALESCE(excluded.kind, content.kind)",
        params![sha256, size, kind],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn
    }

    #[test]
    fn tag_hierarchy_and_attach() {
        let c = db();
        let country = upsert_tag(&c, "country", "Hong Kong", None, None).unwrap();
        let city = upsert_tag(&c, "city", "Hong Kong", Some(country), None).unwrap();
        // upsert is idempotent
        assert_eq!(
            upsert_tag(&c, "country", "Hong Kong", None, None).unwrap(),
            country
        );
        attach(&c, TARGET_CONTENT, "deadbeef", city, "user", 1.0).unwrap();
        let got = tags_of(&c, TARGET_CONTENT, "deadbeef").unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].tag.kind, "city");
        assert_eq!(got[0].tag.parent_id, Some(country));
        assert_eq!(
            targets_of_tag(&c, city).unwrap(),
            vec![(TARGET_CONTENT.into(), "deadbeef".into())]
        );
        detach(&c, TARGET_CONTENT, "deadbeef", city).unwrap();
        assert!(tags_of(&c, TARGET_CONTENT, "deadbeef").unwrap().is_empty());
    }

    #[test]
    fn flags_and_edges() {
        let c = db();
        set_flag(&c, TARGET_CONTENT, "h1", "canonical", None, "user", None).unwrap();
        set_flag(
            &c,
            TARGET_CONTENT,
            "h2",
            "duplicate",
            Some("h1"),
            "auto",
            None,
        )
        .unwrap();
        let f = flags_of(&c, TARGET_CONTENT, "h2").unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].state, "duplicate");
        assert_eq!(f[0].canonical_key.as_deref(), Some("h1"));
        clear_flag(&c, TARGET_CONTENT, "h2", "duplicate").unwrap();
        assert!(flags_of(&c, TARGET_CONTENT, "h2").unwrap().is_empty());

        add_edge(
            &c,
            TARGET_CONTENT,
            "h1",
            "taken_at",
            TARGET_ENTITY,
            "place:fcc",
            None,
        )
        .unwrap();
        assert_eq!(edges_from(&c, TARGET_CONTENT, "h1").unwrap().len(), 1);
        assert_eq!(edges_to(&c, TARGET_ENTITY, "place:fcc").unwrap().len(), 1);
    }

    #[test]
    fn rename_merges_and_delete_lifts_children() {
        let c = db();
        let trip = upsert_tag(&c, "event", "HK trip", None, None).unwrap();
        let day = upsert_tag(&c, "event", "Day 1", Some(trip), None).unwrap();
        let other = upsert_tag(&c, "event", "Hong Kong 2023", None, None).unwrap();
        let other_day = upsert_tag(&c, "event", "Day 1", Some(other), None).unwrap();
        attach(&c, TARGET_CONTENT, "p1", trip, "user", 1.0).unwrap();
        attach(&c, TARGET_CONTENT, "p2", day, "user", 1.0).unwrap();
        attach(&c, TARGET_CONTENT, "p3", other, "user", 1.0).unwrap();

        // a plain rename keeps the id
        assert_eq!(rename_tag(&c, trip, "HK trip 2023").unwrap(), trip);
        assert_eq!(tag(&c, trip).unwrap().unwrap().value, "HK trip 2023");
        // renaming onto an existing value merges the two, children and all
        assert_eq!(rename_tag(&c, trip, "Hong Kong 2023").unwrap(), other);
        assert!(tag(&c, trip).unwrap().is_none());
        assert!(
            tag(&c, day).unwrap().is_none(),
            "its Day 1 became the other's Day 1"
        );
        let mut on: Vec<_> = targets_of_tag(&c, other)
            .unwrap()
            .into_iter()
            .map(|t| t.1)
            .collect();
        on.sort();
        assert_eq!(on, ["p1", "p3"]);
        assert_eq!(targets_of_tag(&c, other_day).unwrap().len(), 1);

        let counts = tag_counts(&c, Some("event")).unwrap();
        let hk = counts.iter().find(|t| t.tag.id == other).unwrap();
        assert_eq!((hk.count, hk.by_hand, hk.own), (2, true, true));

        // deleting the parent lifts Day 1 to the top and takes the tag off everything
        delete_tag(&c, other).unwrap();
        assert!(tag(&c, other).unwrap().is_none());
        assert_eq!(tag(&c, other_day).unwrap().unwrap().parent_id, None);
        assert!(tags_of(&c, TARGET_CONTENT, "p1").unwrap().is_empty());
        assert!(rename_tag(&c, other, "x").is_err());
    }

    #[test]
    fn find_tags_matches_values_most_carried_first() {
        let c = db();
        let hk = upsert_tag(&c, "country", "Hong Kong", None, None).unwrap();
        let ev = upsert_tag(&c, "event", "HK 100% trip_1", None, None).unwrap();
        let loc = upsert_tag(&c, "location", "22.3,114.1", None, None).unwrap();
        for k in ["a", "b"] {
            attach(&c, TARGET_CONTENT, k, hk, "geo", 1.0).unwrap();
        }
        attach(&c, TARGET_CONTENT, "a", ev, "user", 1.0).unwrap();
        attach(&c, TARGET_CONTENT, "a", loc, "exif", 1.0).unwrap();
        let got = find_tags(&c, "hong", 5).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].tag.id, got[0].count, got[0].own), (hk, 2, false));
        // LIKE's wildcards are only characters here
        assert_eq!(find_tags(&c, "100%", 5).unwrap()[0].tag.id, ev);
        assert!(find_tags(&c, "1_%", 5).unwrap().is_empty());
        assert!(find_tags(&c, "22.3", 5).unwrap().is_empty(), "no locations");
    }
}
