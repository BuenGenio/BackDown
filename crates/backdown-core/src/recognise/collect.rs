//! The passes over a drive's indexed names that collect those markers, once, for every rule.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::params;

use crate::{Db, fmt, names, paths};

use super::*;

/// The base of a dump's file name: "shop_2021-03-04.sql.gz" → "shop_2021-03-04".
pub fn dump_stem(name: &str) -> &str {
    let lower = name.to_ascii_lowercase();
    let cut = [".sql.gz", ".sql.bz2", ".sql.xz", ".sql.zst", ".sql", ".dump", ".pgdump"]
        .iter()
        .find(|s| lower.ends_with(*s))
        .map_or(0, |s| s.len());
    &name[..name.len() - cut]
}

/// A SQLite file by its name: certain ones, and ".db", which many programs use for other things.
fn sqlite_name(lower: &str) -> Option<bool> {
    if lower.ends_with(".sqlite") || lower.ends_with(".sqlite3") || lower.ends_with(".db3") {
        Some(true)
    } else if lower.ends_with(".db")
        && !matches!(
            lower,
            "thumbs.db" | "manifest.db" | "shared-mailboxes.db" | "desktop.db"
        )
    {
        Some(false)
    } else {
        None
    }
}

/// A MySQL or MariaDB table file: .frm (the definition, up to MySQL 5.7 and in MariaDB) or the data
/// (InnoDB .ibd, MyISAM .MYD, Aria .MAD).
fn table_file(lower: &str) -> Option<usize> {
    match ext(lower).as_str() {
        "frm" => Some(0),
        "ibd" | "myd" | "mad" => Some(1),
        _ => None,
    }
}

// ---------- what the passes collect

#[derive(Default)]
pub(super) struct Markers {
    /// parent folder -> lower-case names of marker folders directly in it
    pub(super) dirs: HashMap<Vec<u8>, HashSet<String>>,
    /// (parent folder, lower-case name) -> the folder's real name, where they differ: paths are
    /// built from the real one ("System Volume Information", not "system volume information")
    pub(super) dir_case: HashMap<(Vec<u8>, String), String>,
    /// folder -> lower-case names of marker files directly in it
    pub(super) files: HashMap<Vec<u8>, HashSet<String>>,
    /// folder -> [audio, video, image] files directly in it, and their bytes
    pub(super) media: HashMap<Vec<u8>, [(u64, u64); 3]>,
    /// folder -> (files directly in it, of those: installers/archives)
    pub(super) downloads: HashMap<Vec<u8>, (u64, u64)>,
    pub(super) disk_images: Vec<(Vec<u8>, u64)>,
    pub(super) sql_dumps: Vec<(Vec<u8>, u64)>,
    /// small scripts and crontabs, for provenance
    pub(super) scripts: Vec<Vec<u8>>,
    /// possible ddrescue mapfiles (small .log/.map/.mapfile next to images)
    pub(super) mapfiles: Vec<Vec<u8>>,
    /// folder -> Dovecot dbox files directly in it: (mdbox "m.N", sdbox "u.N")
    pub(super) dbox: HashMap<Vec<u8>, (u64, u64)>,
    /// folder -> mongodump output directly in it: (.bson files, their bytes, .metadata.json files)
    pub(super) bson: HashMap<Vec<u8>, (u64, u64, u64)>,
    /// SQLite files by name: (path, size, certain by its extension)
    pub(super) sqlite: Vec<(Vec<u8>, u64, bool)>,
    /// folder -> MySQL/MariaDB table files directly in it: (.frm, data files)
    pub(super) tables: HashMap<Vec<u8>, (u64, u64)>,
}

/// "m.12" / "u.7": a Dovecot dbox storage file's name (0 for m, 1 for u).
pub(super) fn dbox_file(lower: &str) -> Option<usize> {
    let (kind, digits) = match lower.as_bytes() {
        [b'm', b'.', rest @ ..] => (0, rest),
        [b'u', b'.', rest @ ..] => (1, rest),
        _ => return None,
    };
    (!digits.is_empty() && digits.iter().all(u8::is_ascii_digit)).then_some(kind)
}

impl Markers {
    pub(super) fn has_dir(&self, dir: &[u8], name: &str) -> bool {
        self.dirs.get(dir).is_some_and(|s| s.contains(name))
    }
    pub(super) fn has_file(&self, dir: &[u8], name: &str) -> bool {
        self.files.get(dir).is_some_and(|s| s.contains(name))
    }
    pub(super) fn dir_names(&self, dir: &[u8]) -> HashSet<String> {
        self.dirs.get(dir).cloned().unwrap_or_default()
    }
}

pub(crate) fn join(dir: &[u8], name: &str) -> Vec<u8> {
    paths::to_bytes(&paths::from_bytes(dir).join(name))
}

/// Folder rows directly inside `dir` (current), by name.
pub(crate) fn children(db: &Db, dir: &[u8]) -> Result<Vec<String>> {
    let sep = sep();
    let mut stmt = db
        .conn
        .prepare_cached("SELECT path FROM dirs WHERE parent = ?1 AND current = 1")?;
    let rows = stmt.query_map(params![dir], |r| r.get::<_, Vec<u8>>(0))?;
    let mut out = Vec::new();
    for p in rows {
        if let Some(n) = child_name(dir, &p?, &sep) {
            out.push(n);
        }
    }
    out.sort();
    Ok(out)
}

pub(super) fn collect(db: &Db, root_id: i64, progress: &mut dyn FnMut(&str)) -> Result<Markers> {
    let sep = sep();
    let mut m = Markers::default();
    progress("reading folders");
    {
        let mut stmt = db
            .conn
            .prepare("SELECT path, parent FROM dirs WHERE root_id = ?1 AND current = 1 AND parent IS NOT NULL")?;
        let rows = stmt.query_map(params![root_id], |r| {
            Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        // folder names added as junk in Settings are markers too
        let extra = EXTRA_JUNK.read().unwrap_or_else(|e| e.into_inner()).clone();
        for row in rows {
            let (path, parent) = row?;
            let Some(name) = child_name(&parent, &path, &sep) else {
                continue;
            };
            let lower = names::nfc(&name).to_lowercase();
            if dir_marker(&lower) || extra.contains(&lower) {
                if lower != name {
                    m.dir_case.insert((parent.clone(), lower.clone()), name);
                }
                m.dirs.entry(parent).or_default().insert(lower);
            }
        }
    }
    progress("reading files");
    let mut stmt = db
        .conn
        .prepare("SELECT dir, path, size FROM files WHERE root_id = ?1 AND current = 1")?;
    let rows = stmt.query_map(params![root_id], |r| {
        Ok((
            r.get::<_, Option<Vec<u8>>>(0)?,
            r.get::<_, Vec<u8>>(1)?,
            r.get::<_, i64>(2)? as u64,
        ))
    })?;
    let mut n = 0u64;
    for row in rows {
        let (dir, path, size) = row?;
        n += 1;
        if n.is_multiple_of(500_000) {
            progress(&format!("reading files: {}", fmt::count(n)));
        }
        let Some(dir) = dir else { continue };
        let Some(name) = child_name(&dir, &path, &sep) else {
            continue;
        };
        let lower = names::nfc(&name).to_lowercase();
        let e = ext(&lower);
        if file_marker(&lower) {
            m.files
                .entry(dir.clone())
                .or_default()
                .insert(lower.clone());
        }
        if let Some(k) = dbox_file(&lower) {
            let c = m.dbox.entry(dir.clone()).or_default();
            if k == 0 {
                c.0 += 1;
            } else {
                c.1 += 1;
            }
        }
        let kind = if AUDIO.contains(&e.as_str()) {
            Some(0)
        } else if VIDEO.contains(&e.as_str()) {
            Some(1)
        } else if IMAGE.contains(&e.as_str()) {
            Some(2)
        } else {
            None
        };
        if let Some(k) = kind {
            let e = &mut m.media.entry(dir.clone()).or_default()[k];
            e.0 += 1;
            e.1 += size;
        }
        let dl = m.downloads.entry(dir.clone()).or_default();
        dl.0 += 1;
        if INSTALLER.contains(&e.as_str()) {
            dl.1 += 1;
        }
        if DISK_IMAGE.contains(&e.as_str()) && size >= 1 << 20 {
            m.disk_images.push((path.clone(), size));
        }
        if is_sql_dump(&lower) && size >= 1024 {
            m.sql_dumps.push((path.clone(), size));
        }
        if lower.ends_with(".bson") || lower.ends_with(".bson.gz") {
            let b = m.bson.entry(dir.clone()).or_default();
            b.0 += 1;
            b.1 += size;
        } else if lower.ends_with(".metadata.json") || lower.ends_with(".metadata.json.gz") {
            m.bson.entry(dir.clone()).or_default().2 += 1;
        }
        if let Some(certain) = sqlite_name(&lower) {
            // a .db needs some substance (or, with the drive here, its magic bytes) to count
            if size >= 4096 && (certain || size >= 1 << 20) {
                m.sqlite.push((path.clone(), size, certain));
            }
        }
        if let Some(k) = table_file(&lower) {
            let t = m.tables.entry(dir.clone()).or_default();
            if k == 0 {
                t.0 += 1;
            } else {
                t.1 += 1;
            }
        }
        if (SCRIPT.contains(&e.as_str()) || lower == "crontab" || lower.starts_with("crontab"))
            && size <= 256 * 1024
        {
            m.scripts.push(path.clone());
        }
        if (e == "log" || e == "map" || e == "mapfile") && size <= 4 << 20 {
            m.mapfiles.push(path);
        }
    }
    // the downloads tally only matters for folders that could be a downloads pile
    m.downloads.retain(|dir, (files, _)| {
        *files >= 20 && {
            let name = paths::from_bytes(dir)
                .file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            matches!(
                name.as_str(),
                "downloads"
                    | "download"
                    | "incoming"
                    | "téléchargements"
                    | "descargas"
                    | "downloads (old)"
            )
        }
    });
    Ok(m)
}
