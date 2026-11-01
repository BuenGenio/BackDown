//! The receiving end of copying into the pool (docs/SWARM.md §5): what the machine whose pool
//! folder a file goes into does with it. `backdown swarm-stdio` runs this for another machine (over
//! ssh, as the person whose keys reach it); a copy into a pool folder on the sending machine itself
//! runs the very same code in-process.
//!
//! Whoever asks, a file is only ever written inside one of this machine's own pool folders, at a
//! relative place made of plain names (no `..`, no absolute paths, no control characters), never
//! through a link out of the folder, never over anything: it's written to a temporary name beside
//! its place, flushed, given its time, and renamed into place only if nothing is there; a different
//! file already there keeps its name and this one gets a numbered one; the same content already
//! there is left as it is. Then it's read back from the disk and hashed: only a match counts, and
//! the hash read back is what's answered (the sender compares it with the one it computed while
//! reading the original). A pool folder's reserve is kept: a file that would eat into it is refused.
//!
//! Each file received is remembered (`pool_received`: where, its size, time, SHA-256 and sample,
//! and which machine sent it) and is an event in the chain of custody, so when the pool folder is
//! next indexed its hashes are taken from there (`adopt`) instead of reading it all again.

use std::fs::{self, File, FileTimes};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use rusqlite::params;
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::Area;
use crate::custody::{self, Action, Event};
use crate::{Db, fmt, hash, names, paths, volumes};

pub const SCHEMA: &str = "
-- files written into this machine's pool folders (pool/receive.rs): their hashes are taken from
-- here when the folder is next indexed, so nothing is read twice. Added without a schema bump
CREATE TABLE IF NOT EXISTS pool_received (
    path    BLOB PRIMARY KEY,         -- where it was written
    size    INTEGER NOT NULL,
    mtime   INTEGER NOT NULL,
    sha256  BLOB NOT NULL,            -- as read back from the disk
    sample  BLOB,
    from_machine TEXT NOT NULL,       -- which machine sent it
    at      INTEGER NOT NULL
);
";

const BUF: usize = 1 << 20;

/// What became of one file.
#[derive(Debug, Clone, Serialize)]
pub struct Received {
    /// "copied", "renamed" (a different file was at its place: it's beside it under a numbered
    /// name), or "same" (that very content was there already)
    pub outcome: &'static str,
    /// where it is now, relative to the pool folder, with `/` between names
    pub path: String,
    pub sha256: String,
}

/// A place inside a pool folder, as sent: plain names separated by `/`, checked.
pub fn checked_rel(rel: &str) -> Result<PathBuf> {
    if rel.is_empty() || rel.len() > 4096 {
        bail!("a place in the pool is 1 to 4096 bytes");
    }
    let mut out = PathBuf::new();
    for part in rel.split('/') {
        let bad = part.is_empty()
            || part == "."
            || part == ".."
            || part.len() > 255
            || part.chars().any(|c| c.is_control())
            || part.contains('\\')
            || (cfg!(windows) && part.contains(':'));
        if bad {
            bail!("{rel:?} isn't a place in the pool (plain names separated by /)");
        }
        out.push(part);
    }
    Ok(out)
}

/// Read and drop whatever is left of `input` (so a stream stays in step after a refusal).
fn drain(input: &mut dyn Read) -> io::Result<u64> {
    io::copy(input, &mut io::sink())
}

/// Where a file comes from, for the record: the sending machine, the file's path there, and the
/// sender's pool plan.
#[derive(Debug, Clone)]
pub struct Origin<'a> {
    pub machine: &'a str,
    pub path: &'a str,
    pub plan: i64,
}

/// Write `size` bytes from `input` as `rel` in pool folder `area` (this machine's), keeping
/// `mtime`; see the module's notes. All `size` bytes are read from `input` whatever happens,
/// unless `input` itself fails.
pub fn put(
    db: &Db,
    area: &Area,
    rel: &str,
    size: u64,
    mtime: i64,
    input: &mut dyn Read,
    from: &Origin,
) -> Result<Received> {
    let mut limited = input.take(size);
    let r = write_in(db, area, rel, size, mtime, &mut limited, from);
    // a refusal before or during writing: the rest of the file's bytes are still read
    let left = drain(&mut limited).context("the rest of the file didn't come")?;
    if r.is_ok() && left > 0 {
        bail!("internal: {left} bytes left unread");
    }
    r
}

fn write_in(
    db: &Db,
    area: &Area,
    rel: &str,
    size: u64,
    mtime: i64,
    input: &mut dyn Read,
    from: &Origin,
) -> Result<Received> {
    let rel_path = checked_rel(rel)?;
    let root = PathBuf::from(&area.path);
    if !root.is_dir() {
        bail!("the pool folder {} isn't there", area.path);
    }
    let canon_root = dunce::canonicalize(&root)?;
    let dest = root.join(&rel_path);
    let parent = dest
        .parent()
        .context("a place without a folder")?
        .to_path_buf();
    fs::create_dir_all(&parent).with_context(|| format!("can't create {}", parent.display()))?;
    // never through a link that leads out of the pool folder
    let canon_parent = dunce::canonicalize(&parent)?;
    if !canon_parent.starts_with(&canon_root) {
        bail!("{rel} would land outside the pool folder (a link in it leads elsewhere)");
    }
    let dest = canon_parent.join(dest.file_name().context("a place without a name")?);
    if let Some(space) = volumes::space_at(&canon_parent)
        && space.free.saturating_sub(size) < area.reserve
    {
        bail!(
            "the pool folder {} keeps {} free on its drive, and has {} free: no room for {}",
            area.path,
            fmt::bytes(area.reserve),
            fmt::bytes(space.free),
            fmt::bytes(size)
        );
    }
    let name = dest
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let tmp = canon_parent.join(format!(".{name}.backdown-partial"));
    let (sum, file) = (|| -> Result<([u8; 32], File)> {
        let mut out =
            File::create(&tmp).with_context(|| format!("can't write {}", tmp.display()))?;
        let mut h = Sha256::new();
        let mut buf = vec![0u8; BUF];
        let mut got = 0u64;
        loop {
            let n = input.read(&mut buf).context("receiving the file")?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
            out.write_all(&buf[..n])
                .with_context(|| format!("writing {}", tmp.display()))?;
            got += n as u64;
        }
        if got != size {
            bail!("the file stopped after {got} of {size} bytes");
        }
        out.sync_all()?;
        let t = UNIX_EPOCH + Duration::from_secs(mtime.max(0) as u64);
        out.set_times(FileTimes::new().set_modified(t))?;
        crate::fastcopy::forget_cached(&out);
        Ok((h.finalize().into(), out))
    })()
    .inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })?;
    drop(file);
    // its place: free, the same content already, or something else (then a numbered name)
    let (final_path, outcome) = if fs::symlink_metadata(&dest).is_ok() {
        let same = fs::metadata(&dest).is_ok_and(|m| m.is_file() && m.len() == size)
            && crate::apply::hash_file(&dest).is_ok_and(|h| h == sum);
        if same {
            let _ = fs::remove_file(&tmp);
            return Ok(Received {
                outcome: "same",
                path: rel.to_string(),
                sha256: fmt::hex(&sum),
            });
        }
        (names::free_name(&dest), "renamed")
    } else {
        (dest.clone(), "copied")
    };
    if fs::symlink_metadata(&final_path).is_ok() {
        let _ = fs::remove_file(&tmp);
        bail!("{} appeared while receiving", final_path.display());
    }
    fs::rename(&tmp, &final_path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })?;
    // read back what the disk now holds
    let back = crate::apply::hash_file(&final_path).context("reading the copy back")?;
    if back != sum {
        let _ = fs::remove_file(&final_path); // our own bad copy, never anyone else's file
        bail!("the copy read back differently: removed (a failing disk or cable?)");
    }
    let sample = hash::sample_of(&final_path, size).ok().map(|(s, _)| s);
    let final_b = paths::to_bytes(&final_path);
    // in the chain of custody: copied from that machine's file, by its pool plan, to here
    let src = format!("{}:{}", from.machine, from.path);
    let ev = Event::new(
        Action::Copy,
        ("pool", from.plan),
        src.as_bytes(),
        Some(&sum),
        size,
    )
    .to(&final_b, Some(&back));
    custody::journaled(&db.conn, &ev, || {
        db.conn.execute(
            "INSERT INTO pool_received(path, size, mtime, sha256, sample, from_machine, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(path) DO UPDATE SET size = excluded.size, mtime = excluded.mtime,
               sha256 = excluded.sha256, sample = excluded.sample,
               from_machine = excluded.from_machine, at = excluded.at",
            params![
                final_b,
                size as i64,
                mtime,
                sum.to_vec(),
                sample,
                from.machine,
                crate::clock::now()
            ],
        )
    })?;
    let rel_final = final_path
        .strip_prefix(&canon_root)
        .map(|p| {
            p.components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/")
        })
        .unwrap_or_else(|_| rel.to_string());
    Ok(Received {
        outcome,
        path: rel_final,
        sha256: fmt::hex(&back),
    })
}

/// Take the hashes of the files received into this machine's pool folders for their rows in the
/// index (the same path, size and time, not hashed yet): what was verified on arrival isn't read
/// again. How many rows got one.
pub fn adopt(db: &Db) -> Result<usize> {
    let has: bool = db.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'pool_received')",
        [],
        |r| r.get(0),
    )?;
    if !has {
        return Ok(0);
    }
    Ok(db.conn.execute(
        "UPDATE files SET sha256 = r.sha256, sample = COALESCE(files.sample, r.sample)
         FROM pool_received r
         WHERE files.path = r.path AND files.current = 1 AND files.sha256 IS NULL
           AND files.size = r.size AND files.mtime = r.mtime AND files.error IS NULL",
        [],
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn places_are_plain_names() {
        assert_eq!(
            checked_rel("Media/Photos/a.jpg").unwrap(),
            Path::new("Media").join("Photos").join("a.jpg")
        );
        for bad in [
            "",
            "/etc/passwd",
            "a/../b",
            "a//b",
            "./a",
            "a/\u{7}b",
            "a\\b",
            "..",
        ] {
            assert!(checked_rel(bad).is_err(), "{bad:?}");
        }
    }
}
