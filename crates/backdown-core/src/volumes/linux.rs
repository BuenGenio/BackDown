//! Linux: what the kernel and udev already know, read from files.
//!
//! `/sys/block` lists the disks, their partitions and sizes, whether they spin, and what's stacked
//! on each device (`holders`: device-mapper and md devices). udev's database
//! (`/run/udev/data/b<major>:<minor>`) holds blkid's findings for every device (filesystem, label,
//! UUID, partition type) and the disk's model and serial. `/proc/self/mountinfo` says what's mounted
//! where and whether read-only; `/proc/swaps`, which swap is in use. None of that needs root. Only
//! where udev has nothing on a device (a container without udev, say) is the device itself read,
//! a few KiB where signatures live, if its node happens to be readable; one that can't be read is
//! reported as unknown.
//!
//! The file locations are parameters ([`Roots`]) so the whole prober runs against a fake tree in
//! tests; nothing here is Linux-only code, it just reads files.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::{Disk, Encryption, Mount, Volume};
use crate::magic;

/// Where the prober reads from.
pub struct Roots {
    pub sys: PathBuf,
    pub udev: PathBuf,
    pub mountinfo: PathBuf,
    pub swaps: PathBuf,
    /// Device nodes, for sniffing what udev couldn't describe.
    pub dev: PathBuf,
}

impl Default for Roots {
    fn default() -> Self {
        Roots {
            sys: "/sys".into(),
            udev: "/run/udev/data".into(),
            mountinfo: "/proc/self/mountinfo".into(),
            swaps: "/proc/swaps".into(),
            dev: "/dev".into(),
        }
    }
}

/// Mount points that make a volume part of the running system.
const SYSTEM_MOUNTS: [&str; 8] = [
    "/",
    "/boot",
    "/boot/efi",
    "/boot/firmware",
    "/efi",
    "/usr",
    "/var",
    "/home",
];

/// Device names that are never disks of their own: RAM disks, and the device-mapper, md, bcache
/// and zvol devices that show up as holders of real partitions instead.
const NOT_DISKS: [&str; 7] = ["ram", "zram", "dm-", "md", "drbd", "bcache", "zd"];

struct MountEntry {
    /// `major:minor` (0:N for btrfs and FUSE, so they're matched by source too)
    dev: String,
    source: String,
    fs: String,
    path: String,
    ro: bool,
}

type Udev = HashMap<String, String>;

struct Probe<'a> {
    r: &'a Roots,
    mounts: Vec<MountEntry>,
    /// kernel names of swap devices in use
    swaps: HashSet<String>,
}

/// Every disk under `roots`, classified.
pub fn probe(roots: &Roots) -> Result<(Vec<Disk>, Vec<String>)> {
    let block = roots.sys.join("block");
    fs::read_dir(&block).with_context(|| format!("can't list {}", block.display()))?;
    let names = list(&block);
    let dm_names: HashMap<String, String> = names
        .iter()
        .filter_map(|n| Some((read(&block.join(n).join("dm/name"))?, n.clone())))
        .collect();
    let p = Probe {
        r: roots,
        mounts: read_mountinfo(&roots.mountinfo),
        swaps: read_swaps(&roots.swaps, &dm_names),
    };
    let mut disks: Vec<Disk> = names.iter().filter_map(|n| p.disk(n)).collect();
    multi_device_btrfs(&mut disks);
    super::each_volume(&mut disks, &mut |v| {
        if v.mounts
            .iter()
            .any(|m| SYSTEM_MOUNTS.contains(&m.path.as_str()))
        {
            v.hint.system = true;
        }
    });
    boot_partitions(&mut disks);
    system_ssd_namespaces(&p, &mut disks);
    super::finish(&mut disks);
    let mut notes = Vec::new();
    if !roots.udev.is_dir() {
        notes.push(
            "no udev database here: filesystems are known only on devices BackDown can read".into(),
        );
    }
    Ok((disks, notes))
}

impl Probe<'_> {
    fn sys(&self, name: &str) -> PathBuf {
        self.r.sys.join("block").join(name)
    }

    fn udev(&self, dir: &Path) -> Option<Udev> {
        let dev = read(&dir.join("dev"))?;
        let text = fs::read_to_string(self.r.udev.join(format!("b{dev}"))).ok()?;
        Some(
            text.lines()
                .filter_map(|l| l.strip_prefix("E:")?.split_once('='))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    fn disk(&self, name: &str) -> Option<Disk> {
        if NOT_DISKS.iter().any(|p| name.starts_with(p))
            || (name.starts_with("mmcblk") && (name.contains("boot") || name.ends_with("rpmb")))
        {
            return None;
        }
        let dir = self.sys(name);
        let size = read_u64(&dir.join("size"))? * 512;
        if size == 0 || read(&dir.join("hidden")).as_deref() == Some("1") {
            return None; // no medium (card readers, empty loop devices), or an NVMe multipath leg
        }
        let backing = read(&dir.join("loop/backing_file"));
        if name.starts_with("loop")
            && backing
                .as_deref()
                .is_none_or(|b| b.starts_with("/var/lib/snapd/") || b.ends_with(".snap"))
        {
            return None; // snap packages: part of the system, and dozens of them
        }
        let u = self.udev(&dir).unwrap_or_default();
        let link = fs::read_link(&dir)
            .map(|l| l.to_string_lossy().into_owned())
            .unwrap_or_default();
        let transport = transport(name, &link, &u);
        let removable = read(&dir.join("removable")).as_deref() == Some("1")
            || matches!(transport, Some("usb" | "firewire"))
            || (transport == Some("mmc")
                && read(&dir.join("device/type")).as_deref() == Some("SD"));
        let model = read(&dir.join("device/model"))
            .or_else(|| udev_text(&u, "ID_MODEL_ENC"))
            .or_else(|| u.get("ID_MODEL").map(|m| m.replace('_', " ")));
        let vendor = read(&dir.join("device/vendor"))
            .or_else(|| udev_text(&u, "ID_VENDOR_ENC"))
            .filter(|v| !v.eq_ignore_ascii_case("ATA"));
        let model = match (vendor, model) {
            (Some(v), Some(m)) if !m.to_lowercase().starts_with(&v.to_lowercase()) => {
                Some(format!("{v} {m}"))
            }
            (v, m) => m.or(v),
        };
        let serial = [
            u.get("ID_SERIAL_SHORT").cloned(),
            read(&dir.join("device/serial")),
            u.get("ID_SCSI_SERIAL").cloned(),
        ]
        .into_iter()
        .flatten()
        .map(|s| s.trim().to_string())
        .find(|s| !s.is_empty());
        let mut parts: Vec<(u64, String)> = list(&dir)
            .into_iter()
            .filter_map(|p| Some((read_u64(&dir.join(&p).join("partition"))?, p)))
            .collect();
        parts.sort();
        let mut volumes = Vec::new();
        // a filesystem on the whole disk (a USB stick without a partition table, a hybrid ISO)
        if parts.is_empty() || u.contains_key("ID_FS_TYPE") {
            volumes.push(self.volume(&dir, name, 0));
        }
        for (_, p) in &parts {
            volumes.push(self.volume(&dir.join(p), p, 0));
        }
        Some(Disk {
            device: format!("/dev/{name}"),
            name: name.to_string(),
            size,
            model,
            serial,
            transport: transport.map(str::to_string),
            rotational: if name.starts_with("loop") {
                None
            } else {
                read(&dir.join("queue/rotational")).map(|r| r == "1")
            },
            removable,
            read_only: read(&dir.join("ro")).as_deref() == Some("1"),
            partition_table: u.get("ID_PART_TABLE_TYPE").cloned(),
            backing_file: backing,
            volumes,
            ..Default::default()
        })
    }

    /// A partition, whole disk or holder at `dir`, with what's stacked on it.
    fn volume(&self, dir: &Path, kname: &str, depth: usize) -> Volume {
        self.volume_in(dir, kname, depth, &mut HashSet::new())
    }

    fn volume_in(
        &self,
        dir: &Path,
        kname: &str,
        depth: usize,
        seen: &mut HashSet<String>,
    ) -> Volume {
        let u = self.udev(dir);
        let get = |k: &str| {
            u.as_ref()
                .and_then(|u| u.get(k))
                .filter(|v| !v.is_empty())
                .cloned()
        };
        let dm_name = read(&dir.join("dm/name"));
        let partition = read_u64(&dir.join("partition")).map(|n| n as u32);
        let mut v = Volume {
            device: match &dm_name {
                Some(n) => format!("/dev/mapper/{n}"),
                None => format!("/dev/{kname}"),
            },
            name: dm_name.clone().unwrap_or_else(|| kname.to_string()),
            size: read_u64(&dir.join("size")).unwrap_or(0) * 512,
            partition,
            part_type: get("ID_PART_ENTRY_TYPE")
                .and_then(|t| magic::part_type_name(&t))
                .map(str::to_string),
            part_label: get("ID_PART_ENTRY_NAME").map(|s| unescape_hex(&s)),
            fs: get("ID_FS_TYPE"),
            label: get("ID_FS_LABEL_ENC")
                .map(|s| unescape_hex(&s))
                .or_else(|| get("ID_FS_LABEL"))
                .filter(|l| !l.is_empty()),
            uuid: get("ID_FS_UUID"),
            ..Default::default()
        };
        v.hint.uuid_sub = get("ID_FS_UUID_SUB");
        if v.fs.is_none() {
            match sniff(&self.r.dev.join(kname)) {
                Some(Sniff::Fs(fs)) => {
                    v.fs = Some(fs.id.to_string());
                    v.label = v.label.or(fs.label);
                    if fs.id == "vfat" {
                        v.fs_name = Some(fs.kind);
                    }
                }
                Some(Sniff::Blank) => v.hint.blank = true,
                Some(Sniff::Data) => {}
                // blkid looked (udev has a record) and found no signature
                None => v.hint.blank = u.is_some(),
            }
        } else if v.fs.as_deref() == Some("vfat") {
            v.fs_name = get("ID_FS_VERSION"); // FAT12/16/32
        }
        v.mounts = self.mounts_of(
            read(&dir.join("dev")).as_deref(),
            kname,
            &v,
            dm_name.as_deref(),
        );
        let raw_holders = list(&dir.join("holders"));
        let unlocked = raw_holders
            .iter()
            .any(|h| read(&self.sys(h).join("dm/uuid")).is_some_and(|id| id.starts_with("CRYPT-")));
        match v.fs.as_deref() {
            Some("crypto_LUKS") => {
                v.encryption = Some(Encryption {
                    scheme: format!("LUKS{}", get("ID_FS_VERSION").unwrap_or_default()),
                    unlocked,
                })
            }
            Some("BitLocker") => {
                v.encryption = Some(Encryption {
                    scheme: "BitLocker".into(),
                    unlocked,
                })
            }
            Some("LVM2_member") if raw_holders.is_empty() => {
                v.note = Some("LVM physical volume; its volume group isn't active".into())
            }
            Some("LVM2_member") => v.note = Some("LVM physical volume".into()),
            Some("linux_raid_member") => {
                v.note = Some(format!(
                    "RAID member{}{}",
                    v.label
                        .as_deref()
                        .map(|l| format!(" of {l}"))
                        .unwrap_or_default(),
                    if raw_holders.is_empty() {
                        "; array not assembled"
                    } else {
                        ""
                    }
                ))
            }
            Some("zfs_member") => {
                v.note = Some(format!(
                    "ZFS pool{}{}",
                    v.label
                        .as_deref()
                        .map(|l| format!(" {l}"))
                        .unwrap_or_default(),
                    if v.mounts.is_empty() {
                        "; not imported"
                    } else {
                        ""
                    }
                ))
            }
            Some("apfs") => v.note = Some("Linux reads APFS only with apfs-fuse".into()),
            Some("refs") => v.note = Some("Linux can't read ReFS".into()),
            _ => {}
        }
        if self.swaps.contains(kname) {
            v.hint.system = true;
            v.note = Some("swap in use".into());
        }
        // Apple silicon's own boot and recovery containers (Asahi Linux keeps them beside Linux)
        if matches!(
            v.part_type.as_deref(),
            Some("Apple Silicon boot" | "Apple Silicon recovery")
        ) {
            v.hint.system = true;
        }
        if depth == 0
            && partition.is_none()
            && v.fs.is_none()
            && let Some(table) = get("ID_PART_TABLE_TYPE")
        {
            v.note = Some(format!("{table} partition table without partitions"));
            v.hint.blank = true;
        }
        if depth < 6 {
            v.holders = self.holders(&raw_holders, depth, seen);
        }
        v
    }

    fn holders(&self, names: &[String], depth: usize, seen: &mut HashSet<String>) -> Vec<Volume> {
        let mut out = Vec::new();
        for h in names {
            if !seen.insert(h.clone()) {
                continue;
            }
            let dir = self.sys(h);
            // LVM's internal layers (thin-pool data and metadata, RAID legs) aren't anything to
            // read: show what's stacked on them instead
            let internal = self.udev(&dir).is_some_and(|u| {
                u.get("DM_LV_LAYER").is_some_and(|l| !l.is_empty())
                    || u.get("DM_UDEV_DISABLE_OTHER_RULES_FLAG")
                        .is_some_and(|f| f == "1")
            });
            if internal {
                if depth < 6 {
                    out.extend(self.holders(&list(&dir.join("holders")), depth + 1, seen));
                }
                continue;
            }
            out.push(self.volume_in(&dir, h, depth + 1, seen));
        }
        out
    }

    fn mounts_of(
        &self,
        dev: Option<&str>,
        kname: &str,
        v: &Volume,
        dm_name: Option<&str>,
    ) -> Vec<Mount> {
        let mut out: Vec<Mount> = Vec::new();
        for m in &self.mounts {
            let src = m.source.as_str();
            let hit = dev == Some(m.dev.as_str())
                || src.strip_prefix("/dev/") == Some(kname)
                || dm_name.is_some_and(|n| src.strip_prefix("/dev/mapper/") == Some(n))
                || v.uuid
                    .as_deref()
                    .is_some_and(|u| src.strip_prefix("/dev/disk/by-uuid/") == Some(u))
                // ZFS datasets are mounted by pool name, which udev reports as the label
                || (v.fs.as_deref() == Some("zfs_member")
                    && m.fs == "zfs"
                    && v.label.as_deref().is_some_and(|pool| {
                        src == pool || src.strip_prefix(pool).is_some_and(|r| r.starts_with('/'))
                    }));
            if hit && !out.iter().any(|o| o.path == m.path) {
                out.push(Mount {
                    path: m.path.clone(),
                    read_only: m.ro,
                });
            }
        }
        out
    }
}

/// A btrfs filesystem spanning several devices is mounted through one of them; every device of it
/// is in use then. Unmounted, each is a member (a dd clone shares the filesystem UUID but not the
/// per-device one, so clones aren't mistaken for members).
fn multi_device_btrfs(disks: &mut [Disk]) {
    let mut groups: HashMap<String, (HashSet<String>, Vec<Mount>)> = HashMap::new();
    super::each_volume(disks, &mut |v| {
        if let (Some("btrfs"), Some(uuid)) = (v.fs.as_deref(), &v.uuid) {
            let g = groups.entry(uuid.clone()).or_default();
            g.0.extend(v.hint.uuid_sub.clone());
            for m in &v.mounts {
                if !g.1.contains(m) {
                    g.1.push(m.clone());
                }
            }
        }
    });
    super::each_volume(disks, &mut |v| {
        if let (Some("btrfs"), Some(uuid)) = (v.fs.as_deref(), &v.uuid)
            && let Some((devices, mounts)) = groups.get(uuid)
            && devices.len() > 1
        {
            v.mounts = mounts.clone();
            v.hint.member = mounts.is_empty();
            v.note = Some(format!("btrfs across {} devices", devices.len()));
        }
    });
}

/// Whether anything on the disk belongs to the running system (before classification).
fn holds_system(d: &mut Disk) -> bool {
    let mut sys = false;
    super::each_volume(std::slice::from_mut(d), &mut |v| sys |= v.hint.system);
    sys
}

/// The firmware starts this machine from an EFI system (or BIOS boot) partition on the system disk,
/// whether or not Linux has it mounted (systemd often mounts the ESP only on access): it's the
/// system's too.
fn boot_partitions(disks: &mut [Disk]) {
    for d in disks {
        if holds_system(d) {
            for v in &mut d.volumes {
                if matches!(v.part_type.as_deref(), Some("EFI system" | "BIOS boot")) {
                    v.hint.system = true;
                }
            }
        }
    }
}

/// The other namespaces of the NVMe drive the system runs from (Apple's firmware namespaces on
/// Asahi Linux, say) carry no filesystem, but they aren't free space either: whatever in them has
/// no signature counts as the system's.
fn system_ssd_namespaces(p: &Probe, disks: &mut [Disk]) {
    let controller = |name: &str| fs::canonicalize(p.sys(name).join("device")).ok();
    let mut system = HashSet::new();
    let mut others = Vec::new();
    for d in disks.iter_mut().filter(|d| d.name.starts_with("nvme")) {
        match (holds_system(d), controller(&d.name)) {
            (true, Some(c)) => {
                system.insert(c);
            }
            (false, Some(c)) => others.push((d, c)),
            _ => {}
        }
    }
    for (d, c) in others {
        if !system.contains(&c) {
            continue;
        }
        for v in &mut d.volumes {
            if v.fs.is_none() && v.mounts.is_empty() && v.holders.is_empty() {
                v.hint.system = true;
                v.note = Some("another namespace of the system SSD".into());
            }
        }
    }
}

/// How a disk is attached, from where it sits in the device tree (the sysfs link), else udev.
fn transport(name: &str, link: &str, u: &Udev) -> Option<&'static str> {
    let bus = u.get("ID_BUS").map_or("", String::as_str);
    let path = u.get("ID_PATH").map_or("", String::as_str);
    Some(if name.starts_with("loop") {
        "loop"
    } else if link.contains("/usb") || bus == "usb" || path.contains("-usb-") {
        "usb"
    } else if name.starts_with("nvme") || link.contains("/nvme") || path.contains("-nvme-") {
        "nvme"
    } else if name.starts_with("mmcblk") || link.contains("/mmc") {
        "mmc"
    } else if name.starts_with("vd") || link.contains("/virtio") {
        "virtio"
    } else if link.contains("/ata") || u.contains_key("ID_ATA_SATA") || path.contains("-ata-") {
        "sata"
    } else if bus == "ata" {
        "ata"
    } else if link.contains("/firewire") || bus == "ieee1394" {
        "firewire"
    } else if link.contains("/end_device-") || path.contains("-sas-") {
        "sas"
    } else if name.starts_with("nbd") {
        "nbd"
    } else if name.starts_with("xvd") {
        "xen"
    } else if bus == "scsi" || link.contains("/host") {
        "scsi"
    } else {
        return None;
    })
}

enum Sniff {
    Fs(magic::Fs),
    /// All zeros where a signature would be.
    Blank,
    /// Something, but nothing recognised.
    Data,
}

/// Look for a signature where filesystems keep theirs and, finding none, whether the first 64 KiB
/// are blank; if the node can be opened at all (usually only as root or in the `disk` group). Only
/// block devices and plain files are opened: anything else could block.
fn sniff(node: &Path) -> Option<Sniff> {
    let meta = fs::metadata(node).ok()?;
    #[cfg(unix)]
    let openable = {
        use std::os::unix::fs::FileTypeExt;
        meta.is_file() || meta.file_type().is_block_device()
    };
    #[cfg(not(unix))]
    let openable = meta.is_file();
    if !openable {
        return None;
    }
    let f = fs::File::open(node).ok()?;
    if let Some(fs) = magic::probe_fs(&f, 0).ok()? {
        return Some(Sniff::Fs(fs));
    }
    let head = magic::read_at(&f, 0, 64 << 10).ok()?;
    Some(if head.iter().all(|&b| b == 0) {
        Sniff::Blank
    } else {
        Sniff::Data
    })
}

fn read_mountinfo(path: &Path) -> Vec<MountEntry> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            // id parent major:minor root mount-point options [optional…] - fstype source super-options
            let f: Vec<&str> = line.split(' ').collect();
            let sep = 6 + f.get(6..)?.iter().position(|x| *x == "-")?;
            let ro = |opts: &str| opts.split(',').any(|o| o == "ro");
            Some(MountEntry {
                dev: f.get(2)?.to_string(),
                path: unescape_octal(f.get(4)?),
                fs: f.get(sep + 1)?.to_string(),
                source: unescape_octal(f.get(sep + 2)?),
                ro: ro(f.get(5)?) || ro(f.get(sep + 3).unwrap_or(&"")),
            })
        })
        .collect()
}

/// Kernel names of the swap devices in use (swap files are no volume's business).
fn read_swaps(path: &Path, dm_names: &HashMap<String, String>) -> HashSet<String> {
    let Ok(text) = fs::read_to_string(path) else {
        return HashSet::new();
    };
    text.lines()
        .skip(1)
        .filter_map(|l| {
            let dev = unescape_octal(l.split_whitespace().next()?);
            let name = dev.strip_prefix("/dev/")?;
            Some(match name.strip_prefix("mapper/") {
                Some(dm) => dm_names.get(dm).cloned().unwrap_or_else(|| dm.to_string()),
                None => name.to_string(),
            })
        })
        .collect()
}

/// mountinfo's escapes: `\040` for a space, `\011` tab, `\012` newline, `\134` backslash.
fn unescape_octal(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && let Some(n) = b
                .get(i + 1..i + 4)
                .and_then(|d| std::str::from_utf8(d).ok())
                .and_then(|d| u8::from_str_radix(d, 8).ok())
        {
            out.push(n);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// udev's `_ENC` values: unsafe bytes as `\xNN`.
fn unescape_hex(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && b.get(i + 1) == Some(&b'x')
            && let Some(n) = b
                .get(i + 2..i + 4)
                .and_then(|d| std::str::from_utf8(d).ok())
                .and_then(|d| u8::from_str_radix(d, 16).ok())
        {
            out.push(n);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).trim().to_string()
}

fn udev_text(u: &Udev, key: &str) -> Option<String> {
    u.get(key)
        .map(|v| unescape_hex(v))
        .filter(|v| !v.is_empty())
}

/// A sysfs attribute, trimmed; None if missing or empty.
fn read(path: &Path) -> Option<String> {
    let s = fs::read_to_string(path).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

fn read_u64(path: &Path) -> Option<u64> {
    read(path)?.parse().ok()
}

/// Entry names in a directory, sorted; empty if it can't be read.
fn list(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort_by_key(|n| super::natural(n));
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::volumes::State;

    fn paths(v: &Volume) -> Vec<&str> {
        v.mounts.iter().map(|m| m.path.as_str()).collect()
    }

    #[test]
    fn multi_device_btrfs_and_its_clones() {
        let dev = |sub: &str, mount: Option<&str>| {
            let mut v = Volume {
                fs: Some("btrfs".into()),
                uuid: Some("feedface-0000-4000-8000-000000000001".into()),
                mounts: mount
                    .map(|p| Mount {
                        path: p.into(),
                        read_only: false,
                    })
                    .into_iter()
                    .collect(),
                ..Default::default()
            };
            v.hint.uuid_sub = Some(sub.into());
            v
        };
        let mut disks = vec![Disk {
            volumes: vec![dev("a", Some("/srv/pool")), dev("b", None)],
            ..Default::default()
        }];
        multi_device_btrfs(&mut disks);
        crate::volumes::finish(&mut disks);
        let v = &disks[0].volumes;
        assert_eq!(paths(&v[1]), ["/srv/pool"]);
        assert_eq!(v[1].state, State::Mounted);
        assert_eq!(v[1].note.as_deref(), Some("btrfs across 2 devices"));
        // a dd clone of one filesystem: same UUIDs, two separate filesystems
        let mut disks = vec![Disk {
            volumes: vec![dev("a", Some("/mnt/original")), dev("a", None)],
            ..Default::default()
        }];
        multi_device_btrfs(&mut disks);
        crate::volumes::finish(&mut disks);
        assert_eq!(disks[0].volumes[1].state, State::Mountable);
    }

    #[test]
    fn escapes() {
        assert_eq!(unescape_octal(r"/media/a\040b\134c"), r"/media/a b\c");
        assert_eq!(unescape_hex(r"My\x20Photos\x20\x20"), "My Photos");
        assert_eq!(unescape_hex(r"caf\xc3\xa9"), "café");
        assert_eq!(unescape_hex(r"bad\x2"), r"bad\x2");
    }

    /// The whole prober over a fake machine: sysfs has symlinks and ':' in its names, so Unix
    /// only.
    #[cfg(unix)]
    mod fake_machine {
        use super::*;

        /// A fake /sys, udev database, /proc and /dev. Every name, serial and label is invented.
        struct Tree(tempfile::TempDir);

        impl Tree {
            fn put(&self, rel: &str, content: &str) {
                let p = self.0.path().join(rel);
                fs::create_dir_all(p.parent().unwrap()).unwrap();
                fs::write(p, content).unwrap();
            }

            /// A block device's sysfs attributes, under its place in the device tree.
            fn attrs(&self, devpath: &str, attrs: &[(&str, &str)]) {
                for (k, v) in attrs {
                    self.put(&format!("sys/devices/{devpath}/{k}"), &format!("{v}\n"));
                }
            }

            /// /sys/block/<name>: a symlink into the device tree, as the kernel makes it.
            fn block(&self, name: &str, devpath: &str) {
                let link = self.0.path().join("sys/block").join(name);
                fs::create_dir_all(link.parent().unwrap()).unwrap();
                std::os::unix::fs::symlink(format!("../devices/{devpath}"), &link).unwrap();
            }

            fn udev(&self, dev: &str, lines: &[&str]) {
                let mut text = String::from("I:1790000000\n");
                for l in lines {
                    text += &format!("E:{l}\n");
                }
                text += "G:systemd\nQ:systemd\nV:1\n";
                self.put(&format!("udev/b{dev}"), &text);
            }

            fn part(&self, disk: &str, name: &str, n: u32, dev: &str, sectors: u64, udev: &[&str]) {
                self.attrs(
                    &format!("{disk}/{name}"),
                    &[
                        ("dev", dev),
                        ("size", &sectors.to_string()),
                        ("partition", &n.to_string()),
                    ],
                );
                if !udev.is_empty() {
                    self.udev(dev, udev);
                }
            }

            fn holder(&self, disk: &str, part: &str, holder: &str) {
                self.put(&format!("sys/devices/{disk}/{part}/holders/{holder}"), "");
            }

            /// A device-mapper device.
            fn dm(&self, n: u32, name: &str, uuid: &str, sectors: u64, udev: &[&str]) {
                let path = format!("virtual/block/dm-{n}");
                let dev = format!("253:{n}");
                self.attrs(
                    &path,
                    &[
                        ("dev", &dev),
                        ("size", &sectors.to_string()),
                        ("dm/name", name),
                        ("dm/uuid", uuid),
                    ],
                );
                fs::create_dir_all(
                    self.0
                        .path()
                        .join("sys/devices")
                        .join(&path)
                        .join("holders"),
                )
                .unwrap();
                let mut lines = vec![format!("DM_NAME={name}"), format!("DM_UUID={uuid}")];
                lines.extend(udev.iter().map(|l| l.to_string()));
                self.udev(&dev, &lines.iter().map(String::as_str).collect::<Vec<_>>());
                self.block(&format!("dm-{n}"), &path);
            }

            fn roots(&self) -> Roots {
                let p = self.0.path();
                Roots {
                    sys: p.join("sys"),
                    udev: p.join("udev"),
                    mountinfo: p.join("proc/mountinfo"),
                    swaps: p.join("proc/swaps"),
                    dev: p.join("dev"),
                }
            }
        }

        fn strs(v: &[String]) -> Vec<&str> {
            v.iter().map(String::as_str).collect()
        }

        const NVME: &str = "pci0000:00/0000:00:1d.0/0000:3c:00.0/nvme/nvme0/nvme0n1";
        const SATA: &str = "pci0000:00/0000:00:17.0/ata2/host1/target1:0:0/1:0:0:0/block/sda";
        const USB: &str =
            "pci0000:00/0000:00:14.0/usb2/2-1/2-1:1.0/host4/target4:0:0/4:0:0:0/block/sdb";

        fn machine() -> Tree {
            let t = Tree(tempfile::tempdir().unwrap());

            // The system NVMe: EFI, btrfs root (subvolumes at / and /var), swap in use, and /home
            // on LUKS.
            t.attrs(
                NVME,
                &[
                    ("dev", "259:0"),
                    ("size", "1953525168"),
                    ("removable", "0"),
                    ("ro", "0"),
                    ("queue/rotational", "0"),
                    ("device/model", "Fakedisk NV900 1TB                      "),
                    ("device/serial", "FNV0000000001       "),
                ],
            );
            t.udev(
                "259:0",
                &[
                    "ID_SERIAL_SHORT=FNV0000000001",
                    "ID_MODEL=Fakedisk NV900 1TB",
                    "ID_PATH=pci-0000:3c:00.0-nvme-1",
                    "ID_PART_TABLE_UUID=0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0",
                    "ID_PART_TABLE_TYPE=gpt",
                ],
            );
            let gpt = |n: &str, t: &str| {
                [
                    "ID_PART_TABLE_TYPE=gpt".to_string(),
                    "ID_PART_ENTRY_SCHEME=gpt".to_string(),
                    format!("ID_PART_ENTRY_NUMBER={n}"),
                    format!("ID_PART_ENTRY_TYPE={t}"),
                ]
            };
            let with = |base: [String; 4], more: &[&str]| -> Vec<String> {
                base.into_iter()
                    .chain(more.iter().map(|s| s.to_string()))
                    .collect()
            };
            let p1 = with(
                gpt("1", "c12a7328-f81f-11d2-ba4b-00a0c93ec93b"),
                &[
                    "ID_FS_TYPE=vfat",
                    "ID_FS_VERSION=FAT32",
                    "ID_FS_LABEL=EFI",
                    "ID_FS_LABEL_ENC=EFI",
                    "ID_FS_UUID=1A2B-3C4D",
                    "ID_FS_USAGE=filesystem",
                    r"ID_PART_ENTRY_NAME=EFI\x20System\x20Partition",
                ],
            );
            t.part(NVME, "nvme0n1p1", 1, "259:1", 1_048_576, &strs(&p1));
            let p2 = with(
                gpt("2", "0fc63daf-8483-4772-8e79-3d69d8477de4"),
                &[
                    "ID_FS_TYPE=btrfs",
                    "ID_FS_LABEL=fedora",
                    "ID_FS_LABEL_ENC=fedora",
                    "ID_FS_UUID=6c1f9a52-8d0e-4b7a-9f3c-2e5d7a1b0c94",
                    "ID_FS_UUID_SUB=0d2e4f6a-1b3c-4d5e-8f90-a1b2c3d4e5f6",
                ],
            );
            t.part(NVME, "nvme0n1p2", 2, "259:2", 1_600_000_000, &strs(&p2));
            let p3 = with(
                gpt("3", "0657fd6d-a4ab-43c4-84e5-0933c84b4f4f"),
                &[
                    "ID_FS_TYPE=swap",
                    "ID_FS_VERSION=1",
                    "ID_FS_UUID=a0b1c2d3-e4f5-4a6b-8c7d-9e0f1a2b3c4d",
                ],
            );
            t.part(NVME, "nvme0n1p3", 3, "259:3", 16_777_216, &strs(&p3));
            let p4 = with(
                gpt("4", "ca7d7ccb-63ed-4c53-861c-1742536059cc"),
                &[
                    "ID_FS_TYPE=crypto_LUKS",
                    "ID_FS_VERSION=2",
                    "ID_FS_USAGE=crypto",
                    "ID_FS_UUID=5e1d0c9b-8a7f-4e3d-a2c1-b0a9f8e7d6c5",
                ],
            );
            t.part(NVME, "nvme0n1p4", 4, "259:4", 330_000_000, &strs(&p4));
            t.holder(NVME, "nvme0n1p4", "dm-0");
            t.dm(
                0,
                "luks-home",
                "CRYPT-LUKS2-5e1d0c9b8a7f4e3da2c1b0a9f8e7d6c5-luks-home",
                329_990_000,
                &[
                    "ID_FS_TYPE=ext4",
                    "ID_FS_LABEL=home",
                    "ID_FS_UUID=77aa88bb-99cc-4dd0-8ee1-ff2233445566",
                ],
            );
            let p5 = gpt("5", "21686148-6449-6e6f-744e-656564454649");
            t.part(NVME, "nvme0n1p5", 5, "259:5", 2048, &strs(&p5));
            t.block("nvme0n1", NVME);

            // A SATA disk: LUKS unlocked and mounted, an LVM PV (with a thin-pool layer), a partition
            // with nothing on it, and one udev never described whose node happens to be readable.
            t.attrs(
                SATA,
                &[
                    ("dev", "8:0"),
                    ("size", "7814037168"),
                    ("removable", "0"),
                    ("ro", "0"),
                    ("queue/rotational", "1"),
                    ("device/vendor", "ATA     "),
                    ("device/model", "FakeSpin 4000   "),
                ],
            );
            t.udev(
                "8:0",
                &[
                    "ID_ATA=1",
                    "ID_ATA_SATA=1",
                    "ID_BUS=ata",
                    "ID_MODEL=FakeSpin_4000",
                    r"ID_MODEL_ENC=FakeSpin\x204000\x20\x20\x20",
                    "ID_SERIAL=FakeSpin_4000_FS4K00000042",
                    "ID_SERIAL_SHORT=FS4K00000042",
                    "ID_PATH=pci-0000:00:17.0-ata-2",
                    "ID_PART_TABLE_TYPE=gpt",
                ],
            );
            let a1 = with(
                gpt("1", "ca7d7ccb-63ed-4c53-861c-1742536059cc"),
                &[
                    "ID_FS_TYPE=crypto_LUKS",
                    "ID_FS_VERSION=2",
                    "ID_FS_UUID=0c0ffee0-1111-4222-8333-444455556666",
                ],
            );
            t.part(SATA, "sda1", 1, "8:1", 2_000_000_000, &strs(&a1));
            t.holder(SATA, "sda1", "dm-1");
            t.dm(
                1,
                "vault",
                "CRYPT-LUKS2-0c0ffee011114222833344445555666-vault",
                1_999_990_000,
                &["ID_FS_TYPE=ext4", "ID_FS_LABEL=vault"],
            );
            let a2 = with(
                gpt("2", "e6d6d379-f507-44c2-a23c-238f2a3df928"),
                &["ID_FS_TYPE=LVM2_member", "ID_FS_VERSION=LVM2 001"],
            );
            t.part(SATA, "sda2", 2, "8:2", 4_000_000_000, &strs(&a2));
            t.holder(SATA, "sda2", "dm-2");
            t.holder(SATA, "sda2", "dm-3");
            t.dm(
                2,
                "vg0-data",
                "LVM-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaabbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                3_000_000_000,
                &[
                    "DM_VG_NAME=vg0",
                    "DM_LV_NAME=data",
                    "ID_FS_TYPE=xfs",
                    "ID_FS_LABEL=data",
                ],
            );
            t.put("sys/devices/virtual/block/dm-3/holders/dm-4", "");
            t.dm(
                3,
                "vg0-pool_tdata",
                "LVM-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaacccccccccccccccccccccccccccccccc-tdata",
                900_000_000,
                &[
                    "DM_VG_NAME=vg0",
                    "DM_LV_NAME=pool",
                    "DM_LV_LAYER=tdata",
                    "DM_UDEV_DISABLE_OTHER_RULES_FLAG=1",
                ],
            );
            t.dm(
                4,
                "vg0-scratch",
                "LVM-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaadddddddddddddddddddddddddddddddd",
                800_000_000,
                &["ID_FS_TYPE=ext4", "ID_FS_LABEL=scratch"],
            );
            let a3 = gpt("3", "0fc63daf-8483-4772-8e79-3d69d8477de4");
            t.part(SATA, "sda3", 3, "8:3", 1_000_000_000, &strs(&a3));
            t.part(SATA, "sda4", 4, "8:4", 800_000_000, &[]);
            // its device node: an ext4 superblock, 4 KiB blocks, labelled "rescued"
            let mut img = vec![0u8; 64 << 10];
            let sb = 1024;
            img[sb + 4..sb + 8].copy_from_slice(&1024u32.to_le_bytes());
            img[sb + 24] = 2;
            img[sb + 56..sb + 58].copy_from_slice(&0xEF53u16.to_le_bytes());
            img[sb + 96] = 0x40;
            img[sb + 120..sb + 127].copy_from_slice(b"rescued");
            fs::create_dir_all(t.0.path().join("dev")).unwrap();
            fs::write(t.0.path().join("dev/sda4"), &img).unwrap();
            t.block("sda", SATA);

            // A USB disk: ext4 mounted read-only (a space in its mount point), NTFS not mounted.
            t.attrs(
                USB,
                &[
                    ("dev", "8:16"),
                    ("size", "3907029168"),
                    ("removable", "0"),
                    ("ro", "0"),
                    ("queue/rotational", "1"),
                    ("device/vendor", "Fakeco  "),
                    ("device/model", "Portable 2TB    "),
                ],
            );
            t.udev(
                "8:16",
                &[
                    "ID_BUS=usb",
                    "ID_USB_DRIVER=uas",
                    "ID_VENDOR=Fakeco",
                    "ID_MODEL=Portable_2TB",
                    "ID_SERIAL_SHORT=FAKE0000BEEF",
                    "ID_PART_TABLE_TYPE=dos",
                ],
            );
            t.part(
                USB,
                "sdb1",
                1,
                "8:17",
                2_000_000_000,
                &[
                    "ID_FS_TYPE=ext4",
                    "ID_FS_LABEL=My_Photos",
                    r"ID_FS_LABEL_ENC=My\x20Photos",
                    "ID_PART_ENTRY_TYPE=0x83",
                ],
            );
            t.part(
                USB,
                "sdb2",
                2,
                "8:18",
                1_900_000_000,
                &[
                    "ID_FS_TYPE=ntfs",
                    "ID_FS_LABEL=Old_Windows",
                    r"ID_FS_LABEL_ENC=Old\x20Windows",
                    "ID_PART_ENTRY_TYPE=0x7",
                ],
            );
            t.block("sdb", USB);

            // not disks: a snap, zram swap, a card reader without a card; a disk image is one
            t.attrs(
                "virtual/block/loop0",
                &[
                    ("dev", "7:0"),
                    ("size", "131072"),
                    ("loop/backing_file", "/var/lib/snapd/snaps/core22_1111.snap"),
                ],
            );
            t.block("loop0", "virtual/block/loop0");
            t.attrs(
                "virtual/block/loop1",
                &[
                    ("dev", "7:1"),
                    ("size", "2097152"),
                    ("loop/backing_file", "/home/test/images/old-laptop.img"),
                ],
            );
            t.udev("7:1", &["ID_FS_TYPE=ext4", "ID_FS_LABEL=laptop"]);
            t.block("loop1", "virtual/block/loop1");
            t.attrs(
                "virtual/block/zram0",
                &[("dev", "251:0"), ("size", "16777216")],
            );
            t.block("zram0", "virtual/block/zram0");
            let reader =
                "pci0000:00/0000:00:14.0/usb1/1-3/1-3:1.0/host5/target5:0:0/5:0:0:0/block/sdc";
            t.attrs(
                reader,
                &[("dev", "8:32"), ("size", "0"), ("removable", "1")],
            );
            t.block("sdc", reader);

            t.put(
                "proc/mountinfo",
                &[
                    "23 1 0:31 /@ / rw,relatime shared:1 - btrfs /dev/nvme0n1p2 rw,seclabel,compress=zstd:1,ssd,subvolid=257,subvol=/@",
                    "24 23 0:31 /@var /var rw,relatime shared:2 - btrfs /dev/nvme0n1p2 rw,seclabel,compress=zstd:1,ssd,subvolid=258,subvol=/@var",
                    "25 23 0:5 / /dev rw,nosuid shared:3 - devtmpfs devtmpfs rw,size=4096k,mode=755",
                    "26 23 259:1 / /boot/efi rw,relatime shared:4 - vfat /dev/nvme0n1p1 rw,fmask=0077,dmask=0077",
                    "27 23 253:0 / /home rw,relatime shared:5 - ext4 /dev/mapper/luks-home rw,seclabel",
                    "40 23 8:17 / /media/test/My\\040Photos ro,nosuid,nodev,relatime shared:6 - ext4 /dev/sdb1 ro,seclabel",
                    "41 23 253:1 / /mnt/vault rw,relatime shared:7 - ext4 /dev/mapper/vault rw",
                    "42 23 0:45 / /tmp rw,nosuid,nodev shared:8 - tmpfs tmpfs rw,nr_inodes=1048576",
                    "",
                ]
                .join("\n"),
            );
            t.put(
                "proc/swaps",
                &[
                    "Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority",
                    "/dev/nvme0n1p3                          partition\t8388604\t\t0\t\t-2",
                    "/dev/zram0                              partition\t8388604\t\t0\t\t100",
                    "",
                ]
                .join("\n"),
            );
            t
        }

        fn find<'a>(vs: &'a [Volume], name: &str) -> &'a Volume {
            vs.iter()
                .find(|v| v.name == name)
                .unwrap_or_else(|| panic!("no volume {name}"))
        }

        #[test]
        fn a_machine_with_every_kind_of_volume() {
            let t = machine();
            let (disks, notes) = probe(&t.roots()).unwrap();
            assert!(notes.is_empty(), "{notes:?}");
            let names: Vec<&str> = disks.iter().map(|d| d.name.as_str()).collect();
            assert_eq!(names, ["nvme0n1", "loop1", "sda", "sdb"]);

            let nvme = &disks[0];
            assert_eq!(
                (
                    nvme.model.as_deref(),
                    nvme.serial.as_deref(),
                    nvme.size,
                    nvme.partition_table.as_deref()
                ),
                (
                    Some("Fakedisk NV900 1TB"),
                    Some("FNV0000000001"),
                    1_953_525_168 * 512,
                    Some("gpt")
                )
            );
            assert_eq!(
                (
                    nvme.transport.as_deref(),
                    nvme.rotational,
                    nvme.removable,
                    nvme.system
                ),
                (Some("nvme"), Some(false), false, true)
            );
            let v = &nvme.volumes;
            assert!(v.iter().all(|v| v.state == State::System));
            let efi = find(v, "nvme0n1p1");
            assert_eq!(
                (
                    efi.fs.as_deref(),
                    efi.fs_name.as_deref(),
                    efi.label.as_deref(),
                    efi.part_type.as_deref(),
                    efi.part_label.as_deref()
                ),
                (
                    Some("vfat"),
                    Some("FAT32"),
                    Some("EFI"),
                    Some("EFI system"),
                    Some("EFI System Partition")
                )
            );
            assert_eq!(paths(efi), ["/boot/efi"]);
            // btrfs is mounted from a 0:N device: matched by its source instead
            assert_eq!(paths(find(v, "nvme0n1p2")), ["/", "/var"]);
            assert_eq!(find(v, "nvme0n1p3").note.as_deref(), Some("swap in use"));
            let home = find(v, "nvme0n1p4");
            assert_eq!(
                home.encryption,
                Some(Encryption {
                    scheme: "LUKS2".into(),
                    unlocked: true
                })
            );
            assert_eq!(home.holders.len(), 1);
            let h = &home.holders[0];
            assert_eq!(
                (h.name.as_str(), h.device.as_str(), h.fs.as_deref(), h.state),
                (
                    "luks-home",
                    "/dev/mapper/luks-home",
                    Some("ext4"),
                    State::System
                )
            );
            assert_eq!(paths(h), ["/home"]);

            let img = &disks[1];
            assert_eq!(
                (
                    img.transport.as_deref(),
                    img.backing_file.as_deref(),
                    img.rotational
                ),
                (Some("loop"), Some("/home/test/images/old-laptop.img"), None)
            );
            assert_eq!(img.volumes.len(), 1);
            assert_eq!(
                (
                    img.volumes[0].device.as_str(),
                    img.volumes[0].label.as_deref(),
                    img.volumes[0].state
                ),
                ("/dev/loop1", Some("laptop"), State::Mountable)
            );

            let sata = &disks[2];
            assert_eq!(
                (
                    sata.model.as_deref(),
                    sata.serial.as_deref(),
                    sata.transport.as_deref(),
                    sata.rotational,
                    sata.removable,
                    sata.system
                ),
                (
                    Some("FakeSpin 4000"),
                    Some("FS4K00000042"),
                    Some("sata"),
                    Some(true),
                    false,
                    false
                )
            );
            let v = &sata.volumes;
            let vault = find(v, "sda1");
            assert_eq!(vault.state, State::Encrypted);
            assert_eq!(vault.encryption.as_ref().map(|e| e.unlocked), Some(true));
            assert_eq!(
                (vault.holders[0].state, vault.holders[0].read_only),
                (State::Mounted, Some(false))
            );
            assert_eq!(paths(&vault.holders[0]), ["/mnt/vault"]);
            let pv = find(v, "sda2");
            assert_eq!(
                (pv.state, pv.note.as_deref()),
                (State::Member, Some("LVM physical volume"))
            );
            // the thin pool's internal layer is skipped, the thin volume on it shown
            let lvs: Vec<(&str, State)> = pv
                .holders
                .iter()
                .map(|h| (h.name.as_str(), h.state))
                .collect();
            assert_eq!(
                lvs,
                [
                    ("vg0-data", State::Mountable),
                    ("vg0-scratch", State::Mountable)
                ]
            );
            let blank = find(v, "sda3");
            assert_eq!(
                (blank.state, blank.part_type.as_deref()),
                (State::Empty, Some("Linux filesystem"))
            );
            let sniffed = find(v, "sda4");
            assert_eq!(
                (
                    sniffed.fs.as_deref(),
                    sniffed.label.as_deref(),
                    sniffed.state
                ),
                (Some("ext4"), Some("rescued"), State::Mountable)
            );

            let usb = &disks[3];
            assert_eq!(
                (
                    usb.model.as_deref(),
                    usb.serial.as_deref(),
                    usb.transport.as_deref(),
                    usb.removable,
                    usb.partition_table.as_deref()
                ),
                (
                    Some("Fakeco Portable 2TB"),
                    Some("FAKE0000BEEF"),
                    Some("usb"),
                    true,
                    Some("dos")
                )
            );
            let photos = find(&usb.volumes, "sdb1");
            assert_eq!(
                (
                    photos.label.as_deref(),
                    photos.state,
                    photos.read_only,
                    photos.part_type.as_deref()
                ),
                (Some("My Photos"), State::Mounted, Some(true), Some("Linux"))
            );
            assert_eq!(paths(photos), ["/media/test/My Photos"]);
            let win = find(&usb.volumes, "sdb2");
            assert_eq!(
                (
                    win.fs_name.as_deref(),
                    win.label.as_deref(),
                    win.state,
                    win.part_type.as_deref()
                ),
                (
                    Some("NTFS"),
                    Some("Old Windows"),
                    State::Mountable,
                    Some("NTFS / exFAT")
                )
            );

            // what the web UI and --json get
            let j = serde_json::to_value(&disks[2]).unwrap();
            assert_eq!(j["health"], serde_json::Value::Null);
            assert_eq!(j["volumes"][0]["state"], "encrypted");
            assert_eq!(
                j["volumes"][0]["holders"][0]["mounts"][0]["path"],
                "/mnt/vault"
            );
            assert!(j["volumes"][0].get("hint").is_none());
        }

        #[test]
        fn without_udev_only_readable_devices_are_described() {
            let t = machine();
            fs::remove_dir_all(t.0.path().join("udev")).unwrap();
            let (disks, notes) = probe(&t.roots()).unwrap();
            assert_eq!(notes.len(), 1);
            let sda = disks.iter().find(|d| d.name == "sda").unwrap();
            assert_eq!(find(&sda.volumes, "sda4").fs.as_deref(), Some("ext4"));
            // no udev record and no readable node: unknown, not empty
            assert_eq!(find(&sda.volumes, "sda3").state, State::Unknown);
        }
    }
}
