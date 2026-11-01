//! The filing plan (APP-PLAN §7): where everything on the old drives goes in one archive, and why.
//!
//! Each recognised thing moves as a unit (a mail store, a Time Machine set, a phone backup — never
//! scattered file by file), placed by a schema of type → origin → time: `Mail/example.org`,
//! `Machines/webhost/System 2021`, `Phones/Office iPad/2022`. What isn't recognised keeps its
//! shape under `Unsorted/<drive>/`. Every placement says why, so the plan can be read, argued
//! with and edited before anything is copied.
//!
//! Two rules keep the archive from filling with copies of itself: a thing that already exists
//! in the archive is skipped (it only needs verifying), and a thing that is mostly (≥ 90 %) the
//! same as another of its kind and origin copies only what's unique to it, next to the other.
//! Low-value things (downloads piles, recovered fragments) are listed but not copied unless asked.
//!
//! The plan copies; it never deletes. Once it's applied and the archive indexed, `check` proves
//! which source drives are safe to wipe.
//!
//! The archive's drive may take names that differ only by case or Unicode form for one (APFS,
//! NTFS, exFAT), so places are compared that way: names are written composed (NFC), a folder
//! keeps the spelling it's first given ("Mail/Example.org" joins "Mail/example.org" rather than
//! merging with it unseen), and two things never share a place in any spelling.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsStr;
use std::path::{MAIN_SEPARATOR_STR, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::overlap::Overlap;
use crate::recognise::{Entity, RootEntities};
use crate::{Db, browse, names, paths};

// ---------- the schema

/// Where each kind of thing goes. Values are paths below the archive root with placeholders:
/// {origin} (host, domain, device or person it came from), {name} (its folder or file name),
/// {title}, {drive} (the drive it's on now), {year} (its newest file), {years} (oldest–newest).
pub const DEFAULT_SCHEMA: &str = r#"# BackDown filing schema: where each kind of thing goes in the archive.
# Placeholders: {origin} {name} {title} {drive} {year} {years}
# Edit and pass with: backdown plan --schema this-file.toml

[place]
mail-store = "Mail/{origin}"
maildir = "Mail/{origin}/{name}"
mdbox = "Mail/{origin}"
thunderbird = "Mail/{origin}/Thunderbird"
nextcloud = "Servers/{origin}/Nextcloud data"
nextcloud-server = "Servers/{origin}/{name}"
website = "Servers/Websites/{name}"
sql-dump = "Servers/Databases/{name}"
timemachine = "Machines/{origin}/Time Machine"
system = "Machines/{origin}/System {year}"
home-mac = "People/{name}/{origin}"
home-linux = "People/{name}/{origin}"
home-windows = "People/{name}/{origin}"
ios-backup = "Phones/{origin}/{year}"
android-backup = "Phones/{origin}/{year}"
git = "Code/{origin}/{name}"
disk-image = "DiskImages/{origin}/{name}"
photos = "Media/Photos/{name} ({drive})"
music = "Media/Music/{name} ({drive})"
videos = "Media/Video/{name} ({drive})"
export = "Exports/{title}/{year}"
downloads = "Unsorted/{drive}/{name}"
recovery = "Unsorted/{drive}/Recovered/{name}"
unsorted = "Unsorted/{drive}/{name}"

[options]
# downloads piles and recovered fragments: list them, but don't copy
skip_low_value = true
# regenerable caches, previews, build output and trash: copy them too (a drive is only provably
# safe to wipe when every file has a copy)
skip_junk = false
# a thing at least this identical to another of its kind and origin copies only its own files
merge_overlap = 0.9
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schema {
    pub place: BTreeMap<String, String>,
    pub skip_low_value: bool,
    pub skip_junk: bool,
    pub merge_overlap: f64,
}

impl Default for Schema {
    fn default() -> Self {
        parse_schema(DEFAULT_SCHEMA).expect("the built-in schema parses")
    }
}

/// The schema's small TOML subset: `[place]` and `[options]` tables of `key = value` lines.
pub fn parse_schema(text: &str) -> Result<Schema> {
    let mut s = Schema {
        place: BTreeMap::new(),
        skip_low_value: true,
        skip_junk: false,
        merge_overlap: 0.9,
    };
    let mut table = String::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.split(" #").next().unwrap_or(raw).trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(t) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            table = t.trim().to_string();
            continue;
        }
        let (k, v) = line
            .split_once('=')
            .with_context(|| format!("schema line {}: expected key = value", n + 1))?;
        let (k, v) = (k.trim().trim_matches('"'), v.trim());
        match table.as_str() {
            "place" => {
                let v = v
                    .strip_prefix('"')
                    .and_then(|v| v.strip_suffix('"'))
                    .with_context(|| format!("schema line {}: a place is a quoted path", n + 1))?;
                if v.starts_with('/') || v.contains("..") {
                    bail!(
                        "schema line {}: places are relative paths inside the archive",
                        n + 1
                    );
                }
                s.place.insert(k.to_string(), v.to_string());
            }
            "options" => match k {
                "skip_low_value" => s.skip_low_value = v == "true",
                "skip_junk" => s.skip_junk = v == "true",
                "merge_overlap" => {
                    s.merge_overlap = v.parse().with_context(|| {
                        format!("schema line {}: merge_overlap is a number", n + 1)
                    })?
                }
                _ => bail!("schema line {}: unknown option {k}", n + 1),
            },
            _ => bail!("schema line {}: unknown table [{table}]", n + 1),
        }
    }
    Ok(s)
}

// ---------- the plan

/// One thing to file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Item {
    /// "Mail server store: 4 mailboxes in 2 domains", or "Folder"
    pub what: String,
    /// entity kind, or "unsorted"
    pub kind: String,
    pub source: String,
    pub source_key: String,
    pub drive: String,
    pub is_file: bool,
    /// only the files directly in `source` (a drive's loose top-level files)
    pub only_files: bool,
    /// below the archive root
    pub dest: String,
    pub files: u64,
    pub bytes: u64,
    /// "copy" (everything), "copy-unique" (only content not already in the archive), "skip", or
    /// "merge" (mailboxes: this one and those in `merge`, message by message)
    pub action: String,
    pub why: String,
    /// source paths inside this one that aren't copied with it (copied separately, or junk)
    pub exclude: Vec<String>,
    /// for a thing that's mostly the same as another of its kind and origin: that one's source
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub twin: Option<String>,
    /// "merge" (mailboxes): the other snapshots merged with `source` into one, by Message-ID
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merge: Vec<String>,
}

/// Kinds whose snapshots can be merged message by message (mailmerge.rs).
pub const MERGEABLE: &[&str] = &["maildir", "mail-store"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DriveOutcome {
    pub drive: String,
    pub path: String,
    pub bytes: u64,
    pub copy: u64,
    pub already: u64,
    pub not_copied: u64,
    /// every byte will have a copy in the archive (once applied and the archive indexed)
    pub covered: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Plan {
    pub target: String,
    pub created_at: i64,
    pub schema: String,
    pub items: Vec<Item>,
    pub drives: Vec<DriveOutcome>,
    pub copy_files: u64,
    pub copy_bytes: u64,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn fact<'a>(e: &'a Entity, key: &str) -> Option<&'a str> {
    e.facts
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

fn year(t: Option<i64>) -> Option<i64> {
    // days since 1970 -> year (civil-from-days, as in summary.rs, year only)
    let z = t?.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    Some(yoe + era * 400 + i64::from(mp >= 10))
}

/// A path component made safe for any filesystem the archive might live on: composed (NFC),
/// without the characters Windows, FAT and exFAT refuse, and never a device name (`CON`).
fn clean(s: &str) -> String {
    let c: String = names::nfc(s)
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() {
                '-'
            } else {
                c
            }
        })
        .collect();
    let c = c.trim().trim_end_matches('.').trim().to_string();
    let c: String = if c.is_empty() {
        "unknown".into()
    } else {
        c.chars().take(120).collect()
    };
    names::windows_name(&c).unwrap_or(c)
}

/// What two places the archive's drive may take for one have in common (case, Unicode form).
fn place_key(s: &str) -> Vec<u8> {
    names::Folding::ANY.key(OsStr::new(s))
}

fn host_of_url(url: &str) -> Option<String> {
    // git@host:org/repo.git, https://host/org/repo.git, ssh://git@host/org/repo
    let rest = url.split("://").nth(1).unwrap_or(url);
    let rest = rest.rsplit_once('@').map_or(rest, |(_, r)| r);
    let mut parts = rest.split(['/', ':']).filter(|p| !p.is_empty());
    let host = parts.next()?;
    let org = parts.next();
    Some(match org {
        Some(o) if parts.next().is_some() => format!("{host}/{o}"),
        _ => host.to_string(),
    })
}

/// Who or what a thing came from, for {origin}.
fn origin(e: &Entity, drive: &str, all: &[Entity]) -> String {
    let container = |kind: &str| -> Option<&Entity> {
        let mut at = e.inside.as_deref();
        while let Some(k) = at {
            let c = all.iter().find(|x| x.key == k)?;
            if c.kind == kind {
                return Some(c);
            }
            at = c.inside.as_deref();
        }
        None
    };
    let o = match e.kind.as_str() {
        "mail-store" | "maildir" | "mdbox" | "thunderbird" | "nextcloud" | "nextcloud-server" => {
            fact(e, "Source host")
                .map(str::to_string)
                .or_else(|| fact(e, "Main domain").map(str::to_string))
                .or_else(|| {
                    fact(e, "Domains")
                        .and_then(|d| d.split(", ").next())
                        .map(str::to_string)
                })
                .or_else(|| {
                    fact(e, "Accounts")
                        .and_then(|d| d.split(", ").next())
                        .map(str::to_string)
                })
        }
        "timemachine" => fact(e, "Computers")
            .and_then(|c| c.split(", ").next())
            .map(str::to_string),
        "system" => fact(e, "Host name").map(str::to_string),
        "home-mac" | "home-linux" | "home-windows" => container("system")
            .and_then(|s| fact(s, "Host name"))
            .map(str::to_string),
        "ios-backup" => fact(e, "Device").map(str::to_string),
        "git" => fact(e, "Remote")
            .and_then(host_of_url)
            .or_else(|| Some("local".into())),
        "disk-image" => fact(e, "ddrescue").and_then(|c| {
            c.split_whitespace()
                .skip(1)
                .find(|w| w.starts_with("/dev/"))
                .map(|d| d.trim_start_matches("/dev/").to_string())
        }),
        _ => None,
    };
    o.unwrap_or_else(|| drive.to_string())
}

fn fill(template: &str, e: Option<&Entity>, name: &str, drive: &str, origin: &str) -> String {
    let (y, ys) = match e {
        Some(e) => {
            let (a, b) = (year(e.oldest), year(e.newest));
            (
                b.map(|y| y.to_string()).unwrap_or_else(|| "undated".into()),
                match (a, b) {
                    (Some(a), Some(b)) if a != b => format!("{a}–{b}"),
                    (_, Some(b)) => b.to_string(),
                    _ => "undated".into(),
                },
            )
        }
        None => ("undated".into(), "undated".into()),
    };
    let title = e.map(|e| e.title.clone()).unwrap_or_default();
    template
        .split('/')
        .map(|part| {
            clean(
                &part
                    .replace("{origin}", origin)
                    .replace("{name}", name)
                    .replace("{title}", &title)
                    .replace("{drive}", drive)
                    .replace("{years}", &ys)
                    .replace("{year}", &y),
            )
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn name_of(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// Make a plan to file every source drive into `target` (a folder on the archive drive; it may not
/// exist yet). Sources are every indexed drive except the one `target` is on, or just `only`.
pub fn make(
    db: &Db,
    target: &Path,
    only: Option<&[i64]>,
    schema: &Schema,
    recognised: &[RootEntities],
    overlaps: &[Overlap],
) -> Result<Plan> {
    if !target.is_absolute() {
        bail!("the archive folder must be an absolute path");
    }
    make_in(db, Some(target), only, schema, recognised, overlaps)
}

/// The same, for an archive that isn't one folder on this machine (the swarm's pool: pool/plan.rs
/// works out what's in it already, and where each thing goes): no drive here is left out for
/// holding it, and nothing is skipped as "already in the archive".
pub fn make_elsewhere(
    db: &Db,
    only: Option<&[i64]>,
    schema: &Schema,
    recognised: &[RootEntities],
) -> Result<Plan> {
    make_in(db, None, only, schema, recognised, &[])
}

fn make_in(
    db: &Db,
    target: Option<&Path>,
    only: Option<&[i64]>,
    schema: &Schema,
    recognised: &[RootEntities],
    overlaps: &[Overlap],
) -> Result<Plan> {
    let sep = paths::to_bytes(Path::new(MAIN_SEPARATOR_STR));
    let mut roots = db
        .conn
        .prepare("SELECT id, path FROM roots WHERE last_scan IS NOT NULL ORDER BY path")?;
    let roots: Vec<(i64, PathBuf)> = roots
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                paths::from_bytes(&r.get::<_, Vec<u8>>(1)?),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut totals = db
        .conn
        .prepare("SELECT files, bytes FROM dirs WHERE path = ?1 AND current = 1")?;
    let mut tops = db.conn.prepare(
        "SELECT path, files, bytes FROM dirs WHERE parent = ?1 AND current = 1 ORDER BY path",
    )?;
    let mut loose = db.conn.prepare(
        "SELECT COUNT(*), COALESCE(SUM(size), 0) FROM files WHERE dir = ?1 AND current = 1",
    )?;
    let ov_by_key: HashMap<(&str, &str), &Overlap> = overlaps
        .iter()
        .map(|o| ((o.unit.level.as_str(), o.unit.key.as_str()), o))
        .collect();
    let target_s = target.map(|t| t.display().to_string()).unwrap_or_default();
    // copies of `bytes` bytes of a unit that already sit in the archive
    let in_archive = |level: &str, key: &str, bytes: u64| -> Option<String> {
        let target = target?;
        let o = ov_by_key.get(&(level, key))?;
        o.containers
            .iter()
            .find(|c| {
                names::within(Path::new(&c.path), target) && c.bytes as f64 >= bytes as f64 * 0.999
            })
            .map(|c| c.path.clone())
    };

    let mut items: Vec<Item> = Vec::new();
    let mut drives = Vec::new();
    for (id, root) in &roots {
        let root_b = paths::to_bytes(root);
        // the archive's own drive isn't a source (nor is anything that holds or sits in the
        // archive), whichever Unicode form the archive's path was given in
        if target.is_some_and(|t| names::within(t, root) || names::within(root, t))
            || only.is_some_and(|o| !o.contains(id))
        {
            continue;
        }
        let drive = name_of(&root.display().to_string());
        let Some(rec) = recognised.iter().find(|r| r.root_id == *id) else {
            continue;
        };
        let first_item = items.len();
        // things at the top (nested ones travel inside their container)
        let top: Vec<&Entity> = rec.entities.iter().filter(|e| e.inside.is_none()).collect();
        for e in &top {
            let name = name_of(&e.path);
            let org = origin(e, &drive, &rec.entities);
            let tmpl = schema
                .place
                .get(&e.kind)
                .or_else(|| schema.place.get("unsorted"))
                .cloned()
                .unwrap_or_else(|| "Unsorted/{drive}/{name}".into());
            let dest = fill(&tmpl, Some(e), &name, &drive, &org);
            let inner: Vec<String> = rec
                .entities
                .iter()
                .filter(|x| x.inside.as_deref() == Some(&e.key))
                .map(|x| format!("{} {}", x.label.to_lowercase(), x.title))
                .collect();
            let mut why = format!("{} → {}", e.label, tmpl.split('/').next().unwrap_or(""));
            if org != drive {
                why += &format!("; from {org}");
            }
            if let Some(y) = year(e.newest) {
                why += &format!("; newest file {y}");
            }
            if !inner.is_empty() {
                why += &format!("; includes {}", inner.join(", "));
            }
            let mut exclude = Vec::new();
            let mut bytes = e.bytes;
            let mut files = e.files;
            if schema.skip_junk {
                for j in &e.junk {
                    exclude.push(j.path.clone());
                    bytes = bytes.saturating_sub(j.bytes);
                    files = files.saturating_sub(j.files);
                }
            }
            let (action, why) = if e.low_value && schema.skip_low_value {
                (
                    "skip".to_string(),
                    format!(
                        "{}: probably not worth keeping (set skip_low_value = false to copy it)",
                        e.label
                    ),
                )
            } else if let Some(at) = in_archive("entity", &e.key, e.bytes) {
                (
                    "skip".to_string(),
                    format!("already in the archive at {at}: nothing to copy"),
                )
            } else {
                ("copy".to_string(), why)
            };
            items.push(Item {
                what: format!("{}: {}", e.label, e.title),
                kind: e.kind.clone(),
                source: e.path.clone(),
                source_key: e.key.clone(),
                drive: drive.clone(),
                is_file: e.is_file,
                only_files: false,
                dest,
                files,
                bytes,
                action,
                why,
                exclude,
                twin: None,
                merge: Vec::new(),
            });
        }
        // everything else keeps its shape under Unsorted/<drive>/
        let entity_prefixes: Vec<Vec<u8>> = top
            .iter()
            .map(|e| {
                if e.is_file {
                    paths::to_bytes(Path::new(&e.path))
                } else {
                    paths::dir_prefix(Path::new(&e.path))
                }
            })
            .collect();
        let covered = |p: &[u8]| {
            entity_prefixes
                .iter()
                .any(|x| p.starts_with(x) || x == &[p, &sep[..]].concat())
        };
        let tmpl = schema
            .place
            .get("unsorted")
            .cloned()
            .unwrap_or_else(|| "Unsorted/{drive}/{name}".into());
        let dirs: Vec<(Vec<u8>, i64, i64)> = tops
            .query_map(params![root_b], |r| {
                Ok((r.get::<_, Vec<u8>>(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        for (path, dfiles, dbytes) in dirs {
            let prefix = [&path[..], &sep[..]].concat();
            if covered(&path) {
                continue;
            }
            let inside: Vec<&&Entity> = top
                .iter()
                .filter(|e| paths::to_bytes(Path::new(&e.path)).starts_with(&prefix))
                .collect();
            let p = paths::from_bytes(&path);
            let name = name_of(&p.display().to_string());
            let mut bytes = dbytes as u64;
            let mut files = dfiles as u64;
            let mut exclude: Vec<String> = Vec::new();
            for e in &inside {
                exclude.push(e.path.clone());
                bytes = bytes.saturating_sub(e.bytes);
                files = files.saturating_sub(e.files);
            }
            if schema.skip_junk {
                for j in rec
                    .junk
                    .iter()
                    .filter(|j| paths::to_bytes(Path::new(&j.path)).starts_with(&prefix))
                {
                    exclude.push(j.path.clone());
                    bytes = bytes.saturating_sub(j.bytes);
                    files = files.saturating_sub(j.files);
                }
            }
            if files == 0 {
                continue;
            }
            let key = browse::key(&p);
            let (action, why) = match in_archive("folder", &key, dbytes as u64) {
                Some(at) if inside.is_empty() => (
                    "skip".to_string(),
                    format!("already in the archive at {at}: nothing to copy"),
                ),
                _ => (
                    "copy".to_string(),
                    if inside.is_empty() {
                        "not recognised as anything in particular: kept as it is, under its drive's name".to_string()
                    } else {
                        format!(
                            "the rest of this folder, without the {} thing{} filed separately",
                            inside.len(),
                            if inside.len() == 1 { "" } else { "s" }
                        )
                    },
                ),
            };
            items.push(Item {
                what: "Folder".into(),
                kind: "unsorted".into(),
                source: p.display().to_string(),
                source_key: key,
                drive: drive.clone(),
                is_file: false,
                only_files: false,
                dest: fill(&tmpl, None, &name, &drive, &drive),
                files,
                bytes,
                action,
                why,
                exclude,
                twin: None,
                merge: Vec::new(),
            });
        }
        let (lf, lb): (i64, i64) =
            loose.query_row(params![root_b], |r| Ok((r.get(0)?, r.get(1)?)))?;
        if lf > 0 {
            items.push(Item {
                what: "Loose files".into(),
                kind: "unsorted".into(),
                source: root.display().to_string(),
                source_key: browse::key(root),
                drive: drive.clone(),
                is_file: false,
                only_files: true,
                dest: fill(&tmpl, None, "(top level)", &drive, &drive),
                files: lf as u64,
                bytes: lb as u64,
                action: "copy".into(),
                why: "files directly on the drive, outside any folder".into(),
                exclude: Vec::new(),
                twin: None,
                merge: Vec::new(),
            });
        }
        let (dfiles, dbytes): (i64, i64) = totals
            .query_row(params![root_b], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?
            .unwrap_or((0, 0));
        let _ = dfiles;
        let mine = &items[first_item..];
        let copy: u64 = mine
            .iter()
            .filter(|i| i.action != "skip")
            .map(|i| i.bytes)
            .sum();
        let already: u64 = mine
            .iter()
            .filter(|i| i.action == "skip" && i.why.starts_with("already"))
            .map(|i| i.bytes)
            .sum();
        let not_copied = (dbytes as u64).saturating_sub(copy + already);
        drives.push(DriveOutcome {
            drive,
            path: root.display().to_string(),
            bytes: dbytes as u64,
            copy,
            already,
            not_copied,
            covered: not_copied == 0,
        });
    }

    // the same thing twice (a mailbox backed up on two drives): the larger one is filed, the other
    // only contributes what's unique to it, next to it
    let mut order: Vec<usize> = (0..items.len())
        .filter(|&i| items[i].action == "copy" && items[i].kind != "unsorted")
        .collect();
    order.sort_by_key(|&i| std::cmp::Reverse(items[i].bytes));
    let mut filed: Vec<usize> = Vec::new();
    for i in order {
        let twin = filed.iter().copied().find(|&j| {
            items[j].kind == items[i].kind
                && place_key(&dest_origin(&items[j].dest))
                    == place_key(&dest_origin(&items[i].dest))
                && ov_by_key
                    .get(&("entity", items[i].source_key.as_str()))
                    .is_some_and(|o| {
                        o.containers.iter().any(|c| {
                            c.key == items[j].source_key
                                && c.bytes as f64 >= items[i].bytes as f64 * schema.merge_overlap
                        })
                    })
        });
        match twin {
            Some(j) => {
                let share = ov_by_key[&("entity", items[i].source_key.as_str())]
                    .containers
                    .iter()
                    .find(|c| c.key == items[j].source_key)
                    .map_or(0.0, |c| c.bytes as f64 / items[i].bytes.max(1) as f64);
                let dest = format!(
                    "{}/Also from {}/{}",
                    items[j].dest,
                    clean(&items[i].drive),
                    clean(&name_of(&items[i].source))
                );
                let (twin_src, twin_drive) = (items[j].source.clone(), items[j].drive.clone());
                let why = format!(
                    "{:.0}% the same as {twin_src} on {twin_drive}: only what's unique to it is copied, next to it",
                    share * 100.0
                );
                let it = &mut items[i];
                it.twin = Some(twin_src.clone());
                if share >= 0.999 {
                    // nothing in it that the other hasn't got
                    it.action = "skip".into();
                    it.why = format!("all of it is in {twin_src} on {twin_drive}, which is filed");
                } else {
                    it.action = "copy-unique".into();
                    it.why = why;
                    it.dest = dest;
                }
            }
            None => filed.push(i),
        }
    }

    // two things can't land in the same place, in any spelling the archive's drive may take for
    // one; and each folder keeps the spelling it's first given
    let mut spelling: HashMap<Vec<u8>, String> = HashMap::new();
    let mut used: HashSet<Vec<u8>> = HashSet::new();
    for it in items.iter_mut().filter(|i| i.action != "skip") {
        let mut parts: Vec<String> = Vec::new();
        for part in it.dest.split('/') {
            let key = place_key(&[parts.join("/"), part.to_string()].join("/"));
            parts.push(
                spelling
                    .entry(key)
                    .or_insert_with(|| part.to_string())
                    .clone(),
            );
        }
        let base = parts.join("/");
        let mut d = base.clone();
        if used.contains(&place_key(&d)) {
            d = format!("{base} ({})", clean(&it.drive));
            let mut n = 2;
            while used.contains(&place_key(&d)) {
                d = format!("{base} ({} {n})", clean(&it.drive));
                n += 1;
            }
        }
        used.insert(place_key(&d));
        it.dest = d;
    }
    let copy_files = items
        .iter()
        .filter(|i| i.action != "skip")
        .map(|i| i.files)
        .sum();
    let copy_bytes = items
        .iter()
        .filter(|i| i.action != "skip")
        .map(|i| i.bytes)
        .sum();
    Ok(Plan {
        target: target_s,
        created_at: now(),
        schema: String::new(),
        items,
        drives,
        copy_files,
        copy_bytes,
    })
}

/// "Mail/example.org/…" -> "Mail/example.org": the type and origin levels of a destination
fn dest_origin(dest: &str) -> String {
    dest.split('/').take(2).collect::<Vec<_>>().join("/")
}

// ---------- merging mailboxes instead of copying twins

/// The item that item `i` (a mailbox mostly the same as another) would be merged into, if it can
/// be: both of a kind mailmerge.rs reads, the other one filed (or merging already).
pub fn merge_target(p: &Plan, i: usize) -> Option<usize> {
    let it = p.items.get(i)?;
    if !MERGEABLE.contains(&it.kind.as_str())
        || !matches!(it.action.as_str(), "copy-unique" | "skip")
    {
        return None;
    }
    let twin = it.twin.as_deref()?;
    p.items.iter().position(|j| {
        j.source == twin
            && j.kind == it.kind
            && matches!(j.action.as_str(), "copy" | "merge")
            && !j.merge.contains(&it.source)
    })
}

/// Merge item `i` with its twin: the twin's place gets one mailbox merged from both snapshots by
/// Message-ID (mailmerge.rs) instead of one copied whole and the other's extra files beside it.
/// Returns the merged item.
pub fn merge_into_twin(p: &mut Plan, i: usize) -> Result<usize> {
    let j = merge_target(p, i)
        .context("that isn't a mailbox with a twin in this plan to merge it with")?;
    let (src, drive) = (p.items[i].source.clone(), p.items[i].drive.clone());
    let (twin_dest, twin_src) = (p.items[j].dest.clone(), p.items[j].source.clone());
    let t = &mut p.items[j];
    if !t.merge.contains(&src) {
        t.merge.push(src);
    }
    t.action = "merge".into();
    t.why = format!(
        "{} snapshots of this mailbox (also on {}) merged into one by Message-ID: every message once, in the folder the newest snapshot has it in, flags combined",
        1 + t.merge.len(),
        drive
    );
    let it = &mut p.items[i];
    it.action = "skip".into();
    it.why = format!("merged by Message-ID with {twin_src}, into {twin_dest}");
    p.copy_files = p
        .items
        .iter()
        .filter(|x| x.action != "skip")
        .map(|x| x.files)
        .sum();
    p.copy_bytes = p
        .items
        .iter()
        .filter(|x| x.action != "skip")
        .map(|x| x.bytes)
        .sum();
    Ok(j)
}

/// Merge every mailbox in the plan that has a twin (`backdown plan --merge-mail`). Returns how
/// many were merged.
pub fn merge_all_mail(p: &mut Plan) -> usize {
    let mut n = 0;
    for i in 0..p.items.len() {
        if merge_target(p, i).is_some() && merge_into_twin(p, i).is_ok() {
            n += 1;
        }
    }
    n
}

/// `merge_into_twin` on a saved plan that hasn't started yet.
pub fn merge_mail(db: &Db, id: i64, item: usize) -> Result<Plan> {
    let (mut p, state) = load(db, id)?;
    if state != "new" {
        bail!("plan {id} has started copying ({state}): make a new plan to merge mailboxes");
    }
    merge_into_twin(&mut p, item)?;
    db.conn.execute(
        "UPDATE plans SET data = ?2 WHERE id = ?1",
        params![id, serde_json::to_string(&p)?],
    )?;
    Ok(p)
}

// ---------- storage

pub fn save(db: &Db, plan: &Plan) -> Result<i64> {
    db.conn.execute(
        "INSERT INTO plans(created_at, target, data, state) VALUES (?1, ?2, ?3, 'new')",
        params![plan.created_at, plan.target, serde_json::to_string(plan)?],
    )?;
    Ok(db.conn.last_insert_rowid())
}

pub fn load(db: &Db, id: i64) -> Result<(Plan, String)> {
    let (data, state): (String, String) = db
        .conn
        .query_row(
            "SELECT data, state FROM plans WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .with_context(|| format!("there's no plan {id}"))?;
    Ok((serde_json::from_str(&data)?, state))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_schema_parses() {
        let s = Schema::default();
        assert_eq!(s.place["mail-store"], "Mail/{origin}");
        assert!(s.skip_low_value && !s.skip_junk);
        assert_eq!(s.merge_overlap, 0.9);
        assert!(parse_schema("[place]\ngit = \"/abs\"\n").is_err());
        assert!(parse_schema("[place]\ngit = \"../out\"\n").is_err());
        assert!(parse_schema("[options]\nnope = 1\n").is_err());
    }

    #[test]
    fn names_are_safe_on_every_filesystem() {
        assert_eq!(clean("Alex's iPhone: 2/3"), "Alex's iPhone- 2-3");
        assert_eq!(clean(" ... "), "unknown");
        assert_eq!(
            host_of_url("git@git.example.org:office/intranet.git").as_deref(),
            Some("git.example.org/office")
        );
        assert_eq!(
            host_of_url("https://github.com/user/repo.git").as_deref(),
            Some("github.com/user")
        );
        assert_eq!(year(Some(1_600_000_000)), Some(2020));
        assert_eq!(year(Some(1_609_459_199)), Some(2020)); // 2020-12-31 23:59:59
        assert_eq!(year(Some(1_609_459_200)), Some(2021));
    }
}
