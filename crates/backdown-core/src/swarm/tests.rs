use std::collections::HashMap;

use serde_json::{Value, json};

use super::*;

/// A machine's storage as `storage` reports it: a system SSD with / and an EFI partition, a USB
/// drive with a LUKS volume holding an ext4 file system, and four indexed drives (one on the USB
/// drive, one in a home folder, one on a network share, one unplugged).
fn linux_storage() -> Value {
    let mount = |p: &str| json!([{ "path": p, "read_only": false }]);
    let efi = json!({ "device": "/dev/nvme0n1p1", "name": "nvme0n1p1", "fs": "vfat", "fs_name": "FAT32",
        "label": "EFI", "size": 500_000_000, "state": "system", "mounts": mount("/boot/efi"),
        "space": { "total": 500_000_000, "free": 400_000_000 }, "indexed": [], "holders": [] });
    let photos = json!([{ "root_id": 2, "root": "/home/me/Photos", "key": "k2", "relation": "contains",
        "last_scan": 5, "scanned_at": 1 }]);
    let root = json!({ "device": "/dev/nvme0n1p2", "name": "nvme0n1p2", "fs": "ext4", "fs_name": "ext4",
        "uuid": "root-uuid", "size": 999_000_000_000u64, "state": "system", "mounts": mount("/"),
        "space": { "total": 990_000_000_000u64, "free": 100_000_000_000u64 }, "indexed": photos, "holders": [] });
    let swap = json!({ "device": "/dev/nvme0n1p3", "name": "nvme0n1p3", "state": "swap", "fs": "swap",
        "size": 8_000_000_000u64, "mounts": [], "indexed": [], "holders": [] });
    let odd = json!({ "device": "/dev/nvme0n1p4", "name": "nvme0n1p4", "state": "unknown", "size": 3_000_000,
        "mounts": [], "indexed": [], "holders": [] });
    let ssd = json!({ "name": "nvme0n1", "device": "/dev/nvme0n1", "model": "Samsung SSD 980",
        "serial": "S64ANS0T1", "size": 1_000_000_000_000u64, "transport": "nvme", "rotational": false,
        "removable": false, "system": true, "volumes": [efi, root, swap, odd] });
    let vault_ix = json!([{ "root_id": 1, "root": "/media/me/Vault", "key": "k1", "relation": "same",
        "last_scan": 9, "scanned_at": 2 }]);
    let vault = json!({ "device": "/dev/mapper/vault", "name": "vault", "fs": "ext4", "fs_name": "ext4",
        "label": "Vault", "uuid": "vault-uuid", "size": 3_990_000_000_000u64, "state": "mounted",
        "mounts": mount("/media/me/Vault"),
        "space": { "total": 3_900_000_000_000u64, "free": 900_000_000_000u64 }, "indexed": vault_ix, "holders": [] });
    let luks = json!({ "device": "/dev/sdb1", "name": "sdb1", "fs": "crypto_LUKS", "fs_name": "LUKS",
        "uuid": "luks-uuid", "size": 4_000_000_000_000u64, "state": "encrypted", "mounts": [], "indexed": [],
        "holders": [vault] });
    let usb = json!({ "name": "sdb", "device": "/dev/sdb", "model": "WD Elements 2620", "serial": "WD-WXC2D71",
        "size": 4_000_000_000_000u64, "transport": "usb", "rotational": true, "removable": false,
        "system": false, "volumes": [luks] });
    // Apple silicon's extra NVMe namespaces: a sliver with nothing on it
    let sliver = json!({ "name": "nvme0n3", "device": "/dev/nvme0n3", "model": "APPLE SSD", "size": 134_000_000,
        "volumes": [{ "device": "/dev/nvme0n3", "name": "nvme0n3", "state": "unknown", "size": 134_000_000,
                      "mounts": [], "indexed": [], "holders": [] }] });
    let r1 = json!({ "id": 1, "name": "Vault", "path": "/media/me/Vault", "key": "k1", "files": 1000,
        "bytes": 2_000_000_000u64, "present": true, "on_volume": true, "space": null, "drive": null });
    let r2 = json!({ "id": 2, "name": "Photos", "path": "/home/me/Photos", "key": "k2", "files": 50,
        "bytes": 70_000_000, "present": true, "on_volume": true, "space": null, "drive": null });
    let r3 = json!({ "id": 3, "name": "share", "path": "/mnt/share", "key": "k3", "files": 7, "bytes": 9_000,
        "present": true, "on_volume": false, "drive": null,
        "space": { "total": 2_000_000_000_000u64, "free": 500_000_000_000u64 } });
    let gone = json!({ "disk": { "model": "WDC WD10SPZX", "serial": "WX12" },
        "volume": { "label": "bd1a", "uuid": "bd1a-uuid", "mount": "/mnt/bd1a" } });
    let r4 = json!({ "id": 4, "name": "bd1a", "path": "/mnt/bd1a", "key": "k4", "files": 1_200_000,
        "bytes": 913_000_000_000u64, "present": false, "on_volume": false, "space": null, "drive": gone });
    json!({ "os": "linux", "at": 1_700_000_000, "disks": [ssd, usb, sliver], "roots": [r1, r2, r3, r4] })
}

#[test]
fn a_path_is_on_the_deepest_mount_above_it() {
    let s = linux_storage();
    let p = place(&s, "/media/me/Vault/Archive/2019/a.jpg", Some(1));
    assert_eq!(p["volume"]["label"], "Vault");
    assert_eq!(p["volume"]["mount"], "/media/me/Vault");
    assert_eq!(
        p["disk"]["serial"], "WD-WXC2D71",
        "the disk under the LUKS volume"
    );
    assert_eq!(p["present"], true);
    let home = place(&s, "/home/me/Photos/x.jpg", Some(2));
    assert_eq!(home["volume"]["uuid"], "root-uuid");
    assert_eq!(home["disk"]["model"], "Samsung SSD 980");
    // whole names only: /media/me/Vaulted isn't inside /media/me/Vault
    assert_eq!(
        place(&s, "/media/me/Vaulted/x", None)["volume"]["mount"],
        "/"
    );
    // a drive that isn't connected: the one it was last scanned on
    let away = place(&s, "/mnt/bd1a/old/file.txt", Some(4));
    assert_eq!(away["disk"]["serial"], "WX12");
    assert_eq!(away["present"], false);
    // nothing known: no machine report at all
    assert!(place(&json!({}), "/x", None).is_null());
}

#[test]
fn windows_paths_are_matched_their_way() {
    let vol = json!({ "device": "\\\\?\\Volume{1}", "label": "Backup", "fs_name": "NTFS", "state": "mounted",
        "mounts": [{ "path": "E:\\", "read_only": false }], "indexed": [], "holders": [] });
    let s = json!({ "os": "windows", "disks": [{ "name": "Disk 1", "model": "ST2000", "serial": "Z4Z", "volumes": [vol] }] });
    let p = place(&s, "e:\\Photos\\2020\\a.jpg", None);
    assert_eq!(p["volume"]["label"], "Backup");
    assert!(place(&s, "F:\\x", None).is_null());
}

#[test]
fn a_machine_view_counts_each_file_system_once_and_sorts_drives_out() {
    let s = linux_storage();
    let v = machine_view(
        &s,
        &HashMap::from([("sn:WDWXC2D71".to_string(), "warning".to_string())]),
    );
    let disks = v["disks"].as_array().unwrap();
    // the namespace sliver is left out; swap and the unrecognised partition too
    assert_eq!(disks.len(), 2, "{v}");
    let ssd = &disks[0]["volumes"];
    assert_eq!(ssd.as_array().unwrap().len(), 2, "EFI and /: {ssd}");
    // the LUKS volume shows as what's in it
    let usb = &disks[1];
    assert_eq!(usb["health"], "warning");
    assert_eq!(usb["volumes"][0]["label"], "Vault");
    assert_eq!(usb["volumes"][0]["via"], "LUKS");
    assert_eq!(usb["volumes"][0]["indexed"][0]["files"], 1000);
    assert_eq!(ssd[1]["indexed"][0]["name"], "Photos");
    // the share counts its room; the unplugged drive is listed with the drive it was on
    assert_eq!(v["elsewhere"][0]["name"], "share");
    assert_eq!(v["away"][0]["name"], "bd1a");
    assert_eq!(v["away"][0]["drive"]["disk"]["serial"], "WX12");
    let t = &v["totals"];
    assert_eq!(t["disks"], 2);
    assert_eq!(
        t["capacity"].as_u64().unwrap(),
        500_000_000 + 990_000_000_000 + 3_900_000_000_000 + 2_000_000_000_000
    );
    assert_eq!(
        t["free"].as_u64().unwrap(),
        400_000_000 + 100_000_000_000 + 900_000_000_000 + 500_000_000_000
    );
    assert_eq!(t["files"], 1000 + 50 + 7 + 1_200_000);
}

#[test]
fn apfs_volumes_sharing_a_container_are_counted_once() {
    let vol = |name: &str, mount: &str| {
        json!({ "device": format!("/dev/{name}"), "name": name, "fs": "apfs", "fs_name": "APFS", "state": "mounted",
                "mounts": [{ "path": mount, "read_only": false }], "space": { "total": 500, "free": 200 },
                "indexed": [], "holders": [] })
    };
    let s = json!({ "os": "macos", "disks": [{ "name": "disk3", "size": 500, "volumes": [
        vol("disk3s1", "/System/Volumes/Data"), vol("disk3s5", "/"), vol("disk3s6", "/System/Volumes/VM") ] }], "roots": [] });
    let v = machine_view(&s, &HashMap::new());
    assert_eq!(
        (v["totals"]["capacity"].clone(), v["totals"]["free"].clone()),
        (json!(500), json!(200))
    );
    // what's under / is on the data volume, through the firmlinks
    assert_eq!(
        place(&s, "/Users/me/x", None)["volume"]["mount"],
        "/System/Volumes/Data"
    );
}

#[test]
fn names_rank_as_find_ranks_them() {
    let t = |q: &str| terms(q);
    assert_eq!(rank("report.txt", &t("report")), 4);
    assert_eq!(rank("report", &t("Report")), 4);
    assert_eq!(rank("reports 2020.pdf", &t("report")), 3);
    assert_eq!(rank("q3 report.pdf", &t("report")), 2);
    assert_eq!(rank("misreported.txt", &t("report")), 1);
    assert_eq!(rank("q3 report.pdf", &t("report q4")), 0, "every word");
    assert_eq!(rank("anything", &[]), 0);
    assert_eq!(terms("  Résumé   2021 "), vec!["resume", "2021"]);
}

#[test]
fn the_same_drive_is_known_by_its_serial_or_its_file_system() {
    let a = json!({ "disk": { "serial": " WD-WX12 " }, "volume": { "uuid": "u1" } });
    let b = json!({ "disk": { "serial": "WDWX12" }, "volume": { "uuid": "u2" } });
    assert!(
        same_drive(&a, &b),
        "serials as the health readings spell them"
    );
    let c = json!({ "disk": { "serial": null }, "volume": { "uuid": "u1" } });
    assert!(same_drive(&a, &c), "no serial: the file system's UUID");
    let d = json!({ "disk": {}, "volume": { "uuid": "u3" } });
    assert!(!same_drive(&c, &d));
}

#[test]
fn within_gives_up_on_what_hangs() {
    assert_eq!(within(Duration::from_secs(2), || 7), Some(7));
    let slow = within(Duration::from_millis(50), || {
        std::thread::sleep(Duration::from_millis(500));
        1
    });
    assert_eq!(slow, None);
}

// ---------- is it the drive it was?

#[test]
fn a_drive_is_the_one_on_record_by_its_serial_or_its_file_system() {
    let s = linux_storage();
    let vault = place(&s, "/media/me/Vault/Archive", None);
    // the folder a drive is mounted on, with the drive mounted elsewhere: the system disk
    let stub = place(&s, "/media/me/Big/Archive", None);
    assert_eq!(stub["disk"]["system"], true);
    assert_eq!(judge(&vault, &vault), OnDrive::Same);
    assert_eq!(judge(&vault, &stub), OnDrive::Other);
    assert_eq!(judge(&stub, &vault), OnDrive::Other);
    // another USB enclosure reports its own serial; the file system is the one it was
    let moved = json!({ "disk": { "serial": "ENCLOSURE-77" }, "volume": { "uuid": "vault-uuid" } });
    assert_eq!(judge(&vault, &moved), OnDrive::Same);
    // reformatted: a new file system on the same disk
    let reformatted =
        json!({ "disk": { "serial": "WD-WXC2D71" }, "volume": { "uuid": "new-uuid" } });
    assert_eq!(judge(&vault, &reformatted), OnDrive::Same);
    // serials as the health readings spell them, UUIDs whatever their case
    let spelt = json!({ "disk": { "serial": " wd wxc2d71 " }, "volume": { "uuid": null } });
    assert_eq!(judge(&vault, &spelt), OnDrive::Same);
    let upper = json!({ "disk": {}, "volume": { "uuid": "VAULT-UUID" } });
    assert_eq!(judge(&vault, &upper), OnDrive::Same);
    // one thing to compare, and it differs
    let other_fs = json!({ "disk": { "serial": null }, "volume": { "uuid": "u9" } });
    assert_eq!(judge(&vault, &other_fs), OnDrive::Other);
    // nothing to compare: no drive on record, the drive here unknown, a junk serial, or a serial
    // on one side and only a file system on the other
    assert_eq!(judge(&Value::Null, &vault), OnDrive::Unknown);
    assert_eq!(judge(&vault, &Value::Null), OnDrive::Unknown);
    let junk = json!({ "disk": { "serial": "0000000000" }, "volume": {} });
    assert_eq!(judge(&vault, &junk), OnDrive::Unknown);
    let serial_only = json!({ "disk": { "serial": "X1" }, "volume": {} });
    let uuid_only = json!({ "disk": {}, "volume": { "uuid": "u1" } });
    assert_eq!(judge(&serial_only, &uuid_only), OnDrive::Unknown);
}

#[test]
fn drives_are_named_for_people() {
    let s = linux_storage();
    assert_eq!(
        drive_name(&place(&s, "/media/me/Vault", None)),
        "WD Elements 2620 (serial WD-WXC2D71), file system \"Vault\""
    );
    assert_eq!(
        drive_name(&place(&s, "/srv/x", None)),
        "the system disk (Samsung SSD 980, serial S64ANS0T1), file system UUID root-uuid"
    );
    assert_eq!(drive_name(&json!({ "disk": {}, "volume": {} })), "a disk");
}

#[test]
fn what_the_probe_doesnt_know_isnt_taken_for_the_drive_below() {
    let mounts = ["/", "/boot/efi", "/media/me/Vault", "/mnt/share", "/tmp"];
    let here = Here {
        storage: linux_storage(),
        mounts: mounts.iter().map(|m| m.to_string()).collect(),
    };
    let on = |p: &str| here.drive(Path::new(p));
    assert_eq!(on("/media/me/Vault/a")["volume"]["label"], "Vault");
    assert!(
        on("/media/me/Vault/a").get("present").is_none(),
        "kept as it's recorded"
    );
    assert_eq!(on("/media/me/Big")["volume"]["uuid"], "root-uuid");
    // a network share, a tmpfs: mounted, but on nothing the probe knows
    assert!(on("/mnt/share/docs").is_null());
    assert!(on("/tmp/x").is_null());
    // a drive with nothing to know it by
    let bare = json!({ "os": "linux", "disks": [{ "name": "sdz", "volumes": [{ "device": "/dev/sdz1",
        "mounts": [{ "path": "/mnt/bare", "read_only": false }], "indexed": [], "holders": [] }] }] });
    let here = Here {
        storage: bare,
        mounts: vec!["/".into(), "/mnt/bare".into()],
    };
    assert!(here.drive(Path::new("/mnt/bare/x")).is_null());
}

/// A machine with a system disk (mounted at /) and a USB drive mounted at `usb_at`, if anywhere.
fn machine(usb_at: Option<&Path>) -> Value {
    let mounts = |p: Option<&Path>| match p {
        Some(p) => json!([{ "path": p.display().to_string(), "read_only": false }]),
        None => json!([]),
    };
    let root = json!({ "device": "/dev/nvme0n1p2", "fs": "ext4", "uuid": "root-uuid", "state": "system",
        "mounts": mounts(Some(Path::new("/"))), "indexed": [], "holders": [] });
    let big = json!({ "device": "/dev/sdb1", "fs": "ext4", "label": "BIG", "uuid": "big-uuid",
        "state": "mounted", "mounts": mounts(usb_at), "indexed": [], "holders": [] });
    json!({ "os": "linux", "disks": [
        { "name": "nvme0n1", "model": "Samsung SSD 980", "serial": "S64ANS0T1", "system": true, "volumes": [root] },
        { "name": "sdb", "model": "WDC WD180EDGZ", "serial": "2BG0ABCD", "transport": "usb", "volumes": [big] },
    ] })
}

/// What `Here::probe` finds from now on, on this thread.
fn probed(storage: Value) {
    PROBED.with(|p| *p.borrow_mut() = Some(storage));
}

fn scan(db: &mut Db, root: &Path, allow_other_drive: bool) -> Result<crate::index::Stats> {
    let opts = crate::index::Options {
        one_file_system: true,
        allow_empty: false,
        allow_other_drive,
    };
    crate::index::scan(db, root, &opts, crate::index::Method::Walk, &mut |_| {})
}

fn count(db: &Db, sql: &str) -> i64 {
    db.conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

/// A drive with three files at `tmp/media/me/BIG`, indexed and its drive remembered while it's
/// mounted there: the index, the drive's folder and its root id.
fn indexed_drive(tmp: &Path) -> (Db, std::path::PathBuf, i64) {
    let drive = tmp.join("media/me/BIG");
    for f in ["Archive/2019/a.jpg", "Archive/2019/b.jpg", "notes.txt"] {
        std::fs::create_dir_all(drive.join(f).parent().unwrap()).unwrap();
        std::fs::write(drive.join(f), f.as_bytes()).unwrap();
    }
    let drive = std::fs::canonicalize(&drive).unwrap();
    let mut db = Db::open(&tmp.join("idx.db")).unwrap();
    probed(machine(Some(&drive)));
    assert_eq!(scan(&mut db, &drive, false).unwrap().files, 3);
    let root = crate::forget::root_id(&db, &drive).unwrap().unwrap();
    assert!(remember_drive(&db, root, &drive, false).unwrap());
    assert_eq!(
        recorded_drive(&db, root).unwrap().unwrap()["disk"]["serial"],
        "2BG0ABCD"
    );
    (db, drive, root)
}

/// What a reboot did to a user's server: something made folders at the drive's path, on the
/// system disk, before the drive was mounted, so the drive was mounted elsewhere. What's there
/// isn't empty (nor free of files).
fn stub_in_its_place(tmp: &Path, drive: &Path) {
    probed(machine(Some(&tmp.join("media/me/BIG1"))));
    std::fs::remove_dir_all(drive).unwrap();
    for d in ["Archive/_stalwart/etc", "Archive/_stalwart/data"] {
        std::fs::create_dir_all(drive.join(d)).unwrap();
    }
    std::fs::write(drive.join("Archive/_stalwart/etc/config.toml"), b"x").unwrap();
}

#[test]
fn a_scan_of_a_folder_on_another_drive_is_refused_and_records_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut db, drive, root) = indexed_drive(tmp.path());
    stub_in_its_place(tmp.path(), &drive);

    let scans = count(&db, "SELECT COUNT(*) FROM scans");
    let err = scan(&mut db, &drive, false).unwrap_err();
    let refused = err
        .downcast_ref::<crate::index::Refused>()
        .expect("refused");
    assert_eq!(refused.why, crate::index::Refusal::OtherDrive);
    let msg = err.to_string();
    for part in [
        "isn't on the drive it was last scanned on",
        "WDC WD180EDGZ (serial 2BG0ABCD), file system \"BIG\"",
        "the system disk (Samsung SSD 980, serial S64ANS0T1)",
        "(mounted at /)",
        "--allow-other-drive",
        "backdown forget",
    ] {
        assert!(msg.contains(part), "{part:?} in {msg}");
    }
    let current = "SELECT COUNT(*) FROM files WHERE current = 1";
    assert_eq!(count(&db, current), 3, "nothing recorded as gone");
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM scans"),
        scans,
        "not even an unfinished scan"
    );
    // the change log and file table ways are refused alike
    let opts = crate::index::Options {
        one_file_system: true,
        allow_empty: false,
        allow_other_drive: false,
    };
    let auto = crate::index::scan(
        &mut db,
        &drive,
        &opts,
        crate::index::Method::Auto,
        &mut |_| {},
    );
    assert!(auto.unwrap_err().is::<crate::index::Refused>());

    // whatever was scanned at the path, the drive on record stays the one it was
    assert!(!remember_drive(&db, root, &drive, false).unwrap());
    assert_eq!(
        recorded_drive(&db, root).unwrap().unwrap()["disk"]["serial"],
        "2BG0ABCD"
    );

    // told it really is the drive now (a new one in its place): recorded, and from then on
    // remembered as the drive
    let st = scan(&mut db, &drive, true).unwrap();
    assert_eq!((st.files, st.removed), (1, 3));
    assert!(remember_drive(&db, root, &drive, true).unwrap());
    assert_eq!(
        recorded_drive(&db, root).unwrap().unwrap()["disk"]["system"],
        true
    );
    assert_eq!(scan(&mut db, &drive, false).unwrap().files, 1);
}

#[test]
fn where_the_drive_cant_be_told_the_other_guards_decide() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut db, drive, _) = indexed_drive(tmp.path());
    stub_in_its_place(tmp.path(), &drive);
    // the probe failed, or the folder is on a network share
    probed(json!({ "os": "linux", "disks": [] }));
    let st = scan(&mut db, &drive, false).unwrap();
    assert_eq!((st.files, st.removed), (1, 3));
}
