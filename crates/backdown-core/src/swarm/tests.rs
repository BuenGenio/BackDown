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
