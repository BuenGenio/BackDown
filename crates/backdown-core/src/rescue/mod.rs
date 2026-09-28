//! Rescue: getting data off failing, damaged or emptied media. Everything here only reads the
//! source (a drive, a partition, a memory card, or an image file of one) and writes somewhere else.
//!
//! - `image`: copy a failing drive or card into an image file, safely: the good areas first, the
//!   bad ones skipped and come back to later, with a map so it can stop and resume (like GNU
//!   ddrescue, whose mapfile format it reads and writes).
//! - `carve`: find files by their content, whatever the filesystem says: deleted photos,
//!   documents and videos on a card that was formatted or a drive whose filesystem is gone (like
//!   PhotoRec).
//! - `parts`: find partitions missing from the partition table, by their filesystems' own
//!   signatures (like TestDisk's search).
//!
//! Each takes a path (a device node or an image file), a progress callback and a stop flag, so the
//! daemon can run it as a job that reports how far it is and can be cancelled. Rescues are kept on
//! record (`rescues` table) so they can be followed, resumed and looked back on.

use std::fs::File;
use std::io::{self, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use serde_json::Value;

use crate::Db;

pub mod carve;
pub mod image;
pub mod parts;

/// The size of a file or a block device (a device's metadata says 0; seeking to its end doesn't).
pub fn size_of(f: &File) -> io::Result<u64> {
    let mut r = f;
    let n = r.seek(SeekFrom::End(0))?;
    r.seek(SeekFrom::Start(0))?;
    Ok(n)
}

/// Whether this process can read `path`, and if it can't, what would let it.
#[derive(Debug, Clone, Serialize)]
pub struct Access {
    pub path: String,
    pub readable: bool,
    /// a device node (a disk, a partition, a card reader) rather than an image file
    pub device: bool,
    pub size: Option<u64>,
    /// why it can't be read, for people
    pub why: Option<String>,
    /// one command that gives this user read access to that device only (until it's unplugged
    /// or the computer restarts), to paste into a terminal
    pub fix: Option<String>,
}

pub fn access(path: &Path) -> Access {
    let device = is_device(path);
    let mut a = Access {
        path: path.display().to_string(),
        readable: false,
        device,
        size: None,
        why: None,
        fix: None,
    };
    match File::open(path) {
        Ok(f) => match size_of(&f) {
            Ok(n) => {
                a.readable = true;
                a.size = Some(n);
            }
            Err(e) => a.why = Some(format!("it opened, but its size can't be read: {e}")),
        },
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            a.why = Some(if device {
                "reading a whole drive directly needs permission your user account doesn't have"
                    .into()
            } else {
                "your user account can't read that file".into()
            });
            a.fix = device.then(|| grant_command(path));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            a.why = Some("it isn't there: is it connected?".into());
        }
        Err(e) => a.why = Some(e.to_string()),
    }
    a
}

#[cfg(unix)]
fn is_device(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::metadata(path)
        .is_ok_and(|m| m.file_type().is_block_device() || m.file_type().is_char_device())
}

#[cfg(windows)]
fn is_device(path: &Path) -> bool {
    path.to_string_lossy().starts_with(r"\\.\")
}

/// The narrowest way to let this user read one device: an ACL on its node (Linux), or read
/// permission on it (macOS); both are gone when it's unplugged or the computer restarts.
fn grant_command(path: &Path) -> String {
    let dev = path.display();
    if cfg!(target_os = "linux") {
        let user = std::env::var("USER").unwrap_or_else(|_| "$USER".into());
        format!("sudo setfacl -m u:{user}:r {dev}")
    } else if cfg!(target_os = "macos") {
        format!("sudo chmod o+r {dev}")
    } else {
        "Run BackDown as administrator".into()
    }
}

// ---------- rescues on record

#[derive(Debug, Clone, Serialize)]
pub struct Rescue {
    pub id: i64,
    pub created_at: i64,
    pub updated_at: i64,
    /// image | carve | parts
    pub mode: String,
    pub source: String,
    pub dest: Option<String>,
    pub options: Value,
    /// new | running | done | stopped | failed
    pub state: String,
    pub summary: Option<String>,
    pub result: Value,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

pub fn record(
    db: &Db,
    mode: &str,
    source: &Path,
    dest: Option<&Path>,
    options: &Value,
) -> Result<i64> {
    db.conn.execute(
        "INSERT INTO rescues(created_at, mode, source, dest, options, state, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'new', ?1)",
        params![
            now(),
            mode,
            source.display().to_string(),
            dest.map(|d| d.display().to_string()),
            options.to_string()
        ],
    )?;
    Ok(db.conn.last_insert_rowid())
}

/// The latest state of rescue `id`: what it's doing or how it ended, and its result so far (a
/// summary or result left out keeps the one on record).
pub fn set(
    db: &Db,
    id: i64,
    state: &str,
    summary: Option<&str>,
    result: Option<&Value>,
) -> Result<()> {
    db.conn.execute(
        "UPDATE rescues SET state = ?2, summary = COALESCE(?3, summary),
         result = COALESCE(?4, result), updated_at = ?5 WHERE id = ?1",
        params![id, state, summary, result.map(Value::to_string), now()],
    )?;
    Ok(())
}

const COLUMNS: &str =
    "id, created_at, updated_at, mode, source, dest, options, state, summary, result";

fn row(r: &rusqlite::Row) -> rusqlite::Result<Rescue> {
    let json = |s: Option<String>| {
        s.and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(Value::Null)
    };
    Ok(Rescue {
        id: r.get(0)?,
        created_at: r.get(1)?,
        updated_at: r.get(2)?,
        mode: r.get(3)?,
        source: r.get(4)?,
        dest: r.get(5)?,
        options: json(r.get(6)?),
        state: r.get(7)?,
        summary: r.get(8)?,
        result: json(r.get(9)?),
    })
}

pub fn load(db: &Db, id: i64) -> Result<Rescue> {
    db.conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM rescues WHERE id = ?1"),
            params![id],
            row,
        )
        .optional()?
        .with_context(|| format!("there's no rescue {id}"))
}

/// Rescues on record, newest first.
pub fn history(db: &Db) -> Result<Vec<Rescue>> {
    let mut stmt = db.conn.prepare(&format!(
        "SELECT {COLUMNS} FROM rescues ORDER BY id DESC LIMIT 100"
    ))?;
    let rows = stmt.query_map([], row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Where an image's map goes: beside it, named the way ddrescue users expect (`card.img.map`).
pub fn map_path(image: &Path) -> PathBuf {
    let mut s = image.as_os_str().to_owned();
    s.push(".map");
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rescues_on_record() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("i.db")).unwrap();
        let id = record(
            &db,
            "image",
            Path::new("/dev/sdz"),
            Some(Path::new("/media/archive/card.img")),
            &serde_json::json!({ "retries": 1 }),
        )
        .unwrap();
        set(&db, id, "running", Some("copying"), None).unwrap();
        set(
            &db,
            id,
            "done",
            None,
            Some(&serde_json::json!({ "bad": 0 })),
        )
        .unwrap();
        let r = load(&db, id).unwrap();
        assert_eq!(
            (r.state.as_str(), r.summary.as_deref()),
            ("done", Some("copying"))
        );
        assert_eq!(r.result["bad"], 0);
        assert_eq!(r.options["retries"], 1);
        assert_eq!(history(&db).unwrap().len(), 1);
        assert_eq!(
            map_path(Path::new("/media/archive/card.img")),
            Path::new("/media/archive/card.img.map")
        );
    }

    #[test]
    fn an_image_file_is_readable_and_missing_ones_say_so() {
        let tmp = tempfile::tempdir().unwrap();
        let img = tmp.path().join("card.img");
        std::fs::write(&img, vec![0u8; 4096]).unwrap();
        let a = access(&img);
        assert!(a.readable && !a.device && a.size == Some(4096));
        let b = access(&tmp.path().join("nope.img"));
        assert!(!b.readable && b.why.as_deref().is_some_and(|w| w.contains("connected")));
    }
}
