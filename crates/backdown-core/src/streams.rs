//! NTFS alternate data streams: named contents a file has beside its main one. Most are marks
//! ("downloaded from the internet": `Zone.Identifier`), but some programs keep real data in them,
//! and a copy that only takes the main stream loses them silently.
//!
//! On Windows the indexer notes, for each new or changed file (and for every file on a drive's
//! first scan since BackDown started looking), how many alternate streams it has, their size and
//! names (table `streams`, by file version; only files that have some). The browser shows them,
//! and copies (`extras`) carry them over or say which weren't. Elsewhere nothing is noted: other
//! systems read NTFS drives without them.

use std::io;
use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::Db;

/// A file's alternate streams: how many, their bytes, and each one's name and size.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Streams {
    pub count: u64,
    pub bytes: u64,
    pub names: Vec<(String, u64)>,
}

/// Marks Windows adds to downloads ("downloaded from the internet", SmartScreen's verdict):
/// copies leave them behind without a warning, but still list them.
pub fn quiet(name: &str) -> bool {
    name.eq_ignore_ascii_case("Zone.Identifier") || name.eq_ignore_ascii_case("SmartScreen")
}

/// A stream's own name from Windows' `:name:$DATA`; `None` for the main stream (`::$DATA`) and
/// anything that isn't data.
pub fn stream_name(raw: &str) -> Option<&str> {
    let rest = raw.strip_prefix(':')?;
    let (name, kind) = rest.rsplit_once(':')?;
    (!name.is_empty() && kind.eq_ignore_ascii_case("$DATA")).then_some(name)
}

/// The alternate streams among everything Windows lists for a file.
pub fn summarize(listed: impl IntoIterator<Item = (String, u64)>) -> Streams {
    let mut s = Streams::default();
    for (raw, size) in listed {
        if let Some(name) = stream_name(&raw) {
            s.count += 1;
            s.bytes += size;
            s.names.push((name.to_string(), size));
        }
    }
    s
}

/// `file:name`, the path Windows opens one stream of a file by.
pub fn stream_path(file: &Path, name: &str) -> PathBuf {
    let mut s = crate::paths::os(file).into_owned().into_os_string();
    s.push(":");
    s.push(name);
    PathBuf::from(s)
}

/// A file's alternate streams (none outside Windows).
#[cfg(windows)]
pub fn list(path: &Path) -> io::Result<Streams> {
    Ok(summarize(crate::win::streams(path)?))
}

#[cfg(not(windows))]
pub fn list(_path: &Path) -> io::Result<Streams> {
    Ok(Streams::default())
}

/// What's noted for one version of a file, if it has alternate streams.
pub fn get(db: &Db, file_id: i64) -> Option<Streams> {
    let row: Option<(i64, i64, String)> = db
        .conn
        .query_row(
            "SELECT count, bytes, names FROM streams WHERE file_id = ?1",
            params![file_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .ok()
        .flatten();
    let (count, bytes, names) = row?;
    Some(Streams {
        count: count as u64,
        bytes: bytes as u64,
        names: serde_json::from_str(&names).unwrap_or_default(),
    })
}

type Lister = fn(&Path) -> io::Result<Streams>;

/// The indexer's side: notes the streams of files as it records them.
pub(crate) struct Recorder<'c> {
    conn: &'c Connection,
    root_id: i64,
    lister: Lister,
    on: bool,
    /// look at every file, not just new and changed ones: this drive's first scan with streams
    all: bool,
}

impl<'c> Recorder<'c> {
    /// For a scan of `root_id`: on Windows only.
    pub(crate) fn new(conn: &'c Connection, root_id: i64) -> Result<Recorder<'c>> {
        Recorder::with(conn, root_id, list, cfg!(windows))
    }

    pub(crate) fn with(
        conn: &'c Connection,
        root_id: i64,
        lister: Lister,
        on: bool,
    ) -> Result<Recorder<'c>> {
        let all = on
            && conn
                .query_row(
                    "SELECT 1 FROM streams_roots WHERE root_id = ?1",
                    params![root_id],
                    |r| r.get::<_, i64>(0),
                )
                .optional()?
                .is_none();
        Ok(Recorder {
            conn,
            root_id,
            lister,
            on,
            all,
        })
    }

    /// The file row `file_id` was just written for `path`: `fresh` when it's a new version (new or
    /// changed file), not an unchanged one carried over.
    pub(crate) fn note(&mut self, file_id: i64, path: &Path, fresh: bool) -> Result<()> {
        if !self.on || !(fresh || self.all) {
            return Ok(());
        }
        let Ok(s) = (self.lister)(path) else {
            return Ok(()); // unreadable: the file's own read will say so
        };
        if s.count == 0 {
            if !fresh {
                self.conn
                    .prepare_cached("DELETE FROM streams WHERE file_id = ?1")?
                    .execute(params![file_id])?;
            }
            return Ok(());
        }
        self.conn
            .prepare_cached(
                "INSERT OR REPLACE INTO streams(file_id, count, bytes, names) VALUES (?1, ?2, ?3, ?4)",
            )?
            .execute(params![
                file_id,
                s.count as i64,
                s.bytes as i64,
                serde_json::to_string(&s.names)?
            ])?;
        Ok(())
    }

    /// The scan finished: every file on this drive has been looked at once.
    pub(crate) fn finish(&self) -> Result<()> {
        if self.on && self.all {
            self.conn.execute(
                "INSERT OR IGNORE INTO streams_roots(root_id) VALUES (?1)",
                params![self.root_id],
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_as_windows_lists_them() {
        assert_eq!(stream_name("::$DATA"), None);
        assert_eq!(
            stream_name(":Zone.Identifier:$DATA"),
            Some("Zone.Identifier")
        );
        assert_eq!(
            stream_name(":notes:with:colons:$DATA"),
            Some("notes:with:colons")
        );
        assert_eq!(stream_name(":x:$INDEX_ALLOCATION"), None);
        let s = summarize([
            ("::$DATA".to_string(), 1000),
            (":Zone.Identifier:$DATA".to_string(), 26),
            (":thumbnail:$DATA".to_string(), 4096),
        ]);
        assert_eq!((s.count, s.bytes), (2, 4122));
        assert_eq!(s.names[1], ("thumbnail".to_string(), 4096));
        assert!(quiet("zone.identifier") && quiet("SmartScreen") && !quiet("thumbnail"));
        #[cfg(windows)]
        assert!(
            stream_path(Path::new(r"C:\x\a.txt"), "Zone.Identifier")
                .to_string_lossy()
                .ends_with(r"\x\a.txt:Zone.Identifier")
        );
    }

    fn fake(p: &Path) -> io::Result<Streams> {
        let n = p.to_string_lossy();
        if n.contains("unreadable") {
            return Err(io::Error::other("denied"));
        }
        Ok(if n.contains("tagged") {
            summarize([
                ("::$DATA".to_string(), 10),
                (":Zone.Identifier:$DATA".to_string(), 26),
            ])
        } else {
            Streams::default()
        })
    }

    #[test]
    fn the_indexer_notes_new_files_and_backfills_once() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("i.db")).unwrap();
        let c = &db.conn;
        // first scan of root 1 since streams were noted: every file is looked at
        let mut r = Recorder::with(c, 1, fake, true).unwrap();
        r.note(10, Path::new("/d/old-tagged.txt"), false).unwrap();
        r.note(11, Path::new("/d/plain.txt"), false).unwrap();
        r.note(12, Path::new("/d/unreadable-tagged"), true).unwrap();
        r.finish().unwrap();
        assert_eq!(get(&db, 10).unwrap().count, 1);
        assert_eq!(get(&db, 10).unwrap().names[0].0, "Zone.Identifier");
        assert!(get(&db, 11).is_none() && get(&db, 12).is_none());
        // the next scan only looks at new versions
        let mut r = Recorder::with(c, 1, fake, true).unwrap();
        r.note(11, Path::new("/d/plain-now-tagged.txt"), false)
            .unwrap();
        r.note(13, Path::new("/d/new-tagged.txt"), true).unwrap();
        r.finish().unwrap();
        assert!(
            get(&db, 11).is_none(),
            "unchanged files aren't looked at again"
        );
        assert_eq!(get(&db, 13).unwrap().bytes, 26);
        // another drive still gets its first full look; off (not Windows): nothing at all
        let mut r = Recorder::with(c, 2, fake, true).unwrap();
        r.note(20, Path::new("/e/tagged"), false).unwrap();
        assert!(get(&db, 20).is_some());
        let mut r = Recorder::with(c, 3, fake, false).unwrap();
        r.note(30, Path::new("/f/tagged"), true).unwrap();
        r.finish().unwrap();
        assert!(get(&db, 30).is_none());
    }
}
