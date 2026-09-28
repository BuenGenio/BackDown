//! macOS: `diskutil` knows every disk, partition and APFS container, and speaks XML property lists.
//!
//! `diskutil list -plist` gives the layout: whole disks with their partitions, and each APFS
//! container (a synthesized disk) with its volumes and the partition it lives on (its physical
//! store). `diskutil apfs list -plist` adds each APFS volume's roles and FileVault state, and
//! `diskutil info -plist <id>` the rest (model, bus, solid state, filesystem, writable). Serial
//! numbers aren't in diskutil at all; `ioreg` has them for internal SATA and NVMe disks.
//!
//! An APFS container isn't a disk of its own in the report: its volumes are the holders of the
//! partition it lives on, the way logical volumes sit on an LVM physical volume on Linux. The
//! running system's volumes are recognised by where they're mounted (`/`, `/System/Volumes/…`); the
//! unmounted ones beside them (Recovery, Update) by their APFS role on the same disk. An old Mac's
//! disk in an enclosure has the same roles but isn't the system here: its Data volume is just data.

use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use serde_json::{Map, Value};

use super::{Disk, Encryption, Mount, Volume};
use crate::magic;

/// APFS roles of the operating system's own volumes.
const SYSTEM_ROLES: [&str; 9] = [
    "System", "Data", "Preboot", "Recovery", "VM", "Update", "xART", "Hardware", "Baseband",
];

/// Partition types that only the machine's firmware and macOS itself use.
const SYSTEM_CONTENT: [&str; 4] = ["EFI", "Apple_APFS_ISC", "Apple_APFS_Recovery", "Apple_Boot"];

/// An XML property list as JSON: dict → object, array → array, integer and real → number,
/// true/false → bool; string, date and data → string (data stays base64).
pub fn plist(xml: &[u8]) -> Result<Value> {
    let mut r = Reader::from_reader(xml);
    loop {
        match r.read_event()? {
            Event::Start(e) if e.local_name().as_ref() == b"plist" => {}
            Event::Start(e) => return value(&mut r, &e),
            Event::Empty(e) => return Ok(empty(&e)),
            Event::Eof => bail!("no property list found"),
            _ => {}
        }
    }
}

fn value(r: &mut Reader<&[u8]>, start: &BytesStart) -> Result<Value> {
    Ok(match start.local_name().as_ref() {
        b"dict" => {
            let mut map = Map::new();
            let mut key = None;
            loop {
                let v = match r.read_event()? {
                    Event::Start(e) if e.local_name().as_ref() == b"key" => {
                        key = Some(text(r, &e)?);
                        continue;
                    }
                    Event::Start(e) => value(r, &e)?,
                    Event::Empty(e) => empty(&e),
                    Event::End(_) => break,
                    Event::Eof => bail!("property list ends inside a dict"),
                    _ => continue,
                };
                if let Some(k) = key.take() {
                    map.insert(k, v);
                }
            }
            Value::Object(map)
        }
        b"array" => {
            let mut items = Vec::new();
            loop {
                match r.read_event()? {
                    Event::Start(e) => items.push(value(r, &e)?),
                    Event::Empty(e) => items.push(empty(&e)),
                    Event::End(_) => break,
                    Event::Eof => bail!("property list ends inside an array"),
                    _ => {}
                }
            }
            Value::Array(items)
        }
        b"integer" => {
            let t = text(r, start)?;
            let t = t.trim();
            t.parse::<i64>()
                .map(Value::from)
                .or_else(|_| t.parse::<u64>().map(Value::from))
                .unwrap_or(Value::Null)
        }
        b"real" => text(r, start)?
            .trim()
            .parse::<f64>()
            .map_or(Value::Null, Value::from),
        b"true" | b"false" => {
            r.read_to_end(start.name())?;
            Value::Bool(start.local_name().as_ref() == b"true")
        }
        _ => Value::String(text(r, start)?),
    })
}

fn empty(e: &BytesStart) -> Value {
    match e.local_name().as_ref() {
        b"true" => Value::Bool(true),
        b"false" => Value::Bool(false),
        b"dict" => Value::Object(Map::new()),
        b"array" => Value::Array(Vec::new()),
        b"string" | b"data" | b"date" | b"key" => Value::String(String::new()),
        _ => Value::Null,
    }
}

/// An element's text, entities resolved.
fn text(r: &mut Reader<&[u8]>, start: &BytesStart) -> Result<String> {
    let raw = r.read_text(start.name())?;
    let s = raw.decode()?;
    Ok(match quick_xml::escape::unescape(&s) {
        Ok(t) => t.into_owned(),
        Err(_) => s.into_owned(),
    })
}

/// What diskutil (and ioreg) said, parsed.
#[derive(Debug, Default)]
pub struct Diskutil {
    /// `diskutil list -plist`
    pub list: Value,
    /// `diskutil apfs list -plist` (Null if it failed)
    pub apfs: Value,
    /// `diskutil info -plist <id>` by device identifier
    pub info: HashMap<String, Value>,
    /// whole-disk identifier → serial number, from `ioreg`
    pub serials: HashMap<String, String>,
}

/// The identifiers to ask `diskutil info` about: whole disks, partitions and APFS volumes.
pub fn identifiers(list: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    for e in items(&list["AllDisksAndPartitions"]) {
        ids.push(s(e, "DeviceIdentifier").to_string());
        for p in items(&e["Partitions"])
            .into_iter()
            .chain(items(&e["APFSVolumes"]))
        {
            ids.push(s(p, "DeviceIdentifier").to_string());
        }
    }
    ids.retain(|i| !i.is_empty());
    ids
}

/// Whole-disk identifier → serial, from `ioreg -a -r -c IOBlockStorageDevice`: each storage
/// device's "Device Characteristics" carry its serial, and its media descendants their BSD names.
pub fn ioreg_serials(tree: &Value) -> HashMap<String, String> {
    fn walk(v: &Value, serial: Option<&str>, out: &mut HashMap<String, String>) {
        let here = v["Device Characteristics"]["Serial Number"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or(serial);
        if let (Some(bsd), Some(sn), true) = (v["BSD Name"].as_str(), here, v["Whole"] == true) {
            out.insert(bsd.to_string(), sn.to_string());
        }
        for c in items(&v["IORegistryEntryChildren"]) {
            walk(c, here, out);
        }
    }
    let mut out = HashMap::new();
    for root in items(tree) {
        walk(root, None, &mut out);
    }
    out
}

/// The disks as diskutil describes them, classified.
pub fn from_diskutil(d: &Diskutil) -> (Vec<Disk>, Vec<String>) {
    let entries = items(&d.list["AllDisksAndPartitions"]);
    let is_container =
        |e: &Value| e.get("APFSVolumes").is_some() || e.get("APFSPhysicalStores").is_some();
    // physical store (a partition) → the APFS container on it
    let mut store_of: HashMap<&str, &Value> = HashMap::new();
    for c in entries.iter().copied().filter(|e| is_container(e)) {
        for ps in items(&c["APFSPhysicalStores"]) {
            store_of.insert(s(ps, "DeviceIdentifier"), c);
        }
    }
    let mut apfs: HashMap<&str, &Value> = HashMap::new();
    for c in items(&d.apfs["Containers"]) {
        for v in items(&c["Volumes"]) {
            apfs.insert(s(v, "DeviceIdentifier"), v);
        }
    }
    // the disks the running system is on
    let mut system_disks = HashSet::new();
    for e in &entries {
        if is_container(e) {
            if items(&e["APFSVolumes"])
                .iter()
                .any(|v| mount_paths(v).iter().any(|m| system_mount(m)))
            {
                for ps in items(&e["APFSPhysicalStores"]) {
                    system_disks.insert(whole_of(s(ps, "DeviceIdentifier")));
                }
            }
        } else if items(&e["Partitions"])
            .iter()
            .chain([*e].iter())
            .any(|p| p["MountPoint"].as_str().is_some_and(system_mount))
        {
            system_disks.insert(s(e, "DeviceIdentifier"));
        }
    }
    let p = Parts { d, store_of, apfs };
    let mut disks: Vec<Disk> = entries
        .iter()
        .filter(|e| !is_container(e))
        .map(|e| p.disk(e, system_disks.contains(s(e, "DeviceIdentifier"))))
        .collect();
    super::finish(&mut disks);
    (disks, Vec::new())
}

struct Parts<'a> {
    d: &'a Diskutil,
    store_of: HashMap<&'a str, &'a Value>,
    apfs: HashMap<&'a str, &'a Value>,
}

impl Parts<'_> {
    fn info(&self, id: &str) -> &Value {
        self.d.info.get(id).unwrap_or(&Value::Null)
    }

    fn disk(&self, e: &Value, system: bool) -> Disk {
        let id = s(e, "DeviceIdentifier");
        let info = self.info(id);
        let flag = |k: &str| info[k] == true;
        let table = match s(e, "Content") {
            "GUID_partition_scheme" => Some("gpt"),
            "FDisk_partition_scheme" => Some("dos"),
            "Apple_partition_scheme" => Some("apm"),
            _ => None,
        };
        let mut volumes: Vec<Volume> = items(&e["Partitions"])
            .iter()
            .map(|p| self.partition(p, system))
            .collect();
        if volumes.is_empty() {
            let mut v = self.partition(e, system);
            v.partition = None;
            if let Some(t) = table {
                v.part_type = None; // that was the table's name
                if v.fs.is_none() {
                    v.note = Some(format!("{t} partition table without partitions"));
                    v.hint.blank = true;
                }
            }
            volumes.push(v);
        }
        Disk {
            device: format!("/dev/{id}"),
            name: id.to_string(),
            size: e["Size"].as_u64().unwrap_or(0),
            model: nonempty(&info["MediaName"]),
            serial: self.d.serials.get(id).cloned(),
            transport: info["BusProtocol"].as_str().map(bus),
            rotational: info["SolidState"].as_bool().map(|ssd| !ssd),
            removable: flag("Removable")
                || flag("RemovableMedia")
                || flag("RemovableMediaOrExternalDevice")
                || info["Internal"] == false,
            read_only: info["WritableMedia"] == false,
            partition_table: table.map(str::to_string),
            volumes,
            ..Default::default()
        }
    }

    /// A partition (or a whole disk that is one filesystem).
    fn partition(&self, p: &Value, system_disk: bool) -> Volume {
        let id = s(p, "DeviceIdentifier");
        let info = self.info(id);
        let content = s(p, "Content");
        let fs = fs_id(info["FilesystemType"].as_str().unwrap_or("")).or(match content {
            "EFI" | "DOS_FAT_32" | "DOS_FAT_16" => Some("vfat".into()),
            "Apple_HFS" => Some("hfsplus".into()),
            _ => None,
        });
        let ro = info["WritableVolume"] == false;
        let mut v = Volume {
            device: format!("/dev/{id}"),
            name: id.to_string(),
            size: p["Size"].as_u64().unwrap_or(0),
            partition: id.rsplit_once('s').and_then(|(_, n)| n.parse().ok()),
            part_type: content_name(content),
            fs_name: fat_name(fs.as_deref(), info),
            fs,
            label: nonempty(&p["VolumeName"]).or_else(|| nonempty(&info["VolumeName"])),
            uuid: nonempty(&p["VolumeUUID"]).or_else(|| nonempty(&info["VolumeUUID"])),
            mounts: nonempty(&p["MountPoint"])
                .or_else(|| nonempty(&info["MountPoint"]))
                .map(|path| Mount {
                    path,
                    read_only: ro,
                })
                .into_iter()
                .collect(),
            ..Default::default()
        };
        if let Some(c) = self.store_of.get(id) {
            v.fs = Some("apfs".into());
            v.fs_name = Some("APFS container".into());
            v.note = Some(format!("APFS container {}", s(c, "DeviceIdentifier")));
            v.hint.member = true;
            v.holders = items(&c["APFSVolumes"])
                .iter()
                .map(|av| self.apfs_volume(av, system_disk))
                .collect();
        } else if info["Encryption"] == true || info["FileVault"] == true {
            // Core Storage (FileVault on HFS+), or an encrypted APFS volume reported directly
            v.encryption = Some(Encryption {
                scheme: "FileVault".into(),
                unlocked: info["Locked"] != true,
            });
        }
        if v.fs.is_none() && content.starts_with("Linux") {
            v.note = Some("macOS can't read Linux filesystems".into());
        }
        v.hint.system = v.mounts.iter().any(|m| system_mount(&m.path))
            || (system_disk && SYSTEM_CONTENT.contains(&content));
        v.hint.blank = content.is_empty() && v.fs.is_none();
        v
    }

    fn apfs_volume(&self, av: &Value, system_disk: bool) -> Volume {
        let id = s(av, "DeviceIdentifier");
        let info = self.info(id);
        let a = self.apfs.get(id).copied().unwrap_or(&Value::Null);
        let roles: Vec<&str> = items(&a["Roles"])
            .iter()
            .filter_map(|r| r.as_str())
            .collect();
        let ro = info["WritableVolume"] == false;
        let mut mounts: Vec<Mount> = nonempty(&av["MountPoint"])
            .map(|path| Mount {
                path,
                read_only: ro,
            })
            .into_iter()
            .collect();
        for snap in items(&av["MountedSnapshots"]) {
            if let Some(path) = nonempty(&snap["SnapshotMountPoint"]) {
                mounts.push(Mount {
                    path,
                    read_only: true,
                });
            }
        }
        let yes = |k: &str| a[k] == true || info[k] == true;
        let mut v = Volume {
            device: format!("/dev/{id}"),
            name: id.to_string(),
            size: av["CapacityInUse"]
                .as_u64()
                .or_else(|| av["Size"].as_u64())
                .unwrap_or(0),
            fs: Some("apfs".into()),
            label: nonempty(&av["VolumeName"]).or_else(|| nonempty(&a["Name"])),
            uuid: nonempty(&av["VolumeUUID"]).or_else(|| nonempty(&a["APFSVolumeUUID"])),
            encryption: (yes("FileVault") || yes("Encryption")).then(|| Encryption {
                scheme: if yes("FileVault") {
                    "FileVault"
                } else {
                    "APFS"
                }
                .into(),
                unlocked: !yes("Locked"),
            }),
            note: (!roles.is_empty()).then(|| format!("role: {}", roles.join(", "))),
            mounts,
            ..Default::default()
        };
        v.hint.system = v.mounts.iter().any(|m| system_mount(&m.path))
            || (system_disk && roles.iter().any(|r| SYSTEM_ROLES.contains(r)));
        v
    }
}

/// Where only the running macOS mounts things.
fn system_mount(path: &str) -> bool {
    path == "/" || path.starts_with("/System/Volumes/")
}

/// Mount points of an APFS volume entry in `diskutil list`, its sealed system snapshot's included.
fn mount_paths(v: &Value) -> Vec<String> {
    let mut out: Vec<String> = nonempty(&v["MountPoint"]).into_iter().collect();
    out.extend(
        items(&v["MountedSnapshots"])
            .iter()
            .filter_map(|s| nonempty(&s["SnapshotMountPoint"])),
    );
    out
}

/// "disk0s2" → "disk0"
fn whole_of(id: &str) -> &str {
    let Some(rest) = id.strip_prefix("disk") else {
        return id;
    };
    let digits = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    &id[..4 + digits]
}

fn bus(b: &str) -> String {
    match b {
        "USB" => "usb",
        "SATA" => "sata",
        "PCI-Express" | "PCI" => "pcie",
        "NVMe" | "NVMExpress" => "nvme",
        "Apple Fabric" => "fabric",
        "Thunderbolt" => "thunderbolt",
        "FireWire" => "firewire",
        "SAS" => "sas",
        "Secure Digital" | "SD" => "mmc",
        "Disk Image" => "image",
        "Virtual Interface" => "virtual",
        other => return other.to_ascii_lowercase(),
    }
    .to_string()
}

/// diskutil's filesystem type (its "bundle" name) in blkid's vocabulary.
fn fs_id(t: &str) -> Option<String> {
    Some(
        match t {
            "" => return None,
            "hfs" => "hfsplus",
            "msdos" => "vfat",
            "cd9660" => "iso9660",
            other => other,
        }
        .to_string(),
    )
}

fn fat_name(fs: Option<&str>, info: &Value) -> Option<String> {
    let name = info["FilesystemName"].as_str()?;
    (fs == Some("vfat"))
        .then(|| {
            ["FAT32", "FAT16", "FAT12"]
                .into_iter()
                .find(|f| name.contains(f))
        })
        .flatten()
        .map(str::to_string)
}

/// A partition's `Content` (diskutil's name for its type, or a GUID) by name.
fn content_name(c: &str) -> Option<String> {
    let name = match c {
        "" => return None,
        "EFI" => "EFI system",
        "Apple_APFS" => "Apple APFS",
        "Apple_APFS_ISC" => "Apple Silicon boot",
        "Apple_APFS_Recovery" => "Apple Silicon recovery",
        "Apple_HFS" => "Apple HFS+",
        "Apple_Boot" => "Apple boot",
        "Apple_CoreStorage" => "Apple Core Storage",
        "Microsoft Basic Data" => "Microsoft basic data",
        "Microsoft Reserved" => "Microsoft reserved",
        "Windows_Recovery" => "Windows recovery",
        "Windows_NTFS" => "NTFS / exFAT",
        "DOS_FAT_32" => "FAT32",
        "DOS_FAT_16" => "FAT16",
        "Linux" => "Linux filesystem",
        "Linux_Swap" => "Linux swap",
        "Linux_LVM" => "Linux LVM",
        "Bios Boot Partition" => "BIOS boot",
        other => {
            return Some(
                magic::part_type_name(other)
                    .map(str::to_string)
                    .unwrap_or_else(|| other.replace('_', " ")),
            );
        }
    };
    Some(name.to_string())
}

fn items(v: &Value) -> Vec<&Value> {
    match v {
        Value::Array(a) => a.iter().collect(),
        Value::Null => Vec::new(),
        one => vec![one],
    }
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}

fn nonempty(v: &Value) -> Option<String> {
    v.as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[cfg(target_os = "macos")]
pub(super) fn probe() -> Result<(Vec<Disk>, Vec<String>)> {
    use std::time::Duration;
    const DISKUTIL: &str = "/usr/sbin/diskutil";
    let t = Duration::from_secs(30);
    let list = plist(&super::run(DISKUTIL, &["list", "-plist"], t)?)?;
    let apfs = super::run(DISKUTIL, &["apfs", "list", "-plist"], t)
        .ok()
        .and_then(|b| plist(&b).ok())
        .unwrap_or_default();
    let mut info = HashMap::new();
    // one diskutil per device, a few at a time
    for chunk in identifiers(&list).chunks(8) {
        std::thread::scope(|scope| {
            let running: Vec<_> = chunk
                .iter()
                .map(|id| {
                    scope.spawn(move || {
                        let out = super::run(DISKUTIL, &["info", "-plist", id.as_str()], t);
                        (id.clone(), out.ok().and_then(|b| plist(&b).ok()))
                    })
                })
                .collect();
            for r in running {
                if let Ok((id, Some(v))) = r.join() {
                    info.insert(id, v);
                }
            }
        });
    }
    let serials = super::run(
        "/usr/sbin/ioreg",
        &["-a", "-r", "-c", "IOBlockStorageDevice"],
        Duration::from_secs(10),
    )
    .ok()
    .and_then(|b| plist(&b).ok())
    .map(|v| ioreg_serials(&v))
    .unwrap_or_default();
    Ok(from_diskutil(&Diskutil {
        list,
        apfs,
        info,
        serials,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::volumes::State;

    /// An invented Apple silicon Mac (internal SSD: boot, macOS and recovery containers) with a USB
    /// disk (EFI, exFAT, and an APFS container holding an old Mac's locked Data volume) and a card
    /// in a reader (FAT32 and a Linux partition), as diskutil and ioreg print them.
    fn sample() -> Diskutil {
        macro_rules! data {
            ($f:literal) => {
                plist(include_bytes!(concat!("testdata/", $f))).unwrap()
            };
        }
        let info = [
            ("disk0", data!("diskutil-info-disk0.plist")),
            ("disk3s1", data!("diskutil-info-disk3s1.plist")),
            ("disk3s5", data!("diskutil-info-disk3s5.plist")),
            ("disk4", data!("diskutil-info-disk4.plist")),
            ("disk4s1", data!("diskutil-info-disk4s1.plist")),
            ("disk4s2", data!("diskutil-info-disk4s2.plist")),
            ("disk5s1", data!("diskutil-info-disk5s1.plist")),
            ("disk5s2", data!("diskutil-info-disk5s2.plist")),
            ("disk6", data!("diskutil-info-disk6.plist")),
            ("disk6s1", data!("diskutil-info-disk6s1.plist")),
            ("disk6s2", data!("diskutil-info-disk6s2.plist")),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        Diskutil {
            list: data!("diskutil-list.plist"),
            apfs: data!("diskutil-apfs-list.plist"),
            info,
            serials: ioreg_serials(&data!("ioreg-block-storage.plist")),
        }
    }

    #[test]
    fn property_lists() {
        let v = plist(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>n</key>
	<integer>-5</integer>
	<key>big</key>
	<integer>18446744073709551615</integer>
	<key>r</key>
	<real>1.5</real>
	<key>d</key>
	<data>AAEC</data>
	<key>s</key>
	<string>Photos &amp; Videos &#233;</string>
	<key>empty</key>
	<string></string>
	<key>yes</key>
	<true/>
	<key>no</key>
	<false/>
	<key>none</key>
	<dict/>
	<key>list</key>
	<array>
		<dict>
			<key>k</key>
			<string>v</string>
		</dict>
		<array/>
	</array>
</dict>
</plist>"#,
        )
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "n": -5, "big": 18446744073709551615u64, "r": 1.5, "d": "AAEC",
                "s": "Photos & Videos é", "empty": "", "yes": true, "no": false,
                "none": {}, "list": [{ "k": "v" }, []],
            })
        );
        assert!(plist(b"<plist><dict><key>a</key>").is_err());
        assert!(plist(b"not xml at all").is_err());
    }

    #[test]
    fn identifiers_and_serials() {
        let d = sample();
        let ids = identifiers(&d.list);
        for id in [
            "disk0", "disk0s2", "disk3", "disk3s5", "disk4s3", "disk5s1", "disk6s2",
        ] {
            assert!(ids.iter().any(|i| i == id), "{id} missing");
        }
        assert_eq!(d.serials.len(), 1);
        assert_eq!(d.serials["disk0"], "FAKEMAC0000SSD01");
    }

    #[test]
    fn a_mac_with_an_external_disk_and_a_card() {
        let (disks, notes) = from_diskutil(&sample());
        assert!(notes.is_empty());
        let names: Vec<&str> = disks.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["disk0", "disk4", "disk6"]); // containers are not disks

        let int = &disks[0];
        assert_eq!(
            (
                int.model.as_deref(),
                int.serial.as_deref(),
                int.transport.as_deref(),
                int.rotational,
                int.removable,
                int.system
            ),
            (
                Some("FAKEAPPLE SSD AF0500"),
                Some("FAKEMAC0000SSD01"),
                Some("fabric"),
                Some(false),
                false,
                true
            )
        );
        assert!(int.volumes.iter().all(|v| v.state == State::System));
        let types: Vec<Option<&str>> = int.volumes.iter().map(|v| v.part_type.as_deref()).collect();
        assert_eq!(
            types,
            [
                Some("Apple Silicon boot"),
                Some("Apple APFS"),
                Some("Apple Silicon recovery")
            ]
        );
        let macos = &int.volumes[1];
        assert_eq!(macos.note.as_deref(), Some("APFS container disk3"));
        let find = |id: &str| macos.holders.iter().find(|h| h.name == id).unwrap();
        let system = find("disk3s1");
        assert_eq!(
            (system.label.as_deref(), system.state, system.read_only),
            (Some("Macintosh HD"), State::System, Some(true))
        );
        assert_eq!(system.mounts[0].path, "/");
        let data = find("disk3s5");
        assert_eq!(
            (data.mounts[0].path.as_str(), data.state, data.size),
            ("/System/Volumes/Data", State::System, 301_256_421_376)
        );
        assert_eq!(
            data.encryption,
            Some(Encryption {
                scheme: "FileVault".into(),
                unlocked: true
            })
        );
        // not mounted, but the running system's own
        let recovery = find("disk3s3");
        assert_eq!(
            (recovery.state, recovery.note.as_deref()),
            (State::System, Some("role: Recovery"))
        );

        let usb = &disks[1];
        assert_eq!(
            (
                usb.model.as_deref(),
                usb.serial.as_deref(),
                usb.transport.as_deref(),
                usb.rotational,
                usb.removable,
                usb.system
            ),
            (
                Some("Fakeco Portable 2TB Media"),
                None,
                Some("usb"),
                Some(true),
                true,
                false
            )
        );
        let v = &usb.volumes;
        assert_eq!(
            (
                v[0].fs.as_deref(),
                v[0].fs_name.as_deref(),
                v[0].part_type.as_deref(),
                v[0].state
            ),
            (
                Some("vfat"),
                Some("FAT32"),
                Some("EFI system"),
                State::Mountable
            )
        );
        assert_eq!(
            (
                v[1].fs.as_deref(),
                v[1].label.as_deref(),
                v[1].state,
                v[1].read_only
            ),
            (Some("exfat"), Some("Archive"), State::Mounted, Some(false))
        );
        assert_eq!(v[1].mounts[0].path, "/Volumes/Archive");
        assert_eq!(v[2].state, State::Member);
        // an old Mac's Data volume: same role as the system's, but on another disk it's just data
        let old = &v[2].holders[0];
        assert_eq!(
            (old.label.as_deref(), old.state, old.note.as_deref()),
            (Some("Old Mac - Data"), State::Encrypted, Some("role: Data"))
        );
        assert_eq!(old.encryption.as_ref().map(|e| e.unlocked), Some(false));
        let backups = &v[2].holders[1];
        assert_eq!(
            (backups.label.as_deref(), backups.state),
            (Some("Photos & Backups"), State::Mounted)
        );

        let card = &disks[2];
        assert_eq!(
            (
                card.partition_table.as_deref(),
                card.rotational,
                card.removable
            ),
            (Some("dos"), None, true)
        );
        assert_eq!(
            (
                card.volumes[0].fs_name.as_deref(),
                card.volumes[0].label.as_deref(),
                card.volumes[0].state
            ),
            (Some("FAT32"), Some("CAMERA"), State::Mounted)
        );
        assert_eq!(
            (
                card.volumes[1].state,
                card.volumes[1].part_type.as_deref(),
                card.volumes[1].note.as_deref()
            ),
            (
                State::Unknown,
                Some("Linux filesystem"),
                Some("macOS can't read Linux filesystems")
            )
        );
    }

    #[test]
    fn a_whole_disk_filesystem() {
        let list = serde_json::json!({ "AllDisksAndPartitions": [{
            "Content": "Apple_HFS", "DeviceIdentifier": "disk7", "Size": 1_000_000_000u64,
            "MountPoint": "/Volumes/Old Stick", "VolumeName": "Old Stick"
        }]});
        let (disks, _) = from_diskutil(&Diskutil {
            list,
            ..Default::default()
        });
        let v = &disks[0].volumes[0];
        assert_eq!(
            (v.device.as_str(), v.partition, v.fs.as_deref(), v.state),
            ("/dev/disk7", None, Some("hfsplus"), State::Mounted)
        );
    }
}
