//! End-to-end: Time Machine's newer layouts (APFS snapshots, where macOS mounts them, network
//! bundles with their records in binary and XML property lists) next to the old one, indexed and
//! recognised.

use std::fs;
use std::path::Path;

use backdown_core::recognise::{self, Entity};
use backdown_core::{Db, index};

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn find<'a>(es: &'a [Entity], kind: &str, ends: &str) -> &'a Entity {
    es.iter()
        .find(|e| e.kind == kind && e.path.ends_with(ends))
        .unwrap_or_else(|| {
            panic!(
                "no {kind} at …{ends}; got {:#?}",
                es.iter().map(|e| (&e.kind, &e.path)).collect::<Vec<_>>()
            )
        })
}

fn fact<'a>(e: &'a Entity, key: &str) -> Option<&'a str> {
    e.facts
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// `{ "VerificationDate": <2021-03-04 05:06:07 UTC>, "com.apple.backupd.HostUUID": …,
/// "com.apple.backupd.ModelID": "iMac19,1" }` as a binary property list.
fn binary_machine_id() -> Vec<u8> {
    let ascii = |s: &str| {
        let mut v = if s.len() < 15 {
            vec![0x50 | s.len() as u8]
        } else {
            vec![0x5F, 0x10, s.len() as u8]
        };
        v.extend_from_slice(s.as_bytes());
        v
    };
    let mut date = vec![0x33];
    date.extend_from_slice(&((1_614_834_367i64 - 978_307_200) as f64).to_be_bytes());
    let objects = [
        vec![0xD3, 1, 2, 3, 4, 5, 6],
        ascii("VerificationDate"),
        ascii("com.apple.backupd.HostUUID"),
        ascii("com.apple.backupd.ModelID"),
        date,
        ascii("0F4C2A55-1B6E-4F7A-9C3D-5E2B8A7D6C10"),
        ascii("iMac19,1"),
    ];
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
    out.extend_from_slice(&[2, 1]);
    out.extend_from_slice(&(objects.len() as u64).to_be_bytes());
    out.extend_from_slice(&0u64.to_be_bytes());
    out.extend_from_slice(&table.to_be_bytes());
    out
}

const HISTORY: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>Snapshots</key><array>
  <dict><key>com.apple.backupd.SnapshotName</key><string>2020-11-21-094216.backup</string>
        <key>com.apple.backupd.SnapshotCompletionDate</key><date>2020-11-21T10:01:00Z</date></dict>
  <dict><key>com.apple.backupd.SnapshotName</key><string>2021-03-02-095512.backup</string>
        <key>com.apple.backupd.SnapshotCompletionDate</key><date>2021-03-02T10:00:00Z</date></dict>
  <dict><key>com.apple.backupd.SnapshotName</key><string>2021-03-03-080000.inprogress</string></dict>
</array></dict></plist>"#;

fn drive(d: &Path) {
    // a local APFS backup volume, named by macOS after the Mac (decomposed, as HFS+ and older
    // tools write it): two finished snapshots and one being made
    let set = d.join("Backups of Zoe\u{308}\u{2019}s MacBook");
    for snap in ["2021-01-10-101010.backup", "2021-03-02-095512.backup"] {
        write(
            &set.join(snap)
                .join("Macintosh HD - Data/Users/zoe/notes.txt"),
            snap.as_bytes(),
        );
        write(
            &set.join(snap)
                .join("Macintosh HD - Data/Users/zoe/Pictures/p.jpg"),
            &[0xFF, 0xD8, 0xFF],
        );
    }
    write(
        &set.join("2021-03-03-080000.inprogress/Macintosh HD - Data/Users/zoe/new.txt"),
        b"half",
    );
    // where macOS mounts a snapshot: the folder holds its own `.backup` folder once more
    let mounted = d.join(".timemachine/0F4C2A55-1B6E-4F7A-9C3D-5E2B8A7D6C10");
    write(
        &mounted.join(
            "2022-05-01-120000.backup/2022-05-01-120000.backup/Macintosh HD - Data/Users/sam/a.txt",
        ),
        b"a",
    );
    // network backups: an APFS backup bundle with both records, an HFS+ sparse bundle with its
    // machine record in XML, and a sparse bundle that's just a disk image
    let apfs = d.join("shares/Office iMac.backupbundle");
    write(
        &apfs.join("com.apple.TimeMachine.MachineID.plist"),
        &binary_machine_id(),
    );
    write(
        &apfs.join("com.apple.TimeMachine.SnapshotHistory.plist"),
        HISTORY,
    );
    write(&apfs.join("Info.plist"), b"<plist/>");
    write(&apfs.join("bands/0"), &[1; 4096]);
    write(&apfs.join("bands/1"), &[2; 4096]);
    let hfs = d.join("shares/Alexs-MacBook.sparsebundle");
    write(
        &hfs.join("com.apple.TimeMachine.MachineID.plist"),
        br#"<plist><dict><key>com.apple.backupd.ModelID</key><string>MacBookAir7,2</string></dict></plist>"#,
    );
    write(&hfs.join("bands/0"), &[3; 4096]);
    let img = d.join("images/Project.sparsebundle");
    write(&img.join("Info.plist"), b"<plist/>");
    write(&img.join("bands/0"), &[4; 4096]);
    // and the old layout, for comparison
    write(
        &d.join("Backups.backupdb/Old-iMac/2015-06-01-120000/Macintosh HD/Users/x/y.txt"),
        b"y",
    );
}

#[test]
fn every_time_machine_layout() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().join("TM");
    drive(&d);
    let mut db = Db::open(&dir.path().join("index.db")).unwrap();
    index::index(
        &mut db,
        &d,
        &index::Options {
            one_file_system: true,
            allow_empty: false,
            allow_other_drive: false,
        },
        &mut |_| {},
    )
    .unwrap();
    let id = recognise::root_ids(&db).unwrap()[0];
    let r = recognise::recognise(&db, id, true, &mut |_| {}).unwrap();
    let es = &r.entities;

    // APFS, local: the computer from the volume's name, shown composed
    let local = find(es, "timemachine", "MacBook");
    assert_eq!(local.title, "2 snapshots from Zo\u{eb}\u{2019}s MacBook");
    assert_eq!(fact(local, "Computers"), Some("Zo\u{eb}\u{2019}s MacBook"));
    assert_eq!(fact(local, "Span"), Some("2021-01-10 – 2021-03-02"));
    assert_eq!(fact(local, "Volumes"), Some("Macintosh HD - Data"));
    assert!(fact(local, "Unfinished").is_some_and(|u| u.starts_with('1')));
    assert!(fact(local, "Layout").is_some_and(|l| l.starts_with("APFS")));
    assert!(
        local.provenance.contains(
            &"Backed up by Time Machine from Zo\u{eb}\u{2019}s MacBook, last on 2 Mar 2021."
                .to_string()
        ),
        "{:?}",
        local.provenance
    );

    // APFS, where macOS mounts the snapshots: no computer name, the volume found one level down
    let mounted = find(es, "timemachine", "0F4C2A55-1B6E-4F7A-9C3D-5E2B8A7D6C10");
    assert_eq!(mounted.title, "1 snapshot (APFS)");
    assert_eq!(fact(mounted, "Volumes"), Some("Macintosh HD - Data"));
    assert!(
        mounted
            .provenance
            .contains(&"Backed up by Time Machine, last on 1 May 2022.".to_string()),
        "{:?}",
        mounted.provenance
    );

    // network, APFS: the machine record (binary) and the snapshot history (XML)
    let net = find(es, "timemachine", "Office iMac.backupbundle");
    assert_eq!(net.title, "network backup of Office iMac, 2 snapshots");
    assert_eq!(fact(net, "Computers"), Some("Office iMac"));
    assert_eq!(fact(net, "Model"), Some("iMac19,1"));
    assert_eq!(fact(net, "Last verified"), Some("4 Mar 2021"));
    assert_eq!(fact(net, "Snapshots"), Some("2"));
    assert_eq!(fact(net, "Span"), Some("2020-11-21 – 2021-03-02"));
    assert!(
        net.provenance
            .iter()
            .any(|p| p == "Backed up by Time Machine from Office iMac, last on 2 Mar 2021."),
        "{:?}",
        net.provenance
    );

    // network, HFS+: its snapshots are inside the image, so only the Mac is known
    let hfs = find(es, "timemachine", "Alexs-MacBook.sparsebundle");
    assert_eq!(hfs.title, "network backup of Alexs-MacBook");
    assert_eq!(fact(hfs, "Model"), Some("MacBookAir7,2"));
    assert!(fact(hfs, "Span").is_none());

    // a sparse bundle without Time Machine's record is a disk image, at its own spelling
    let img = find(es, "disk-image", "Project.sparsebundle");
    assert!(img.files >= 2 && img.bytes >= 4096, "{img:?}");

    // the old layout still works, and nothing inside any set is listed on its own
    let old = find(es, "timemachine", "Backups.backupdb");
    assert_eq!(fact(old, "Computers"), Some("Old-iMac"));
    assert_eq!(es.iter().filter(|e| e.kind == "timemachine").count(), 5);
    assert!(
        !es.iter()
            .any(|e| e.kind != "timemachine" && e.kind != "disk-image"),
        "{:#?}",
        es.iter().map(|e| (&e.kind, &e.path)).collect::<Vec<_>>()
    );
}
