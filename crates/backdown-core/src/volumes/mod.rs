//! `volumes`: every disk on this machine, what's on each partition, and what BackDown can do with it.
//!
//! BackDown schedules its reads per physical disk (a *spindle*: two readers on one spinning disk
//! thrash it), and what it may do with a partition depends on what is there. A mounted filesystem
//! can be indexed now; an unmounted one needs a read-only mount first; an encrypted one needs
//! unlocking; an LVM, RAID or APFS-container member is only readable through the device stacked on
//! it; and the system's own volumes are never candidates for wiping. So for each partition the
//! probe reports what it is (filesystem, label, UUID, mount points) and one [`State`] saying which
//! of those cases applies, plus what's stacked on it (`holders`) and whether it's already indexed.
//!
//! Probing never needs root, never mounts and never writes. On Linux it reads sysfs, udev's database
//! and mountinfo, and sniffs a superblock only where udev knows nothing and the device node happens
//! to be readable; on macOS it asks `diskutil` (and `ioreg` for serial numbers); on Windows,
//! PowerShell's storage cmdlets. Each platform's parser is ordinary code over text or files, so all
//! three are tested on every platform; only the part that runs the tools is platform-specific.
//! SMART health is read separately (`backdown health`, backdown_core::health): `health` here
//! stays null.

use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use crate::{Db, browse};

pub mod linux;
pub mod macos;
pub mod mount;
pub mod mountinfo;
pub mod windows;

/// The probe of one machine.
#[derive(Debug, Serialize)]
pub struct Report {
    /// `linux`, `macos` or `windows`.
    pub os: &'static str,
    /// Physical disks (and attached disk images), the system disk first.
    pub disks: Vec<Disk>,
    /// The index the `indexed` marks were read from, if one was.
    pub index: Option<String>,
    /// Things worth knowing about the probe itself, e.g. where it had to fall back.
    pub notes: Vec<String>,
}

/// A physical disk: the unit BackDown schedules reads by.
#[derive(Debug, Default, Serialize)]
pub struct Disk {
    /// The whole-disk device: `/dev/sda`, `/dev/disk4`, `\\.\PHYSICALDRIVE1`.
    pub device: String,
    /// Short name: `sda`, `disk4`, `Disk 1`.
    pub name: String,
    pub size: u64,
    pub model: Option<String>,
    pub serial: Option<String>,
    /// How it's attached: usb, sata, ata, nvme, sas, scsi, mmc, virtio, firewire, thunderbolt,
    /// pcie, fabric (Apple silicon's internal SSD), loop, image, virtual; null when unknown.
    pub transport: Option<String>,
    /// Spinning (true) or solid state (false); null when the system doesn't say.
    pub rotational: Option<bool>,
    /// Removable media, or a device you can unplug (USB, FireWire, SD card).
    pub removable: bool,
    pub read_only: bool,
    /// gpt, dos (MBR), apm; null for a disk without a partition table.
    pub partition_table: Option<String>,
    /// The operating system runs from it (it holds a `system` volume).
    pub system: bool,
    /// For a loop device or disk image: the file behind it.
    pub backing_file: Option<String>,
    /// Not filled here: drive health is read by backdown_core::health (`backdown health`).
    pub health: Option<serde_json::Value>,
    pub volumes: Vec<Volume>,
}

/// A partition, a filesystem that fills a whole disk, or something stacked on one of those (an
/// unlocked LUKS mapping, an LVM logical volume, an md array, an APFS volume).
#[derive(Debug, Default, Serialize)]
pub struct Volume {
    pub device: String,
    pub name: String,
    /// Bytes. For an APFS volume, the space it uses in its container.
    pub size: u64,
    /// Partition number on its disk.
    pub partition: Option<u32>,
    /// The partition's type by name: "EFI system", "Linux filesystem", "Microsoft basic data".
    pub part_type: Option<String>,
    /// A GPT partition's own name.
    pub part_label: Option<String>,
    /// The content signature in blkid's vocabulary (what udev reports too): ext4, vfat, ntfs,
    /// exfat, apfs, hfsplus, btrfs, xfs, swap, crypto_LUKS, BitLocker, LVM2_member,
    /// linux_raid_member, zfs_member, …; null when nothing was recognised.
    pub fs: Option<String>,
    /// The same for people: FAT32, NTFS, LUKS, LVM PV.
    pub fs_name: Option<String>,
    pub label: Option<String>,
    pub uuid: Option<String>,
    pub mounts: Vec<Mount>,
    pub state: State,
    /// Whether it's mounted read-only (every mount); null when not mounted.
    pub read_only: Option<bool>,
    pub encryption: Option<Encryption>,
    /// A short explanation when the state needs one ("LVM physical volume", "swap in use").
    pub note: Option<String>,
    /// Whether this computer can mount it (on Linux: the kernel, one of its modules, or a FUSE
    /// helper knows the file system); left out where it isn't known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mountable_here: Option<bool>,
    /// Indexed roots on this volume (or that it lies inside).
    pub indexed: Vec<Indexed>,
    /// What's stacked on it: the readable side of an encrypted or member volume.
    pub holders: Vec<Volume>,
    /// Its file system's size and the room left on it, when it's mounted (see `Space`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space: Option<Space>,
    #[serde(skip)]
    pub(crate) hint: Hint,
}

/// A mounted file system's size and the room left on it for this account, as `df` says: asked of
/// the file system at its first mount point (statvfs; GetDiskFreeSpaceEx on Windows), which reads
/// nothing from the drive itself. For an APFS volume, its container's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Space {
    pub total: u64,
    pub free: u64,
}

/// The size of the file system holding `path`, and the room left on it; None when it can't be
/// asked (not there, or it reports no size).
pub fn space_at(path: &Path) -> Option<Space> {
    let total = fs4::total_space(path).ok()?;
    let free = fs4::available_space(path).ok()?;
    (total > 0).then_some(Space {
        total,
        free: free.min(total),
    })
}

/// What the platform prober knows beyond the visible fields; `finish` turns it into a state.
#[derive(Debug, Default)]
pub(crate) struct Hint {
    /// Belongs to the running system (system mount point, swap in use, boot firmware partition).
    pub system: bool,
    /// Only readable through something stacked on it (multi-device btrfs, APFS store, …).
    pub member: bool,
    /// Probed and found no signature at all.
    pub blank: bool,
    /// btrfs's per-device UUID, to tell a multi-device filesystem from clones of one.
    pub uuid_sub: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Mount {
    pub path: String,
    pub read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Encryption {
    /// LUKS1, LUKS2, BitLocker, FileVault, APFS.
    pub scheme: String,
    /// Unlocked: a LUKS/BitLocker mapping exists (see `holders`), or the volume itself is readable.
    pub unlocked: bool,
}

/// An indexed root that touches this volume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Indexed {
    pub root_id: i64,
    pub root: String,
    /// The browser's key for the root folder.
    pub key: String,
    /// `same`: the root is this volume's mount point; `contains`: the root is a folder on this
    /// volume; `inside`: this volume is mounted inside the root and that root's scans went into it.
    pub relation: &'static str,
    pub last_scan: Option<i64>,
    /// Unix seconds.
    pub scanned_at: Option<i64>,
}

/// What BackDown can do with a volume.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Belongs to the running system: /, /boot, /home, swap in use, macOS's system and data
    /// volumes, Windows' C:, boot firmware partitions. Never a candidate for wiping.
    System,
    /// Mounted (see `read_only`): can be indexed now.
    Mounted,
    /// A known filesystem, not mounted: needs a (read-only) mount first.
    Mountable,
    /// Locked, or an encrypted container whose readable side is a holder (`encryption.unlocked`).
    Encrypted,
    /// Part of something bigger (LVM, md RAID, ZFS, multi-device btrfs, an APFS container): read
    /// it through its holders.
    Member,
    /// Swap space not in use.
    Swap,
    /// No filesystem signature at all.
    Empty,
    /// Something BackDown can't identify (or couldn't read).
    #[default]
    Unknown,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::System => "system",
            State::Mounted => "mounted",
            State::Mountable => "mountable",
            State::Encrypted => "encrypted",
            State::Member => "member",
            State::Swap => "swap",
            State::Empty => "empty",
            State::Unknown => "unknown",
        }
    }
}

/// Probe this machine's disks. Errors only when the platform's own source is unavailable (no
/// /sys, no diskutil, no PowerShell) or on an operating system BackDown doesn't probe yet.
pub fn probe() -> Result<Report> {
    #[allow(unused_mut)]
    let (mut disks, notes) = platform()?;
    // offering "Mount" only makes sense where the mount can work (APFS on Linux usually can't)
    #[cfg(target_os = "linux")]
    {
        fn mark(v: &mut Volume) {
            if v.state == State::Mountable {
                v.mountable_here = v.fs.as_deref().map(linux::can_mount);
            }
            v.holders.iter_mut().for_each(mark);
        }
        disks
            .iter_mut()
            .flat_map(|d| d.volumes.iter_mut())
            .for_each(mark);
    }
    // each mounted file system's size and free space (the swarm's "how much room")
    fn room(v: &mut Volume) {
        if let Some(m) = v.mounts.first() {
            v.space = space_at(Path::new(&m.path));
        }
        v.holders.iter_mut().for_each(room);
    }
    disks
        .iter_mut()
        .flat_map(|d| d.volumes.iter_mut())
        .for_each(room);
    Ok(Report {
        os: std::env::consts::OS,
        disks,
        index: None,
        notes,
    })
}

#[cfg(target_os = "linux")]
fn platform() -> Result<(Vec<Disk>, Vec<String>)> {
    linux::probe(&linux::Roots::default())
}

#[cfg(target_os = "macos")]
fn platform() -> Result<(Vec<Disk>, Vec<String>)> {
    macos::probe()
}

#[cfg(windows)]
fn platform() -> Result<(Vec<Disk>, Vec<String>)> {
    windows::probe()
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn platform() -> Result<(Vec<Disk>, Vec<String>)> {
    anyhow::bail!(
        "volumes: not supported on this OS yet ({})",
        std::env::consts::OS
    )
}

/// Filesystem ids that are only ever part of something bigger.
fn member_fs(fs: &str) -> bool {
    fs.ends_with("_member") || matches!(fs, "bcache" | "drbd" | "ceph")
}

/// A filesystem id for people.
pub fn fs_name(id: &str) -> String {
    match id {
        "vfat" => "FAT",
        "exfat" => "exFAT",
        "ntfs" => "NTFS",
        "refs" => "ReFS",
        "apfs" => "APFS",
        "hfsplus" => "HFS+",
        "hfs" => "HFS",
        "xfs" => "XFS",
        "f2fs" => "F2FS",
        "udf" => "UDF",
        "iso9660" => "ISO 9660",
        "squashfs" => "SquashFS",
        "crypto_LUKS" => "LUKS",
        "LVM2_member" => "LVM PV",
        "linux_raid_member" => "RAID member",
        "zfs_member" => "ZFS member",
        "ufs" => "UFS",
        other => other,
    }
    .to_string()
}

/// Settle every volume's state (holders first: a system volume makes what it's stacked on system
/// too), then each disk's `system` flag, and put the system disk first.
pub(crate) fn finish(disks: &mut [Disk]) {
    fn classify(v: &mut Volume) {
        for h in &mut v.holders {
            classify(h);
        }
        let fs = v.fs.as_deref().unwrap_or("");
        let container = fs.starts_with("crypto_") || fs == "BitLocker";
        let locked = v.encryption.as_ref().is_some_and(|e| !e.unlocked);
        v.read_only = (!v.mounts.is_empty()).then(|| v.mounts.iter().all(|m| m.read_only));
        v.state = if v.hint.system || v.holders.iter().any(|h| h.state == State::System) {
            State::System
        } else if locked || container {
            State::Encrypted
        } else if !v.mounts.is_empty() {
            State::Mounted
        } else if v.hint.member || member_fs(fs) || !v.holders.is_empty() {
            State::Member
        } else if fs == "swap" {
            State::Swap
        } else if !fs.is_empty() {
            State::Mountable
        } else if v.hint.blank {
            State::Empty
        } else {
            State::Unknown
        };
        if v.fs_name.is_none() {
            v.fs_name = v.fs.as_deref().map(fs_name);
        }
    }
    for d in disks.iter_mut() {
        for v in &mut d.volumes {
            classify(v);
        }
        d.system = d.volumes.iter().any(|v| v.state == State::System);
    }
    disks.sort_by_key(|d| (!d.system, natural(&d.name)));
}

/// A sort key that orders `disk2` before `disk10`, and `sdz` before `sdaa` (the letters after
/// `sd`, `hd`, `vd` count up like digits).
fn natural(s: &str) -> Vec<(u64, String)> {
    let mut out = Vec::new();
    let mut rest = s;
    if let Some(prefix) = ["sd", "hd", "vd"].into_iter().find(|p| s.starts_with(p)) {
        let letters = s[2..]
            .find(|c: char| !c.is_ascii_lowercase())
            .unwrap_or(s.len() - 2);
        out.push((0, prefix.to_string()));
        out.push((letters as u64, s[2..2 + letters].to_string()));
        rest = &s[2 + letters..];
    }
    while !rest.is_empty() {
        let digits = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        if digits > 0 {
            out.push((rest[..digits].parse().unwrap_or(u64::MAX), String::new()));
            rest = &rest[digits..];
        } else {
            let text = rest
                .find(|c: char| c.is_ascii_digit())
                .unwrap_or(rest.len());
            out.push((0, rest[..text].to_string()));
            rest = &rest[text..];
        }
    }
    out
}

/// Every volume, depth first (holders after what they're stacked on).
pub(crate) fn each_volume(disks: &mut [Disk], f: &mut dyn FnMut(&mut Volume)) {
    fn walk(v: &mut Volume, f: &mut dyn FnMut(&mut Volume)) {
        f(v);
        for h in &mut v.holders {
            walk(h, f);
        }
    }
    for d in disks {
        for v in &mut d.volumes {
            walk(v, f);
        }
    }
}

/// Mark the volumes that hold indexed roots, from `db` (opened read-only by the caller).
pub fn mark_indexed(report: &mut Report, db: &Db) -> Result<()> {
    let roots = browse::roots(db)?;
    if roots.is_empty() {
        return Ok(());
    }
    let mut mounts: Vec<String> = Vec::new();
    each_volume(&mut report.disks, &mut |v| {
        mounts.extend(v.mounts.iter().map(|m| m.path.clone()))
    });
    let mut err = None;
    each_volume(&mut report.disks, &mut |v| {
        for m in &v.mounts {
            let mount = Path::new(&m.path);
            for r in &roots {
                let root = Path::new(&r.path);
                let relation = if root == mount {
                    "same"
                } else if holding_mount(root, &mounts) == Some(mount) {
                    "contains"
                } else if mount.starts_with(root) {
                    match scanned_into(db, r.id, mount) {
                        Ok(true) => "inside",
                        Ok(false) => continue,
                        Err(e) => {
                            err.get_or_insert(e);
                            continue;
                        }
                    }
                } else {
                    continue;
                };
                if v.indexed.iter().all(|i| i.root_id != r.id) {
                    v.indexed.push(Indexed {
                        root_id: r.id,
                        root: r.path.clone(),
                        key: r.key.clone(),
                        relation,
                        last_scan: r.last_scan,
                        scanned_at: r.scanned_at,
                    });
                }
            }
        }
    });
    err.map_or(Ok(()), Err)
}

/// The mount point whose filesystem holds `path`: the deepest one above it. On macOS, what looks
/// like it's on the sealed system volume at `/` (`/Users`, `/Applications`, …) is really on the
/// data volume, reached through firmlinks.
fn holding_mount<'a>(path: &Path, mounts: &'a [String]) -> Option<&'a Path> {
    let best = mounts
        .iter()
        .map(Path::new)
        .filter(|m| path.starts_with(m))
        .max_by_key(|m| m.components().count())?;
    if best == Path::new("/")
        && let Some(data) = mounts
            .iter()
            .map(Path::new)
            .find(|m| *m == Path::new("/System/Volumes/Data"))
    {
        return Some(data);
    }
    Some(best)
}

/// Did the root's scans go into the filesystem mounted at `mount` (below the root)? `index`
/// normally stays on one filesystem, so a drive mounted inside an indexed folder usually isn't
/// indexed with it; the files' device numbers tell.
#[cfg(unix)]
fn scanned_into(db: &Db, root_id: i64, mount: &Path) -> Result<bool> {
    use rusqlite::OptionalExtension;
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = std::fs::metadata(mount) else {
        return Ok(false);
    };
    let found: Option<i64> = db
        .conn
        .query_row(
            "SELECT 1 FROM files WHERE dev = ?1 AND current = 1 AND root_id = ?2 LIMIT 1",
            rusqlite::params![meta.dev() as i64, root_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

#[cfg(not(unix))]
fn scanned_into(_: &Db, _: i64, _: &Path) -> Result<bool> {
    Ok(false) // no device numbers in the index here
}

/// Run a platform tool and collect its output, giving up (and killing it) after `timeout`: a
/// dying disk can make `diskutil` or the storage cmdlets hang.
#[cfg(any(target_os = "macos", windows))]
fn run(program: &str, args: &[&str], timeout: std::time::Duration) -> Result<Vec<u8>> {
    use anyhow::Context;
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flashing up under the UI
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("can't run {program}"))?;
    let mut out = child.stdout.take().context("no stdout")?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out.read_to_end(&mut buf);
        buf
    });
    let until = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            let buf = reader.join().unwrap_or_default();
            if !status.success() {
                anyhow::bail!("{program} failed ({status})");
            }
            return Ok(buf);
        }
        if Instant::now() >= until {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("{program} didn't answer in {}s", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vol(fs: Option<&str>) -> Volume {
        Volume {
            fs: fs.map(str::to_string),
            ..Default::default()
        }
    }

    fn state(v: Volume) -> State {
        let mut d = vec![Disk {
            volumes: vec![v],
            ..Default::default()
        }];
        finish(&mut d);
        d[0].volumes[0].state
    }

    #[test]
    fn states() {
        assert_eq!(state(vol(Some("ext4"))), State::Mountable);
        assert_eq!(state(vol(Some("LVM2_member"))), State::Member);
        assert_eq!(state(vol(Some("swap"))), State::Swap);
        assert_eq!(state(vol(None)), State::Unknown);
        let mut v = vol(None);
        v.hint.blank = true;
        assert_eq!(state(v), State::Empty);
        let mut v = vol(Some("ntfs"));
        v.mounts.push(Mount {
            path: "/mnt/x".into(),
            read_only: true,
        });
        assert_eq!(state(v), State::Mounted);
        // a LUKS container stays `encrypted` even when unlocked; its holder carries the mount
        let mut v = vol(Some("crypto_LUKS"));
        v.encryption = Some(Encryption {
            scheme: "LUKS2".into(),
            unlocked: true,
        });
        let mut h = vol(Some("ext4"));
        h.mounts.push(Mount {
            path: "/mnt/vault".into(),
            read_only: false,
        });
        v.holders.push(h);
        assert_eq!(state(v), State::Encrypted);
        // an in-place encrypted volume that's unlocked and mounted is simply mounted
        let mut v = vol(Some("apfs"));
        v.encryption = Some(Encryption {
            scheme: "FileVault".into(),
            unlocked: true,
        });
        v.mounts.push(Mount {
            path: "/Volumes/X".into(),
            read_only: false,
        });
        assert_eq!(state(v), State::Mounted);
        v = vol(Some("apfs"));
        v.encryption = Some(Encryption {
            scheme: "FileVault".into(),
            unlocked: false,
        });
        assert_eq!(state(v), State::Encrypted);
        // system wins, also through a holder
        let mut v = vol(Some("crypto_LUKS"));
        let mut h = vol(Some("btrfs"));
        h.hint.system = true;
        v.holders.push(h);
        assert_eq!(state(v), State::System);
    }

    #[test]
    fn natural_order() {
        let mut names = vec!["sdaa", "sdb", "disk10", "disk2", "sda", "nvme0n1"];
        names.sort_by_key(|n| natural(n));
        assert_eq!(names, ["disk2", "disk10", "nvme0n1", "sda", "sdb", "sdaa"]);
    }

    fn report(mounts: &[&str]) -> Report {
        let volumes = mounts
            .iter()
            .map(|m| Volume {
                name: m.to_string(),
                mounts: vec![Mount {
                    path: m.to_string(),
                    read_only: false,
                }],
                ..Default::default()
            })
            .collect();
        Report {
            os: "linux",
            disks: vec![Disk {
                volumes,
                ..Default::default()
            }],
            index: None,
            notes: Vec::new(),
        }
    }

    fn add_root(db: &Db, path: &str, finished: i64) -> i64 {
        db.conn
            .execute(
                "INSERT INTO roots (path) VALUES (?1)",
                [crate::paths::to_bytes(Path::new(path))],
            )
            .unwrap();
        let root = db.conn.last_insert_rowid();
        db.conn
            .execute(
                "INSERT INTO scans (root_id, started_at, finished_at, files, bytes) VALUES (?1, ?2, ?2, 0, 0)",
                rusqlite::params![root, finished],
            )
            .unwrap();
        let scan = db.conn.last_insert_rowid();
        db.conn
            .execute(
                "UPDATE roots SET last_scan = ?1 WHERE id = ?2",
                rusqlite::params![scan, root],
            )
            .unwrap();
        root
    }

    fn marks(r: &Report, name: &str) -> Vec<(String, &'static str)> {
        r.disks[0]
            .volumes
            .iter()
            .find(|v| v.name == name)
            .unwrap()
            .indexed
            .iter()
            .map(|i| (i.root.clone(), i.relation))
            .collect()
    }

    #[test]
    fn indexed_roots_land_on_the_volume_that_holds_them() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("index.db")).unwrap();
        add_root(&db, "/media/u/Photos", 1_790_000_000);
        add_root(&db, "/home/u/Projects", 1_790_000_100);
        add_root(&db, "/Users/me/Pictures", 1_790_000_200);
        let mut r = report(&["/", "/home", "/media/u/Photos"]);
        mark_indexed(&mut r, &db).unwrap();
        assert_eq!(
            marks(&r, "/media/u/Photos"),
            [("/media/u/Photos".to_string(), "same")]
        );
        assert_eq!(
            marks(&r, "/home"),
            [("/home/u/Projects".to_string(), "contains")]
        );
        // "/" is above everything, but /home and the photos drive are their own filesystems
        assert_eq!(
            marks(&r, "/"),
            [("/Users/me/Pictures".to_string(), "contains")]
        );
        let v = &r.disks[0].volumes[2].indexed[0];
        assert_eq!(
            (v.last_scan.is_some(), v.scanned_at),
            (true, Some(1_790_000_000))
        );
        // macOS: /Users lives on the data volume, not the sealed system snapshot at /
        let mut r = report(&["/", "/System/Volumes/Data"]);
        mark_indexed(&mut r, &db).unwrap();
        assert!(marks(&r, "/").is_empty());
        assert_eq!(
            marks(&r, "/System/Volumes/Data"),
            [
                ("/Users/me/Pictures".to_string(), "contains"),
                ("/home/u/Projects".to_string(), "contains"),
                ("/media/u/Photos".to_string(), "contains"),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_volume_inside_a_root_counts_only_if_its_scans_went_there() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("index.db")).unwrap();
        let top = dir.path().to_str().unwrap().to_string();
        let inner = dir.path().join("drive");
        std::fs::create_dir(&inner).unwrap();
        let inner = inner.to_str().unwrap().to_string();
        let root = add_root(&db, &top, 1_790_000_000);
        let mut r = report(&[&inner]);
        mark_indexed(&mut r, &db).unwrap();
        assert!(marks(&r, &inner).is_empty());
        let dev = std::fs::metadata(&inner).unwrap().dev() as i64;
        db.conn
            .execute(
                "INSERT INTO files (root_id, path, size, mtime, dev, ino, first_scan, last_scan)
                 VALUES (?1, x'00', 0, 0, ?2, 1, 1, 1)",
                rusqlite::params![root, dev],
            )
            .unwrap();
        mark_indexed(&mut r, &db).unwrap();
        assert_eq!(marks(&r, &inner), [(top, "inside")]);
    }
}
