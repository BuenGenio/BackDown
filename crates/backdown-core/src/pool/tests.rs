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
    // in its normal form, as a person gives a path (not `\\?\` on Windows)
    tempfile::tempdir_in(crate::paths::canonical(&base).unwrap()).unwrap()
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
            allow_other_drive: false,
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
            params![m.id, b"/srv/pool".to_vec(), crate::clock::now()],
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

// ---------- copying into the pool (send.rs, receive.rs)

use std::io::Cursor;
use std::path::PathBuf;

use super::receive::{self, Origin};
use super::send::{self, Connect, Here, Sink};

/// Another machine's end, in-process: its own index, its own pool folders, written by the same
/// code `backdown swarm-stdio` runs there.
struct Nas {
    db: PathBuf,
}

impl Sink for Nas {
    fn put(
        &mut self,
        area: &AreaRef,
        rel: &str,
        size: u64,
        mtime: i64,
        src: &mut dyn std::io::Read,
        origin: &Origin,
    ) -> anyhow::Result<std::result::Result<receive::Received, String>> {
        let db = Db::open(&self.db)?;
        let a = get(&db, area.area)?;
        Ok(receive::put(&db, &a, rel, size, mtime, src, origin).map_err(|e| format!("{e:#}")))
    }
    fn done(&mut self, _: &AreaRef) -> anyhow::Result<String> {
        Ok("indexing it".into())
    }
}

struct ToNas(PathBuf);

impl Connect for ToNas {
    fn connect(&mut self, _: i64) -> anyhow::Result<Box<dyn Sink>> {
        Ok(Box::new(Nas { db: self.0.clone() }))
    }
}

/// This machine's system, as it says it in its `hello` and its storage report.
const THIS_OS: &str = if cfg!(windows) { "windows" } else { "linux" };

/// The nas's pool folder as this machine has it cached (as `a_nas_with_a_pool_folder` does), but
/// a real folder of a real second index, so files can be written into it.
fn a_real_nas(db: &Db, nas_db: &Path, pool_dir: &Path, already: &[u8]) -> (i64, i64) {
    let nas = Db::open(nas_db).unwrap();
    let a = add(&nas, pool_dir, Some(Reserve::Bytes(0)), &[]).unwrap();
    let m = remote::add(db, "nas", "ssh://nas.example.org", None, None).unwrap();
    // it's this machine really, so its paths are spelt as this machine's are
    remote::record_hello(db, m.id, &json!({ "os": THIS_OS }), crate::clock::now()).unwrap();
    let raw = crate::paths::to_bytes(pool_dir);
    db.conn
        .execute(
            "INSERT INTO remote_roots(remote_id, root_id, path, display, last_scan, synced_scan, seen_at)
             VALUES (?1, 7, ?2, ?3, 3, 3, ?4)",
            params![m.id, raw, pool_dir.display().to_string(), crate::clock::now()],
        )
        .unwrap();
    let in_it = pool_dir.join("Media").join("old.jpg");
    db.conn
        .execute(
            "INSERT INTO remote_files(remote_id, rid, root_id, path, size, mtime, sample, sha256, error)
             VALUES (?1, 1, 7, ?2, ?3, 1, NULL, ?4, 0)",
            params![m.id, crate::paths::to_bytes(&in_it), already.len() as i64, Sha256::digest(already).to_vec()],
        )
        .unwrap();
    let area = json!({ "id": a.id, "path": pool_dir.display().to_string(), "key": URL_SAFE_NO_PAD.encode(&raw),
        "reserve": 0, "takes": [], "present": true, "indexed": true, "answering": true,
        "space": { "total": 10_000_000_000u64, "free": 9_000_000_000u64 }, "files": 1, "bytes": already.len() });
    let report = json!({ "os": THIS_OS, "disks": [], "roots": [], "pool": [area] });
    remote::store_volumes(db, m.id, &Ok(report)).unwrap();
    (m.id, a.id)
}

fn plan_for(db: &mut Db, drive: &Path) -> (plan::PoolPlan, Vec<plan::Area>) {
    hash_all(db);
    let rid = root_id(db, drive);
    let rec = recognise::recognise(db, rid, true, &mut |_| {}).unwrap();
    recognise::store(db, &rec).unwrap();
    let here = swarm::storage(db).unwrap();
    let areas = plan::areas(db, "laptop", &here).unwrap();
    let pp = plan::make(
        db,
        "laptop",
        rid,
        &crate::plan::Schema::default(),
        &[rec],
        &areas,
    )
    .unwrap();
    (pp, areas)
}

#[test]
fn a_plan_is_carried_out_into_pool_folders_here_and_there() {
    let tmp = scratch();
    let drive = tmp.path().join("OLD-DRIVE");
    let pool_here = tmp.path().join("pool-here");
    let nas_pool = tmp.path().join("nas/pool");
    let photo_a = blob(1, 300_000);
    let photo_b = blob(2, 300_001);
    write(&drive.join("Photos/a.jpg"), &photo_a);
    write(&drive.join("Photos/b.jpg"), &photo_b);
    write(&drive.join("Photos/b copy.jpg"), &photo_b);
    write(&drive.join("Docs/notes.txt"), &blob(3, 2_000));
    write(&drive.join("Docs/letter.odt"), &blob(4, 9_000));
    write(
        &pool_here.join("Unsorted/OLD-DRIVE/Docs/older.txt"),
        &blob(5, 777),
    );
    fs::create_dir_all(&nas_pool).unwrap();
    let mut db = Db::open(&tmp.path().join("i.db")).unwrap();
    scan(&mut db, &drive);
    scan(&mut db, &pool_here);
    add(&db, &pool_here, Some(Reserve::Bytes(0)), &[]).unwrap();
    let nas_db = tmp.path().join("nas.db");
    let (nas, nas_area) = a_real_nas(&db, &nas_db, &nas_pool, &photo_a);
    let (mut pp, areas) = plan_for(&mut db, &drive);
    // the photos go to the nas, by choice; the docs where their place is, here
    let i = pp
        .things
        .iter()
        .position(|t| t.item.source.ends_with("Photos"))
        .unwrap();
    let to_nas = AreaRef {
        machine: "nas".into(),
        machine_id: Some(nas),
        area: nas_area,
        path: nas_pool.display().to_string(),
    };
    plan::choose(&db, &mut pp, &areas, i, Some(to_nas.clone()), false).unwrap();
    let docs = pp
        .things
        .iter()
        .find(|t| t.item.source.ends_with("Docs"))
        .unwrap();
    assert!(
        docs.to.as_ref().is_some_and(AreaRef::here),
        "the docs stay where their place is: {docs:#?} {areas:#?}"
    );
    let id = plan::save(&db, &mut pp).unwrap();

    let mut here = Here {
        db: &db,
        to_index: Vec::new(),
    };
    let st = send::send(
        &db,
        id,
        "laptop",
        &areas,
        &mut here,
        &mut ToNas(nas_db.clone()),
        None,
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(
        (st.copied, st.twice, st.in_pool, st.failed, st.changed),
        (3, 1, 1, 0, 0),
        "{st:?}"
    );
    // there: b.jpg (a.jpg was in the pool already; its copy went once)
    let there = nas_pool.join("Unsorted/OLD-DRIVE/Photos");
    assert_eq!(fs::read(there.join("b.jpg")).unwrap(), photo_b);
    assert!(!there.join("a.jpg").exists() && !there.join("b copy.jpg").exists());
    // here: the docs, next to what was there
    let docs = pool_here.join("Unsorted/OLD-DRIVE/Docs");
    assert_eq!(fs::read(docs.join("notes.txt")).unwrap(), blob(3, 2_000));
    assert!(docs.join("older.txt").exists() && docs.join("letter.odt").exists());
    assert_eq!(
        here.to_index,
        vec![PathBuf::from(pool_here.display().to_string())]
    );
    assert_eq!(plan::load(&db, id).unwrap().state, "done");
    assert!(
        st.done
            .iter()
            .any(|d| d.contains("nas:") && d.contains("indexing it")),
        "{:?}",
        st.done
    );
    // the nas keeps what it received, and where from, in its chain of custody
    let nas_db_open = Db::open(&nas_db).unwrap();
    let (from, sha): (String, Vec<u8>) = nas_db_open
        .conn
        .query_row("SELECT from_machine, sha256 FROM pool_received", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(
        (from.as_str(), sha),
        ("laptop", Sha256::digest(&photo_b).to_vec())
    );
    let (job, src): (String, Vec<u8>) = nas_db_open
        .conn
        .query_row(
            "SELECT ref, src FROM custody ORDER BY seq DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(job, format!("pool:{id}"));
    assert!(
        String::from_utf8_lossy(&src).starts_with("laptop:")
            && String::from_utf8_lossy(&src).ends_with("b.jpg")
    );
    // what was sent has its hash known here now
    let unhashed: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM files WHERE root_id = ?1 AND current = 1 AND sha256 IS NULL",
            [root_id(&db, &drive)],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(unhashed, 0);
    // again: everything carries on from the log, nothing is sent twice
    let st = send::send(
        &db,
        id,
        "laptop",
        &areas,
        &mut here,
        &mut ToNas(nas_db.clone()),
        None,
        &mut |_| {},
    )
    .unwrap();
    assert_eq!((st.resumed, st.copied), (5, 0), "{st:?}");
    // a plan of another machine's drive isn't carried out here
    assert!(
        send::send(
            &db,
            id,
            "elsewhere",
            &areas,
            &mut here,
            &mut ToNas(nas_db),
            None,
            &mut |_| {}
        )
        .is_err()
    );
    drop(here);
    // the pool folder here, indexed: what it received is hashed without being read again
    scan(&mut db, &pool_here);
    assert!(receive::adopt(&db).unwrap() >= 2);
    let sha: Option<Vec<u8>> = db
        .conn
        .query_row(
            "SELECT sha256 FROM files WHERE path = ?1 AND current = 1",
            [crate::paths::to_bytes(&docs.join("notes.txt"))],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(sha, Some(Sha256::digest(blob(3, 2_000)).to_vec()));
}

#[test]
fn what_changed_since_the_scan_isnt_sent_and_a_different_file_keeps_its_name() {
    let tmp = scratch();
    let drive = tmp.path().join("D");
    let pool_here = tmp.path().join("pool");
    write(&drive.join("f/x.txt"), &blob(1, 1000));
    write(&drive.join("f/y.txt"), &blob(2, 1001));
    // something else at y.txt's place in the pool
    write(&pool_here.join("Unsorted/D/f/y.txt"), &blob(9, 50));
    let mut db = Db::open(&tmp.path().join("i.db")).unwrap();
    scan(&mut db, &drive);
    scan(&mut db, &pool_here);
    add(&db, &pool_here, Some(Reserve::Bytes(0)), &[]).unwrap();
    let (mut pp, areas) = plan_for(&mut db, &drive);
    let id = plan::save(&db, &mut pp).unwrap();
    // x.txt changes after its scan
    write(&drive.join("f/x.txt"), &blob(1, 1500));
    let mut here = Here {
        db: &db,
        to_index: Vec::new(),
    };
    let st = send::send(
        &db,
        id,
        "laptop",
        &areas,
        &mut here,
        &mut ToNas(PathBuf::new()),
        None,
        &mut |_| {},
    )
    .unwrap();
    assert_eq!((st.copied, st.renamed, st.changed), (1, 1, 1), "{st:?}");
    assert!(
        !pool_here.join("Unsorted/D/f/x.txt").exists(),
        "not as scanned: not sent"
    );
    assert_eq!(
        fs::read(pool_here.join("Unsorted/D/f/y.txt")).unwrap(),
        blob(9, 50),
        "left as it was"
    );
    let beside: Vec<String> = fs::read_dir(pool_here.join("Unsorted/D/f"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(beside.len(), 2, "{beside:?}");
    assert_eq!(plan::load(&db, id).unwrap().state, "partial");
    let s = send::summary(&db, id).unwrap();
    assert!(
        s.iter().any(|(state, n, _)| state == "changed" && *n == 1),
        "{s:?}"
    );
}

#[test]
fn a_refused_file_leaves_the_stream_in_step_and_nothing_lands_outside() {
    let tmp = scratch();
    let db = Db::open(&tmp.path().join("i.db")).unwrap();
    let pool_dir = tmp.path().join("pool");
    let a = add(&db, &pool_dir, Some(Reserve::Bytes(0)), &[]).unwrap();
    let from = Origin {
        machine: "laptop",
        path: "/x",
        plan: 1,
    };
    // two files back to back on one stream: the first refused, the second written whole
    let mut stream = Cursor::new([b"EVIL!".to_vec(), b"good".to_vec()].concat());
    assert!(receive::put(&db, &a, "../evil.txt", 5, 1, &mut stream, &from).is_err());
    let r = receive::put(
        &db,
        &a,
        "Docs/good.txt",
        4,
        1_600_000_000,
        &mut stream,
        &from,
    )
    .unwrap();
    assert_eq!((r.outcome, r.path.as_str()), ("copied", "Docs/good.txt"));
    assert_eq!(fs::read(pool_dir.join("Docs/good.txt")).unwrap(), b"good");
    let mt = fs::metadata(pool_dir.join("Docs/good.txt"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(
        mt.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
        1_600_000_000
    );
    assert!(!tmp.path().join("evil.txt").exists());
    // the same content again: left as it is
    let r = receive::put(
        &db,
        &a,
        "Docs/good.txt",
        4,
        1,
        &mut Cursor::new(b"good".to_vec()),
        &from,
    )
    .unwrap();
    assert_eq!(r.outcome, "same");
    // a link in the pool folder that leads out of it isn't followed
    #[cfg(unix)]
    {
        let outside = tmp.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, pool_dir.join("out")).unwrap();
        let e = receive::put(
            &db,
            &a,
            "out/x.txt",
            3,
            1,
            &mut Cursor::new(b"abc".to_vec()),
            &from,
        )
        .unwrap_err();
        assert!(
            format!("{e:#}").contains("outside the pool folder"),
            "{e:#}"
        );
        assert!(!outside.join("x.txt").exists());
    }
    // a stream that ends early: nothing is left behind
    assert!(
        receive::put(
            &db,
            &a,
            "short.txt",
            10,
            1,
            &mut Cursor::new(b"abc".to_vec()),
            &from
        )
        .is_err()
    );
    assert!(
        !pool_dir.join("short.txt").exists()
            && !pool_dir.join(".short.txt.backdown-partial").exists()
    );
    // the reserve is kept
    let big = set(&db, a.id, Some(Reserve::Percent(89.0)), None).unwrap();
    let free = crate::volumes::space_at(&pool_dir).unwrap();
    if free.free < free.total * 89 / 100 {
        let e = receive::put(
            &db,
            &big,
            "r.txt",
            3,
            1,
            &mut Cursor::new(b"abc".to_vec()),
            &from,
        )
        .unwrap_err();
        assert!(format!("{e:#}").contains("keeps"), "{e:#}");
    }
}
