//! Where a drive's things go in the pool (docs/SWARM.md §4): the filing plan's places (plan.rs,
//! with the pool as its archive), how much of each thing is in the pool already (by SHA-256, in
//! any machine's pool folder), and which pool folder each one goes into.
//!
//! What's "in the pool already" is decided file by file, by content, never by name: a file
//! whose size no pool file has is new for sure; one whose SHA-256 a pool file has (same size) is
//! there; one whose size matches but whose content can't be told yet (not hashed on one side, or
//! samples that match without a full hash) is "not compared yet" and counted as needing room. A
//! file whose content is planned already (the same photo twice on the drive) goes once: the pool
//! keeps one copy of everything.
//!
//! Each thing goes whole into one pool folder (docs/SWARM.md §4.3): where the same place already
//! is, if it fits; else a folder that takes its top-level place; else one that takes anything;
//! a folder on this machine before one elsewhere, then the one with most room; never past a
//! folder's reserve (folders on one file system share its room). What fits nowhere is listed.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::plan::{self, Item, Schema};
use crate::recognise::RootEntities;
use crate::remote::{self, Os};
use crate::{Db, fmt, paths, swarm};

/// Where a pool folder is: on which machine (this one: `machine_id` None), which folder there.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AreaRef {
    pub machine: String,
    pub machine_id: Option<i64>,
    pub area: i64,
    pub path: String,
}

impl AreaRef {
    pub fn here(&self) -> bool {
        self.machine_id.is_none()
    }
    /// "nextcloud:/srv/pool"
    pub fn label(&self) -> String {
        format!("{}:{}", self.machine, self.path)
    }
}

/// A pool folder as the planner sees it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Area {
    pub at: AreaRef,
    pub takes: Vec<String>,
    /// the file system it shares room with (its mount point there)
    pub fs: String,
    /// room left to fill on that file system: its free space less the biggest reserve there
    pub room: u64,
    pub indexed: bool,
    pub present: bool,
    /// its path there, as that machine spells paths, and how
    #[serde(skip)]
    raw: Vec<u8>,
    #[serde(skip)]
    os: Option<Os>,
    /// "WD Elements s/n …": the drive it's on, for people
    pub drive: String,
}

/// Every pool folder of the swarm as this machine knows it: this machine's from `here` (its
/// storage, as `swarm::storage` makes it, asked just now), the others' as they last said.
pub fn areas(db: &Db, name: &str, here: &Value) -> Result<Vec<Area>> {
    let mut out = Vec::new();
    let mut add = |storage: &Value, machine: &str, id: Option<i64>, os: Option<Os>| {
        let (list, _) = swarm::pool_view(storage);
        // room per file system: free less the biggest reserve of the folders on it
        let mut by_fs: HashMap<String, (u64, u64)> = HashMap::new();
        for a in &list {
            if let (Some(free), Some(fs)) = (a["space"]["free"].as_u64(), a["fs"].as_str()) {
                let e = by_fs.entry(fs.to_string()).or_insert((free, 0));
                e.1 = e.1.max(a["reserve"].as_u64().unwrap_or(0));
            }
        }
        for a in list {
            let fs = a["fs"].as_str().unwrap_or("").to_string();
            let room = by_fs.get(&fs).map_or(0, |(f, r)| f.saturating_sub(*r));
            let raw = a["key"]
                .as_str()
                .and_then(|k| URL_SAFE_NO_PAD.decode(k).ok())
                .unwrap_or_default();
            out.push(Area {
                at: AreaRef {
                    machine: machine.to_string(),
                    machine_id: id,
                    area: a["id"].as_i64().unwrap_or(0),
                    path: a["path"].as_str().unwrap_or("").to_string(),
                },
                takes: a["takes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|t| t.as_str().map(str::to_string))
                    .collect(),
                fs,
                room,
                indexed: a["indexed"] == json!(true),
                present: a["present"] == json!(true),
                raw,
                os,
                drive: drive_words(&a["drive"]),
            });
        }
    };
    add(here, name, None, None);
    for m in remote::list(db)? {
        if let Some((_, Some(report), ..)) = remote::kept_volumes(db, m.id)? {
            add(&report, &m.name, Some(m.id), Some(m.os()));
        }
    }
    Ok(out)
}

fn drive_words(d: &Value) -> String {
    let label = d["volume"]["label"].as_str().filter(|s| !s.is_empty());
    let model = d["disk"]["model"].as_str().filter(|s| !s.is_empty());
    match (label, model) {
        (Some(l), Some(m)) => format!("{l} ({m})"),
        (Some(l), None) => l.to_string(),
        (None, Some(m)) => m.to_string(),
        (None, None) => String::new(),
    }
}

// ---------- the plan

/// One thing on the drive, and where it goes in the pool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thing {
    /// what it is and its place in the pool (`dest`, below every pool folder), as plan.rs says
    pub item: Item,
    /// in the pool already, by content (and one example of where)
    pub in_pool_files: u64,
    pub in_pool_bytes: u64,
    pub in_pool_at: Option<String>,
    /// the same content is elsewhere on this drive, and goes once
    pub twice_files: u64,
    pub twice_bytes: u64,
    /// to copy: nothing like it in the pool
    pub new_files: u64,
    pub new_bytes: u64,
    /// a file of the same size is in the pool, and the content hasn't been compared yet
    pub unknown_files: u64,
    pub unknown_bytes: u64,
    /// left out: regenerable junk (with skip_junk), unreadable files
    pub left_files: u64,
    pub left_bytes: u64,
    /// where it goes (None: nothing to copy, left out, or nowhere has room)
    pub to: Option<AreaRef>,
    pub why_to: String,
    /// left out by the person (or by the schema: low value)
    pub skip: bool,
}

impl Thing {
    /// room it needs: what's new, and what can't be told from what's there yet
    pub fn need(&self) -> u64 {
        if self.skip {
            0
        } else {
            self.new_bytes + self.unknown_bytes
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AreaUse {
    pub at: Option<AreaRef>,
    pub drive: String,
    pub takes: Vec<String>,
    pub room: u64,
    pub assigned_files: u64,
    pub assigned_bytes: u64,
    pub indexed: bool,
    pub present: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolPlan {
    pub id: Option<i64>,
    pub created_at: i64,
    /// the machine the drive is on (this one), the drive, and its scan the plan is of
    pub machine: String,
    pub root_id: i64,
    pub root: String,
    pub drive: String,
    pub scan: Option<i64>,
    pub things: Vec<Thing>,
    pub areas: Vec<AreaUse>,
    pub files: u64,
    pub bytes: u64,
    pub new_bytes: u64,
    pub in_pool_bytes: u64,
    pub twice_bytes: u64,
    pub unknown_bytes: u64,
    pub left_bytes: u64,
    /// to copy: what's new or not compared yet, of the things not left out
    #[serde(default)]
    pub copy_bytes: u64,
    /// what nowhere has room for
    pub unplaced_bytes: u64,
    /// what's worth saying before copying: folders not indexed, machines not reporting
    pub notes: Vec<String>,
    pub state: String,
}

/// A pool folder's path range in its machine's index: every path inside it is in [lo, hi).
fn range(a: &Area) -> (Vec<u8>, Vec<u8>) {
    let lo = match a.os {
        None => paths::dir_prefix(Path::new(&a.at.path)),
        Some(os) => remote::prefix(os, &a.raw),
    };
    let hi = remote::past(&lo);
    (lo, hi)
}

/// How one source file compares with the pool.
pub(crate) enum Seen {
    In(String),
    New,
    Unknown,
}

/// Looks up content in every indexed pool folder: this machine's files and the others' cached.
pub(crate) struct Pool<'a> {
    db: &'a Db,
    areas: Vec<(Area, Vec<u8>, Vec<u8>)>,
    sizes: HashSet<i64>,
}

impl<'a> Pool<'a> {
    pub(crate) fn new(db: &'a Db, areas: &[Area]) -> Result<Self> {
        let mut out = Pool {
            db,
            areas: Vec::new(),
            sizes: HashSet::new(),
        };
        for a in areas.iter().filter(|a| a.indexed) {
            let (lo, hi) = range(a);
            let mut stmt = match a.at.machine_id {
                None => db.conn.prepare_cached(
                    "SELECT DISTINCT size FROM files WHERE current = 1 AND path >= ?1 AND path < ?2 AND ?3 IS NULL",
                )?,
                Some(_) => db.conn.prepare_cached(
                    "SELECT DISTINCT size FROM remote_files WHERE remote_id = ?3 AND path >= ?1 AND path < ?2",
                )?,
            };
            let sizes = stmt.query_map(params![lo, hi, a.at.machine_id], |r| r.get::<_, i64>(0))?;
            for s in sizes {
                out.sizes.insert(s?);
            }
            out.areas.push((a.clone(), lo, hi));
        }
        Ok(out)
    }

    pub(crate) fn see(&self, size: i64, sample: Option<&[u8]>, sha: Option<&[u8]>) -> Result<Seen> {
        if !self.sizes.contains(&size) {
            return Ok(Seen::New);
        }
        let mut unknown = false;
        for (a, lo, hi) in &self.areas {
            let local = a.at.machine_id.is_none();
            if let Some(h) = sha {
                let hit: Option<Vec<u8>> = if local {
                    self.db.conn.prepare_cached(
                        "SELECT path FROM files WHERE sha256 = ?1 AND size = ?2 AND current = 1 AND error IS NULL
                           AND path >= ?3 AND path < ?4 LIMIT 1",
                    )?
                    .query_row(params![h, size, lo, hi], |r| r.get(0))
                    .optional()?
                } else {
                    self.db.conn.prepare_cached(
                        "SELECT path FROM remote_files WHERE sha256 = ?1 AND size = ?2 AND remote_id = ?5 AND error = 0
                           AND path >= ?3 AND path < ?4 LIMIT 1",
                    )?
                    .query_row(params![h, size, lo, hi, a.at.machine_id], |r| r.get(0))
                    .optional()?
                };
                if let Some(p) = hit {
                    let shown = match a.os {
                        None => paths::from_bytes(&p).display().to_string(),
                        Some(os) => remote::display(os, &p),
                    };
                    return Ok(Seen::In(format!("{}: {shown}", a.at.machine)));
                }
            }
            // the same size there: can its content be told apart?
            let mut stmt = if local {
                self.db.conn.prepare_cached(
                    "SELECT sample, sha256 FROM files WHERE size = ?1 AND current = 1 AND path >= ?2 AND path < ?3 AND ?4 IS NULL LIMIT 50",
                )?
            } else {
                self.db.conn.prepare_cached(
                    "SELECT sample, sha256 FROM remote_files WHERE size = ?1 AND remote_id = ?4 AND path >= ?2 AND path < ?3 LIMIT 50",
                )?
            };
            let rows = stmt.query_map(params![size, lo, hi, a.at.machine_id], |r| {
                Ok((
                    r.get::<_, Option<Vec<u8>>>(0)?,
                    r.get::<_, Option<Vec<u8>>>(1)?,
                ))
            })?;
            for row in rows {
                let (their_sample, their_sha) = row?;
                let differs = match (sha, their_sha.as_deref()) {
                    (Some(a), Some(b)) => a != b,
                    _ => match (sample, their_sample.as_deref()) {
                        (Some(a), Some(b)) => a != b,
                        _ => false,
                    },
                };
                if !differs {
                    unknown = true;
                }
            }
        }
        Ok(if unknown { Seen::Unknown } else { Seen::New })
    }
}

/// The plan for drive `root_id` on this machine (`machine`): its things (`plan::make`'s items,
/// placed by `schema`), each compared with the pool and given a pool folder among `areas`.
pub fn make(
    db: &Db,
    machine: &str,
    root_id: i64,
    schema: &Schema,
    recognised: &[RootEntities],
    areas: &[Area],
) -> Result<PoolPlan> {
    let (root_b, scan): (Vec<u8>, Option<i64>) = db
        .conn
        .query_row(
            "SELECT path, last_scan FROM roots WHERE id = ?1",
            [root_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .with_context(|| format!("there's no indexed drive {root_id}"))?;
    let root = paths::from_bytes(&root_b);
    if scan.is_none() {
        bail!("{} hasn't been scanned yet", root.display());
    }
    if !recognised.iter().any(|r| r.root_id == root_id) {
        bail!(
            "what's on {} hasn't been recognised yet (backdown entities)",
            root.display()
        );
    }
    // a pool folder on this very drive isn't somewhere to file it into
    let areas: Vec<Area> = areas
        .iter()
        .filter(|a| !(a.at.here() && crate::names::within(Path::new(&a.at.path), &root)))
        .cloned()
        .collect();
    // the filing plan's places, with the pool as the archive
    let p = plan::make_elsewhere(db, Some(&[root_id]), schema, recognised)?;
    let mut things: Vec<Thing> = p
        .items
        .into_iter()
        .map(|item| Thing {
            skip: item.action == "skip",
            item,
            in_pool_files: 0,
            in_pool_bytes: 0,
            in_pool_at: None,
            twice_files: 0,
            twice_bytes: 0,
            new_files: 0,
            new_bytes: 0,
            unknown_files: 0,
            unknown_bytes: 0,
            left_files: 0,
            left_bytes: 0,
            to: None,
            why_to: String::new(),
        })
        .collect();

    // which thing each file belongs to: the nearest of its folders that's a thing's source
    let mut sources: HashMap<Vec<u8>, usize> = HashMap::new();
    let mut excluded: HashSet<Vec<u8>> = HashSet::new();
    let mut loose: Option<usize> = None;
    for (i, t) in things.iter().enumerate() {
        if t.item.only_files {
            loose = Some(i);
            continue;
        }
        sources.insert(paths::to_bytes(Path::new(&t.item.source)), i);
        for x in &t.item.exclude {
            excluded.insert(paths::to_bytes(Path::new(x)));
        }
    }
    let pool = Pool::new(db, &areas)?;
    let mut planned: HashSet<(Vec<u8>, i64)> = HashSet::new();
    let (mut files, mut bytes) = (0u64, 0u64);
    let mut stmt = db.conn.prepare(
        "SELECT path, dir, size, sample, sha256, error IS NOT NULL FROM files WHERE root_id = ?1 AND current = 1",
    )?;
    let mut rows = stmt.query([root_id])?;
    while let Some(r) = rows.next()? {
        let path: Vec<u8> = r.get(0)?;
        let dir: Option<Vec<u8>> = r.get(1)?;
        let size: i64 = r.get(2)?;
        let sample: Option<Vec<u8>> = r.get(3)?;
        let sha: Option<Vec<u8>> = r.get(4)?;
        let unreadable: bool = r.get(5)?;
        files += 1;
        bytes += size.max(0) as u64;
        // its thing: the nearest of the file itself and its folders that's a thing's source
        let mut owner = None;
        let mut junk = false;
        let file = paths::from_bytes(&path);
        let mut cur: &Path = &file;
        loop {
            let b = paths::to_bytes(cur);
            if excluded.contains(&b) {
                junk = true;
                break;
            }
            if let Some(&i) = sources.get(&b) {
                owner = Some(i);
                break;
            }
            match cur.parent() {
                Some(up) if up != root && up.starts_with(&root) => cur = up,
                _ => break,
            }
        }
        if owner.is_none() && !junk && dir.as_deref() == Some(&root_b[..]) {
            owner = loose;
        }
        let Some(i) = owner else {
            continue; // junk, or inside nothing filed
        };
        let t = &mut things[i];
        let sz = size.max(0) as u64;
        if junk || unreadable {
            t.left_files += 1;
            t.left_bytes += sz;
            continue;
        }
        if let Some(h) = &sha
            && !planned.insert((h.clone(), size))
        {
            t.twice_files += 1;
            t.twice_bytes += sz;
            continue;
        }
        match pool.see(size, sample.as_deref(), sha.as_deref())? {
            Seen::In(at) => {
                t.in_pool_files += 1;
                t.in_pool_bytes += sz;
                t.in_pool_at.get_or_insert(at);
            }
            Seen::New => {
                t.new_files += 1;
                t.new_bytes += sz;
            }
            Seen::Unknown => {
                t.unknown_files += 1;
                t.unknown_bytes += sz;
            }
        }
    }
    drop(rows);
    drop(stmt);

    let mut notes = Vec::new();
    for a in &areas {
        if !a.indexed {
            notes.push(format!(
                "{} isn't indexed yet: nothing is filed there until it is",
                a.at.label()
            ));
        } else if !a.present {
            notes.push(format!(
                "{} wasn't there when its machine last said",
                a.at.label()
            ));
        }
    }
    if areas.is_empty() {
        notes.push(
            "there's no pool folder yet: add one on the machine whose drive it's on (backdown pool add FOLDER)"
                .into(),
        );
    }
    for m in remote::list(db)? {
        if remote::kept_volumes(db, m.id)?.is_none_or(|k| k.1.is_none()) {
            notes.push(format!(
                "{} hasn't said what pool folders it has (sync it, or update BackDown there)",
                m.name
            ));
        }
    }

    let mut pp = PoolPlan {
        id: None,
        created_at: crate::clock::now(),
        machine: machine.to_string(),
        root_id,
        root: root.display().to_string(),
        drive: root.file_name().map_or_else(
            || root.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        ),
        scan,
        things,
        areas: Vec::new(),
        files,
        bytes,
        new_bytes: 0,
        in_pool_bytes: 0,
        twice_bytes: 0,
        unknown_bytes: 0,
        left_bytes: 0,
        copy_bytes: 0,
        unplaced_bytes: 0,
        notes,
        state: "new".into(),
    };
    place(db, &mut pp, &areas)?;
    Ok(pp)
}

/// The first two levels of a place: "Mail/example.org/…" → "Mail/example.org".
fn origin(dest: &str) -> String {
    dest.split('/').take(2).collect::<Vec<_>>().join("/")
}

fn top(dest: &str) -> &str {
    dest.split('/').next().unwrap_or("")
}

/// Does pool folder `a` hold `place` already (the folder exists there, as indexed)?
fn holds(db: &Db, a: &Area, place: &str) -> Result<bool> {
    let (lo, _) = range(a);
    let mut p = lo.clone();
    match a.os {
        None => {
            // name by name: joined whole, its `/`s would stay in the path on Windows, which the
            // index (`\` between names) would never match
            let full = place
                .split('/')
                .fold(PathBuf::from(&a.at.path), |p, part| p.join(part));
            let dir = paths::dir_prefix(&full);
            let hi = remote::past(&dir);
            return Ok(db
                .conn
                .prepare_cached(
                    "SELECT 1 FROM files WHERE current = 1 AND path >= ?1 AND path < ?2 LIMIT 1",
                )?
                .query_row(params![dir, hi], |_| Ok(()))
                .optional()?
                .is_some());
        }
        Some(os) => {
            for (i, part) in place.split('/').enumerate() {
                if i > 0 {
                    p.extend_from_slice(match os {
                        Os::Unix => b"/",
                        Os::Windows => b"\\\0",
                    });
                }
                match os {
                    Os::Unix => p.extend_from_slice(part.as_bytes()),
                    Os::Windows => {
                        for u in part.encode_utf16() {
                            p.extend_from_slice(&u.to_le_bytes());
                        }
                    }
                }
            }
            let dir = remote::prefix(os, &p);
            let hi = remote::past(&dir);
            Ok(db
                .conn
                .prepare_cached(
                    "SELECT 1 FROM remote_files WHERE remote_id = ?3 AND path >= ?1 AND path < ?2 LIMIT 1",
                )?
                .query_row(params![dir, hi, a.at.machine_id], |_| Ok(()))
                .optional()?
                .is_some())
        }
    }
}

/// Give each thing that needs room a pool folder (see the module's notes), and total it all up.
/// Things already given one by the person (`to` set on a thing that needs room) keep it if it
/// still fits.
pub fn place(db: &Db, pp: &mut PoolPlan, areas: &[Area]) -> Result<()> {
    let usable: Vec<&Area> = areas.iter().filter(|a| a.indexed && a.present).collect();
    // room left on each file system, as things are given folders
    let mut left: HashMap<(Option<i64>, String), u64> = HashMap::new();
    for a in &usable {
        left.insert((a.at.machine_id, a.fs.clone()), a.room);
    }
    let key = |a: &Area| (a.at.machine_id, a.fs.clone());
    // where each place went in this plan, to keep a place's things together
    let mut went: HashMap<String, AreaRef> = HashMap::new();
    let mut order: Vec<usize> = (0..pp.things.len()).collect();
    // the person's choices first, then the biggest
    order.sort_by_key(|&i| {
        (
            pp.things[i].to.is_none(),
            std::cmp::Reverse(pp.things[i].need()),
        )
    });
    for i in order {
        let need = pp.things[i].need();
        if need == 0 {
            pp.things[i].to = None;
            pp.things[i].why_to = if pp.things[i].skip {
                "left out".into()
            } else if pp.things[i].in_pool_bytes + pp.things[i].twice_bytes > 0 {
                "all of it is in the pool already".into()
            } else {
                "nothing to copy".into()
            };
            continue;
        }
        let dest = pp.things[i].item.dest.clone();
        let fits = |a: &Area, left: &HashMap<(Option<i64>, String), u64>| {
            left.get(&key(a)).is_some_and(|&r| r >= need)
        };
        // the person's choice, if it still fits
        if let Some(chosen) = pp.things[i].to.clone()
            && let Some(a) = usable.iter().find(|a| a.at == chosen)
        {
            if fits(a, &left) {
                *left.get_mut(&key(a)).unwrap() -= need;
                pp.things[i].why_to = "chosen".into();
                went.entry(origin(&dest)).or_insert(chosen);
                continue;
            }
            pp.things[i].why_to = format!(
                "chosen, but {} hasn't room for {} now",
                chosen.label(),
                fmt::bytes(need)
            );
            pp.things[i].to = None;
            continue;
        }
        // 1. where the same place is: its own place in a pool folder already; then with the rest
        // of its origin in this plan; then where its origin is already
        let mut pick: Option<(&Area, String)> = None;
        let homes_of = |place: &str| -> Result<Vec<&Area>> {
            let mut homes: Vec<&Area> = Vec::new();
            for a in &usable {
                if holds(db, a, place)? {
                    homes.push(a);
                }
            }
            homes.sort_by_key(|a| (!a.at.here(), std::cmp::Reverse(left[&key(a)])));
            Ok(homes)
        };
        let mut full_home: Option<(String, AreaRef)> = None;
        for (n, place) in [dest.clone(), origin(&dest)].into_iter().enumerate() {
            if pick.is_some() {
                break;
            }
            if n == 1
                && let Some(r) = went.get(&place)
                && let Some(a) = usable.iter().find(|a| a.at == *r)
                && fits(a, &left)
            {
                pick = Some((a, format!("with the rest of {place} in this plan")));
                break;
            }
            if n == 1 && place == dest {
                break; // its place is its origin: asked already
            }
            let homes = homes_of(&place)?;
            if let Some(a) = homes.iter().find(|a| fits(a, &left)) {
                pick = Some((a, format!("{place} is there already")));
            } else if let Some(a) = homes.first() {
                full_home.get_or_insert((place, a.at.clone()));
            }
        }
        if pick.is_none()
            && let Some((place, at)) = &full_home
        {
            pp.things[i].why_to = format!(
                "{place} is in {}, which hasn't room for {} more",
                at.label(),
                fmt::bytes(need)
            );
        }
        // 2. a folder that takes its top-level place; 3. one that takes anything
        for wants in [true, false] {
            if pick.is_some() {
                break;
            }
            let mut c: Vec<&&Area> = usable
                .iter()
                .filter(|a| {
                    if wants {
                        a.takes.iter().any(|t| t.eq_ignore_ascii_case(top(&dest)))
                    } else {
                        a.takes.is_empty()
                    }
                })
                .filter(|a| fits(a, &left))
                .collect();
            c.sort_by_key(|a| (!a.at.here(), std::cmp::Reverse(left[&key(a)])));
            if let Some(a) = c.first() {
                pick = Some((
                    a,
                    if wants {
                        format!("it takes {}", top(&dest))
                    } else if a.at.here() {
                        "on this machine, with room".into()
                    } else {
                        "the most room".into()
                    },
                ));
            }
        }
        match pick {
            Some((a, why)) => {
                *left.get_mut(&key(a)).unwrap() -= need;
                went.entry(origin(&dest)).or_insert(a.at.clone());
                pp.things[i].to = Some(a.at.clone());
                pp.things[i].why_to = why;
            }
            None => {
                pp.things[i].to = None;
                if pp.things[i].why_to.is_empty() {
                    pp.things[i].why_to = if usable.is_empty() {
                        "there's no pool folder to put it in".into()
                    } else {
                        format!(
                            "no pool folder has room for {} (or takes {})",
                            fmt::bytes(need),
                            top(&dest)
                        )
                    };
                }
            }
        }
    }
    // totals, and each folder's share
    let mut uses: Vec<AreaUse> = areas
        .iter()
        .map(|a| AreaUse {
            at: Some(a.at.clone()),
            drive: a.drive.clone(),
            takes: a.takes.clone(),
            room: a.room,
            assigned_files: 0,
            assigned_bytes: 0,
            indexed: a.indexed,
            present: a.present,
        })
        .collect();
    let (mut new_b, mut in_b, mut twice_b, mut unk_b, mut left_b, mut unplaced) =
        (0, 0, 0, 0, 0, 0);
    let mut copy = 0;
    for t in &pp.things {
        copy += t.need();
        new_b += t.new_bytes;
        in_b += t.in_pool_bytes;
        twice_b += t.twice_bytes;
        unk_b += t.unknown_bytes;
        left_b += t.left_bytes;
        if t.skip {
            left_b += t.new_bytes + t.unknown_bytes;
        }
        match &t.to {
            Some(at) => {
                if let Some(u) = uses.iter_mut().find(|u| u.at.as_ref() == Some(at)) {
                    u.assigned_files += t.new_files + t.unknown_files;
                    u.assigned_bytes += t.need();
                }
            }
            None => unplaced += t.need(),
        }
    }
    pp.areas = uses;
    pp.new_bytes = new_b;
    pp.in_pool_bytes = in_b;
    pp.twice_bytes = twice_b;
    pp.unknown_bytes = unk_b;
    pp.left_bytes = left_b;
    pp.copy_bytes = copy;
    pp.unplaced_bytes = unplaced;
    Ok(())
}

// ---------- kept plans

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS pool_plans (
    id         INTEGER PRIMARY KEY,
    created_at INTEGER NOT NULL,
    root_id    INTEGER NOT NULL,
    state      TEXT NOT NULL,              -- new | sending | done | partial
    data       TEXT NOT NULL               -- JSON: PoolPlan
);
";

pub fn save(db: &Db, pp: &mut PoolPlan) -> Result<i64> {
    let data = serde_json::to_string(&pp)?;
    match pp.id {
        Some(id) => {
            db.conn.execute(
                "UPDATE pool_plans SET state = ?2, data = ?3 WHERE id = ?1",
                params![id, pp.state, data],
            )?;
            Ok(id)
        }
        None => {
            db.conn.execute(
                "INSERT INTO pool_plans(created_at, root_id, state, data) VALUES (?1, ?2, ?3, ?4)",
                params![pp.created_at, pp.root_id, pp.state, data],
            )?;
            let id = db.conn.last_insert_rowid();
            pp.id = Some(id);
            save(db, pp)
        }
    }
}

pub fn load(db: &Db, id: i64) -> Result<PoolPlan> {
    let data: String = db
        .conn
        .query_row("SELECT data FROM pool_plans WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .optional()?
        .with_context(|| format!("there's no pool plan {id}"))?;
    let mut pp: PoolPlan = serde_json::from_str(&data).context("a pool plan")?;
    pp.id = Some(id);
    Ok(pp)
}

/// The plans made, newest first: (id, when, drive, state, bytes to copy).
pub fn list(db: &Db) -> Result<Vec<Value>> {
    let has: bool = db.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'pool_plans')",
        [],
        |r| r.get(0),
    )?;
    if !has {
        return Ok(Vec::new());
    }
    let mut stmt = db
        .conn
        .prepare("SELECT id, data FROM pool_plans ORDER BY id DESC LIMIT 50")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = Vec::new();
    for row in rows {
        let (id, data) = row?;
        if let Ok(p) = serde_json::from_str::<PoolPlan>(&data) {
            out.push(json!({
                "id": id, "created_at": p.created_at, "drive": p.drive, "root": p.root, "state": p.state,
                "new_bytes": p.new_bytes, "unknown_bytes": p.unknown_bytes, "unplaced_bytes": p.unplaced_bytes,
            }));
        }
    }
    Ok(out)
}

/// The person's choice for thing `i`: a pool folder (`to`), or leaving it out (`skip`). The
/// plan is placed again around it.
pub fn choose(
    db: &Db,
    pp: &mut PoolPlan,
    areas: &[Area],
    i: usize,
    to: Option<AreaRef>,
    skip: bool,
) -> Result<()> {
    let t = pp
        .things
        .get_mut(i)
        .context("there's no such thing in the plan")?;
    if pp.state != "new" {
        bail!("this plan is {}: make a new one to change it", pp.state);
    }
    if let Some(a) = &to
        && !areas.iter().any(|x| x.at == *a)
    {
        bail!("{} isn't a pool folder (any more)", a.label());
    }
    t.skip = skip;
    t.to = if skip { None } else { to };
    // everything else is placed afresh around the choices
    for (j, x) in pp.things.iter_mut().enumerate() {
        if j != i && x.why_to != "chosen" {
            x.to = None;
            x.why_to.clear();
        }
    }
    place(db, pp, areas)
}
