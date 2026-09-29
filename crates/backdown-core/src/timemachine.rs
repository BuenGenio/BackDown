//! Time Machine's layouts, and what its own records say (recognise.rs finds the sets).
//!
//! - HFS+ (up to macOS 10.15, and still readable later): `Backups.backupdb/<computer>/<YYYY-MM-DD-
//!   HHMMSS>/<volume>/…`, one folder per backup, unchanged files hard-linked between them.
//! - APFS (macOS 11 and later, on a local disk): every backup is a snapshot of the backup volume,
//!   seen as a folder `YYYY-MM-DD-HHMMSS.backup` (the one being made is `.inprogress`, the one
//!   before the latest `.previous`), at the volume's root when it's browsed or where macOS mounts
//!   it (`/Volumes/.timemachine/<uuid>/<name>.backup/`). `Backups.backupdb` is gone. The volume
//!   is often named "Backups of <computer>".
//! - Network backups: a disk image per computer, `<computer>.sparsebundle` (HFS+ inside) or
//!   `<computer>.backupbundle` (APFS, macOS 11 and later), holding `com.apple.TimeMachine.
//!   MachineID.plist` (the Mac's model and hardware UUID, when the backup was last verified) and,
//!   for APFS, `com.apple.TimeMachine.SnapshotHistory.plist` (each snapshot's name and when it
//!   was completed). Their snapshots are inside the image, so only these records tell them.
//!
//! The records are property lists, binary or XML (`plist`); their keys are read loosely (any
//! dictionary with a snapshot's name in it counts), since Apple doesn't document them.

use crate::plist::{self, Plist};

/// A snapshot folder's name, taken apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// "2021-05-07"
    pub day: String,
    /// "2021-05-07-123456"
    pub stamp: String,
    pub state: State,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// a finished backup (`.backup`, `.previous`, or HFS+'s plain name)
    Done,
    /// being made, or given up on (`.inprogress`, `.interrupted`, HFS+'s `.inProgress`)
    Unfinished,
}

/// `2021-05-07-123456`, `2021-05-07-123456.backup`, `….inprogress` and the like (any case).
pub fn snapshot(name: &str) -> Option<Snapshot> {
    let (stamp, suffix) = match name.split_once('.') {
        Some((s, x)) => (s, Some(x.to_ascii_lowercase())),
        None => (name, None),
    };
    let b = stamp.as_bytes();
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    if b.len() != 17
        || !(digits(0..4) && digits(5..7) && digits(8..10) && digits(11..17))
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'-'
    {
        return None;
    }
    let state = match suffix.as_deref() {
        None | Some("backup") | Some("previous") => State::Done,
        Some("inprogress") | Some("interrupted") => State::Unfinished,
        Some(_) => return None,
    };
    Some(Snapshot {
        day: stamp[..10].to_string(),
        stamp: stamp.to_string(),
        state,
    })
}

/// A folder name that only APFS Time Machine gives: a snapshot with its suffix.
pub fn apfs_snapshot(name: &str) -> bool {
    name.contains('.') && snapshot(name).is_some()
}

/// "Backups of Alex's MacBook" → "Alex's MacBook".
pub fn computer_of_volume(name: &str) -> Option<&str> {
    let rest = name.strip_prefix("Backups of ")?.trim();
    (!rest.is_empty()).then_some(rest)
}

/// "Alex's MacBook.backupbundle" → "Alex's MacBook" (and `.sparsebundle`).
pub fn bundle_computer(name: &str) -> Option<&str> {
    let lower = name.to_ascii_lowercase();
    let cut = [".backupbundle", ".sparsebundle"]
        .iter()
        .find(|x| lower.ends_with(*x))?
        .len();
    let rest = name[..name.len() - cut].trim();
    (!rest.is_empty()).then_some(rest)
}

/// The snapshots a `SnapshotHistory.plist` lists: each one's folder name and when it was
/// completed (if it says).
pub fn history(p: &Plist) -> Vec<(Snapshot, Option<i64>)> {
    let mut out = Vec::new();
    walk(p, &mut out);
    out.sort_by(|a, b| a.0.stamp.cmp(&b.0.stamp));
    out.dedup_by(|a, b| a.0.stamp == b.0.stamp);
    out
}

fn walk(p: &Plist, out: &mut Vec<(Snapshot, Option<i64>)>) {
    match p {
        Plist::Dict(d) => {
            let name = d.values().find_map(|v| match v {
                Plist::Str(s) => snapshot(s.trim()),
                _ => None,
            });
            match name {
                Some(s) => {
                    // the completion date if it's named so, else any date in it
                    let date = d
                        .iter()
                        .filter_map(|(k, v)| match v {
                            Plist::Date(t) => Some((k.to_lowercase().contains("complet"), *t)),
                            _ => None,
                        })
                        .max_by_key(|(completed, _)| *completed)
                        .map(|(_, t)| t);
                    out.push((s, date));
                }
                None => d.values().for_each(|v| walk(v, out)),
            }
        }
        Plist::Array(a) => a.iter().for_each(|v| walk(v, out)),
        Plist::Str(s) => {
            if let Some(s) = snapshot(s.trim()) {
                out.push((s, None));
            }
        }
        _ => {}
    }
}

/// What a `MachineID.plist` says about the Mac that was backed up.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Machine {
    /// "MacBookPro16,1"
    pub model: Option<String>,
    pub uuid: Option<String>,
    /// when Time Machine last checked the backup
    pub verified: Option<i64>,
}

pub fn machine(p: &Plist) -> Machine {
    let Plist::Dict(d) = p else {
        return Machine::default();
    };
    let text = |suffix: &str| {
        d.iter()
            .find(|(k, _)| k.to_ascii_lowercase().ends_with(suffix))
            .and_then(|(_, v)| match v {
                Plist::Str(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
                _ => None,
            })
    };
    Machine {
        model: text("modelid"),
        uuid: text("hostuuid"),
        verified: d.iter().find_map(|(k, v)| match v {
            Plist::Date(t) if k.to_ascii_lowercase().contains("verification") => Some(*t),
            _ => None,
        }),
    }
}

/// Read one of Time Machine's records in `dir`, with or without a leading dot on its name.
pub fn read(dir: &std::path::Path, name: &str) -> Option<Plist> {
    [name.to_string(), format!(".{name}")]
        .iter()
        .map(|n| dir.join(n))
        .find_map(|p| {
            let meta = std::fs::metadata(&p).ok()?;
            if !meta.is_file() || meta.len() > 16 << 20 {
                return None;
            }
            plist::parse(&std::fs::read(&p).ok()?)
        })
}

pub const MACHINE_ID: &str = "com.apple.TimeMachine.MachineID.plist";
pub const SNAPSHOT_HISTORY: &str = "com.apple.TimeMachine.SnapshotHistory.plist";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_names() {
        let s = snapshot("2021-05-07-123456.backup").unwrap();
        assert_eq!((s.day.as_str(), s.state), ("2021-05-07", State::Done));
        assert_eq!(s.stamp, "2021-05-07-123456");
        assert_eq!(snapshot("2019-03-01-120000").unwrap().state, State::Done);
        assert_eq!(
            snapshot("2019-03-01-120000.inProgress").unwrap().state,
            State::Unfinished
        );
        assert_eq!(
            snapshot("2022-01-02-030405.previous").unwrap().state,
            State::Done
        );
        for bad in [
            "2021-05-07",
            "2021-05-07-12345x.backup",
            "2021-05-07-123456.bak",
            "Latest",
            "20210507-123456.backup",
        ] {
            assert!(snapshot(bad).is_none(), "{bad}");
        }
        assert!(apfs_snapshot("2021-05-07-123456.backup"));
        assert!(!apfs_snapshot("2021-05-07-123456"));
        assert_eq!(
            computer_of_volume("Backups of Alex’s MacBook Air"),
            Some("Alex’s MacBook Air")
        );
        assert_eq!(computer_of_volume("Time Machine"), None);
        assert_eq!(
            bundle_computer("Office iMac.backupbundle"),
            Some("Office iMac")
        );
        assert_eq!(
            bundle_computer("Alexs-MacBook.SparseBundle"),
            Some("Alexs-MacBook")
        );
        assert_eq!(bundle_computer(".sparsebundle"), None);
    }

    #[test]
    fn an_xml_snapshot_history() {
        let x = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>Snapshots</key><array>
  <dict><key>com.apple.backupd.SnapshotCompletionDate</key><date>2021-03-02T10:00:00Z</date>
        <key>com.apple.backupd.SnapshotName</key><string>2021-03-02-095512.backup</string>
        <key>com.apple.backupd.SnapshotTotalBytesCopied</key><integer>123456</integer></dict>
  <dict><key>com.apple.backupd.SnapshotName</key><string>2020-11-21-094216.backup</string>
        <key>com.apple.backupd.SnapshotCompletionDate</key><date>2020-11-21T10:01:00Z</date></dict>
  <dict><key>com.apple.backupd.SnapshotName</key><string>not a snapshot</string></dict>
</array></dict></plist>"#;
        let h = history(&plist::parse(x).unwrap());
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].0.day, "2020-11-21");
        assert_eq!(h[1].1, plist::iso_date("2021-03-02T10:00:00Z"));
    }

    /// A binary plist of one dictionary of strings and dates (as Apple's tools write them).
    fn bplist_dict(entries: &[(&str, Plist)]) -> Vec<u8> {
        let mut objects: Vec<Vec<u8>> = Vec::new();
        let n = entries.len();
        let mut top = vec![0xD0 | n as u8];
        top.extend((1..=n).map(|i| i as u8));
        top.extend((n + 1..=2 * n).map(|i| i as u8));
        objects.push(top);
        let ascii = |s: &str| {
            let mut v = vec![0x50 | s.len() as u8];
            if s.len() >= 15 {
                v = vec![0x5F, 0x10, s.len() as u8];
            }
            v.extend_from_slice(s.as_bytes());
            v
        };
        for (k, _) in entries {
            objects.push(ascii(k));
        }
        for (_, v) in entries {
            objects.push(match v {
                Plist::Str(s) => ascii(s),
                Plist::Date(t) => {
                    let mut d = vec![0x33];
                    d.extend_from_slice(&((*t - 978_307_200) as f64).to_be_bytes());
                    d
                }
                _ => unreachable!(),
            });
        }
        let mut out = b"bplist00".to_vec();
        let mut offs = Vec::new();
        for o in &objects {
            offs.push(out.len() as u16);
            out.extend_from_slice(o);
        }
        let table = out.len() as u64;
        for o in offs {
            out.extend_from_slice(&o.to_be_bytes());
        }
        out.extend_from_slice(&[0; 6]);
        out.push(2);
        out.push(1);
        out.extend_from_slice(&(objects.len() as u64).to_be_bytes());
        out.extend_from_slice(&0u64.to_be_bytes());
        out.extend_from_slice(&table.to_be_bytes());
        out
    }

    #[test]
    fn a_binary_machine_id() {
        let b = bplist_dict(&[
            ("VerificationDate", Plist::Date(1_605_951_736)),
            (
                "com.apple.backupd.HostUUID",
                Plist::Str("0F4C2A55-1B6E-4F7A-9C3D-5E2B8A7D6C10".into()),
            ),
            (
                "com.apple.backupd.ModelID",
                Plist::Str("MacBookPro16,1".into()),
            ),
        ]);
        let p = plist::parse(&b).unwrap();
        let m = machine(&p);
        assert_eq!(m.model.as_deref(), Some("MacBookPro16,1"));
        assert_eq!(
            m.uuid.as_deref(),
            Some("0F4C2A55-1B6E-4F7A-9C3D-5E2B8A7D6C10")
        );
        assert_eq!(m.verified, Some(1_605_951_736));
        assert_eq!(machine(&Plist::Array(vec![])), Machine::default());
    }
}
