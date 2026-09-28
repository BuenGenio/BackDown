//! How much of each place already exists somewhere else, by content (APP-PLAN §3 "Overlap").
//!
//! Pairwise duplicate lists drown in noise on real drives (one Nextcloud data folder that is a
//! superset of everything else produces millions of pairs). The useful question is containment:
//! "what fraction of this drive / folder / mail store has an identical copy outside it, and where
//! are those copies?". That's answered here for every drive, every top-level folder and every
//! recognised entity in one pass over the files whose SHA-256 occurs more than once. A hardlink is
//! the same data, so it never counts as a copy; files that were never hashed (their size is unique,
//! so nothing can equal them) count as not found elsewhere.

use std::collections::HashMap;
use std::path::{MAIN_SEPARATOR_STR, Path};

use anyhow::Result;
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::recognise::RootEntities;
use crate::{Db, browse, paths};

/// Something overlaps are measured for.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Unit {
    /// "drive" | "folder" | "entity"
    pub level: String,
    pub label: String,
    pub path: String,
    pub key: String,
    pub files: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Container {
    pub label: String,
    pub path: String,
    pub key: String,
    /// bytes of the unit that have a copy in this container
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Overlap {
    pub unit: Unit,
    /// bytes with at least one identical copy outside the unit
    pub elsewhere: u64,
    pub elsewhere_files: u64,
    /// where those copies are, most first (drives for a drive; the most specific place otherwise)
    pub containers: Vec<Container>,
}

impl Overlap {
    /// share of the unit's bytes that exists elsewhere, 0..1
    pub fn share(&self) -> f64 {
        if self.unit.bytes == 0 {
            0.0
        } else {
            self.elsewhere as f64 / self.unit.bytes as f64
        }
    }
}

struct Units {
    list: Vec<Unit>,
    /// root id -> the drive's unit
    drive: HashMap<i64, usize>,
    /// (root id, first component below the root) -> the top-level folder's unit
    folder: HashMap<(i64, Vec<u8>), usize>,
    root_prefix: HashMap<i64, Vec<u8>>,
    /// entity folders: (prefix ending in a separator, unit), sorted by prefix
    entities: Vec<(Vec<u8>, usize)>,
    /// for each entry of `entities`, the index of the closest entry containing it
    entity_parent: Vec<Option<usize>>,
}

fn units(db: &Db, recognised: &[RootEntities]) -> Result<Units> {
    let sep = paths::to_bytes(Path::new(MAIN_SEPARATOR_STR));
    let mut u = Units {
        list: Vec::new(),
        drive: HashMap::new(),
        folder: HashMap::new(),
        root_prefix: HashMap::new(),
        entities: Vec::new(),
        entity_parent: Vec::new(),
    };
    let mut roots = db.conn.prepare(
        "SELECT r.id, r.path, COALESCE(s.files, 0), COALESCE(s.bytes, 0) FROM roots r JOIN scans s ON s.id = r.last_scan ORDER BY r.path",
    )?;
    let rows = roots.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, Vec<u8>>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
        ))
    })?;
    let mut tops = db
        .conn
        .prepare("SELECT path, files, bytes FROM dirs WHERE parent = ?1 AND current = 1")?;
    for row in rows {
        let (id, path, files, bytes) = row?;
        let p = paths::from_bytes(&path);
        u.drive.insert(id, u.list.len());
        u.list.push(Unit {
            level: "drive".into(),
            label: p.file_name().map_or_else(
                || p.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            ),
            path: p.display().to_string(),
            key: browse::key(&p),
            files: files as u64,
            bytes: bytes as u64,
        });
        let prefix = paths::dir_prefix(&p);
        for t in tops.query_map(params![path], |r| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })? {
            let (tp, tf, tb) = t?;
            let Some(name) = tp.strip_prefix(&prefix[..]) else {
                continue;
            };
            let q = paths::from_bytes(&tp);
            u.folder.insert((id, name.to_vec()), u.list.len());
            u.list.push(Unit {
                level: "folder".into(),
                label: q
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                path: q.display().to_string(),
                key: browse::key(&q),
                files: tf as u64,
                bytes: tb as u64,
            });
        }
        u.root_prefix.insert(id, prefix);
    }
    let _ = sep;
    for r in recognised {
        for e in r.entities.iter().filter(|e| !e.is_file) {
            u.entities
                .push((paths::dir_prefix(Path::new(&e.path)), u.list.len()));
            u.list.push(Unit {
                level: "entity".into(),
                label: format!("{}: {}", e.label, e.title),
                path: e.path.clone(),
                key: e.key.clone(),
                files: e.files,
                bytes: e.bytes,
            });
        }
    }
    u.entities.sort();
    u.entity_parent = (0..u.entities.len())
        .map(|i| {
            (0..i)
                .rev()
                .find(|&j| u.entities[i].0.starts_with(&u.entities[j].0))
        })
        .collect();
    Ok(u)
}

impl Units {
    /// Every unit holding the file: its drive, its top-level folder, and the entities around it,
    /// most specific last.
    fn of(&self, root: i64, path: &[u8]) -> Vec<usize> {
        let mut out = Vec::with_capacity(4);
        if let Some(&d) = self.drive.get(&root) {
            out.push(d);
        }
        if let Some(prefix) = self.root_prefix.get(&root)
            && let Some(rest) = path.strip_prefix(&prefix[..])
        {
            let sep = paths::to_bytes(Path::new(MAIN_SEPARATOR_STR));
            if let Some(i) = rest.windows(sep.len()).position(|w| w == &sep[..])
                && let Some(&f) = self.folder.get(&(root, rest[..i].to_vec()))
            {
                out.push(f);
            }
        }
        // the entity with the greatest prefix <= path, then its containers
        let at = self.entities.partition_point(|(p, _)| p.as_slice() <= path);
        let mut found = Vec::new();
        let mut i = at.checked_sub(1);
        while let Some(k) = i {
            if path.starts_with(&self.entities[k].0) {
                found.push(self.entities[k].1);
                i = self.entity_parent[k];
            } else {
                // not inside this one: maybe inside one of its containers
                i = self.entity_parent[k];
            }
        }
        found.reverse();
        out.extend(found);
        out
    }
}

/// Overlaps for every drive, top-level folder and recognised entity (pass what `recognise` stored).
pub fn overlaps(
    db: &Db,
    recognised: &[RootEntities],
    progress: &mut dyn FnMut(&str),
) -> Result<Vec<Overlap>> {
    let u = units(db, recognised)?;
    let n = u.list.len();
    let mut elsewhere = vec![0u64; n];
    let mut elsewhere_files = vec![0u64; n];
    let mut containers: Vec<HashMap<usize, u64>> = vec![HashMap::new(); n];

    let mut stmt = db.conn.prepare(
        "SELECT sha256, root_id, path, size, dev, ino FROM files
         WHERE current = 1 AND error IS NULL AND sha256 IN (
             SELECT sha256 FROM files WHERE current = 1 AND sha256 IS NOT NULL AND error IS NULL GROUP BY sha256 HAVING COUNT(*) > 1)
         ORDER BY sha256",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, Vec<u8>>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, Vec<u8>>(2)?,
            r.get::<_, i64>(3)? as u64,
            r.get::<_, Option<i64>>(4)?,
            r.get::<_, Option<i64>>(5)?,
        ))
    })?;
    struct Member {
        units: Vec<usize>,
        inode: Option<(i64, i64)>,
        size: u64,
    }
    let mut group: Vec<Member> = Vec::new();
    let mut current: Option<Vec<u8>> = None;
    let mut seen = 0u64;
    let mut flush = |group: &mut Vec<Member>| {
        for (i, f) in group.iter().enumerate() {
            for &unit in &f.units {
                // copies of f outside `unit` that aren't f itself under another name
                let mut outside = group.iter().enumerate().filter(|(j, g)| {
                    *j != i
                        && !g.units.contains(&unit)
                        && !(f.inode.is_some() && f.inode == g.inode)
                });
                let Some(first) = outside.next() else {
                    continue;
                };
                elsewhere[unit] += f.size;
                elsewhere_files[unit] += 1;
                let drive_level = u.list[unit].level == "drive";
                let mut places: Vec<usize> = std::iter::once(first)
                    .chain(outside)
                    .map(|(_, g)| {
                        if drive_level {
                            g.units[0]
                        } else {
                            *g.units.last().expect("a file has a drive")
                        }
                    })
                    .collect();
                places.sort_unstable();
                places.dedup();
                for p in places {
                    *containers[unit].entry(p).or_default() += f.size;
                }
            }
        }
        group.clear();
    };
    for row in rows {
        let (sha, root, path, size, dev, ino) = row?;
        seen += 1;
        if seen.is_multiple_of(500_000) {
            progress(&format!("comparing: {seen} files with copies"));
        }
        if current.as_deref() != Some(&sha[..]) {
            flush(&mut group);
            current = Some(sha);
        }
        group.push(Member {
            units: u.of(root, &path),
            inode: dev.zip(ino),
            size,
        });
    }
    flush(&mut group);

    Ok(u.list
        .iter()
        .enumerate()
        .map(|(i, unit)| {
            let mut c: Vec<Container> = containers[i]
                .iter()
                .map(|(&k, &bytes)| Container {
                    label: u.list[k].label.clone(),
                    path: u.list[k].path.clone(),
                    key: u.list[k].key.clone(),
                    bytes,
                })
                .collect();
            c.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.path.cmp(&b.path)));
            c.truncate(5);
            Overlap {
                unit: unit.clone(),
                elsewhere: elsewhere[i],
                elsewhere_files: elsewhere_files[i],
                containers: c,
            }
        })
        .collect())
}
