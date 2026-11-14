//! New drives: the first index of a drive is queued when it's plugged in (Settings > Scanning,
//! "Index a drive the first time it's plugged in", on by default).
//!
//! A thread of its own watches for drives being plugged in and mounted. It watches cheaply (the
//! list of disks and of mounted block devices), and runs the volume probe only when one of those
//! changes, or every few minutes where there's no cheap list (Windows). For each external drive
//! (USB, Thunderbolt, FireWire, SD cards) it hasn't seen before, it queues an index once the drive
//! is mounted. Indexing only reads. The drives it has seen are kept by file system UUID in
//! `drives-seen.json` in the service's folder, so each one is considered once. A drive removed from
//! the index isn't added again when it's plugged back in. One plugged in while the setting is off
//! is left alone after it's turned on. The first time this runs (an upgrade, a new installation),
//! the drives already connected are only noted: what gets indexed is drives plugged in from then on.
//!
//! These are never indexed this way: the system's own disks and internal ones, disk images, a
//! drive that's already indexed (as a whole or as a folder on it), and an indexed drive mounted at
//! another path than the one it was indexed at. The UUID recorded at its last scan gives that one
//! away, and indexing it there would make a second copy of everything it holds. On Linux, a new
//! drive that's plugged in but still not mounted a minute later is told about once in Activity,
//! with the command that mounts it read-only. That's a server with nobody logged in: udisks
//! won't mount for a service. Once mounted, it's indexed.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::Result;
use backdown_core::alerts::Note;
use backdown_core::clock::now;
use backdown_core::volumes::{self, Disk, Report, State, Volume};
use backdown_core::{Db, fmt};
use serde::{Deserialize, Serialize};

use crate::daemon::{Daemon, JobOpts};

/// What has been seen, kept across restarts.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Seen {
    /// drives seen mounted (indexed then, or left alone): never considered again
    pub done: BTreeSet<String>,
    /// drives seen plugged in but not mounted, and told about (or noted): indexed once mounted
    pub told: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// a new drive, mounted: queue its first index
    Index { path: String, name: String },
    /// a new drive plugged in but not mounted, still so a minute on: say how to mount it
    Tell {
        key: String,
        name: String,
        device: String,
    },
    /// an indexed drive mounted at another path: said once, nothing done
    Elsewhere { path: String, name: String },
}

/// One look at the drives.
pub struct Look<'a> {
    pub report: &'a Report,
    /// the file system UUIDs (lowercase) indexed drives were on at their last scan
    pub known: &'a HashSet<String>,
    /// the setting: when off, drives are only noted as seen
    pub on: bool,
    /// the first look ever: what's connected is noted, nothing more
    pub first: bool,
    pub now: i64,
}

/// How long a new drive may stay unmounted before it's told about (a desktop mounts a drive a
/// moment after it appears).
pub const TELL_AFTER: i64 = 60;

/// What to do about the drives in `look`, updating `seen`. `waiting` holds when each new,
/// unmounted drive was first seen so (kept in memory only).
pub fn decide(look: &Look, seen: &mut Seen, waiting: &mut BTreeMap<String, i64>) -> Vec<Action> {
    let mut out = Vec::new();
    let mut still_waiting = BTreeSet::new();
    for disk in look.report.disks.iter().filter(|d| external(d)) {
        let mut vols = Vec::new();
        for v in &disk.volumes {
            readable(v, &mut vols);
        }
        let disk_mounted = vols.iter().any(|v| v.state == State::Mounted);
        for v in vols {
            if firmware(v) {
                continue;
            }
            let Some(key) = key(disk, v) else { continue };
            let name = name(v);
            match v.state {
                State::Mounted => {
                    let Some(path) = v.mounts.first().map(|m| m.path.clone()) else {
                        continue;
                    };
                    if !seen.done.insert(key.clone()) {
                        continue;
                    }
                    seen.told.remove(&key);
                    waiting.remove(&key);
                    if look.first || !look.on || !v.indexed.is_empty() {
                        continue;
                    }
                    if v.uuid
                        .as_deref()
                        .is_some_and(|u| look.known.contains(&norm(u)))
                    {
                        out.push(Action::Elsewhere { path, name });
                        continue;
                    }
                    out.push(Action::Index { path, name });
                }
                State::Mountable
                    if look.report.os == "linux" && v.mountable_here != Some(false) =>
                {
                    if seen.done.contains(&key) || seen.told.contains(&key) {
                        continue;
                    }
                    let quiet = look.first
                        || !look.on
                        || disk_mounted
                        || v.uuid
                            .as_deref()
                            .is_some_and(|u| look.known.contains(&norm(u)));
                    if quiet {
                        seen.told.insert(key);
                        continue;
                    }
                    let since = *waiting.entry(key.clone()).or_insert(look.now);
                    still_waiting.insert(key.clone());
                    if look.now - since >= TELL_AFTER {
                        seen.told.insert(key.clone());
                        waiting.remove(&key);
                        out.push(Action::Tell {
                            key,
                            name,
                            device: v.device.clone(),
                        });
                    }
                }
                _ => {}
            }
        }
    }
    // unplugged before it was told about: forget when it was first seen
    waiting.retain(|k, _| still_waiting.contains(k));
    out
}

/// A disk you can plug in and out: not the system's, not an image, not an internal disk.
fn external(d: &Disk) -> bool {
    if d.system || d.backing_file.is_some() {
        return false;
    }
    match d.transport.as_deref() {
        Some("loop" | "image" | "virtual") => false,
        Some("usb" | "firewire" | "thunderbolt" | "mmc") => true,
        _ => d.removable,
    }
}

/// The volumes that hold files: an encrypted container or an LVM/RAID member is read through what
/// is stacked on it.
fn readable<'a>(v: &'a Volume, out: &mut Vec<&'a Volume>) {
    if matches!(v.state, State::Mounted | State::Mountable) {
        out.push(v);
    }
    for h in &v.holders {
        readable(h, out);
    }
}

/// Boot and recovery partitions, which nobody means when they plug a drive in.
fn firmware(v: &Volume) -> bool {
    let t = v.part_type.as_deref().unwrap_or("").to_ascii_lowercase();
    [
        "efi",
        "bios boot",
        "microsoft reserved",
        "recovery",
        "apple boot",
    ]
    .iter()
    .any(|p| t.contains(p))
}

/// How a volume is told apart across plug-ins (device names change): its file system's UUID, else
/// its label and size; None when there's neither (it isn't indexed this way, being impossible to
/// remember).
fn key(disk: &Disk, v: &Volume) -> Option<String> {
    if let Some(u) = v.uuid.as_deref().map(norm).filter(|u| !u.is_empty()) {
        return Some(u);
    }
    let label = v.label.as_deref().unwrap_or("").trim();
    if !label.is_empty() {
        return Some(format!("label:{label}:{}", v.size));
    }
    let serial = disk.serial.as_deref().unwrap_or("").trim();
    match (serial.is_empty(), v.partition) {
        (false, Some(n)) => Some(format!("disk:{serial}:{n}")),
        _ => None,
    }
}

fn norm(u: &str) -> String {
    u.trim().to_lowercase()
}

/// "Hercules (4.00 TB)", for people.
fn name(v: &Volume) -> String {
    let label = v
        .label
        .as_deref()
        .or(v.part_label.as_deref())
        .map(str::trim)
        .filter(|l| !l.is_empty());
    match label {
        Some(l) => format!("{l} ({})", fmt::bytes(v.size)),
        None => format!("a {} drive", fmt::bytes(v.size)),
    }
}

// ---------- the thread

/// How often to look when nothing cheap says a drive arrived (Windows), or as a fallback.
const EVERY: Duration = Duration::from_secs(10 * 60);
const TICK: Duration = Duration::from_secs(5);

static FILE: Mutex<()> = Mutex::new(());

fn file() -> PathBuf {
    crate::settings::dir().join("drives-seen.json")
}

/// What was seen; None when nothing was (no file yet), or what was can't be read back (a damaged
/// file): then the look is a first one, and what's connected is noted rather than indexed.
fn load() -> Option<Seen> {
    let bytes = std::fs::read(file()).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn save(seen: &Seen) -> Result<()> {
    crate::settings::write_private(
        &file(),
        &serde_json::to_vec_pretty(seen)?,
        crate::settings::shared(),
    )
}

pub fn run(d: &Daemon) {
    let mut sig: Option<String> = None;
    let mut last: Option<Instant> = None;
    let mut waiting = BTreeMap::new();
    loop {
        let now_sig = signature();
        let changed = now_sig.is_some() && now_sig != sig;
        sig = now_sig;
        let due = last.is_none_or(|t| t.elapsed() >= EVERY)
            // a drive waiting to be told about is looked at again when its minute is up
            || (!waiting.is_empty() && last.is_some_and(|t| t.elapsed() >= Duration::from_secs(TELL_AFTER as u64)));
        if changed || due {
            last = Some(Instant::now());
            if let Err(e) = look(d, &mut waiting) {
                eprintln!("new drives: {e:#}");
            }
        }
        std::thread::sleep(TICK);
    }
}

fn look(d: &Daemon, waiting: &mut BTreeMap<String, i64>) -> Result<()> {
    let mut report = volumes::probe()?;
    let known = {
        let db = Db::open_read_only(&d.db)?;
        volumes::mark_indexed(&mut report, &db)?;
        known_uuids(&db)
    };
    let on = crate::settings::load().index_new_drives;
    let actions = {
        let _g = FILE.lock().unwrap_or_else(|e| e.into_inner());
        let before = load();
        let first = before.is_none();
        let mut seen = before.clone().unwrap_or_default();
        let look = Look {
            report: &report,
            known: &known,
            on,
            first,
            now: now(),
        };
        let actions = decide(&look, &mut seen, waiting);
        if before.as_ref() != Some(&seen) {
            save(&seen)?;
        }
        actions
    };
    for a in actions {
        match a {
            Action::Index { path, name } => {
                if d.pending("index", &path) {
                    continue;
                }
                eprintln!("{name} at {path} is a new drive: indexing it");
                d.enqueue("index", Some(path), "new-drive", JobOpts::default());
            }
            Action::Elsewhere { path, name } => eprintln!(
                "{name} at {path} is a drive indexed at another path: not indexed again here"
            ),
            Action::Tell { key, name, device } => {
                let cmd = volumes::mount::udisks::mount(&device, true).typed();
                eprintln!(
                    "{name} ({device}) is plugged in but not mounted: it's indexed once it's mounted ({cmd})"
                );
                crate::health::tell(
                    d,
                    Note {
                        kind: "drive",
                        key: format!("new-drive:{key}"),
                        level: "info",
                        title: format!(
                            "{} is plugged in, but not mounted",
                            fmt::capitalised(&name)
                        ),
                        body: format!(
                            "BackDown indexes a new drive once it's mounted, and only reads it. To mount it read-only, use Drives › Mount, or run this in a terminal: {cmd}"
                        ),
                        drive: None,
                        quiet_for: 7 * 86_400,
                    },
                );
            }
        }
    }
    Ok(())
}

/// The file system UUIDs indexed drives were on at their last scan (swarm::remember_drive).
fn known_uuids(db: &Db) -> HashSet<String> {
    let Ok(mut stmt) = db.conn.prepare("SELECT data FROM root_drives") else {
        return HashSet::new(); // an index from before the table
    };
    let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) else {
        return HashSet::new();
    };
    rows.flatten()
        .filter_map(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .filter_map(|v| v["volume"]["uuid"].as_str().map(norm))
        .filter(|u| !u.is_empty())
        .collect()
}

/// Changes when a disk is plugged in or removed, or a block device is mounted or unmounted. None
/// where there's no cheap way to tell (Windows).
fn signature() -> Option<String> {
    let disks = crate::health::disk_set()?;
    // tests and demos: a change to the fake volumes counts as a drive arriving
    let fake = std::env::var_os("BACKDOWN_FAKE_VOLUMES")
        .and_then(|f| std::fs::metadata(f).ok())
        .and_then(|m| m.modified().ok());
    Some(format!("{disks:?} {:?} {fake:?}", mounted()))
}

/// Where block devices are mounted. Only real devices: containers mount and unmount overlays all
/// the time, and snaps are loop devices.
fn mounted() -> Vec<String> {
    #[cfg(target_os = "linux")]
    {
        let mut v: Vec<String> = volumes::mountinfo::read()
            .into_iter()
            .filter(|m| m.source.starts_with("/dev/") && !m.source.starts_with("/dev/loop"))
            .map(|m| format!("{} {}", m.source, m.point.display()))
            .collect();
        v.sort();
        v
    }
    #[cfg(target_os = "macos")]
    {
        let mut v: Vec<String> = std::fs::read_dir("/Volumes")
            .map(|it| {
                it.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backdown_core::volumes::Indexed;
    use serde_json::json;

    fn report(os: &'static str, disks: serde_json::Value) -> Report {
        Report {
            os,
            disks: serde_json::from_value(disks).unwrap(),
            index: None,
            notes: Vec::new(),
        }
    }

    fn usb(device: &str, volumes: serde_json::Value) -> serde_json::Value {
        json!({ "device": device, "name": device, "size": 4_000_787_030_016u64, "transport": "usb", "serial": "WD-1", "volumes": volumes })
    }

    fn mounted(uuid: &str, label: &str, path: &str) -> serde_json::Value {
        json!({ "device": "/dev/sdf1", "name": "sdf1", "size": 4_000_785_104_896u64, "partition": 1, "fs": "ext4", "label": label, "uuid": uuid,
                "state": "mounted", "mounts": [{ "path": path, "read_only": true }] })
    }

    fn unmounted(uuid: &str, label: &str) -> serde_json::Value {
        json!({ "device": "/dev/sdf1", "name": "sdf1", "size": 4_000_785_104_896u64, "partition": 1, "fs": "ext4", "label": label, "uuid": uuid,
                "state": "mountable", "mountable_here": true })
    }

    fn run(r: &Report, seen: &mut Seen, first: bool, now: i64) -> Vec<Action> {
        run_with(
            r,
            seen,
            &mut BTreeMap::new(),
            first,
            true,
            &HashSet::new(),
            now,
        )
    }

    fn run_with(
        r: &Report,
        seen: &mut Seen,
        waiting: &mut BTreeMap<String, i64>,
        first: bool,
        on: bool,
        known: &HashSet<String>,
        now: i64,
    ) -> Vec<Action> {
        decide(
            &Look {
                report: r,
                known,
                on,
                first,
                now,
            },
            seen,
            waiting,
        )
    }

    #[test]
    fn a_new_mounted_usb_drive_is_indexed_once() {
        let r = report(
            "linux",
            json!([usb(
                "/dev/sdf",
                json!([mounted("8BC1-AA", "Hercules", "/media/me/Hercules")])
            )]),
        );
        let mut seen = Seen::default();
        let a = run(&r, &mut seen, false, 1000);
        assert_eq!(
            a,
            vec![Action::Index {
                path: "/media/me/Hercules".into(),
                name: "Hercules (4.00 TB)".into()
            }]
        );
        assert!(seen.done.contains("8bc1-aa"), "{seen:?}");
        // the next look, or after a restart: nothing more
        assert!(run(&r, &mut seen, false, 1005).is_empty());
    }

    #[test]
    fn the_first_look_only_notes_whats_connected() {
        let r = report(
            "linux",
            json!([
                usb("/dev/sdf", json!([mounted("u1", "Old", "/media/me/Old")])),
                usb("/dev/sdg", json!([unmounted("u2", "Spare")]))
            ]),
        );
        let mut seen = Seen::default();
        assert!(run(&r, &mut seen, true, 1000).is_empty());
        assert!(
            seen.done.contains("u1") && seen.told.contains("u2"),
            "{seen:?}"
        );
        // the one that wasn't mounted is indexed once it is
        let r2 = report(
            "linux",
            json!([usb(
                "/dev/sdg",
                json!([mounted("u2", "Spare", "/media/me/Spare")])
            )]),
        );
        assert_eq!(
            run(&r2, &mut seen, false, 2000),
            vec![Action::Index {
                path: "/media/me/Spare".into(),
                name: "Spare (4.00 TB)".into()
            }]
        );
    }

    #[test]
    fn internal_system_and_image_disks_are_left_alone() {
        let r = report(
            "linux",
            json!([
                { "device": "/dev/nvme0n1", "transport": "nvme", "system": true,
                  "volumes": [{ "device": "/dev/nvme0n1p5", "uuid": "sys", "state": "system", "mounts": [{ "path": "/", "read_only": false }] }] },
                { "device": "/dev/sda", "transport": "sata",
                  "volumes": [{ "device": "/dev/sda1", "uuid": "data", "state": "mounted", "mounts": [{ "path": "/data", "read_only": false }] }] },
                { "device": "/dev/loop9", "transport": "loop", "backing_file": "/x/maximal.img",
                  "volumes": [{ "device": "/dev/loop9", "uuid": "img", "state": "mounted", "mounts": [{ "path": "/mnt/maximal", "read_only": true }] }] },
                // a USB disk the system runs from
                { "device": "/dev/sdz", "transport": "usb", "system": true,
                  "volumes": [{ "device": "/dev/sdz1", "uuid": "boot", "state": "mounted", "mounts": [{ "path": "/boot", "read_only": false }] }] }
            ]),
        );
        let mut seen = Seen::default();
        assert!(run(&r, &mut seen, false, 1000).is_empty());
    }

    #[test]
    fn a_drive_already_indexed_is_not_indexed_again() {
        let mut r = report(
            "linux",
            json!([usb(
                "/dev/sde",
                json!([mounted("big", "FUCKING_BIG", "/media/me/FUCKING_BIG")])
            )]),
        );
        r.disks[0].volumes[0].indexed.push(Indexed {
            root_id: 1,
            root: "/media/me/FUCKING_BIG".into(),
            key: "1".into(),
            relation: "same",
            last_scan: Some(71),
            scanned_at: Some(1),
        });
        let mut seen = Seen::default();
        assert!(run(&r, &mut seen, false, 1000).is_empty());
        assert!(seen.done.contains("big"));
    }

    #[test]
    fn an_indexed_drive_mounted_at_another_path_is_not_indexed_twice() {
        // 3 Nov 2026: the desktop automounted the big drive at FUCKING_BIG1, beside its indexed
        // mount point. Indexing it there would have copied 17 M files' worth of index.
        let r = report(
            "linux",
            json!([usb(
                "/dev/sdh",
                json!([mounted("Big-UUID", "FUCKING_BIG", "/media/me/FUCKING_BIG1")])
            )]),
        );
        let known: HashSet<String> = ["big-uuid".to_string()].into();
        let mut seen = Seen::default();
        let a = run_with(
            &r,
            &mut seen,
            &mut BTreeMap::new(),
            false,
            true,
            &known,
            1000,
        );
        assert_eq!(
            a,
            vec![Action::Elsewhere {
                path: "/media/me/FUCKING_BIG1".into(),
                name: "FUCKING_BIG (4.00 TB)".into()
            }]
        );
    }

    #[test]
    fn with_the_setting_off_new_drives_are_only_noted() {
        let r = report(
            "linux",
            json!([usb(
                "/dev/sdf",
                json!([mounted("u1", "Stick", "/media/me/Stick")])
            )]),
        );
        let mut seen = Seen::default();
        let none = HashSet::new();
        assert!(
            run_with(
                &r,
                &mut seen,
                &mut BTreeMap::new(),
                false,
                false,
                &none,
                1000
            )
            .is_empty()
        );
        // turned on later: that drive was seen already
        assert!(
            run_with(
                &r,
                &mut seen,
                &mut BTreeMap::new(),
                false,
                true,
                &none,
                2000
            )
            .is_empty()
        );
    }

    #[test]
    fn an_unmounted_new_drive_is_told_about_once_after_a_minute() {
        let r = report(
            "linux",
            json!([usb("/dev/sdf", json!([unmounted("u9", "Hercules")]))]),
        );
        let mut seen = Seen::default();
        let mut waiting = BTreeMap::new();
        let none = HashSet::new();
        // just plugged in: a desktop may be about to mount it
        assert!(run_with(&r, &mut seen, &mut waiting, false, true, &none, 1000).is_empty());
        assert!(run_with(&r, &mut seen, &mut waiting, false, true, &none, 1030).is_empty());
        let a = run_with(&r, &mut seen, &mut waiting, false, true, &none, 1061);
        assert_eq!(
            a,
            vec![Action::Tell {
                key: "u9".into(),
                name: "Hercules (4.00 TB)".into(),
                device: "/dev/sdf1".into()
            }]
        );
        assert!(run_with(&r, &mut seen, &mut waiting, false, true, &none, 2000).is_empty());
        // mounted then: indexed
        let r2 = report(
            "linux",
            json!([usb(
                "/dev/sdf",
                json!([mounted("u9", "Hercules", "/media/me/Hercules")])
            )]),
        );
        assert!(matches!(
            run_with(&r2, &mut seen, &mut waiting, false, true, &none, 3000).as_slice(),
            [Action::Index { .. }]
        ));
    }

    #[test]
    fn a_drive_mounted_soon_after_it_appears_is_not_told_about() {
        let mut seen = Seen::default();
        let mut waiting = BTreeMap::new();
        let none = HashSet::new();
        let r = report(
            "linux",
            json!([usb("/dev/sdf", json!([unmounted("u3", "Cam")]))]),
        );
        assert!(run_with(&r, &mut seen, &mut waiting, false, true, &none, 1000).is_empty());
        let r2 = report(
            "linux",
            json!([usb(
                "/dev/sdf",
                json!([mounted("u3", "Cam", "/media/me/Cam")])
            )]),
        );
        assert!(matches!(
            run_with(&r2, &mut seen, &mut waiting, false, true, &none, 1004).as_slice(),
            [Action::Index { .. }]
        ));
        assert!(waiting.is_empty());
        // unplugged before its minute: forgotten
        let r3 = report(
            "linux",
            json!([usb("/dev/sdg", json!([unmounted("u4", "Gone")]))]),
        );
        run_with(&r3, &mut seen, &mut waiting, false, true, &none, 1000);
        run_with(
            &report("linux", json!([])),
            &mut seen,
            &mut waiting,
            false,
            true,
            &none,
            1010,
        );
        assert!(waiting.is_empty());
    }

    #[test]
    fn boot_partitions_and_disks_with_a_mounted_volume_stay_quiet() {
        let r = report(
            "linux",
            json!([usb(
                "/dev/sdf",
                json!([
                    { "device": "/dev/sdf1", "partition": 1, "size": 209_715_200, "fs": "vfat", "uuid": "efi1", "part_type": "EFI system", "state": "mountable", "mountable_here": true },
                    mounted("data1", "Data", "/media/me/Data"),
                    { "device": "/dev/sdf3", "partition": 3, "size": 1_000_000_000, "fs": "ext4", "uuid": "other", "label": "Other", "state": "mountable", "mountable_here": true }
                ])
            )]),
        );
        let mut seen = Seen::default();
        let mut waiting = BTreeMap::new();
        let none = HashSet::new();
        let a = run_with(&r, &mut seen, &mut waiting, false, true, &none, 1000);
        assert!(matches!(a.as_slice(), [Action::Index { .. }]), "{a:?}");
        let a = run_with(&r, &mut seen, &mut waiting, false, true, &none, 5000);
        assert!(a.is_empty(), "{a:?}");
        assert!(
            !seen.told.contains("efi1") && seen.told.contains("other"),
            "{seen:?}"
        );
    }

    #[test]
    fn no_unmounted_notices_away_from_linux() {
        let r = report(
            "macos",
            json!([usb("disk4", json!([unmounted("u5", "Ext")]))]),
        );
        let mut seen = Seen::default();
        let mut waiting = BTreeMap::new();
        let none = HashSet::new();
        assert!(run_with(&r, &mut seen, &mut waiting, false, true, &none, 1000).is_empty());
        assert!(run_with(&r, &mut seen, &mut waiting, false, true, &none, 9000).is_empty());
    }

    #[test]
    fn an_unlocked_encrypted_drive_is_read_through_its_mapping() {
        let r = report(
            "linux",
            json!([usb(
                "/dev/sdf",
                json!([
                    { "device": "/dev/sdf1", "partition": 1, "size": 1_000_000_000_000u64, "fs": "crypto_LUKS", "uuid": "luks", "state": "encrypted",
                      "holders": [{ "device": "/dev/dm-3", "size": 1_000_000_000_000u64, "fs": "ext4", "uuid": "inner", "label": "Safe",
                                    "state": "mounted", "mounts": [{ "path": "/media/me/Safe", "read_only": false }] }] }
                ])
            )]),
        );
        let mut seen = Seen::default();
        assert_eq!(
            run(&r, &mut seen, false, 1000),
            vec![Action::Index {
                path: "/media/me/Safe".into(),
                name: "Safe (1.00 TB)".into()
            }]
        );
    }

    #[test]
    fn a_volume_without_uuid_or_label_is_known_by_its_disk_serial() {
        let r = report(
            "linux",
            json!([usb(
                "/dev/sdf",
                json!([
                    { "device": "/dev/sdf1", "partition": 1, "size": 64_000_000_000u64, "fs": "exfat", "state": "mounted", "mounts": [{ "path": "/media/me/disk", "read_only": false }] }
                ])
            )]),
        );
        let mut seen = Seen::default();
        assert_eq!(run(&r, &mut seen, false, 1000).len(), 1);
        assert!(seen.done.contains("disk:WD-1:1"), "{seen:?}");
    }
}
