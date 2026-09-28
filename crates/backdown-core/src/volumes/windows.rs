//! Windows: the Storage module's cmdlets (`Get-Disk`, `Get-PhysicalDisk`, `Get-Partition`,
//! `Get-Volume`) and BitLocker's, run once through PowerShell and handed back as one JSON document.
//!
//! The script picks the properties itself and turns enums into strings, so the JSON has the same
//! shape under Windows PowerShell 5.1 and PowerShell 7; the parser still accepts the numeric forms
//! of those enums, and a lone object where a list was meant (5.1 unwraps one-element arrays).
//! `Get-BitLockerVolume` needs an administrator. Without one, Explorer's per-drive BitLocker
//! property, which any user can read, still says whether a drive is encrypted and whether it's
//! locked; a locked drive's filesystem is unknown to Windows until it's unlocked.

use std::collections::HashMap;

use serde_json::Value;

use super::{Disk, Encryption, Mount, Volume};
use crate::magic;

/// Everything in one go (PowerShell takes a second to start), as compact JSON on stdout.
pub const SCRIPT: &str = r#"
$ErrorActionPreference = 'SilentlyContinue'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false
function Letter($c) { $s = [string]$c; if ($s.Length -eq 1 -and [int][char]$s -gt 0) { $s } }
$disks = @(Get-Disk | ForEach-Object { [pscustomobject]@{
  Number = $_.Number; FriendlyName = $_.FriendlyName; Model = $_.Model; SerialNumber = $_.SerialNumber
  BusType = [string]$_.BusType; Size = $_.Size; PartitionStyle = [string]$_.PartitionStyle
  IsSystem = $_.IsSystem; IsBoot = $_.IsBoot; IsOffline = $_.IsOffline; IsReadOnly = $_.IsReadOnly } })
$physical = @(Get-PhysicalDisk | ForEach-Object { [pscustomobject]@{
  DeviceId = [string]$_.DeviceId; MediaType = [string]$_.MediaType; SpindleSpeed = $_.SpindleSpeed } })
$partitions = @(Get-Partition | ForEach-Object { [pscustomobject]@{
  DiskNumber = $_.DiskNumber; PartitionNumber = $_.PartitionNumber; DriveLetter = (Letter $_.DriveLetter)
  Size = $_.Size; Type = [string]$_.Type; GptType = $_.GptType; MbrType = $_.MbrType
  IsSystem = $_.IsSystem; IsBoot = $_.IsBoot; IsHidden = $_.IsHidden; IsReadOnly = $_.IsReadOnly
  AccessPaths = @($_.AccessPaths); Guid = $_.Guid } })
$volumes = @(Get-Volume | ForEach-Object { [pscustomobject]@{
  DriveLetter = (Letter $_.DriveLetter); Path = $_.Path; FileSystem = [string]$_.FileSystem
  FileSystemLabel = $_.FileSystemLabel; Size = $_.Size; DriveType = [string]$_.DriveType } })
$source = 'cmdlet'
$bitlocker = $null
try {
  $bitlocker = @(Get-BitLockerVolume -ErrorAction Stop | ForEach-Object { [pscustomobject]@{
    MountPoint = [string]$_.MountPoint; VolumeStatus = [string]$_.VolumeStatus; LockStatus = [string]$_.LockStatus } })
} catch { }
if ($null -eq $bitlocker) {
  $source = 'shell'
  $shell = New-Object -ComObject Shell.Application
  $bitlocker = @($volumes | Where-Object { $_.DriveLetter } | ForEach-Object {
    $d = "$($_.DriveLetter):"
    [pscustomobject]@{ MountPoint = $d
      Protection = [string]$shell.NameSpace($d).Self.ExtendedProperty('System.Volume.BitLockerProtection') } })
}
[pscustomobject]@{ SystemDrive = $env:SystemDrive; Disks = $disks; Physical = $physical
  Partitions = $partitions; Volumes = $volumes; BitLocker = $bitlocker; BitLockerSource = $source
} | ConvertTo-Json -Depth 5 -Compress
"#;

/// The disks as PowerShell described them (`SCRIPT`'s output), classified.
pub fn from_json(v: &Value) -> (Vec<Disk>, Vec<String>) {
    let system_drive = text(&v["SystemDrive"])
        .trim_end_matches('\\')
        .to_uppercase();
    let system_drive = if system_drive.is_empty() {
        "C:".to_string()
    } else {
        system_drive
    };
    let mut notes = Vec::new();
    if text(&v["BitLockerSource"]) == "shell" {
        notes.push(
            "BitLocker: Get-BitLockerVolume needs an administrator, so encryption and lock state \
             come from Explorer (drives with a letter only)"
                .to_string(),
        );
    }
    let ctx = Ctx {
        system_drive,
        partitions: items(&v["Partitions"]),
        volumes: items(&v["Volumes"]),
        bitlocker: bitlocker(&v["BitLocker"]),
    };
    let physical = items(&v["Physical"]);
    let mut disks = Vec::new();
    for d in items(&v["Disks"]) {
        let Some(n) = d["Number"].as_u64() else {
            continue;
        };
        let physical = physical
            .iter()
            .find(|p| text(&p["DeviceId"]) == n.to_string());
        disks.push(ctx.disk(d, n, physical));
    }
    super::finish(&mut disks);
    (disks, notes)
}

struct Ctx<'a> {
    /// "C:"
    system_drive: String,
    partitions: Vec<&'a Value>,
    volumes: Vec<&'a Value>,
    /// by "D:" and by volume path, upper case, without a trailing backslash
    bitlocker: HashMap<String, Encryption>,
}

impl Ctx<'_> {
    fn disk(&self, d: &Value, n: u64, physical: Option<&&Value>) -> Disk {
        let transport = bus(&text(&d["BusType"]));
        let rotational = physical.and_then(|p| match text(&p["MediaType"]).as_str() {
            "HDD" | "3" => Some(true),
            "SSD" | "4" | "SCM" | "5" => Some(false),
            _ => match p["SpindleSpeed"].as_u64() {
                Some(0) => Some(false),
                Some(s) if s < u64::from(u32::MAX) => Some(true),
                _ => None, // "unknown" is u32::MAX
            },
        });
        let system_disk = d["IsSystem"] == true || d["IsBoot"] == true;
        let read_only = d["IsReadOnly"] == true;
        let mut parts: Vec<&&Value> = self
            .partitions
            .iter()
            .filter(|p| p["DiskNumber"].as_u64() == Some(n))
            .collect();
        parts.sort_by_key(|p| p["PartitionNumber"].as_u64());
        let mut volumes: Vec<Volume> = parts
            .iter()
            .map(|p| self.partition(p, n, system_disk, read_only))
            .collect();
        let style = text(&d["PartitionStyle"]).to_ascii_uppercase();
        if volumes.is_empty() {
            let offline = d["IsOffline"] == true;
            let table = matches!(style.as_str(), "GPT" | "MBR" | "1" | "2");
            let mut v = Volume {
                device: format!(r"\\.\PHYSICALDRIVE{n}"),
                name: format!("Disk {n}"),
                size: d["Size"].as_u64().unwrap_or(0),
                note: Some(if offline {
                    "offline: Windows lists its partitions once it's brought online".into()
                } else if table {
                    "partition table without partitions".into()
                } else {
                    "no partition table Windows recognises".into()
                }),
                ..Default::default()
            };
            v.hint.blank = table && !offline;
            volumes.push(v);
        }
        let model = nonempty(&d["FriendlyName"]).or_else(|| nonempty(&d["Model"]));
        Disk {
            device: format!(r"\\.\PHYSICALDRIVE{n}"),
            name: format!("Disk {n}"),
            size: d["Size"].as_u64().unwrap_or(0),
            model,
            serial: nonempty(&d["SerialNumber"]),
            removable: matches!(transport, Some("usb" | "mmc" | "firewire")),
            transport: transport.map(str::to_string),
            rotational,
            read_only,
            partition_table: match style.as_str() {
                "GPT" | "2" => Some("gpt".into()),
                "MBR" | "1" => Some("dos".into()),
                _ => None,
            },
            volumes,
            ..Default::default()
        }
    }

    fn partition(&self, p: &Value, disk: u64, system_disk: bool, disk_ro: bool) -> Volume {
        let number = p["PartitionNumber"].as_u64().unwrap_or(0);
        let letter = text(&p["DriveLetter"])
            .chars()
            .next()
            .filter(char::is_ascii_alphabetic)
            .map(|l| format!("{}:", l.to_ascii_uppercase()));
        let access: Vec<&str> = items(&p["AccessPaths"])
            .iter()
            .filter_map(|a| a.as_str())
            .collect();
        let vol = self
            .volumes
            .iter()
            .find(|v| {
                v["Path"]
                    .as_str()
                    .is_some_and(|vp| access.iter().any(|a| a.eq_ignore_ascii_case(vp)))
            })
            .or_else(|| {
                let l = letter.as_deref()?;
                self.volumes
                    .iter()
                    .find(|v| format!("{}:", text(&v["DriveLetter"]).to_uppercase()) == l)
            })
            .copied();
        let gpt = text(&p["GptType"])
            .trim_matches(|c| c == '{' || c == '}')
            .to_uppercase();
        let mbr = text(&p["MbrType"]);
        let kind = text(&p["Type"]);
        let part_type = magic::part_type_name(&gpt)
            .or_else(|| magic::part_type_name(&mbr))
            .map(str::to_string)
            .or_else(|| Some(kind.clone()).filter(|k| !k.is_empty() && k != "Unknown"));
        let fs_raw = vol.map(|v| text(&v["FileSystem"])).unwrap_or_default();
        let fs = match fs_raw.to_ascii_uppercase().as_str() {
            "" | "RAW" | "UNKNOWN" => None,
            "NTFS" => Some("ntfs"),
            "FAT32" | "FAT" | "FAT16" | "FAT12" => Some("vfat"),
            "EXFAT" => Some("exfat"),
            "REFS" => Some("refs"),
            "UDF" => Some("udf"),
            "CSVFS" => Some("csvfs"),
            _ => Some(fs_raw.as_str()),
        }
        .map(str::to_string);
        let vol_path = vol.and_then(|v| v["Path"].as_str());
        let ro = disk_ro || p["IsReadOnly"] == true;
        let mut mounts: Vec<Mount> = letter
            .iter()
            .map(|l| Mount {
                path: format!(r"{l}\"),
                read_only: ro,
            })
            .collect();
        // folder mount points (C:\Mounts\Old) besides the drive letter
        for a in &access {
            if !a.starts_with(r"\\?\") && !mounts.iter().any(|m| m.path.eq_ignore_ascii_case(a)) {
                mounts.push(Mount {
                    path: a.to_string(),
                    read_only: ro,
                });
            }
        }
        let key = |s: &str| s.trim_end_matches('\\').to_uppercase();
        let encryption = letter
            .as_deref()
            .and_then(|l| self.bitlocker.get(&key(l)))
            .or_else(|| vol_path.and_then(|vp| self.bitlocker.get(&key(vp))))
            .cloned();
        let mut v = Volume {
            device: vol_path.map(str::to_string).unwrap_or_else(|| {
                format!(r"\\?\GLOBALROOT\Device\Harddisk{disk}\Partition{number}")
            }),
            name: letter
                .clone()
                .unwrap_or_else(|| format!("Partition {number}")),
            size: p["Size"].as_u64().unwrap_or(0),
            partition: u32::try_from(number).ok(),
            fs_name: (fs.as_deref() == Some("vfat")).then(|| fs_raw.to_uppercase()),
            fs,
            label: vol.and_then(|v| nonempty(&v["FileSystemLabel"])),
            uuid: vol_path.and_then(|vp| {
                let (_, rest) = vp.split_once('{')?;
                Some(rest.split_once('}')?.0.to_ascii_lowercase())
            }),
            mounts,
            encryption,
            part_type,
            ..Default::default()
        };
        let pt = v.part_type.as_deref().unwrap_or("");
        v.hint.system = letter.as_deref() == Some(self.system_drive.as_str())
            || p["IsSystem"] == true
            || p["IsBoot"] == true
            || (system_disk
                && (matches!(kind.as_str(), "System" | "Reserved" | "Recovery")
                    || matches!(pt, "EFI system" | "Microsoft reserved" | "Windows recovery")));
        match pt {
            "Windows Storage Spaces" => {
                v.hint.member = true;
                v.note = Some("Storage Spaces pool member".into());
            }
            "Windows LDM data" | "Windows LDM metadata" | "Windows dynamic disk" => {
                v.hint.member = true;
                v.note = Some("dynamic disk (LDM)".into());
            }
            "Microsoft reserved" => v.hint.blank = true,
            t if t.starts_with("Linux") && v.fs.is_none() => {
                v.note = Some("Windows can't read Linux filesystems".into())
            }
            _ if vol.is_some() && v.fs.is_none() && v.encryption.is_none() => {
                v.note = Some("Windows doesn't recognise its filesystem".into())
            }
            _ => {}
        }
        v
    }
}

/// BitLocker state by mount point ("D:") and by volume path, from either source.
fn bitlocker(v: &Value) -> HashMap<String, Encryption> {
    let mut out = HashMap::new();
    for b in items(v) {
        let at = text(&b["MountPoint"]).trim_end_matches('\\').to_uppercase();
        let state = if b.get("VolumeStatus").is_some() || b.get("LockStatus").is_some() {
            let status = text(&b["VolumeStatus"]);
            let lock = text(&b["LockStatus"]);
            let locked = lock == "Locked" || lock == "1";
            let decrypted = matches!(status.as_str(), "FullyDecrypted" | "0" | "");
            (locked || !decrypted).then_some(!locked)
        } else {
            // Explorer's System.Volume.BitLockerProtection
            match text(&b["Protection"]).as_str() {
                "1" | "3" | "4" | "5" | "8" => Some(true),
                "6" => Some(false),
                _ => None,
            }
        };
        if let (Some(unlocked), false) = (state, at.is_empty()) {
            out.insert(
                at,
                Encryption {
                    scheme: "BitLocker".into(),
                    unlocked,
                },
            );
        }
    }
    out
}

/// MSFT_Disk's BusType, by name or number.
fn bus(b: &str) -> Option<&'static str> {
    Some(match b.to_ascii_lowercase().as_str() {
        "usb" | "7" => "usb",
        "sata" | "11" => "sata",
        "ata" | "3" => "ata",
        "atapi" | "2" => "atapi",
        "nvme" | "17" => "nvme",
        "sas" | "10" => "sas",
        "scsi" | "1" => "scsi",
        "sd" | "12" | "mmc" | "13" => "mmc",
        "1394" | "4" => "firewire",
        "raid" | "8" => "raid",
        "iscsi" | "9" => "iscsi",
        "fibre channel" | "6" => "fc",
        "virtual" | "14" => "virtual",
        "file backed virtual" | "15" => "image",
        "storage spaces" | "spaces" | "16" => "spaces",
        _ => return None,
    })
}

fn items(v: &Value) -> Vec<&Value> {
    match v {
        Value::Array(a) => a.iter().collect(),
        Value::Null => Vec::new(),
        one => vec![one],
    }
}

/// A value as text: strings as they are, numbers and booleans printed, null empty.
fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim().to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn nonempty(v: &Value) -> Option<String> {
    Some(text(v)).filter(|s| !s.is_empty())
}

#[cfg(windows)]
pub(super) fn probe() -> anyhow::Result<(Vec<Disk>, Vec<String>)> {
    use anyhow::Context;
    use base64::Engine;
    // -EncodedCommand (UTF-16LE, base64) sidesteps every quoting rule of the command line
    let utf16: Vec<u8> = SCRIPT.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(utf16);
    let powershell = std::env::var_os("SystemRoot")
        .map(|r| {
            std::path::Path::new(&r)
                .join(r"System32\WindowsPowerShell\v1.0\powershell.exe")
                .display()
                .to_string()
        })
        .unwrap_or_else(|| "powershell".into());
    let out = super::run(
        &powershell,
        &["-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded],
        std::time::Duration::from_secs(90),
    )?;
    let out = out.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&out);
    let v: Value = serde_json::from_slice(out).context("PowerShell's storage cmdlets")?;
    Ok(from_json(&v))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::volumes::State;

    /// An invented desktop: the system NVMe (EFI, MSR, C: with BitLocker on, recovery), a SATA
    /// disk (MSR, D:, a locked BitLocker E:, an NTFS partition without a letter, a Linux one), a USB
    /// disk also mounted in a folder, and an offline disk; BitLocker read without an administrator.
    fn sample() -> (Vec<Disk>, Vec<String>) {
        let v: Value = serde_json::from_str(include_str!("testdata/windows-storage.json")).unwrap();
        from_json(&v)
    }

    fn row(v: &Volume) -> (&str, Option<&str>, Option<&str>, State) {
        (
            v.name.as_str(),
            v.fs.as_deref(),
            v.label.as_deref(),
            v.state,
        )
    }

    #[test]
    fn a_desktop_with_bitlocker_and_foreign_partitions() {
        let (disks, notes) = sample();
        assert_eq!(notes.len(), 1, "{notes:?}"); // BitLocker came from Explorer
        let names: Vec<&str> = disks.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["Disk 0", "Disk 1", "Disk 2", "Disk 3"]);

        let sys = &disks[0];
        assert_eq!(
            (
                sys.device.as_str(),
                sys.serial.as_deref(),
                sys.transport.as_deref(),
                sys.rotational,
                sys.system
            ),
            (
                r"\\.\PHYSICALDRIVE0",
                Some("FNV0_0000_0000_0001."),
                Some("nvme"),
                Some(false),
                true
            )
        );
        let rows: Vec<_> = sys.volumes.iter().map(row).collect();
        assert_eq!(
            rows,
            [
                ("Partition 1", Some("vfat"), None, State::System),
                ("Partition 2", None, None, State::System),
                ("C:", Some("ntfs"), Some("Windows"), State::System),
                ("Partition 4", Some("ntfs"), None, State::System),
            ]
        );
        let c = &sys.volumes[2];
        assert_eq!(c.mounts[0].path, r"C:\");
        assert_eq!(
            c.encryption,
            Some(Encryption {
                scheme: "BitLocker".into(),
                unlocked: true
            })
        );
        assert_eq!(
            c.uuid.as_deref(),
            Some("0d0e0f00-1111-4222-8333-000000000003")
        );
        assert_eq!(
            sys.volumes[3].part_type.as_deref(),
            Some("Windows recovery")
        );

        let hdd = &disks[1];
        assert_eq!(
            (
                hdd.serial.as_deref(),
                hdd.transport.as_deref(),
                hdd.rotational,
                hdd.removable,
                hdd.system
            ),
            (Some("FS4K00000042"), Some("sata"), Some(true), false, false)
        );
        let rows: Vec<_> = hdd.volumes.iter().map(row).collect();
        assert_eq!(
            rows,
            [
                ("Partition 1", None, None, State::Empty),
                ("D:", Some("ntfs"), Some("Data"), State::Mounted),
                ("E:", None, None, State::Encrypted),
                (
                    "Partition 4",
                    Some("ntfs"),
                    Some("Old Backup"),
                    State::Mountable
                ),
                ("Partition 5", None, None, State::Unknown),
            ]
        );
        assert_eq!(hdd.volumes[1].encryption, None);
        assert_eq!(
            hdd.volumes[2].encryption.as_ref().map(|e| e.unlocked),
            Some(false)
        );
        assert_eq!(
            hdd.volumes[4].note.as_deref(),
            Some("Windows can't read Linux filesystems")
        );

        // BusType as a number, and a folder mount point beside the letter
        let usb = &disks[2];
        assert_eq!(
            (
                usb.transport.as_deref(),
                usb.removable,
                usb.rotational,
                usb.partition_table.as_deref()
            ),
            (Some("usb"), true, None, Some("dos"))
        );
        let f = &usb.volumes[0];
        assert_eq!(
            (f.fs_name.as_deref(), f.part_type.as_deref(), f.state),
            (Some("FAT32"), Some("FAT32"), State::Mounted)
        );
        let paths: Vec<&str> = f.mounts.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, [r"F:\", r"C:\Mounts\Camera\"]);

        let offline = &disks[3];
        assert_eq!(offline.volumes.len(), 1);
        assert!(
            offline.volumes[0]
                .note
                .as_deref()
                .unwrap()
                .starts_with("offline")
        );
    }

    #[test]
    fn bitlocker_from_the_cmdlet() {
        let b = bitlocker(&serde_json::json!([
            { "MountPoint": "C:", "VolumeStatus": "FullyEncrypted", "LockStatus": "Unlocked" },
            { "MountPoint": "D:", "VolumeStatus": "FullyDecrypted", "LockStatus": "Unlocked" },
            { "MountPoint": "E:", "VolumeStatus": "FullyEncrypted", "LockStatus": "Locked" },
            {
                "MountPoint": r"\\?\Volume{0d0e0f00-1111-4222-8333-000000000015}\",
                "VolumeStatus": "EncryptionInProgress",
                "LockStatus": "Unlocked"
            },
        ]));
        assert_eq!(b.len(), 3);
        assert!(b["C:"].unlocked);
        assert!(!b["E:"].unlocked);
        assert!(b[r"\\?\VOLUME{0D0E0F00-1111-4222-8333-000000000015}"].unlocked);
        // one volume only: Windows PowerShell hands over an object, not a list
        let one = bitlocker(&serde_json::json!(
            { "MountPoint": "C:", "VolumeStatus": "1", "LockStatus": "0" }
        ));
        assert!(one["C:"].unlocked);
    }
}
