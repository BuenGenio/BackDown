use std::fs;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rusqlite::params;
use serde_json::json;
use sha2::{Digest, Sha256};

use super::plan::{self, AreaRef};
use super::*;
use crate::{Db, hash, index, recognise, remote, swarm};

fn scratch() -> tempfile::TempDir {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp");
    fs::create_dir_all(&base).unwrap();
    tempfile::tempdir_in(fs::canonicalize(base).unwrap()).unwrap()
}

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn blob(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed) ^ ((i >> 8) as u8))
        .collect()
}

fn scan(db: &mut Db, drive: &Path) {
    index::index(
        db,
        drive,
        &index::Options {
            one_file_system: true,
            allow_empty: false,
        },
        &mut |_| {},
    )
    .unwrap();
}

fn hash_all(db: &mut Db) {
    hash::hash(
        db,
        &hash::Options {
            jobs_per_device: 1,
            retry_errors: false,
        },
        &mut |_| {},
    )
    .unwrap();
}

fn root_id(db: &Db, p: &Path) -> i64 {
    db.conn
        .query_row(
            "SELECT id FROM roots WHERE path = ?1",
            [crate::paths::to_bytes(p)],
            |r| r.get(0),
        )
        .unwrap()
}

/// Another machine, "nas", as this one has it cached: a pool folder /srv/pool taking Media, with
/// one file in it whose content is `content`.
fn a_nas_with_a_pool_folder(db: &Db, content: &[u8], free: u64) -> i64 {
    let m = remote::add(db, "nas", "ssh://nas.example.org", None, None).unwrap();
    db.conn
        .execute(
            "INSERT INTO remote_roots(remote_id, root_id, path, display, last_scan, synced_scan, seen_at)
             VALUES (?1, 7, ?2, '/srv/pool', 3, 3, ?3)",
            params![m.id, b"/srv/pool".to_vec(), remote::now()],
        )
        .unwrap();
    let sha = Sha256::digest(content).to_vec();
    db.conn
        .execute(
            "INSERT INTO remote_files(remote_id, rid, root_id, path, size, mtime, sample, sha256, error)
             VALUES (?1, 1, 7, ?2, ?3, 1, NULL, ?4, 0)",
            params![m.id, b"/srv/pool/Media/Photos/a.jpg".to_vec(), content.len() as i64, sha],
        )
        .unwrap();
    let area = json!({ "id": 1, "path": "/srv/pool", "key": URL_SAFE_NO_PAD.encode(b"/srv/pool"),
        "reserve": 0, "takes": ["Media"], "present": true, "indexed": true, "answering": true,
        "space": { "total": 10_000_000_000u64, "free": free }, "room": free, "files": 1, "bytes": content.len() });
    let mount = json!([{ "path": "/srv", "read_only": false }]);
    let vol = json!({ "device": "/dev/sdb1", "name": "sdb1", "fs": "ext4", "fs_name": "ext4", "label": "Pool",
        "state": "mounted", "mounts": mount, "space": { "total": 10_000_000_000u64, "free": free },
        "indexed": [], "holders": [] });
    let report = json!({ "os": "linux", "disks": [{ "name": "sdb", "model": "BIG DISK", "serial": "B1",
        "volumes": [vol] }], "roots": [], "pool": [area] });
    remote::store_volumes(db, m.id, &Ok(report)).unwrap();
    m.id
}

#[test]
fn a_drive_is_planned_into_the_pool_once_per_content() {
    let tmp = scratch();
    let drive = tmp.path().join("OLD-DRIVE");
    let pool_here = tmp.path().join("pool-here");
    let photo_a = blob(1, 300_000);
    let photo_b = blob(2, 300_001);
    write(&drive.join("Photos/a.jpg"), &photo_a);
    write(&drive.join("Photos/b.jpg"), &photo_b);
    write(&drive.join("Photos/b copy.jpg"), &photo_b);
    write(&drive.join("Docs/notes.txt"), &blob(3, 2_000));
    write(&drive.join("Docs/letter.odt"), &blob(4, 9_000));
    // this machine's pool folder already has the drive's Docs place, from an earlier time
    write(
        &pool_here.join("Unsorted/OLD-DRIVE/Docs/older.txt"),
        &blob(5, 777),
    );
    let mut db = Db::open(&tmp.path().join("i.db")).unwrap();
    scan(&mut db, &drive);
    scan(&mut db, &pool_here);
    let nas = a_nas_with_a_pool_folder(&db, &photo_a, 5_000_000_000);
    hash_all(&mut db);
    add(&db, &pool_here, Some(Reserve::Bytes(0)), &[]).unwrap();

    let rid = root_id(&db, &drive);
    let rec = recognise::recognise(&db, rid, true, &mut |_| {}).unwrap();
    recognise::store(&db, &rec).unwrap();
    let here = swarm::storage(&db).unwrap();
    let areas = plan::areas(&db, "laptop", &here).unwrap();
    assert_eq!(areas.len(), 2, "this machine's folder and the nas's");
    let theirs = areas.iter().find(|a| a.at.machine == "nas").unwrap();
    assert_eq!(theirs.at.machine_id, Some(nas));
    assert_eq!(theirs.takes, vec!["Media"]);
    assert_eq!(theirs.fs, "/srv", "its file system, from its drives");
    assert_eq!(theirs.room, 5_000_000_000);

    let mut pp = plan::make(
        &db,
        "laptop",
        rid,
        &crate::plan::Schema::default(),
        std::slice::from_ref(&rec),
        &areas,
    )
    .unwrap();
    assert_eq!(pp.files, 5);
    let thing = |pp: &plan::PoolPlan, name: &str| {
        pp.things
            .iter()
            .find(|t| t.item.source.ends_with(name))
            .unwrap_or_else(|| panic!("no {name} in {:#?}", pp.things))
            .clone()
    };
    // a.jpg is in the nas's pool folder by content; b.jpg's twin goes once
    let photos = thing(&pp, "Photos");
    assert_eq!(photos.in_pool_files, 1);
    assert!(
        photos
            .in_pool_at
            .as_deref()
            .unwrap()
            .starts_with("nas: /srv/pool/Media"),
        "{photos:?}"
    );
    assert_eq!((photos.twice_files, photos.new_files), (1, 1));
    assert_eq!(photos.need(), 300_001);
    // the nas takes only Media, so Unsorted things come here, where the drive's place already
    // is (the biggest first), and the rest of that place with it
    let docs = thing(&pp, "Docs");
    assert_eq!(docs.new_files, 2);
    let here_ref = photos.to.clone().expect("placed");
    assert!(here_ref.here());
    assert!(
        photos
            .why_to
            .contains("Unsorted/OLD-DRIVE is there already"),
        "{}",
        photos.why_to
    );
    assert_eq!(docs.to.as_ref(), Some(&here_ref));
    // its own place first: that very folder is there already
    assert!(
        docs.why_to
            .contains("Unsorted/OLD-DRIVE/Docs is there already"),
        "{}",
        docs.why_to
    );
    assert_eq!(pp.new_bytes, 300_001 + 2_000 + 9_000);
    assert_eq!(pp.in_pool_bytes, 300_000);
    assert_eq!(pp.unplaced_bytes, 0);
    let use_here = pp
        .areas
        .iter()
        .find(|u| u.at.as_ref() == Some(&here_ref))
        .unwrap();
    assert_eq!(use_here.assigned_bytes, pp.new_bytes);

    // the person puts the photos on the nas instead: their choice goes, whatever it takes
    let i = pp
        .things
        .iter()
        .position(|t| t.item.source.ends_with("Photos"))
        .unwrap();
    let nas_ref = AreaRef {
        machine: "nas".into(),
        machine_id: Some(nas),
        area: 1,
        path: "/srv/pool".into(),
    };
    plan::choose(&db, &mut pp, &areas, i, Some(nas_ref.clone()), false).unwrap();
    assert_eq!(pp.things[i].to.as_ref(), Some(&nas_ref));
    assert_eq!(pp.things[i].why_to, "chosen");
    let use_nas = pp
        .areas
        .iter()
        .find(|u| u.at.as_ref() == Some(&nas_ref))
        .unwrap();
    assert_eq!(use_nas.assigned_bytes, 300_001);
    // the docs stay where their own folder is, even though the photos left
    let docs = thing(&pp, "Docs");
    assert_eq!(docs.to.as_ref(), Some(&here_ref), "{}", docs.why_to);
    // ...or leaves the docs out
    let j = pp
        .things
        .iter()
        .position(|t| t.item.source.ends_with("Docs"))
        .unwrap();
    plan::choose(&db, &mut pp, &areas, j, None, true).unwrap();
    assert!(pp.things[j].skip && pp.things[j].to.is_none());
    assert_eq!(pp.left_bytes, 11_000);
    assert_eq!(pp.copy_bytes, 300_001, "what's left out isn't copied");
    // kept, and read back the same
    let id = plan::save(&db, &mut pp).unwrap();
    let back = plan::load(&db, id).unwrap();
    assert_eq!(back.things.len(), pp.things.len());
    assert_eq!(back.things[i].to.as_ref(), Some(&nas_ref));
    assert_eq!(plan::list(&db).unwrap()[0]["id"], id);
}

#[test]
fn what_fits_nowhere_is_said_and_the_reserve_is_kept() {
    let tmp = scratch();
    let drive = tmp.path().join("CARD");
    write(&drive.join("DCIM/big.mov"), &blob(9, 400_000));
    let mut db = Db::open(&tmp.path().join("i.db")).unwrap();
    scan(&mut db, &drive);
    // the only pool folder is the nas's, taking anything, with 100 KB free and 50 KB kept free
    let nas = a_nas_with_a_pool_folder(&db, &blob(8, 10), 100_000);
    let (_, report, ..) = remote::kept_volumes(&db, nas).unwrap().unwrap();
    let mut report = report.unwrap();
    report["pool"][0]["takes"] = json!([]);
    report["pool"][0]["reserve"] = json!(50_000);
    remote::store_volumes(&db, nas, &Ok(report)).unwrap();
    hash_all(&mut db);
    let rid = root_id(&db, &drive);
    let rec = recognise::recognise(&db, rid, true, &mut |_| {}).unwrap();
    recognise::store(&db, &rec).unwrap();
    let here = swarm::storage(&db).unwrap();
    let areas = plan::areas(&db, "laptop", &here).unwrap();
    assert_eq!(areas[0].room, 50_000, "free less the reserve");
    let pp = plan::make(
        &db,
        "laptop",
        rid,
        &crate::plan::Schema::default(),
        &[rec],
        &areas,
    )
    .unwrap();
    assert_eq!(pp.unplaced_bytes, 400_000);
    let t = &pp.things[0];
    assert!(t.to.is_none());
    // its place is on the nas already, and that's what's said
    assert!(
        t.why_to.contains("hasn't room for 400 KB more"),
        "{}",
        t.why_to
    );
}

#[test]
fn pool_folders_are_kept_apart_and_report_their_room() {
    let tmp = scratch();
    let db = Db::open(&tmp.path().join("i.db")).unwrap();
    let a = add(
        &db,
        &tmp.path().join("pool"),
        Some(Reserve::Bytes(1)),
        &["Media".into()],
    )
    .unwrap();
    assert!(tmp.path().join("pool").is_dir(), "created");
    assert!(
        add(&db, &tmp.path().join("pool/inner"), None, &[]).is_err(),
        "inside another"
    );
    assert!(add(&db, tmp.path(), None, &[]).is_err(), "holding another");
    assert!(add(&db, Path::new("relative/pool"), None, &[]).is_err());
    let r = report(&db).unwrap();
    assert_eq!(r[0]["id"], a.id);
    assert_eq!(r[0]["present"], true);
    assert_eq!(r[0]["indexed"], false, "nothing indexes it yet");
    assert!(r[0]["room"].as_u64().unwrap() > 0);
    let a = set(&db, a.id, None, Some(&[])).unwrap();
    assert!(a.takes.is_empty());
    assert_eq!(find(&db, &a.path).unwrap().id, a.id);
    remove(&db, a.id).unwrap();
    assert!(list(&db).unwrap().is_empty());
}
