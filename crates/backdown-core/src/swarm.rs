//! The swarm (docs/SWARM.md): this machine and the machines it reaches over ssh (remote/), seen
//! as one. Every drive with its size and free space, which indexed drives are on it, and where any
//! file is: on which machine, on which drive.
//!
//! Nothing here is a new index. Each machine's own index stays the authority on its files; what a
//! machine says about its drives (`storage`, which its `serve-stdio` answers as the `volumes`
//! request) is kept here with each sync (remote::fetch_volumes) and asked again live when it can
//! be reached.
//!
//! A drive is named by what it is, not where it's mounted: its model and serial, its file system's
//! label and UUID. Each indexed drive (root) remembers which physical drive it was on when it was
//! last scanned (`remember_drive`), so one that's unplugged, or has moved to another machine, can
//! still be named.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

use crate::remote::{self, Machine};
use crate::{Db, browse, find, health, volumes};

/// How long a drive's mount point may take to say whether anything's there (a network share whose
/// server went away can hang whoever asks): past it, the drive is "not answering".
const ASK_PATH: Duration = Duration::from_secs(3);

/// Run `f` on a thread of its own and wait for it at most `t`.
pub(crate) fn within<T: Send + 'static>(
    t: Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(t).ok()
}

// ---------- this machine's storage

/// Is an indexed drive connected: its folder is there, and holds something if its scan found
/// anything (an empty mount point usually means the drive is unplugged). The daemon decides the
/// same way.
fn connected(path: &Path, files: u64) -> bool {
    path.is_dir() && (files == 0 || std::fs::read_dir(path).is_ok_and(|mut it| it.next().is_some()))
}

/// This machine's storage as the swarm sees it: the volume probe (every drive and file system,
/// each mounted one's size and free space, which indexed drives are on each), and every indexed
/// drive with whether it's connected now, the physical drive it was last scanned on, and, when it
/// isn't on a probed drive (a network share), the room where it is. Reads nothing but the index,
/// sysfs and the like, and asks each file system for its size: no drive's files are read.
pub fn storage(db: &Db) -> Result<Value> {
    let mut report = volumes::probe()?;
    volumes::mark_indexed(&mut report, db)?;
    let mut out = serde_json::to_value(&report)?;
    let on_volume: HashSet<i64> = {
        let mut ids = HashSet::new();
        fn walk(v: &Value, ids: &mut HashSet<i64>) {
            for i in v["indexed"].as_array().into_iter().flatten() {
                if let Some(id) = i["root_id"].as_i64() {
                    ids.insert(id);
                }
            }
            for h in v["holders"].as_array().into_iter().flatten() {
                walk(h, ids);
            }
        }
        for d in out["disks"].as_array().into_iter().flatten() {
            for v in d["volumes"].as_array().into_iter().flatten() {
                walk(v, &mut ids);
            }
        }
        ids
    };
    let known = known_drives(db)?;
    let mut roots = Vec::new();
    let mut away = HashSet::new();
    for r in browse::roots(db)? {
        let Ok(path) = browse::path_of(&r.key) else {
            continue;
        };
        let files = r.files;
        let p = path.clone();
        let present = within(ASK_PATH, move || connected(&p, files));
        let on = on_volume.contains(&r.id);
        let space = match (on, present) {
            (false, Some(true)) => {
                let p = path.clone();
                within(ASK_PATH, move || volumes::space_at(&p)).flatten()
            }
            _ => None,
        };
        if present == Some(false) {
            away.insert(r.id);
        }
        roots.push(json!({
            "id": r.id, "name": r.name, "path": r.path, "key": r.key, "files": r.files, "bytes": r.bytes,
            "last_scan": r.last_scan, "scanned_at": r.scanned_at,
            // null: its folder didn't answer in time
            "present": present, "on_volume": on && present == Some(true), "space": space,
            "drive": known.get(&r.id),
        }));
    }
    // a drive that isn't connected isn't "on" whatever its empty mount point happens to be on
    if !away.is_empty() {
        fn strip(v: &mut Value, away: &HashSet<i64>) {
            if let Some(list) = v["indexed"].as_array_mut() {
                list.retain(|i| i["root_id"].as_i64().is_none_or(|id| !away.contains(&id)));
            }
            for h in v["holders"].as_array_mut().into_iter().flatten() {
                strip(h, away);
            }
        }
        for d in out["disks"].as_array_mut().into_iter().flatten() {
            for v in d["volumes"].as_array_mut().into_iter().flatten() {
                strip(v, &away);
            }
        }
    }
    out["roots"] = json!(roots);
    // its pool folders, with the room on their drives now (pool.rs)
    out["pool"] = crate::pool::report(db)?;
    out["at"] = json!(crate::clock::now());
    Ok(out)
}

/// A mounted file system with the disk it's on: what `place` and `remember_drive` match paths
/// against.
struct Fs<'a> {
    disk: &'a Value,
    vol: &'a Value,
    mount: String,
}

fn file_systems(storage: &Value) -> Vec<Fs<'_>> {
    fn walk<'a>(disk: &'a Value, v: &'a Value, out: &mut Vec<Fs<'a>>) {
        for m in v["mounts"].as_array().into_iter().flatten() {
            if let Some(p) = m["path"].as_str() {
                out.push(Fs {
                    disk,
                    vol: v,
                    mount: p.to_string(),
                });
            }
        }
        for h in v["holders"].as_array().into_iter().flatten() {
            walk(disk, h, out);
        }
    }
    let mut out = Vec::new();
    for d in storage["disks"].as_array().into_iter().flatten() {
        for v in d["volumes"].as_array().into_iter().flatten() {
            walk(d, v, &mut out);
        }
    }
    out
}

/// `path` is `mount` or inside it (whole names only; Windows paths compared its way).
fn under(path: &str, mount: &str, windows: bool) -> bool {
    let norm = |s: &str| {
        if windows {
            s.replace('\\', "/").to_lowercase()
        } else {
            s.to_string()
        }
    };
    let (p, m) = (norm(path), norm(mount));
    let m = m.trim_end_matches('/');
    m.is_empty() || p == m || p.starts_with(&format!("{m}/"))
}

fn depth(mount: &str) -> usize {
    mount.split(['/', '\\']).filter(|c| !c.is_empty()).count()
}

/// What a drive is, for people and for matching: the disk (model, serial, size, how it's attached,
/// whether the system runs from it) and the file system (label, UUID, kind, where it's mounted).
fn identity(disk: &Value, vol: &Value, mount: &str) -> Value {
    json!({
        "disk": {
            "name": disk["name"], "model": disk["model"], "serial": disk["serial"], "size": disk["size"],
            "transport": disk["transport"], "rotational": disk["rotational"], "removable": disk["removable"],
            "system": disk["system"],
        },
        "volume": {
            "device": vol["device"], "label": vol["label"], "uuid": vol["uuid"], "fs": vol["fs"],
            "fs_name": vol["fs_name"], "size": vol["size"], "mount": mount,
        },
    })
}

/// The drive a path is on, from a machine's storage (as `storage` makes it): the mounted file
/// system holding it (the deepest mount point above it), or, for an indexed drive that isn't
/// connected now, the drive it was last scanned on. Null when neither is known.
pub fn place(storage: &Value, path: &str, root: Option<i64>) -> Value {
    if let Some(r) = root.and_then(|id| {
        storage["roots"]
            .as_array()?
            .iter()
            .find(|r| r["id"].as_i64() == Some(id))
    }) && r["present"] == json!(false)
    {
        let mut d = r["drive"].clone();
        if d.is_object() {
            d["present"] = json!(false);
        }
        return d;
    }
    let windows = storage["os"] == "windows";
    let fss = file_systems(storage);
    let mut best = fss
        .iter()
        .filter(|f| under(path, &f.mount, windows))
        .max_by_key(|f| depth(&f.mount));
    // macOS: what looks like it's on the sealed system volume is on the data volume
    if best.is_some_and(|b| b.mount == "/")
        && let Some(data) = fss.iter().find(|f| f.mount == "/System/Volumes/Data")
    {
        best = Some(data);
    }
    match best {
        Some(f) => {
            let mut d = identity(f.disk, f.vol, &f.mount);
            d["present"] = json!(true);
            d
        }
        None => Value::Null,
    }
}

/// The drive each indexed drive was on when it was last scanned (`remember_drive`).
fn known_drives(db: &Db) -> Result<HashMap<i64, Value>> {
    let has: bool = db.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'root_drives')",
        [],
        |r| r.get(0),
    )?;
    if !has {
        return Ok(HashMap::new());
    }
    let mut stmt = db
        .conn
        .prepare("SELECT root_id, data, at FROM root_drives")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    let mut out = HashMap::new();
    for row in rows {
        let (id, data, at) = row?;
        if let Ok(mut v) = serde_json::from_str::<Value>(&data) {
            v["seen_at"] = json!(at);
            out.insert(id, v);
        }
    }
    Ok(out)
}

/// The drive the indexed drive `root_id` was on when it was last scanned (`remember_drive`), if
/// one was recorded.
pub fn recorded_drive(db: &Db, root_id: i64) -> Result<Option<Value>> {
    let has: bool = db.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'root_drives')",
        [],
        |r| r.get(0),
    )?;
    if !has {
        return Ok(None);
    }
    let data: Option<String> = db
        .conn
        .query_row(
            "SELECT data FROM root_drives WHERE root_id = ?1",
            [root_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(data.and_then(|d| serde_json::from_str(&d).ok()))
}

/// How long the volume probe may take before the drive a path is on counts as unknown (a network
/// share whose server went away can hang whoever asks how big it is).
const PROBE_WAIT: Duration = Duration::from_secs(10);

#[cfg(test)]
thread_local! {
    /// What `Here::probe` finds, in tests: a storage report, as `storage` makes it.
    pub(crate) static PROBED: std::cell::RefCell<Option<Value>> =
        const { std::cell::RefCell::new(None) };
}

/// What's mounted where on this machine now, to tell which drive a path is on: the volume probe,
/// and (on Linux) every mount the kernel lists, so that a path on something the probe doesn't
/// know (a network share, a FUSE mount, a drive whose device has gone) isn't taken for a path on
/// the drive below it.
pub struct Here {
    storage: Value,
    mounts: Vec<String>,
}

impl Here {
    /// Probe now (at most `PROBE_WAIT`); None when the probe fails or takes too long.
    pub fn probe() -> Option<Here> {
        #[cfg(test)]
        if let Some(storage) = PROBED.with(|p| p.borrow().clone()) {
            return Some(Here {
                storage,
                mounts: Vec::new(),
            });
        }
        within(PROBE_WAIT, || {
            let storage = serde_json::to_value(volumes::probe().ok()?).ok()?;
            Some(Here {
                storage,
                mounts: mount_points(),
            })
        })
        .flatten()
    }

    /// The drive `path` is on now (as `place` tells it), or Null when that can't be told: no file
    /// system the probe knows holds it, the deepest mount above it is one the probe doesn't know,
    /// or the drive has neither a serial nor a file system UUID to know it by.
    pub fn drive(&self, path: &Path) -> Value {
        let p = path.display().to_string();
        let mut d = place(&self.storage, &p, None);
        let Some(mount) = d["volume"]["mount"].as_str() else {
            return Value::Null;
        };
        let windows = self.storage["os"] == "windows";
        if self
            .mounts
            .iter()
            .any(|m| under(&p, m, windows) && depth(m) > depth(mount))
        {
            return Value::Null;
        }
        if serial_of(&d).is_none() && uuid_of(&d).is_none() {
            return Value::Null;
        }
        if let Some(o) = d.as_object_mut() {
            o.remove("present");
        }
        d
    }
}

/// Where the kernel has file systems mounted (Linux; elsewhere the probe is all there is).
fn mount_points() -> Vec<String> {
    #[cfg(target_os = "linux")]
    {
        volumes::mountinfo::read()
            .into_iter()
            .map(|m| m.point.display().to_string())
            .collect()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Vec::new()
    }
}

fn serial_of(d: &Value) -> Option<String> {
    d["disk"]["serial"]
        .as_str()
        .map(|s| health::identity(Some(s), None))
        .filter(|s| !s.is_empty())
}

fn uuid_of(d: &Value) -> Option<String> {
    d["volume"]["uuid"]
        .as_str()
        .map(|u| u.trim().to_lowercase())
        .filter(|u| !u.is_empty())
}

/// Whether a path is on the drive its indexed drive was last scanned on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnDrive {
    /// the same drive: its serial or its file system's UUID matches
    Same,
    /// another drive: neither matches
    Other,
    /// nothing to compare: no drive recorded, or the one it's on now can't be told
    Unknown,
}

/// The drive recorded for an indexed drive and the drive its path is on now (both as `place`
/// gives them, either may be Null): the same if the disk's serial or the file system's UUID
/// matches, another if one of them can be compared and neither matches. Either one is enough to
/// be the same, unlike `same_drive`: a disk moved into another USB enclosure can report the
/// enclosure's serial, but its file system is the one it was.
pub fn judge(recorded: &Value, now: &Value) -> OnDrive {
    let pair = |a: Option<String>, b: Option<String>| a.zip(b).map(|(x, y)| x == y);
    match (
        pair(serial_of(recorded), serial_of(now)),
        pair(uuid_of(recorded), uuid_of(now)),
    ) {
        (None, None) => OnDrive::Unknown,
        (Some(true), _) | (_, Some(true)) => OnDrive::Same,
        _ => OnDrive::Other,
    }
}

/// A drive for people: `the system disk (Samsung SSD 980, serial S64ANS0T1), file system "root"`.
pub fn drive_name(d: &Value) -> String {
    let text = |v: &Value| {
        v.as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
    };
    let disk = &d["disk"];
    let model = text(&disk["model"]).or_else(|| text(&disk["name"]));
    let serial = text(&disk["serial"]);
    let mut out = if disk["system"] == true {
        match (model, serial) {
            (Some(m), Some(s)) => format!("the system disk ({m}, serial {s})"),
            (Some(x), None) | (None, Some(x)) => format!("the system disk ({x})"),
            (None, None) => "the system disk".to_string(),
        }
    } else {
        match (model, serial) {
            (Some(m), Some(s)) => format!("{m} (serial {s})"),
            (Some(m), None) => m,
            (None, Some(s)) => format!("a disk with serial {s}"),
            (None, None) => "a disk".to_string(),
        }
    };
    let vol = &d["volume"];
    if let Some(l) = text(&vol["label"]) {
        out.push_str(&format!(", file system \"{l}\""));
    } else if let Some(u) = text(&vol["uuid"]) {
        out.push_str(&format!(", file system UUID {u}"));
    }
    out
}

/// Remember which physical drive the indexed drive `root_id` (at `path`) is on now: called after
/// it's scanned, so it can be named while it's unplugged, or once it has moved to another machine.
/// Nothing is kept when the drive can't be told (a network share, a probe that failed). A drive
/// already on record that this isn't stays on record unless `replace` (the scan was told it's
/// another drive now): whatever was scanned at its path meanwhile, a folder left where the drive is
/// mounted, say, mustn't take its place.
pub fn remember_drive(db: &Db, root_id: i64, path: &Path, replace: bool) -> Result<bool> {
    let Some(here) = Here::probe() else {
        return Ok(false);
    };
    let d = here.drive(path);
    if d.is_null() {
        return Ok(false);
    }
    if !replace
        && let Some(old) = recorded_drive(db, root_id)?
        && judge(&old, &d) == OnDrive::Other
    {
        return Ok(false);
    }
    db.conn.execute(
        "INSERT INTO root_drives(root_id, at, data) VALUES (?1, ?2, ?3)
         ON CONFLICT(root_id) DO UPDATE SET at = excluded.at, data = excluded.data",
        params![root_id, crate::clock::now(), d.to_string()],
    )?;
    Ok(true)
}

/// After scan `scan` of the drive at `root`: remember which physical drive it is (best effort; a
/// probe that fails costs the scan nothing). `replace`: as for `remember_drive`.
pub fn remember_scanned(db: &Db, scan: i64, root: &Path, replace: bool) {
    let id: Option<i64> = db
        .conn
        .query_row("SELECT root_id FROM scans WHERE id = ?1", [scan], |r| {
            r.get(0)
        })
        .ok();
    if let Some(id) = id {
        let _ = remember_drive(db, id, root, replace);
    }
}

/// The same disk in two storage reports (or a recorded drive and a report): by serial, else by
/// the file system's UUID.
pub fn same_drive(a: &Value, b: &Value) -> bool {
    let serial = |v: &Value| {
        v["disk"]["serial"]
            .as_str()
            .map(|s| health::identity(Some(s), None))
            .filter(|s| !s.is_empty())
    };
    match (serial(a), serial(b)) {
        (Some(x), Some(y)) => x == y,
        _ => a["volume"]["uuid"]
            .as_str()
            .is_some_and(|u| Some(u) == b["volume"]["uuid"].as_str()),
    }
}

// ---------- the swarm at a glance

/// The latest health verdict of each drive this machine has read, by its health id.
fn local_verdicts(db: &Db) -> Result<HashMap<String, String>> {
    if !health::available(db) {
        return Ok(HashMap::new());
    }
    let mut stmt = db.conn.prepare(
        "SELECT drive, verdict FROM health h WHERE at = (SELECT MAX(at) FROM health WHERE drive = h.drive)",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Each machine's drives' verdicts as last synced (only kept with "Drive health alerts").
fn remote_verdicts(db: &Db) -> Result<HashMap<i64, HashMap<String, String>>> {
    let has: bool = db.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'remote_health')",
        [],
        |r| r.get(0),
    )?;
    let mut out: HashMap<i64, HashMap<String, String>> = HashMap::new();
    if !has {
        return Ok(out);
    }
    let mut stmt = db
        .conn
        .prepare("SELECT remote_id, drive, verdict FROM remote_health")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (m, drive, verdict) = row?;
        out.entry(m).or_default().insert(drive, verdict);
    }
    Ok(out)
}

/// One volume for the overview, with what's indexed on it (its files and bytes from `roots`).
fn volume_view(v: &Value, via: Option<&str>, roots: &HashMap<i64, &Value>) -> Value {
    let indexed: Vec<Value> = v["indexed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|i| {
            let id = i["root_id"].as_i64()?;
            let r = roots.get(&id);
            Some(json!({
                "id": id, "path": i["root"], "key": i["key"], "relation": i["relation"],
                "name": r.map_or(Value::Null, |r| r["name"].clone()),
                "files": r.map_or(json!(0), |r| r["files"].clone()),
                "bytes": r.map_or(json!(0), |r| r["bytes"].clone()),
                "scanned_at": i["scanned_at"],
            }))
        })
        .collect();
    let mounts: Vec<Value> = v["mounts"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|m| m["path"].clone())
        .collect();
    json!({
        "device": v["device"], "name": v["name"], "label": v["label"], "part_label": v["part_label"],
        "fs": v["fs"], "fs_name": v["fs_name"], "uuid": v["uuid"], "size": v["size"], "state": v["state"],
        "via": via, "mounts": mounts, "read_only": v["read_only"], "space": v["space"],
        "system": v["state"] == "system", "indexed": indexed,
    })
}

/// A disk's volumes for the overview: what's stacked on an encrypted or member volume stands for
/// it (that's where the files are); swap and blank partitions are left out.
fn flatten(v: &Value, via: Option<&str>, roots: &HashMap<i64, &Value>, out: &mut Vec<Value>) {
    let holders = v["holders"].as_array().filter(|h| !h.is_empty());
    if let Some(hs) = holders {
        let how = v["fs_name"].as_str().or(v["fs"].as_str());
        for h in hs {
            flatten(h, how.or(via), roots, out);
        }
        return;
    }
    if matches!(v["state"].as_str(), Some("swap" | "empty")) {
        return;
    }
    // nothing recognised, mounted or indexed there (firmware areas, the system SSD's other NVMe
    // namespaces on Apple silicon)
    let none = |k: &str| v[k].as_array().is_none_or(|a| a.is_empty());
    if v["fs"].is_null() && none("mounts") && none("indexed") {
        return;
    }
    // the system's own partitions that aren't mounted (boot firmware, recovery, another OS's
    // system) aren't room anyone files into
    if v["state"] == "system" && none("mounts") && none("indexed") {
        return;
    }
    out.push(volume_view(v, via, roots));
}

/// One machine's part of the overview, from its storage report: its disks (each with its health
/// where known), the indexed drives that aren't on any of them (network shares: `elsewhere`; not
/// connected: `away`; not answering: `unanswered`), and its totals.
pub fn machine_view(storage: &Value, verdicts: &HashMap<String, String>) -> Value {
    let roots: HashMap<i64, &Value> = storage["roots"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| Some((r["id"].as_i64()?, r)))
        .collect();
    let mut disks = Vec::new();
    let (mut capacity, mut free) = (0u64, 0u64);
    for d in storage["disks"].as_array().into_iter().flatten() {
        let mut vols = Vec::new();
        for v in d["volumes"].as_array().into_iter().flatten() {
            flatten(v, None, &roots, &mut vols);
        }
        // a blank disk still shows (it's room to use), but not a sliver with nothing on it
        // (Apple silicon's extra NVMe namespaces, a card reader's empty slot)
        if vols.is_empty() && d["size"].as_u64().unwrap_or(0) < 1_000_000_000 {
            continue;
        }
        let id = health::identity(d["serial"].as_str(), None);
        // APFS volumes report their shared container: count each size and room once per disk
        let mut counted: HashSet<(u64, u64)> = HashSet::new();
        for v in &vols {
            if let (Some(t), Some(f)) = (v["space"]["total"].as_u64(), v["space"]["free"].as_u64())
                && counted.insert((t, f))
            {
                capacity += t;
                free += f;
            }
        }
        disks.push(json!({
            "name": d["name"], "device": d["device"], "model": d["model"], "serial": d["serial"],
            "size": d["size"], "transport": d["transport"], "rotational": d["rotational"],
            "removable": d["removable"], "system": d["system"], "backing_file": d["backing_file"],
            "health": (!id.is_empty()).then(|| verdicts.get(&id)).flatten(),
            "volumes": vols,
        }));
    }
    let (mut elsewhere, mut away, mut unanswered) = (Vec::new(), Vec::new(), Vec::new());
    let (mut files, mut bytes) = (0u64, 0u64);
    for r in roots.values() {
        files += r["files"].as_u64().unwrap_or(0);
        bytes += r["bytes"].as_u64().unwrap_or(0);
        let brief = || {
            json!({
                "id": r["id"], "name": r["name"], "path": r["path"], "key": r["key"],
                "files": r["files"], "bytes": r["bytes"], "scanned_at": r["scanned_at"],
                "space": r["space"], "drive": r["drive"],
            })
        };
        match (r["present"].as_bool(), r["on_volume"].as_bool()) {
            (None, _) => unanswered.push(brief()),
            (Some(false), _) => away.push(brief()),
            (Some(true), Some(false)) => {
                if let (Some(t), Some(f)) =
                    (r["space"]["total"].as_u64(), r["space"]["free"].as_u64())
                {
                    capacity += t;
                    free += f;
                }
                elsewhere.push(brief())
            }
            _ => {}
        }
    }
    let by_path = |a: &Value, b: &Value| {
        a["path"]
            .as_str()
            .unwrap_or("")
            .cmp(b["path"].as_str().unwrap_or(""))
    };
    elsewhere.sort_by(by_path);
    away.sort_by(by_path);
    unanswered.sort_by(by_path);
    // its pool folders, each marked on the volume it's on
    let (pool, pool_totals) = pool_view(storage);
    for d in &mut disks {
        for v in d["volumes"].as_array_mut().into_iter().flatten() {
            let mounts: Vec<String> = v["mounts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|m| m.as_str().map(str::to_string))
                .collect();
            let here: Vec<Value> = pool
                .iter()
                .filter(|a| {
                    a["fs"]
                        .as_str()
                        .is_some_and(|f| mounts.iter().any(|m| m == f))
                })
                .map(|a| json!({ "id": a["id"], "path": a["path"] }))
                .collect();
            if !here.is_empty() {
                v["pool"] = json!(here);
            }
        }
    }
    json!({
        "os": storage["os"], "at": storage["at"], "disks": disks, "elsewhere": elsewhere,
        "away": away, "unanswered": unanswered, "notes": storage["notes"], "pool": pool,
        "totals": { "disks": disks.len(), "capacity": capacity, "free": free, "files": files, "bytes": bytes,
                    "pool": pool_totals },
    })
}

/// A machine's pool folders for the overview, each with the drive it's on (`drive`) and the file
/// system it shares room with (`fs`, its mount point); and their totals: what they hold, and the
/// room left to fill, counted once per file system (two folders on one drive share its room,
/// less the bigger of their reserves).
pub fn pool_view(storage: &Value) -> (Vec<Value>, Value) {
    let mut areas = Vec::new();
    let mut by_fs: HashMap<String, (u64, u64)> = HashMap::new();
    let (mut files, mut bytes) = (0u64, 0u64);
    for a in storage["pool"].as_array().into_iter().flatten() {
        let mut a = a.clone();
        let path = a["path"].as_str().unwrap_or("").to_string();
        let drive = place(storage, &path, None);
        let fs = drive["volume"]["mount"]
            .as_str()
            .map_or_else(|| path.clone(), str::to_string);
        files += a["files"].as_u64().unwrap_or(0);
        bytes += a["bytes"].as_u64().unwrap_or(0);
        if a["present"] == json!(true)
            && let Some(free) = a["space"]["free"].as_u64()
        {
            let e = by_fs.entry(fs.clone()).or_insert((free, 0));
            e.1 = e.1.max(a["reserve"].as_u64().unwrap_or(0));
        }
        a["drive"] = drive;
        a["fs"] = json!(fs);
        areas.push(a);
    }
    let room: u64 = by_fs.values().map(|(f, r)| f.saturating_sub(*r)).sum();
    let n = areas.len();
    (
        areas,
        json!({ "areas": n, "files": files, "bytes": bytes, "room": room }),
    )
}

/// A machine's storage as last kept here (remote::store_volumes): (state, its report, when it
/// answered, when it was asked, why not).
pub type Kept = (
    String,
    Option<Value>,
    Option<i64>,
    Option<i64>,
    Option<String>,
);

/// The whole swarm: this machine (`here`, named `name`: its storage asked just now, or why it
/// couldn't be) and every machine it reaches, as last kept or synced; each with its state, disks,
/// indexed drives and totals, and the swarm's totals.
pub fn overview(db: &Db, name: &str, here: Result<Value>) -> Result<Value> {
    let mut machines = Vec::new();
    let local = local_verdicts(db)?;
    let mut v = match &here {
        Ok(s) => machine_view(s, &local),
        Err(_) => {
            json!({ "disks": [], "elsewhere": [], "away": [], "unanswered": [], "totals": {} })
        }
    };
    v["name"] = json!(name);
    v["here"] = json!(true);
    v["id"] = Value::Null;
    v["state"] = json!(if here.is_ok() { "here" } else { "error" });
    v["error"] = json!(here.as_ref().err().map(|e| format!("{e:#}")));
    v["version"] = json!(env!("CARGO_PKG_VERSION"));
    machines.push(v);
    let remote_health = remote_verdicts(db)?;
    let empty = HashMap::new();
    for m in remote::list(db)? {
        let kept = remote::kept_volumes(db, m.id)?;
        machines.push(remote_view(
            db,
            &m,
            kept,
            remote_health.get(&m.id).unwrap_or(&empty),
        )?);
    }
    let mut t = json!({ "machines": machines.len(), "reporting": 0, "disks": 0, "capacity": 0, "free": 0, "files": 0, "bytes": 0 });
    for m in &machines {
        let add = |t: &mut Value, k: &str, n: u64| t[k] = json!(t[k].as_u64().unwrap_or(0) + n);
        if m["disks"].as_array().is_some_and(|d| !d.is_empty()) {
            add(&mut t, "reporting", 1);
        }
        for k in ["disks", "capacity", "free", "files", "bytes"] {
            add(&mut t, k, m["totals"][k].as_u64().unwrap_or(0));
        }
    }
    // the pool: every machine's pool folders together
    let mut pool = json!({ "areas": 0, "files": 0, "bytes": 0, "room": 0 });
    for m in &machines {
        for k in ["areas", "files", "bytes", "room"] {
            pool[k] =
                json!(pool[k].as_u64().unwrap_or(0) + m["totals"]["pool"][k].as_u64().unwrap_or(0));
        }
    }
    t["pool"] = pool;
    Ok(json!({ "here": name, "machines": machines, "totals": t }))
}

/// One other machine's part of the overview, as kept now.
pub fn one(db: &Db, m: &Machine) -> Result<Value> {
    let verdicts = remote_verdicts(db)?;
    let empty = HashMap::new();
    remote_view(
        db,
        m,
        remote::kept_volumes(db, m.id)?,
        verdicts.get(&m.id).unwrap_or(&empty),
    )
}

/// Another machine for the overview: its storage as last kept (refreshed by each sync and each
/// live look), else, from a BackDown there that doesn't report its drives, its indexed drives as
/// synced (with no drive to put them on).
pub fn remote_view(
    db: &Db,
    m: &Machine,
    kept: Option<Kept>,
    verdicts: &HashMap<String, String>,
) -> Result<Value> {
    let (state, report, ok_at, asked_at, error) =
        kept.unwrap_or_else(|| ("never".into(), None, None, None, None));
    let mut v = match &report {
        Some(r) => machine_view(r, verdicts),
        None => {
            // what the sync knows: its drives (roots), not what they're on
            let mut stmt = db.conn.prepare(
                "SELECT root_id, display, path, files, bytes, scanned_at FROM remote_roots WHERE remote_id = ?1 ORDER BY display",
            )?;
            let roots: Vec<Value> = stmt
                .query_map([m.id], |r| {
                    let raw: Vec<u8> = r.get(2)?;
                    Ok(json!({
                        "id": r.get::<_, i64>(0)?, "path": r.get::<_, String>(1)?,
                        "key": browse::key_bytes(&raw), "name": remote::name_of(m.os(), &raw),
                        "files": r.get::<_, i64>(3)?, "bytes": r.get::<_, i64>(4)?,
                        "scanned_at": r.get::<_, Option<i64>>(5)?, "space": null, "drive": null,
                    }))
                })?
                .collect::<rusqlite::Result<_>>()?;
            let files: i64 = roots.iter().map(|r| r["files"].as_i64().unwrap_or(0)).sum();
            let bytes: i64 = roots.iter().map(|r| r["bytes"].as_i64().unwrap_or(0)).sum();
            json!({ "os": null, "disks": [], "elsewhere": roots, "away": [], "unanswered": [],
                    "totals": { "disks": 0, "capacity": 0, "free": 0, "files": files, "bytes": bytes } })
        }
    };
    v["name"] = json!(m.name);
    v["here"] = json!(false);
    v["id"] = json!(m.id);
    v["url"] = json!(m.url);
    v["version"] = m.info["version"].clone();
    v["state"] = json!(state);
    v["error"] = json!(error.or_else(|| m.last_error.clone()));
    v["ok_at"] = json!(ok_at);
    v["asked_at"] = json!(asked_at);
    v["last_sync"] = json!(m.last_sync);
    v["last_try"] = json!(m.last_try);
    Ok(v)
}

// ---------- where is it

/// How a folded name ranks against search terms (as find.rs ranks): 4 the name itself (with or
/// without its extension), 3 starts with them, 2 a word in it starts with them, 1 has them
/// somewhere; 0 when a term is missing.
pub fn rank(name: &str, terms: &[String]) -> u8 {
    if terms.is_empty() || terms.iter().any(|t| !name.contains(t.as_str())) {
        return 0;
    }
    let whole = terms.join(" ");
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    if name == whole || stem == whole {
        return 4;
    }
    if name.starts_with(&terms[0]) {
        return 3;
    }
    let word_start = |t: &str| {
        name.match_indices(t).any(|(i, _)| {
            i == 0
                || !name[..i]
                    .chars()
                    .next_back()
                    .is_some_and(char::is_alphanumeric)
        })
    };
    if terms.iter().all(|t| word_start(t)) {
        return 2;
    }
    1
}

/// Search terms as find.rs reads them: folded words, at most eight.
pub fn terms(q: &str) -> Vec<String> {
    find::fold(q)
        .split_whitespace()
        .take(8)
        .map(str::to_string)
        .collect()
}

/// Files on machine `m` named like `q`, from what's cached here of it (for when it can't be asked
/// itself): the best `limit`, looking for at most `budget`. Folders aren't cached, so only files.
/// (hits, every match seen, whether the whole cache was searched)
pub fn cached_find(
    db: &Db,
    m: &Machine,
    q: &str,
    limit: usize,
    budget: Duration,
) -> Result<(Vec<Value>, u64, bool)> {
    let terms = terms(q);
    if terms.is_empty() || !remote::has_tables(db) {
        return Ok((Vec::new(), 0, true));
    }
    let until = Instant::now() + budget;
    if crate::name_index::ready(&db.conn, crate::name_index::REMOTE)? {
        return cached_by_names(db, m, &terms, limit, until);
    }
    let os = m.os();
    let mut stmt = db.conn.prepare_cached(
        "SELECT f.rid, f.root_id, f.path, f.size, f.mtime, rr.display, rr.path, rr.seen_at
         FROM remote_files f LEFT JOIN remote_roots rr ON rr.remote_id = f.remote_id AND rr.root_id = f.root_id
         WHERE f.remote_id = ?1",
    )?;
    let mut rows = stmt.query([m.id])?;
    let mut hits: Vec<(u8, i64, Value)> = Vec::new();
    let mut total = 0u64;
    let mut complete = true;
    let mut n = 0u64;
    while let Some(r) = rows.next()? {
        n += 1;
        if n.is_multiple_of(4096) && Instant::now() > until {
            complete = false;
            break;
        }
        let raw: Vec<u8> = r.get(2)?;
        let name = remote::name_of(os, &raw);
        let rk = rank(&find::fold(&name), &terms);
        if rk == 0 {
            continue;
        }
        total += 1;
        let mtime: i64 = r.get(4)?;
        let root_raw: Option<Vec<u8>> = r.get(6)?;
        let dir = remote::parent(os, &raw);
        // the folder as a path inside its drive ("" at its top), as find.rs gives it
        let folder = root_raw
            .as_deref()
            .map(|rr| {
                let (d, top) = (remote::display(os, dir), remote::display(os, rr));
                d.strip_prefix(&top)
                    .unwrap_or(&d)
                    .trim_start_matches(['/', '\\'])
                    .to_string()
            })
            .unwrap_or_default();
        hits.push((
            rk,
            mtime,
            json!({
                "kind": "file", "id": r.get::<_, i64>(0)?, "name": name, "path": remote::display(os, &raw),
                "key": browse::key_bytes(&raw), "dir_key": browse::key_bytes(dir),
                "size": r.get::<_, i64>(3)?, "mtime": mtime, "root": r.get::<_, i64>(1)?,
                "drive": root_raw.as_deref().map(|rr| remote::name_of(os, rr)), "folder": folder,
                "rank": rk, "gone": false, "seen": r.get::<_, Option<i64>>(7)?, "cached": true,
            }),
        ));
        if hits.len() > limit * 4 {
            hits.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
            hits.truncate(limit);
        }
    }
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    hits.truncate(limit);
    Ok((hits.into_iter().map(|h| h.2).collect(), total, complete))
}

/// A cached file or folder of machine `m` as a where-is hit, shaped like find.rs's.
#[allow(clippy::too_many_arguments)]
fn cached_hit(
    os: remote::Os,
    dir: bool,
    id: Option<i64>,
    raw: &[u8],
    size: i64,
    mtime: i64,
    root: i64,
    top: Option<&[u8]>,
    seen: Option<i64>,
    rank: u8,
) -> Value {
    let parent = remote::parent(os, raw);
    // the folder as a path inside its drive ("" at its top), as find.rs gives it
    let folder = top
        .map(|t| {
            let (d, top) = (remote::display(os, parent), remote::display(os, t));
            d.strip_prefix(&top)
                .unwrap_or(&d)
                .trim_start_matches(['/', '\\'])
                .to_string()
        })
        .unwrap_or_default();
    json!({
        "kind": if dir { "dir" } else { "file" }, "id": id, "name": remote::name_of(os, raw),
        "path": remote::display(os, raw), "key": browse::key_bytes(raw), "dir_key": browse::key_bytes(parent),
        "size": size, "mtime": mtime, "root": root, "drive": top.map(|t| remote::name_of(os, t)),
        "folder": folder, "rank": rank, "gone": false, "seen": seen, "cached": true,
    })
}

/// `cached_find` through the names index (name_index.rs): the names that match, ranked, then the
/// cached files and folders with each name, best names first, until `limit` are found and the rest
/// are counted, or `until` passes. A folder is worked out from cached files' paths, so it's only
/// given while some cached file is still in it.
fn cached_by_names(
    db: &Db,
    m: &Machine,
    terms: &[String],
    limit: usize,
    until: Instant,
) -> Result<(Vec<Value>, u64, bool)> {
    use crate::name_index::{REMOTE_DIRS, REMOTE_FILES};
    let os = m.os();
    let conn = &db.conn;
    let refs: Vec<&str> = terms.iter().map(String::as_str).collect();
    let mut tiers: [Vec<(String, i64)>; 5] = Default::default();
    for (name, has) in crate::name_index::matching(conn, &refs, REMOTE_FILES | REMOTE_DIRS)? {
        tiers[usize::from(rank(&name, terms))].push((name, has));
    }
    let mut files = conn.prepare_cached(
        "SELECT f.rid, f.root_id, f.path, f.size, f.mtime, rr.path, rr.seen_at
         FROM remote_files f INDEXED BY remote_files_lname
         LEFT JOIN remote_roots rr ON rr.remote_id = f.remote_id AND rr.root_id = f.root_id
         WHERE f.lname = ?1 AND f.remote_id = ?2 ORDER BY f.mtime DESC LIMIT ?3",
    )?;
    let mut count_files = conn.prepare_cached(
        "SELECT COUNT(*) FROM remote_files INDEXED BY remote_files_lname WHERE lname = ?1 AND remote_id = ?2",
    )?;
    let mut dirs = conn.prepare_cached(
        "SELECT d.root_id, d.path, d.newest, rr.path, rr.seen_at
         FROM remote_dirs d INDEXED BY remote_dirs_lname
         LEFT JOIN remote_roots rr ON rr.remote_id = d.remote_id AND rr.root_id = d.root_id
         WHERE d.lname = ?1 AND d.remote_id = ?2 ORDER BY d.newest DESC LIMIT ?3",
    )?;
    let mut count_dirs = conn.prepare_cached(
        "SELECT COUNT(*) FROM remote_dirs INDEXED BY remote_dirs_lname WHERE lname = ?1 AND remote_id = ?2",
    )?;
    let mut holds = conn.prepare_cached(
        "SELECT EXISTS (SELECT 1 FROM remote_files INDEXED BY remote_files_path
         WHERE remote_id = ?1 AND path >= ?2 AND path < ?3)",
    )?;
    let sep: &[u8] = match os {
        remote::Os::Unix => b"/",
        remote::Os::Windows => b"\\\0",
    };
    let keep = limit as i64 + 1;
    let mut hits: Vec<(u8, i64, Value)> = Vec::new();
    let (mut total, mut complete) = (0u64, true);
    'tiers: for rank in (1..=4u8).rev() {
        let collect = hits.len() < limit;
        for (name, has) in &tiers[usize::from(rank)] {
            if Instant::now() > until {
                complete = false;
                break 'tiers;
            }
            if has & REMOTE_FILES != 0 {
                if collect {
                    let mut rows = files.query(params![name, m.id, keep])?;
                    let mut n = 0;
                    while let Some(r) = rows.next()? {
                        n += 1;
                        let raw: Vec<u8> = r.get(2)?;
                        let top: Option<Vec<u8>> = r.get(5)?;
                        let mtime: i64 = r.get(4)?;
                        hits.push((
                            rank,
                            mtime,
                            cached_hit(
                                os,
                                false,
                                Some(r.get(0)?),
                                &raw,
                                r.get(3)?,
                                mtime,
                                r.get(1)?,
                                top.as_deref(),
                                r.get(6)?,
                                rank,
                            ),
                        ));
                    }
                    total += if n >= keep {
                        count_files.query_row(params![name, m.id], |r| r.get::<_, i64>(0))? as u64
                    } else {
                        n as u64
                    };
                } else {
                    total +=
                        count_files.query_row(params![name, m.id], |r| r.get::<_, i64>(0))? as u64;
                }
            }
            if has & REMOTE_DIRS != 0 {
                if collect {
                    let mut rows = dirs.query(params![name, m.id, keep])?;
                    while let Some(r) = rows.next()? {
                        let raw: Vec<u8> = r.get(1)?;
                        let lo = [raw.as_slice(), sep].concat();
                        let mut hi = lo.clone();
                        if let Some(b) = hi.last_mut() {
                            *b += 1; // a separator's last byte is never 0xFF
                        }
                        if !holds.query_row(params![m.id, lo, hi], |r| r.get::<_, bool>(0))? {
                            continue; // nothing cached in it any more
                        }
                        total += 1;
                        let top: Option<Vec<u8>> = r.get(3)?;
                        let newest: i64 = r.get(2)?;
                        hits.push((
                            rank,
                            newest,
                            cached_hit(
                                os,
                                true,
                                None,
                                &raw,
                                0,
                                newest,
                                r.get(0)?,
                                top.as_deref(),
                                r.get(4)?,
                                rank,
                            ),
                        ));
                    }
                } else {
                    total +=
                        count_dirs.query_row(params![name, m.id], |r| r.get::<_, i64>(0))? as u64;
                }
            }
            if hits.len() > limit * 4 {
                hits.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
                hits.truncate(limit);
            }
        }
    }
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    hits.truncate(limit);
    Ok((hits.into_iter().map(|h| h.2).collect(), total, complete))
}

/// A where-is hit with the drive it's on, from its machine's storage (or null where unknown).
pub fn with_place(mut hit: Value, storage: Option<&Value>) -> Value {
    let drive = match storage {
        Some(s) => place(s, hit["path"].as_str().unwrap_or(""), hit["root"].as_i64()),
        None => Value::Null,
    };
    hit["place"] = drive;
    hit
}

#[cfg(test)]
mod tests;
