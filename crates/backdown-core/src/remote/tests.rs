use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use super::serve::{self, Here};
use super::*;
use crate::{Db, hash, index, overlap, report, wipe};

/// Test data lives under the workspace's target/, never in /tmp.
fn scratch() -> tempfile::TempDir {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp");
    fs::create_dir_all(&base).unwrap();
    // in its normal form, as a person gives a path (not `\\?\` on Windows)
    tempfile::tempdir_in(crate::paths::canonical(&base).unwrap()).unwrap()
}

/// `WHERE` a stored path ends in a name, `?1` being `ends(name)`: its bytes are this machine's
/// own (UTF-16 on Windows, which `LIKE` can't read).
const ENDS: &str = "substr(path, -length(?1)) = ?1";

fn ends(name: &str) -> Vec<u8> {
    crate::paths::to_bytes(&Path::new(std::path::MAIN_SEPARATOR_STR).join(name))
}

/// A `/`-separated path as this machine spells it.
fn spelt(p: &str) -> String {
    crate::paths::native(p).display().to_string()
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
    hash_here(db);
}

fn hash_here(db: &mut Db) {
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

/// Forgetting a machine's cached files — all of them, or one drive's — once ran as one statement
/// over millions of rows: a transaction that can't be checkpointed while it runs, and a WAL to
/// match (tomahawk caches 16 M of nextcloud's). It goes a batch at a time now, the WAL reclaimed
/// between.
#[test]
fn forgetting_cached_files_keeps_the_wal_small() {
    let tmp = scratch();
    let path = tmp.path().join("local.db");
    let db = Db::open(&path).unwrap();
    let m = add(&db, "nas", "ssh://nas.example.org", None, None).unwrap();
    db.conn.execute_batch("BEGIN").unwrap();
    {
        let mut put = db
            .conn
            .prepare(
                "INSERT INTO remote_files(remote_id, rid, root_id, path, size, mtime, sample, sha256, error, hashed_at, lname)
                 VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, 0, NULL, ?8)",
            )
            .unwrap();
        for i in 0..200_000i64 {
            let mut sha = [0u8; 32];
            sha[..8].copy_from_slice(&i.to_be_bytes());
            put.execute(params![
                m.id,
                i,
                1 + i % 2,
                format!("/vol/d{}/f{i}", i % 1000).as_bytes(),
                i,
                &sha[..16],
                &sha[..],
                format!("f{i}")
            ])
            .unwrap();
        }
    }
    db.conn.execute_batch("COMMIT").unwrap();
    crate::db::checkpoint(&db.conn);
    let wal = || {
        fs::metadata(path.with_extension("db-wal"))
            .map(|m| m.len())
            .unwrap_or(0)
    };
    let count = || -> i64 {
        db.conn
            .query_row(
                "SELECT COUNT(*) FROM remote_files WHERE remote_id = ?1",
                [m.id],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert_eq!(wal(), 0, "starts with an empty WAL");

    // one drive's files (every other row), then the machine with the rest
    assert_eq!(forget_files(&db.conn, m.id, Some(1)).unwrap(), 100_000);
    assert_eq!(count(), 100_000);
    let after_one = wal();
    assert!(
        after_one < 2 << 20,
        "forgetting one drive's 100,000 cached files left a {after_one}-byte WAL"
    );
    remove(&db, m.id).unwrap();
    assert_eq!(count(), 0);
    assert!(get(&db, m.id).is_err());
    let after_all = wal();
    assert!(
        after_all < 2 << 20,
        "forgetting a machine's 100,000 cached files left a {after_all}-byte WAL"
    );
    assert!(db.conn.is_autocommit());
}

/// A sync of a big drive is hours of pages, each committed on its own. Before, nothing reclaimed
/// the WAL between them: any reader that outlasted a page (a status count, a report) held the
/// backfill off meanwhile, and the file then kept the longest such stretch as its size for the
/// rest of the sync (1 GB on a 17 M-file drive, 9 Nov 2026). Each page's commit reclaims it now.
#[test]
fn a_sync_of_many_pages_keeps_the_wal_small() {
    let tmp = scratch();
    let vol = tmp.path().join("nas/volume1");
    fs::create_dir_all(&vol).unwrap();
    // 60,000 files on one drive there, as a finished scan leaves them (no hashes: nothing to ask for)
    let nas_db = tmp.path().join("nas.db");
    let there = Db::open(&nas_db).unwrap();
    there
        .conn
        .execute(
            "INSERT INTO roots(path) VALUES (?1)",
            [crate::paths::to_bytes(&vol)],
        )
        .unwrap();
    let root_id = there.conn.last_insert_rowid();
    there
        .conn
        .execute(
            "INSERT INTO scans(root_id, started_at, finished_at, files, bytes) VALUES (?1, 1, 2, 60000, 60000)",
            [root_id],
        )
        .unwrap();
    let scan_id = there.conn.last_insert_rowid();
    there
        .conn
        .execute(
            "UPDATE roots SET last_scan = ?2 WHERE id = ?1",
            params![root_id, scan_id],
        )
        .unwrap();
    there.conn.execute_batch("BEGIN").unwrap();
    {
        let mut put = there
            .conn
            .prepare(
                "INSERT INTO files (root_id, path, dir, size, mtime, dev, ino, first_scan, last_scan, current, lname)
                 VALUES (?1, ?2, ?3, ?4, 1, 1, ?5, ?6, ?6, 1, ?7)",
            )
            .unwrap();
        for i in 0..60_000i64 {
            let dir = vol.join(format!("d{}", i % 100));
            let name = format!("f{i}");
            put.execute(params![
                root_id,
                crate::paths::to_bytes(&dir.join(&name)),
                crate::paths::to_bytes(&dir),
                i + 1,
                i + 1,
                scan_id,
                name
            ])
            .unwrap();
        }
    }
    there.conn.execute_batch("COMMIT").unwrap();
    drop(there);

    let path = tmp.path().join("local.db");
    let db = Db::open(&path).unwrap();
    let m = add(&db, "nas", "ssh://nas.example.org", None, None).unwrap();
    let mut peer = Local {
        db: nas_db,
        calls: Vec::new(),
    };
    let opts = SyncOptions {
        page: 5000,
        hash_budget: Duration::ZERO,
        health: false,
        ..SyncOptions::default()
    };
    let st = sync(&db, &m, &mut peer, &opts, &mut |_| {}).unwrap();
    assert_eq!(st.added, 60_000);
    assert!(
        peer.calls.iter().filter(|c| *c == "changes").count() >= 12,
        "60,000 files in pages of 5,000 take at least 12 pages: {:?}",
        peer.calls
    );
    let wal = fs::metadata(path.with_extension("db-wal"))
        .map(|m| m.len())
        .unwrap_or(0);
    assert!(
        wal < 2 << 20,
        "syncing 60,000 files in pages left a {wal}-byte WAL: each page's commit should reclaim it"
    );
    assert!(db.conn.is_autocommit());
}

/// The other machine, in-process: its read-only index answering through the real protocol.
struct Local {
    db: PathBuf,
    calls: Vec<String>,
}

impl Peer for Local {
    fn call(&mut self, op: &str, mut args: Value, _: Duration) -> Result<Value> {
        let db = Db::open_read_only(&self.db)?;
        args["op"] = json!(op);
        args["id"] = json!(self.calls.len() + 1);
        let here = Here {
            db_path: self.db.display().to_string(),
            host: "nas".into(),
        };
        let line = serve::answer_line(
            &db,
            &here,
            &args.to_string(),
            Instant::now() + Duration::from_secs(30),
        );
        let v: Value = serde_json::from_str(&line)?;
        assert_eq!(
            v["id"],
            json!(self.calls.len() + 1),
            "answers carry the request's id"
        );
        self.calls.push(op.to_string());
        if v["ok"] != true {
            bail!("{}", v["error"].as_str().unwrap_or("?"));
        }
        Ok(v["data"].clone())
    }
}

/// A machine running a BackDown whose samples are computed differently: they can't be compared.
struct OtherSamples(Local);

impl Peer for OtherSamples {
    fn call(&mut self, op: &str, args: Value, t: Duration) -> Result<Value> {
        let mut v = self.0.call(op, args, t)?;
        if op == "hello" {
            v["sample"] = json!(SAMPLE + 1);
        }
        Ok(v)
    }
}

#[test]
fn samples_that_cant_be_compared_are_skipped_not_looped_on() {
    let tmp = scratch();
    let laptop = tmp.path().join("local/Laptop");
    let vol = tmp.path().join("nas/volume1");
    write(&laptop.join("big/big.bin"), &blob(3, 320_000));
    write(&vol.join("backup/big.bin"), &blob(3, 320_000));
    write(&laptop.join("decoy.bin"), &blob(4, 310_000));
    write(&vol.join("c.bin"), &blob(5, 310_000));
    let nas_db = tmp.path().join("nas.db");
    let mut there = Db::open(&nas_db).unwrap();
    scan(&mut there, &vol);
    let mut db = Db::open(&tmp.path().join("local.db")).unwrap();
    scan(&mut db, &laptop);
    let m = add(&db, "nas", "ssh://nas.example.org", None, None).unwrap();
    let mut peer = OtherSamples(Local {
        db: nas_db,
        calls: Vec::new(),
    });
    // same size here: hashed there whole, straight away (no samples to compare)
    let st = sync(&db, &m, &mut peer, &SyncOptions::default(), &mut |_| {}).unwrap();
    assert_eq!((st.sampled, st.hashed, st.left), (0, 2, 0));
    let m = get(&db, m.id).unwrap();
    assert!(!waiting(&db, &m).unwrap(), "nothing left to ask for");
    // hashing here then only needs their full hashes
    hash_here(&mut db);
    assert!(
        report::check_indexed(&db, &laptop.join("big"))
            .unwrap()
            .safe
    );
    let st = sync(&db, &m, &mut peer, &SyncOptions::default(), &mut |_| {}).unwrap();
    assert_eq!((st.added, st.sampled, st.hashed), (0, 0, 0));
    assert!(
        !st.changed,
        "a second sync with nothing new changes nothing"
    );
}

struct Reach(Option<PathBuf>);

impl Connect for Reach {
    fn connect(&mut self, _: &Machine) -> Result<Box<dyn Peer>> {
        match &self.0 {
            Some(p) => Ok(Box::new(Local {
                db: p.clone(),
                calls: Vec::new(),
            })),
            None => bail!("ssh: connect to host nas.example.org port 22: No route to host"),
        }
    }
}

#[test]
fn addresses_are_strict() {
    let t = parse_url("ssh://nas.example.org").unwrap();
    assert_eq!(
        (t.user, t.host.as_str(), t.port),
        (None, "nas.example.org", None)
    );
    let t = parse_url("ssh://alice@nas.example.org:2222/").unwrap();
    assert_eq!(t.user.as_deref(), Some("alice"));
    assert_eq!(t.port, Some(2222));
    assert_eq!(t.url(), "ssh://alice@nas.example.org:2222");
    let t = parse_url("ssh://[fd00::1]:22").unwrap();
    assert_eq!(t.host, "fd00::1");
    assert_eq!(t.url(), "ssh://[fd00::1]:22");
    assert_eq!(parse_url("ssh://nas").unwrap().host, "nas"); // an alias from ~/.ssh/config
    for bad in [
        "nas.example.org",
        "http://nas.example.org",
        "ssh://",
        "ssh://-oProxyCommand=touch%20x",
        "ssh://-oProxyCommand=sh -c id",
        "ssh://alice@-oProxyCommand=id",
        "ssh://-alice@nas.example.org",
        "ssh://.nas",
        "ssh://nas.example.org/volume1",
        "ssh://nas.example.org:0",
        "ssh://nas.example.org:99999",
        "ssh://nas.example.org:22x",
        "ssh://nas example.org",
        "ssh://nas;id",
        "ssh://nas`id`",
        "ssh://$(id)",
        "ssh://al'ice@nas",
        "ssh://nas\n.example.org",
        "ssh://[::1;id]",
        "ssh://[fd00::1",
    ] {
        assert!(parse_url(bad).is_err(), "{bad:?} must be refused");
    }
    assert!(check_name("nas").is_ok());
    assert!(check_name("office-pc.2").is_ok());
    for bad in [
        "",
        "-nas",
        "my nas",
        "nas/1",
        "x".repeat(33).as_str(),
        "näs",
    ] {
        assert!(check_name(bad).is_err(), "{bad:?}");
    }
    assert!(check_remote_path("/volume1/@backdown/index.db", "db").is_ok());
    assert!(check_remote_path("/home/alice/it's here/index.db", "db").is_ok());
    for bad in ["", "-x", "/a\nb", "/a\0b"] {
        assert!(check_remote_path(bad, "db").is_err(), "{bad:?}");
    }
    // the command there is only ever backdown itself
    let tmp = scratch();
    let db = Db::open(&tmp.path().join("i.db")).unwrap();
    assert!(add(&db, "a", "ssh://nas", None, Some("/opt/bin/backdown")).is_ok());
    assert!(add(&db, "b", "ssh://nas", None, Some("/bin/rm")).is_err());
    assert!(add(&db, "c", "ssh://nas", None, Some("/opt/backdown-dir/sh")).is_err());
}

#[test]
fn paths_there_are_read_their_own_way() {
    assert_eq!(
        parent(Os::Unix, b"/volume1/photos/a.jpg"),
        b"/volume1/photos"
    );
    assert_eq!(parent(Os::Unix, b"/volume1"), b"/");
    assert_eq!(parent(Os::Unix, b"/"), b"/");
    assert_eq!(name_of(Os::Unix, b"/volume1/photos"), "photos");
    let w = |s: &str| -> Vec<u8> { s.encode_utf16().flat_map(u16::to_le_bytes).collect() };
    assert_eq!(
        parent(Os::Windows, &w("D:\\Backup\\a.jpg")),
        w("D:\\Backup")
    );
    assert_eq!(parent(Os::Windows, &w("D:\\Backup")), w("D:\\"));
    assert_eq!(display(Os::Windows, &w("D:\\Fotos\\Été")), "D:\\Fotos\\Été");
    assert_eq!(name_of(Os::Windows, &w("D:\\Fotos\\Été")), "Été");
}

#[test]
fn requests_are_parsed_strictly() {
    let ok = |l: &str| serve::parse(l).1.is_ok();
    assert!(ok(r#"{"id":1,"op":"hello"}"#));
    assert!(ok(r#"{"v":1,"id":2,"op":"ls","dir":"L3Y"}"#));
    assert!(ok(r#"{"id":3,"op":"hash","ids":[1,2],"mode":"sample"}"#));
    assert!(ok(r#"{"id":4,"op":"file","file":12}"#));
    for bad in [
        "",
        "not json",
        "[1,2]",
        r#"{"op":"hello"}"#,                                     // no id
        r#"{"id":"1","op":"hello"}"#,                            // id not a number
        r#"{"id":-1,"op":"hello"}"#,                             // nor negative
        r#"{"v":2,"id":1,"op":"hello"}"#,                        // unknown format
        r#"{"id":1,"op":"delete","path":"/"}"#,                  // no such op
        r#"{"id":1,"op":"hello","extra":true}"#,                 // unknown field
        r#"{"id":1,"op":"ls"}"#,                                 // missing field
        r#"{"id":1,"op":"file","id2":3}"#,                       // unknown field
        r#"{"id":1,"op":"hash","ids":[1],"mode":"everything"}"#, // bad mode
        r#"{"id":1,"op":"changes","root":1,"since":"0","upto":1,"after":0}"#, // mistyped
    ] {
        assert!(!ok(bad), "{bad:?} must be refused");
    }
    let long = format!(
        r#"{{"id":1,"op":"find","q":"{}"}}"#,
        "a".repeat(serve::MAX_LINE)
    );
    assert!(!ok(&long));
    // refusals still answer, with the request's id when it had one
    let tmp = scratch();
    let db = Db::open(&tmp.path().join("r.db")).unwrap();
    let here = Here {
        db_path: "r.db".into(),
        host: "nas".into(),
    };
    let soon = Instant::now() + Duration::from_secs(5);
    let a: Value = serde_json::from_str(&serve::answer_line(
        &db,
        &here,
        r#"{"id":9,"op":"format-disk"}"#,
        soon,
    ))
    .unwrap();
    assert_eq!((a["id"].clone(), a["ok"].clone()), (json!(9), json!(false)));
    let a: Value = serde_json::from_str(&serve::answer_line(
        &db,
        &here,
        r#"{"id":10,"op":"hash","ids":[],"mode":"full"}"#,
        soon,
    ))
    .unwrap();
    assert_eq!(a["ok"], false);
    let a: Value = serde_json::from_str(&serve::answer_line(
        &db,
        &here,
        r#"{"id":11,"op":"changes","root":1,"since":0,"upto":1,"after":0,"limit":100000}"#,
        soon,
    ))
    .unwrap();
    assert_eq!(a["ok"], false, "pages are bounded");
    let a: Value = serde_json::from_str(&serve::answer_line(
        &db,
        &here,
        r#"{"id":12,"op":"hello"}"#,
        soon,
    ))
    .unwrap();
    assert_eq!(a["data"]["proto"], PROTO);
    assert_eq!(a["data"]["read_only"], true);
}

fn count(db: &Db, sql: &str) -> i64 {
    db.conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn a_big_drive_is_walked_in_id_order_and_gives_the_same_files() {
    let tmp = scratch();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    for i in 0..30u8 {
        write(&a.join(format!("{i:02}.bin")), &blob(i, 100 + i as usize));
    }
    for i in 0..25u8 {
        write(&b.join(format!("{i:02}.bin")), &blob(i, 200 + i as usize));
    }
    let mut db = Db::open(&tmp.path().join("there.db")).unwrap();
    scan(&mut db, &a);
    scan(&mut db, &b);
    // more on drive a, after b's: a's ids now have a gap where b's are
    for i in 30..41u8 {
        write(&a.join(format!("{i:02}.bin")), &blob(i, 100 + i as usize));
    }
    fs::remove_file(a.join("03.bin")).unwrap();
    scan(&mut db, &a);

    let roots = ask(&db, r#"{"id":1,"op":"roots"}"#);
    let root = |name: &str| -> (i64, i64) {
        let r = roots["data"]["roots"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == name)
            .unwrap();
        (r["id"].as_i64().unwrap(), r["last_scan"].as_i64().unwrap())
    };
    let pages = |root: (i64, i64), since: i64, walk: serve::Walk| -> (Vec<i64>, usize, usize) {
        let (mut ids, mut asked, mut empty, mut after) = (Vec::new(), 0, 0, 0);
        loop {
            let p = serve::changes(&db, [root.0, since, root.1, after], 4, walk).unwrap();
            let rows = p["rows"].as_array().unwrap();
            ids.extend(rows.iter().map(|r| r[0].as_i64().unwrap()));
            asked += 1;
            empty += usize::from(rows.is_empty());
            assert!(asked < 1000, "the pages never end");
            match p["next"].as_i64() {
                Some(n) => after = n,
                None => return (ids, asked, empty),
            }
        }
    };
    let indexed = serve::Walk {
        above: i64::MAX,
        window: 1,
    };
    let walked = serve::Walk {
        above: 0,
        window: 7,
    };
    for name in ["a", "b"] {
        for since in [0, 1] {
            let (want, ..) = pages(root(name), since, indexed);
            let (got, asked, _) = pages(root(name), since, walked);
            assert_eq!(got, want, "drive {name} since scan {since}");
            assert!(asked > 1);
        }
    }
    let (all_a, _, empty) = pages(root("a"), 0, walked);
    assert_eq!(all_a.len(), 40, "41 written, 1 deleted");
    assert!(all_a.windows(2).all(|w| w[0] < w[1]));
    assert!(
        empty > 0,
        "a window over b's ids is an empty page that goes on"
    );

    // the request itself, with the real thresholds: small drives still go through the index
    let (id, last) = root("a");
    let full = ask(
        &db,
        &format!(
            r#"{{"id":2,"op":"changes","root":{id},"since":0,"upto":{last},"after":0,"limit":100}}"#
        ),
    );
    let got: Vec<i64> = full["data"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r[0].as_i64().unwrap())
        .collect();
    assert_eq!(got, all_a);
    assert!(full["data"]["next"].is_null());
}

/// What to ask another machine to read walks the partial index of what's waiting, largest first,
/// and stops at its limit: never a sort of every cached file.
#[test]
fn what_is_asked_for_walks_only_the_waiting_rows() {
    let tmp = scratch();
    let db = Db::open(&tmp.path().join("i.db")).unwrap();
    db.build_later_indexes(&mut |_| {}).unwrap();
    for (w, index) in [
        (Want::Sample, "remote_files_unhashed"),
        (Want::Full, "remote_files_unhashed"),
        (Want::FullBySize, "remote_files_unhashed"),
    ] {
        let sql = format!(
            "EXPLAIN QUERY PLAN SELECT r.rid, r.size FROM {} WHERE {}
               AND (r.size < ?3 OR (r.size = ?3 AND r.rid < ?4))
             ORDER BY r.size DESC, r.rid DESC LIMIT ?2",
            want_from(&db).unwrap(),
            want_sql(w)
        );
        let plan: Vec<String> = db
            .conn
            .prepare(&sql)
            .unwrap()
            .query_map(params![1, 64, i64::MAX, i64::MAX], |r| {
                r.get::<_, String>(3)
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let all = plan.join(" / ");
        assert!(all.contains(index), "{all}");
        assert!(!all.contains("TEMP B-TREE"), "{all}");
    }
}

/// A pass asks about each waiting file once, however many of the largest match nothing here.
#[test]
fn a_pass_walks_past_what_matches_nothing_here() {
    let tmp = scratch();
    let db = Db::open(&tmp.path().join("i.db")).unwrap();
    let m = add(&db, "nas", "ssh://nas.example.org", None, None).unwrap();
    db.conn
        .execute("INSERT INTO roots(id, path) VALUES (1, x'2f68')", [])
        .unwrap();
    // 300 big files there that nothing here matches, then 100 smaller ones that do
    for i in 0..400i64 {
        let size = if i < 300 { 10_000_000 + i } else { 1_000 + i };
        db.conn
            .execute(
                "INSERT INTO remote_files(remote_id, rid, root_id, path, size, mtime) VALUES (?1, ?2, 1, ?3, ?4, 0)",
                params![m.id, i + 1, format!("/v/{i}").into_bytes(), size],
            )
            .unwrap();
        if i >= 300 {
            db.conn
                .execute(
                    "INSERT INTO files(root_id, path, size, mtime, current, first_scan, last_scan) VALUES (1, ?1, ?2, 0, 1, 1, 1)",
                    params![format!("/h/{i}").into_bytes(), size],
                )
                .unwrap();
        }
    }
    let m = get(&db, m.id).unwrap();
    // the same before the daemon has built the index and after
    for indexed in [false, true] {
        if indexed {
            db.build_later_indexes(&mut |_| {}).unwrap();
        }
        let mut from = Cursor::START;
        let mut seen = Vec::new();
        loop {
            let b = wanted(&db, &m, Want::FullBySize, from, 64).unwrap();
            let Some(&(rid, size)) = b.last() else { break };
            seen.extend(b.iter().map(|x| x.0));
            from = Cursor(size, rid);
        }
        seen.sort();
        assert_eq!(
            seen,
            (301..=400).collect::<Vec<i64>>(),
            "indexed: {indexed}"
        );
        assert_eq!(left_to_hash(&db, &m).unwrap(), 100);
    }
}

#[test]
fn sync_is_incremental_and_copies_there_count_conservatively() {
    let tmp = scratch();
    let laptop = tmp.path().join("local/Laptop");
    let vol = tmp.path().join("nas/volume1");
    // the same photo both sides (the NAS has it twice, so its own hashing proved it)
    write(&laptop.join("photos/a.jpg"), &blob(1, 300_000));
    write(&vol.join("backup/photos/a.jpg"), &blob(1, 300_000));
    write(&vol.join("backup/photos/a copy.jpg"), &blob(1, 300_000));
    // a small file, unique in size on each side: only the NAS hashing it for us proves it
    write(&laptop.join("docs/b.txt"), &blob(2, 1000));
    write(&vol.join("backup/docs/b.txt"), &blob(2, 1000));
    // a large file unique on each side: sampled there, sampled here, then hashed both sides
    write(&laptop.join("big/big.bin"), &blob(3, 320_000));
    write(&vol.join("backup/big.bin"), &blob(3, 320_000));
    // same size, different content: never a copy
    write(&laptop.join("decoy/decoy.bin"), &blob(4, 310_000));
    write(&vol.join("other/c.bin"), &blob(5, 310_000));
    // only here
    write(&laptop.join("only/thesis.odt"), &blob(6, 50_000));

    let nas_db = tmp.path().join("nas.db");
    let mut there = Db::open(&nas_db).unwrap();
    scan(&mut there, &vol);
    let mut db = Db::open(&tmp.path().join("local.db")).unwrap();
    scan(&mut db, &laptop);

    let m = add(
        &db,
        "nas",
        "ssh://alice@nas.example.org:2222",
        Some("/volume1/@backdown/index.db"),
        None,
    )
    .unwrap();
    assert!(
        add(&db, "nas", "ssh://nas.example.org", None, None).is_err(),
        "names are unique"
    );
    let mut peer = Local {
        db: nas_db.clone(),
        calls: Vec::new(),
    };
    let st = sync(&db, &m, &mut peer, &SyncOptions::default(), &mut |_| {}).unwrap();
    assert_eq!(st.drives, 1);
    assert_eq!(st.added, 5, "every current file there");
    assert!(st.changed);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM remote_files"), 5);
    // b.txt (small) is hashed there on request; big.bin and c.bin only sampled so far
    let sha_of = |db: &Db, name: &str| -> Option<Vec<u8>> {
        db.conn
            .query_row(
                &format!("SELECT sha256 FROM remote_files WHERE {ENDS}"),
                [ends(name)],
                |r| r.get(0),
            )
            .optional()
            .unwrap()
            .flatten()
    };
    assert!(sha_of(&db, "b.txt").is_some());
    assert!(sha_of(&db, "big.bin").is_none());
    let m = get(&db, m.id).unwrap();
    // a file there, by its id there (the request's own id is separate)
    let rid: i64 = db
        .conn
        .query_row(
            &format!("SELECT rid FROM remote_files WHERE {ENDS}"),
            [ends("b.txt")],
            |r| r.get(0),
        )
        .unwrap();
    let f = peer.call("file", json!({ "file": rid }), ASK).unwrap();
    assert_eq!(f["name"], "b.txt");
    assert_eq!(m.info["host"], "nas");
    assert!(m.last_sync.is_some());

    // hashing here now takes the NAS's sizes and samples into account
    hash_here(&mut db);
    let local_sha = |name: &str| -> Option<Vec<u8>> {
        db.conn
            .query_row(
                &format!("SELECT sha256 FROM files WHERE {ENDS}"),
                [ends(name)],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert!(local_sha("big.bin").is_some(), "same sample there: hashed");
    assert!(local_sha("a.jpg").is_some());
    assert!(
        local_sha("decoy.bin").is_none(),
        "a different sample there: left"
    );
    // the next sync has the NAS hash what now matches by sample
    let st = sync(&db, &m, &mut peer, &SyncOptions::default(), &mut |_| {}).unwrap();
    assert_eq!(st.added, 0, "nothing changed there");
    assert_eq!(st.hashed, 1);
    assert_eq!(st.left, 0);
    assert!(sha_of(&db, "big.bin").is_some());
    assert!(
        sha_of(&db, "c.bin").is_none(),
        "never hashed: its sample differs"
    );
    // the machines page reads the counts the sync kept, not millions of rows on every look
    let o = overview(&db).unwrap();
    let r0 = &o["machines"][0]["roots"][0];
    assert_eq!(r0["cached"], 5, "{o}");
    assert_eq!(
        r0["hashed"],
        count(&db, "SELECT COUNT(sha256) FROM remote_files"),
        "{o}"
    );
    assert_eq!(st.files, 5);

    // is the laptop safe to wipe? what's on the NAS counts, what only matches by size doesn't
    let c = report::check_indexed(&db, &laptop).unwrap();
    assert!(!c.safe);
    assert_eq!(c.remote.files, 3, "a.jpg, b.txt, big.bin: {c:?}");
    assert_eq!(c.unique.files, 2, "decoy and thesis");
    assert_eq!(
        c.remote_machines,
        vec!["nas (ssh://alice@nas.example.org:2222)"]
    );
    assert!(
        report::check_indexed(&db, &laptop.join("photos"))
            .unwrap()
            .safe
    );
    assert!(
        !report::check_indexed(&db, &laptop.join("decoy"))
            .unwrap()
            .safe
    );

    // confirmed too long ago: the NAS's scan is 8 days old, and a.jpg was never re-read for us
    let old = now() - 8 * 86_400;
    db.conn
        .execute("UPDATE remote_roots SET seen_at = ?1", [old])
        .unwrap();
    assert!(
        !report::check_indexed(&db, &laptop.join("photos"))
            .unwrap()
            .safe
    );
    // big.bin was re-read there for us just now, so it still counts
    assert!(
        report::check_indexed(&db, &laptop.join("big"))
            .unwrap()
            .safe
    );
    let a_sha = local_sha("a.jpg").unwrap();
    let copies = copies_of(&db, &a_sha, 300_000).unwrap();
    assert_eq!(copies.len(), 2);
    assert!(copies.iter().all(|c| !c.counts && c.why_not.is_some()));
    assert!(
        copies[0]
            .path
            .ends_with(&spelt("/backup/photos/a copy.jpg"))
            || copies[0].path.ends_with(&spelt("/backup/photos/a.jpg"))
    );
    // a longer window makes it count again
    set_setting(&db, "proof_days", 30).unwrap();
    assert!(
        report::check_indexed(&db, &laptop.join("photos"))
            .unwrap()
            .safe
    );
    assert!(set_setting(&db, "proof_days", 0).is_err());
    set_setting(&db, "proof_days", 7).unwrap();
    db.conn
        .execute("UPDATE remote_roots SET seen_at = ?1", [now()])
        .unwrap();

    // Free up's assessment says the same, and names the machine
    let a = wipe::assess(&db, std::slice::from_ref(&laptop), None).unwrap();
    assert_eq!(a.items[0].remote_files, 3);
    assert_eq!(a.items[0].only_files, 2);
    assert!(
        a.items[0]
            .copies
            .iter()
            .any(|p| p.path == "nas (ssh://alice@nas.example.org:2222)")
    );
    assert_eq!(a.proof_days, 7);

    // overlaps count the NAS as a place copies are, without listing it as a unit
    let ov = overlap::overlaps(&db, &[], &mut |_| {}).unwrap();
    let drive = ov.iter().find(|o| o.unit.level == "drive").unwrap();
    assert_eq!(drive.elsewhere_files, 3);
    assert!(
        drive
            .containers
            .iter()
            .any(|c| c.label == "the machine nas")
    );
    assert!(ov.iter().all(|o| o.unit.level != "machine"));

    // duplicates across machines
    let all = report::DupesFilter::default();
    let x = across(&db, 1, &all, 10).unwrap();
    assert_eq!(x.files, 3);
    assert_eq!(x.proven_files, 3);
    assert_eq!(x.largest[0].size, 320_000);
    let mut d = serde_json::to_value(report::dupes(&db, 1, 10).unwrap()).unwrap();
    annotate_dupes(&db, &mut d, 1, &all, 10).unwrap();
    assert_eq!(d["across"]["files"], 3);
    // the Duplicates view's filter bar narrows them too
    let smaller = report::DupesFilter {
        size_below: Some(320_000),
        ..Default::default()
    };
    let x = across(&db, 1, &smaller, 10).unwrap();
    assert_eq!(x.files, 2);
    assert!(x.largest.iter().all(|g| g.size < 320_000));

    // browsing the cache while the NAS is away
    let root = fs::canonicalize(&vol).unwrap();
    let l = cached_list(&db, &m, &crate::paths::to_bytes(&root.join("backup"))).unwrap();
    let names: Vec<&str> = l["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["big.bin", "docs", "photos"]);
    assert_eq!(l["crumbs"].as_array().unwrap().len(), 2);

    // the NAS changes: b.txt goes, d.bin arrives. Only that travels
    fs::remove_file(vol.join("backup/docs/b.txt")).unwrap();
    write(&vol.join("new/d.bin"), &blob(7, 2000));
    drop(there);
    let mut there = Db::open(&nas_db).unwrap();
    scan(&mut there, &vol);
    peer.calls.clear();
    let st = sync(&db, &m, &mut peer, &SyncOptions::default(), &mut |_| {}).unwrap();
    assert_eq!(st.removed, 1);
    assert_eq!(st.added, 1);
    assert!(peer.calls.contains(&"gone".to_string()));
    assert_eq!(count(&db, "SELECT COUNT(*) FROM remote_files"), 5);
    assert!(sha_of(&db, "b.txt").is_none());
    let c = report::check_indexed(&db, &laptop.join("docs")).unwrap();
    assert!(!c.safe, "its only copy is gone from the NAS");

    // forgetting the machine forgets its files
    let m2 = add(&db, "old-pc", "ssh://old-pc", None, None).unwrap();
    remove(&db, m2.id).unwrap();
    assert_eq!(list(&db).unwrap().len(), 1);
}

#[test]
fn a_wipe_trusts_a_copy_there_only_once_it_is_re_read() {
    let tmp = scratch();
    let laptop = tmp.path().join("local/Laptop");
    let vol = tmp.path().join("nas/volume1");
    write(&laptop.join("big/big.bin"), &blob(3, 320_000));
    write(&vol.join("backup/big.bin"), &blob(3, 320_000));
    write(&laptop.join("pics/p.jpg"), &blob(8, 200_000));
    write(&vol.join("backup/p.jpg"), &blob(8, 200_000));
    let nas_db = tmp.path().join("nas.db");
    let mut there = Db::open(&nas_db).unwrap();
    scan(&mut there, &vol);
    let mut db = Db::open(&tmp.path().join("local.db")).unwrap();
    scan(&mut db, &laptop);
    let m = add(&db, "nas", "ssh://nas.example.org", None, None).unwrap();
    let mut peer = Local {
        db: nas_db.clone(),
        calls: Vec::new(),
    };
    sync(&db, &m, &mut peer, &SyncOptions::default(), &mut |_| {}).unwrap();
    hash_here(&mut db);
    sync(&db, &m, &mut peer, &SyncOptions::default(), &mut |_| {}).unwrap();
    assert!(
        report::check_indexed(&db, &laptop.join("big"))
            .unwrap()
            .safe
    );
    assert!(
        report::check_indexed(&db, &laptop.join("pics"))
            .unwrap()
            .safe
    );

    // the NAS can't be reached: nothing is deleted on the strength of its copies
    let id = wipe::record(&db, &[laptop.join("big")], None).unwrap();
    let mut away = Reach(None);
    let mut v = Verifier::new(&mut away);
    let st = wipe::wipe_with(&mut db, id, false, None, Some(&mut v), &mut |_, _| {}).unwrap();
    assert_eq!((st.deleted, st.left_no_copy), (0, 1));
    assert!(
        st.problems
            .iter()
            .any(|p| p.contains("couldn't be reached")),
        "{:?}",
        st.problems
    );
    assert!(laptop.join("big/big.bin").exists());

    // the copy there changed after it was synced: re-reading it shows that, and nothing goes
    write(&vol.join("backup/p.jpg"), &blob(9, 200_000));
    let id = wipe::record(&db, &[laptop.join("pics")], None).unwrap();
    let mut reach = Reach(Some(nas_db.clone()));
    let mut v = Verifier::new(&mut reach);
    let st = wipe::wipe_with(&mut db, id, false, None, Some(&mut v), &mut |_, _| {}).unwrap();
    assert_eq!((st.deleted, st.left_no_copy), (0, 1));
    assert!(laptop.join("pics/p.jpg").exists());

    // reachable, and its copy re-read just now matches: deleted, with the NAS copy as proof
    let id = wipe::record(&db, &[laptop.join("big")], None).unwrap();
    let mut v = Verifier::new(&mut reach);
    let st = wipe::wipe_with(&mut db, id, false, None, Some(&mut v), &mut |_, _| {}).unwrap();
    assert_eq!((st.deleted, st.remote), (1, 1), "{:?}", st.problems);
    assert!(!laptop.join("big/big.bin").exists());
    let (proof, how): (Vec<u8>, String) = db
        .conn
        .query_row(
            "SELECT proof, proof_how FROM deletions WHERE wipe_id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(how, "remote re-read");
    let proof = String::from_utf8(proof).unwrap();
    assert!(
        proof.starts_with("ssh://nas.example.org/") && proof.ends_with(&spelt("/backup/big.bin")),
        "{proof}"
    );
    // and without a verifier (an older caller), a remote copy is never enough
    write(&laptop.join("big/big.bin"), &blob(3, 320_000));
    scan(&mut db, &laptop);
    let id = wipe::record(&db, &[laptop.join("big")], None).unwrap();
    let st = wipe::wipe(&mut db, id, false, None, &mut |_, _| {}).unwrap();
    assert_eq!(st.deleted, 0);
}

// ---------- drive health there

fn reading(serial: &str, model: &str, reallocated: u64, at: i64) -> Health {
    Health {
        id: health::identity(Some(serial), None),
        device: "/dev/sdb".into(),
        name: "sdb".into(),
        model: Some(model.into()),
        serial: Some(serial.into()),
        rotational: Some(true),
        reported: Some(Verdict::Ok),
        reallocated: Some(reallocated),
        pending: Some(0),
        source: "test readings".into(),
        mounts: vec!["/volume1".into()],
        read_at: at,
        ..Health::default()
    }
}

fn note(kind: &'static str, key: &str, title: &str) -> Note {
    Note {
        kind,
        key: key.into(),
        level: "warning",
        title: title.into(),
        body: "2 more sectors have gone bad".into(),
        drive: None,
        quiet_for: 0,
    }
}

fn ask(db: &Db, line: &str) -> Value {
    let here = Here {
        db_path: "nas.db".into(),
        host: "nas".into(),
    };
    serde_json::from_str(&serve::answer_line(
        db,
        &here,
        line,
        Instant::now() + Duration::from_secs(30),
    ))
    .unwrap()
}

#[test]
fn the_health_request_answers_within_its_bounds() {
    let tmp = scratch();
    let nas_db = tmp.path().join("nas.db");
    let there = Db::open(&nas_db).unwrap();
    let t = now() - 10 * 86_400;
    // more drives than one answer carries, each with a history; one of them going bad
    for i in 0..serve::MAX_HEALTH_DRIVES + 20 {
        let sn = format!("FAKE{i:04}");
        for k in 0..8u64 {
            let realloc = if i == 7 { k * 20 } else { 0 };
            health::record(
                &there,
                &reading(&sn, "FAKEDISK HD4000", realloc, t + k as i64 * 86_400),
            )
            .unwrap();
        }
    }
    // its notifications: about its own drives, one it gathered from another machine, a job's
    let l = Limits::default();
    for i in 0..serve::MAX_HEALTH_NOTES as i64 + 5 {
        let n = note("health", &format!("health:sn:FAKE{i:04}:worse"), "its own");
        alerts::add(&there, &n, false, now() - 1000 + i, &l).unwrap();
    }
    let gathered = note(
        "health",
        "health@office-pc:sn:X1:verdict:warning",
        "gathered",
    );
    alerts::add(&there, &gathered, false, now(), &l).unwrap();
    alerts::add(
        &there,
        &note("job", "job:index:/x", "a job"),
        false,
        now(),
        &l,
    )
    .unwrap();
    drop(there);

    let db = Db::open_read_only(&nas_db).unwrap();
    let a = ask(&db, r#"{"id":1,"op":"health"}"#);
    assert_eq!(a["ok"], true, "{a}");
    let d = &a["data"];
    assert_eq!(d["available"], true);
    let drives = d["drives"].as_array().unwrap();
    assert_eq!(drives.len(), serve::MAX_HEALTH_DRIVES);
    assert_eq!(d["truncated"], true);
    assert_eq!(drives[0]["verdict"], "failing", "the worst first");
    assert_eq!(drives[0]["health"]["serial"], "FAKE0007");
    assert_eq!(drives[0]["health"]["reallocated"], 140);
    assert_eq!(drives[0]["readings"], 8);
    assert_eq!(
        drives[0]["changes"].as_array().unwrap().len(),
        serve::MAX_HEALTH_CHANGES,
        "its latest changes only"
    );
    assert!(drives.iter().all(|x| {
        x["changes"].as_array().unwrap().len() <= serve::MAX_HEALTH_CHANGES
            && x.get("series").is_none()
            && x.get("present").is_none()
            && x["health"]["read_at"].is_i64()
    }));
    let notes = d["notes"].as_array().unwrap();
    assert_eq!(notes.len(), serve::MAX_HEALTH_NOTES);
    assert!(
        notes.iter().all(|n| n["title"] == "its own"),
        "only about its own drives: {notes:?}"
    );
    assert!(
        notes[0]["at"].as_i64() > notes[1]["at"].as_i64(),
        "newest first"
    );
    assert!(a.to_string().len() < 1 << 20, "a small answer");
    // strictly parsed, like the others
    assert!(
        serve::parse(r#"{"id":2,"op":"health","limit":5}"#)
            .1
            .is_err()
    );
    assert!(serve::parse(r#"{"v":1,"id":2,"op":"health"}"#).1.is_ok());

    // an index from before drive health: nothing to say, which isn't an error
    let old = tmp.path().join("old.db");
    rusqlite::Connection::open(&old)
        .unwrap()
        .execute_batch("CREATE TABLE scans (id INTEGER PRIMARY KEY, started_at INTEGER)")
        .unwrap();
    let a = ask(
        &Db::open_read_only(&old).unwrap(),
        r#"{"id":3,"op":"health"}"#,
    );
    assert_eq!(a["ok"], true, "{a}");
    assert_eq!(a["data"]["available"], false);
    assert_eq!(a["data"]["drives"], json!([]));

    // a request this end doesn't know says so, so a newer asker can do without it
    let a = ask(&db, r#"{"id":4,"op":"frobnicate"}"#);
    let e = anyhow::anyhow!("nas: {}", a["error"].as_str().unwrap());
    assert!(unknown_request(&e, "frobnicate"), "{e}");
    assert!(!unknown_request(&e, "health"));
    // a mistake in a known one is still a bad request
    let a = ask(&db, r#"{"id":5,"op":"hash","ids":[1],"mode":"everything"}"#);
    let e = anyhow::anyhow!("{}", a["error"].as_str().unwrap());
    assert!(!unknown_request(&e, "hash") && e.to_string().contains("bad request"));
}

#[test]
fn a_sync_keeps_each_machines_drive_health_and_tells_when_a_drive_gets_worse() {
    let tmp = scratch();
    let nas_db = tmp.path().join("nas.db");
    let there = Db::open(&nas_db).unwrap();
    let t = now() - 5 * 86_400;
    health::record(&there, &reading("FAKE0001", "FAKEDISK HD4000", 0, t)).unwrap();
    health::record(&there, &reading("FAKE0002", "FAKEDISK HD2000", 0, t)).unwrap();
    // the NAS's own notifications come along; one it gathered from elsewhere doesn't
    let l = Limits::default();
    let own = note(
        "health",
        "health:sn:FAKE0002:cable",
        "Transfer errors on FAKEDISK HD2000",
    );
    alerts::add(&there, &own, false, now() - 60, &l).unwrap();
    let gathered = note(
        "health",
        "health@office-pc:sn:X1:verdict:warning",
        "gathered",
    );
    alerts::add(&there, &gathered, false, now() - 30, &l).unwrap();

    let db = Db::open(&tmp.path().join("local.db")).unwrap();
    let m = add(&db, "nas", "ssh://alice@nas.example.org", None, None).unwrap();
    let mut peer = Local {
        db: nas_db.clone(),
        calls: Vec::new(),
    };
    let told = |db: &Db| count(db, "SELECT COUNT(*) FROM notifications");
    // a sync, and the notes it would show on the desktop (when it may: `desktop`)
    let go = |peer: &mut Local, desktop: bool| {
        let show = Mutex::new(Vec::new());
        let opts = SyncOptions {
            show: desktop.then_some(&show),
            ..SyncOptions::default()
        };
        let st = sync(&db, &m, peer, &opts, &mut |_| {}).unwrap();
        (st, show.into_inner().unwrap())
    };

    // the first sight of its drives: kept, and nothing told
    let (st, shown) = go(&mut peer, true);
    assert_eq!((st.health, st.health_drives, st.health_notes), ("ok", 2, 0));
    assert!(shown.is_empty());
    assert_eq!(count(&db, "SELECT COUNT(*) FROM remote_health"), 2);
    assert_eq!(told(&db), 0);
    let o = health_overview(&db).unwrap();
    let nas = &o["machines"][0];
    assert_eq!(nas["name"], "nas");
    assert!(nas["last_sync"].is_i64());
    assert_eq!(nas["health"]["state"], "ok");
    assert!(nas["health"]["note"].is_null(), "{nas}");
    assert_eq!(nas["drives"].as_array().unwrap().len(), 2);
    assert_eq!(nas["drives"][0]["verdict"], "ok");
    assert_eq!(nas["drives"][0]["current"], true);
    assert_eq!(nas["drives"][0]["machine"], "nas");
    let notes = nas["notes"].as_array().unwrap();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert_eq!(notes[0]["title"], "Transfer errors on FAKEDISK HD2000");
    assert_eq!(o["worst"], "ok");

    // a drive there gets worse: told once, in Activity only unless the desktop is allowed
    health::record(
        &there,
        &reading("FAKE0001", "FAKEDISK HD4000", 5, t + 86_400),
    )
    .unwrap();
    let (st, shown) = go(&mut peer, false);
    assert_eq!(st.health_notes, 1);
    assert!(shown.is_empty());
    let n = alerts::list(&db, 10).unwrap().remove(0);
    assert_eq!((n.kind.as_str(), n.level.as_str()), ("health", "warning"));
    assert_eq!(n.title, "A drive on nas needs watching");
    assert_eq!(n.key, "health@nas:sn:FAKE0001:verdict:warning");
    assert_eq!(n.drive.as_deref(), Some("@nas:sn:FAKE0001"));
    assert!(
        n.body.starts_with(
            "FAKEDISK HD4000 (S/N FAKE0001): 5 more sectors have gone bad and been replaced by spares (now 5)."
        ),
        "{}",
        n.body
    );
    assert!(n.body.contains("It was ok at the last sync."), "{}", n.body);
    // the same again: nothing new to tell
    let (st, shown) = go(&mut peer, true);
    assert_eq!((st.health_notes, shown.len()), (0, 0));
    assert_eq!(told(&db), 1);

    // worse still: failing, and a failing drive reaches the desktop when it may
    health::record(
        &there,
        &reading("FAKE0001", "FAKEDISK HD4000", 150, t + 2 * 86_400),
    )
    .unwrap();
    let (st, shown) = go(&mut peer, true);
    assert_eq!(st.health_notes, 1);
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].title, "A drive on nas is failing");
    assert_eq!(shown[0].level, "failing");
    assert!(
        shown[0].body.contains("145 more sectors"),
        "{}",
        shown[0].body
    );
    assert!(shown[0].body.contains("Copy what matters off it now"));
    let o = health_overview(&db).unwrap();
    assert_eq!(o["worst"], "failing");
    assert_eq!(o["machines"][0]["worst"], "failing");
    assert_eq!(
        o["machines"][0]["drives"][0]["health"]["serial"], "FAKE0001",
        "the worst first"
    );
    assert_eq!(told(&db), 2);

    // better for a while, then failing again within the week: not told twice
    health::record(
        &there,
        &reading("FAKE0001", "FAKEDISK HD4000", 0, t + 3 * 86_400),
    )
    .unwrap();
    assert_eq!(go(&mut peer, true).0.health_notes, 0);
    health::record(
        &there,
        &reading("FAKE0001", "FAKEDISK HD4000", 150, t + 4 * 86_400),
    )
    .unwrap();
    let (st, shown) = go(&mut peer, true);
    assert_eq!((st.health_notes, shown.len()), (0, 0));
    assert_eq!(told(&db), 2);

    // a drive new there since the last sync that's already going bad is news
    health::record(
        &there,
        &reading("FAKE0003", "FAKEDISK HD8000", 3, now() - 60),
    )
    .unwrap();
    let (st, _) = go(&mut peer, false);
    assert_eq!((st.health_drives, st.health_notes), (3, 1));
    let n = alerts::list(&db, 1).unwrap().remove(0);
    assert_eq!(n.key, "health@nas:sn:FAKE0003:verdict:warning");
    assert!(
        n.body.contains("It wasn't there at the last sync."),
        "{}",
        n.body
    );

    // without the feature, its drive health isn't asked for at all
    peer.calls.clear();
    let opts = SyncOptions {
        health: false,
        ..SyncOptions::default()
    };
    let st = sync(&db, &m, &mut peer, &opts, &mut |_| {}).unwrap();
    assert_eq!(st.health, "");
    assert!(!peer.calls.iter().any(|c| c == "health"));

    // forgetting the machine forgets its drives; what was told stays in Activity
    remove(&db, m.id).unwrap();
    assert_eq!(count(&db, "SELECT COUNT(*) FROM remote_health"), 0);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM remote_health_state"), 0);
    assert_eq!(told(&db), 3);
    assert_eq!(health_overview(&db).unwrap()["machines"], json!([]));
}

/// A machine running a BackDown from before the `health` request: it refuses it the way those do.
struct Older(Local);

impl Peer for Older {
    fn call(&mut self, op: &str, args: Value, t: Duration) -> Result<Value> {
        if op == "health" {
            bail!(
                "nas: bad request: unknown variant `health`, expected one of `hello`, `status`, `roots`, `ls`, `file`, `find`, `search`, `dupes`, `entities`, `changes`, `gone`, `hash`, `stat`"
            );
        }
        self.0.call(op, args, t)
    }
}

/// A machine whose index won't say how its drives are just now.
struct Broken(Local);

impl Peer for Broken {
    fn call(&mut self, op: &str, args: Value, t: Duration) -> Result<Value> {
        if op == "health" {
            bail!("nas: database is locked");
        }
        self.0.call(op, args, t)
    }
}

#[test]
fn an_older_backdown_there_syncs_fine_without_its_drive_health() {
    let tmp = scratch();
    let vol = tmp.path().join("nas/volume1");
    write(&vol.join("backup/a.bin"), &blob(1, 5000));
    let nas_db = tmp.path().join("nas.db");
    let mut there = Db::open(&nas_db).unwrap();
    scan(&mut there, &vol);
    health::record(
        &there,
        &reading("FAKE0001", "FAKEDISK HD4000", 7, now() - 60),
    )
    .unwrap();
    let db = Db::open(&tmp.path().join("local.db")).unwrap();
    let m = add(&db, "nas", "ssh://nas.example.org", None, None).unwrap();
    let local = || Local {
        db: nas_db.clone(),
        calls: Vec::new(),
    };
    let opts = SyncOptions::default();

    // its files sync as ever; its drives can't be shown, and it says what would show them
    let mut older = Older(local());
    let st = sync(&db, &m, &mut older, &opts, &mut |_| {}).unwrap();
    assert_eq!(st.health, "unsupported");
    assert_eq!((st.drives, st.added), (1, 1));
    assert!(older.0.calls.contains(&"changes".to_string()));
    let o = health_overview(&db).unwrap();
    let nas = &o["machines"][0];
    assert_eq!(nas["health"]["state"], "unsupported");
    assert_eq!(
        nas["health"]["note"],
        "update BackDown on nas to see its drive health"
    );
    assert_eq!(nas["drives"], json!([]));
    assert!(get(&db, m.id).unwrap().last_error.is_none());

    // updated there: its drives are a first sight, so nothing is told about them
    let st = sync(&db, &m, &mut local(), &opts, &mut |_| {}).unwrap();
    assert_eq!((st.health, st.health_drives, st.health_notes), ("ok", 1, 0));
    assert_eq!(
        health_overview(&db).unwrap()["machines"][0]["drives"][0]["verdict"],
        "warning"
    );

    // an error there is noted, keeps what was known, and doesn't fail the sync either
    let st = sync(&db, &m, &mut Broken(local()), &opts, &mut |_| {}).unwrap();
    assert_eq!(st.health, "error");
    let o = health_overview(&db).unwrap();
    let nas = &o["machines"][0];
    assert_eq!(nas["health"]["state"], "error");
    assert!(
        nas["health"]["note"]
            .as_str()
            .unwrap()
            .contains("database is locked"),
        "{nas}"
    );
    assert_eq!(nas["drives"].as_array().unwrap().len(), 1);
    // an older one again: what it said before isn't current any more
    sync(&db, &m, &mut Older(local()), &opts, &mut |_| {}).unwrap();
    assert_eq!(count(&db, "SELECT COUNT(*) FROM remote_health"), 0);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM notifications"), 0);
}

// ---------- the swarm: each machine's storage, and where things are (swarm.rs)

/// A BackDown there from before the `volumes` request: it refuses it the way those do.
struct NoVolumes(Local);

impl Peer for NoVolumes {
    fn call(&mut self, op: &str, args: Value, t: Duration) -> Result<Value> {
        if op == "volumes" {
            bail!(
                "nas: bad request: unknown variant `volumes`, expected one of `hello`, `status`, `roots`"
            );
        }
        self.0.call(op, args, t)
    }
}

#[test]
fn a_sync_keeps_each_machines_storage_and_an_older_one_just_doesnt_say() {
    use crate::swarm;
    let tmp = scratch();
    let vol = tmp.path().join("nas/volume1");
    write(&vol.join("backup/report.pdf"), &blob(1, 5000));
    let nas_db = tmp.path().join("nas.db");
    let mut there = Db::open(&nas_db).unwrap();
    scan(&mut there, &vol);
    let db = Db::open(&tmp.path().join("local.db")).unwrap();
    let m = add(&db, "nas", "ssh://nas.example.org", None, None).unwrap();
    let local = || Local {
        db: nas_db.clone(),
        calls: Vec::new(),
    };
    let st = sync(&db, &m, &mut local(), &SyncOptions::default(), &mut |_| {}).unwrap();
    assert_eq!(st.volumes, "ok");
    let (state, report, ok_at, asked, err) = kept_volumes(&db, m.id).unwrap().unwrap();
    assert_eq!(state, "ok");
    assert!(ok_at.is_some() && asked == ok_at && err.is_none());
    let report = report.unwrap();
    assert!(report["disks"].is_array(), "the probe of that machine");
    // its indexed drive, connected, with what its own index has of it
    let roots = report["roots"].as_array().unwrap();
    assert_eq!(roots.len(), 1, "{report}");
    assert_eq!(roots[0]["present"], true);
    assert_eq!(roots[0]["files"], 1);
    assert!(roots[0]["path"].as_str().unwrap().ends_with("volume1"));

    // the swarm: this machine first, then the nas with its drive among its indexed ones
    let o = swarm::overview(&db, "laptop", swarm::storage(&db)).unwrap();
    let ms = o["machines"].as_array().unwrap();
    assert_eq!(ms.len(), 2);
    assert_eq!(
        (ms[0]["name"].clone(), ms[0]["here"].clone()),
        (json!("laptop"), json!(true))
    );
    assert_eq!(
        (ms[1]["name"].clone(), ms[1]["state"].clone()),
        (json!("nas"), json!("ok"))
    );
    assert_eq!(ms[1]["totals"]["files"], 1);
    assert_eq!(o["totals"]["machines"], 2);
    assert_eq!(o["totals"]["files"], 1, "nothing indexed here yet");
    assert!(ms[1].to_string().contains("volume1"), "{}", ms[1]);
    assert_eq!(swarm::one(&db, &m).unwrap()["state"], "ok");

    // an error there keeps what it said last, with the error
    store_volumes(&db, m.id, &Err(anyhow::anyhow!("nas: database is locked"))).unwrap();
    let (state, report, ok_at2, _, err) = kept_volumes(&db, m.id).unwrap().unwrap();
    assert_eq!(state, "error");
    assert!(report.is_some() && ok_at2 == ok_at);
    assert!(err.unwrap().contains("locked"));
    // an answer that isn't a storage report is refused, not kept
    store_volumes(&db, m.id, &Ok(json!({ "disks": "many" }))).unwrap();
    assert_eq!(kept_volumes(&db, m.id).unwrap().unwrap().0, "error");

    // a BackDown there from before the request doesn't say; the sync goes on regardless
    let mut older = NoVolumes(local());
    let st = sync(&db, &m, &mut older, &SyncOptions::default(), &mut |_| {}).unwrap();
    assert_eq!(st.volumes, "unsupported");
    assert!(older.0.calls.contains(&"roots".to_string()));
    let (state, report, ok_at, _, _) = kept_volumes(&db, m.id).unwrap().unwrap();
    assert_eq!(state, "unsupported");
    assert!(
        report.is_none() && ok_at.is_none(),
        "what it said before isn't current"
    );
    // its drives are still listed, from the sync, with nothing to put them on
    let o = swarm::overview(&db, "laptop", swarm::storage(&db)).unwrap();
    let nas = &o["machines"][1];
    assert_eq!(nas["state"], "unsupported");
    assert_eq!(nas["disks"], json!([]));
    assert_eq!(nas["elsewhere"][0]["files"], 1);
    assert_eq!(nas["elsewhere"][0]["name"], "volume1");

    // forgetting the machine forgets its storage too
    remove(&db, m.id).unwrap();
    assert_eq!(count(&db, "SELECT COUNT(*) FROM remote_volumes"), 0);
}

#[test]
fn where_is_it_searches_a_machines_cached_files_when_it_cant_be_asked() {
    use crate::swarm;
    let tmp = scratch();
    let vol = tmp.path().join("nas/volume1");
    write(
        &vol.join("backup/Quarterly Report 2021.pdf"),
        &blob(1, 5000),
    );
    write(&vol.join("backup/docs/report.txt"), &blob(2, 300));
    write(&vol.join("backup/Résumé.odt"), &blob(3, 700));
    write(&vol.join("other.bin"), &blob(4, 900));
    let nas_db = tmp.path().join("nas.db");
    let mut there = Db::open(&nas_db).unwrap();
    scan(&mut there, &vol);
    let db = Db::open(&tmp.path().join("local.db")).unwrap();
    let m = add(&db, "nas", "ssh://nas.example.org", None, None).unwrap();
    let mut peer = Local {
        db: nas_db.clone(),
        calls: Vec::new(),
    };
    sync(&db, &m, &mut peer, &SyncOptions::default(), &mut |_| {}).unwrap();
    let m = get(&db, m.id).unwrap();
    let find = |q: &str| swarm::cached_find(&db, &m, q, 10, Duration::from_secs(5)).unwrap();

    let (hits, total, complete) = find("report");
    assert!(complete);
    assert_eq!(total, 2);
    // the name itself first, then a word in a name
    assert_eq!(hits[0]["name"], "report.txt");
    assert_eq!(hits[0]["rank"], 4);
    assert_eq!(hits[0]["folder"], spelt("backup/docs"));
    assert_eq!(hits[0]["drive"], "volume1");
    assert_eq!(hits[0]["cached"], true);
    assert_eq!(hits[1]["name"], "Quarterly Report 2021.pdf");
    assert_eq!(hits[1]["rank"], 2);
    // every word must be in the name; case and accents don't matter
    assert_eq!(find("report 2021").0.len(), 1);
    assert_eq!(find("RESUME").0[0]["name"], "Résumé.odt");
    assert!(find("nothing-like-it").0.is_empty());
    // the same search asked live gives the same files (its own index answers)
    let live = peer
        .call("find", json!({ "q": "report", "limit": 10 }), ASK)
        .unwrap();
    assert_eq!(live["hits"].as_array().unwrap().len(), 2);
    assert_eq!(live["hits"][0]["name"], "report.txt");
    // each hit can say which drive it's on, from that machine's storage
    let (_, report, ..) = kept_volumes(&db, m.id).unwrap().unwrap();
    let hit = swarm::with_place(hits[0].clone(), report.as_ref());
    // (on a machine whose drives can be probed: a container may have none to show)
    if !hit["place"].is_null() {
        assert_eq!(hit["place"]["present"], true, "{hit}");
        assert!(hit["place"]["volume"]["mount"].is_string(), "{hit}");
    }
}

#[test]
fn where_is_it_finds_cached_folders_too_and_an_older_cache_is_filled_in() {
    use crate::{name_index, swarm};
    let tmp = scratch();
    let vol = tmp.path().join("nas/volume1");
    write(&vol.join("Music/Moretts/01 Intro.flac"), &blob(1, 500));
    write(&vol.join("Music/Moretts/02 Outro.flac"), &blob(2, 600));
    write(&vol.join("Backups/Moretts.tar"), &blob(3, 700));
    write(&vol.join("Backups/old/Moretts/notes.txt"), &blob(4, 800));
    let nas_db = tmp.path().join("nas.db");
    let mut there = Db::open(&nas_db).unwrap();
    scan(&mut there, &vol);
    let db = Db::open(&tmp.path().join("local.db")).unwrap();
    let m = add(&db, "nas", "ssh://nas.example.org", None, None).unwrap();
    let mut peer = Local {
        db: nas_db.clone(),
        calls: Vec::new(),
    };
    sync(&db, &m, &mut peer, &SyncOptions::default(), &mut |_| {}).unwrap();
    let m = get(&db, m.id).unwrap();
    let find = |q: &str| swarm::cached_find(&db, &m, q, 10, Duration::from_secs(5)).unwrap();
    let described = |hits: &[Value]| -> Vec<String> {
        let mut v: Vec<String> = hits
            .iter()
            .map(|h| {
                format!(
                    "{} {}/{}",
                    h["kind"].as_str().unwrap(),
                    // as that machine spells it (`\` on Windows), read here with `/`
                    h["folder"].as_str().unwrap().replace('\\', "/"),
                    h["name"].as_str().unwrap()
                )
            })
            .collect();
        v.sort();
        v
    };
    assert!(name_index::ready(&db.conn, name_index::REMOTE).unwrap());
    let (hits, total, complete) = find("moretts");
    assert!(complete);
    assert_eq!(
        described(&hits),
        [
            "dir Backups/old/Moretts",
            "dir Music/Moretts",
            "file Backups/Moretts.tar",
        ]
    );
    assert_eq!(total, 3);
    // a folder the search finds is opened at its own path there
    let dir = hits
        .iter()
        .find(|h| h["kind"] == "dir" && h["folder"] == "Music")
        .unwrap();
    assert!(
        dir["path"]
            .as_str()
            .unwrap()
            .ends_with(&spelt("/Music/Moretts")),
        "{dir}"
    );
    assert_eq!(dir["drive"], "volume1");
    // the folders above a file are there too, and the drive's own top isn't
    assert_eq!(described(&find("music").0), ["dir /Music"]);
    assert!(find("volume1").0.is_empty());

    // a cache synced by an older BackDown, opened by this one: no names, no folders, no names
    // index. Searched file by file until the daemon fills them in, then through the names index
    // again, folders and all
    db.conn
        .execute_batch(
            "UPDATE remote_files SET lname = NULL;
             DROP TRIGGER remote_dirs_name_add; DROP TABLE remote_dirs;
             DROP INDEX remote_files_lname; DROP TABLE names_filled;",
        )
        .unwrap();
    let db = Db::open(&tmp.path().join("local.db")).unwrap();
    let find = |q: &str| swarm::cached_find(&db, &m, q, 10, Duration::from_secs(5)).unwrap();
    assert!(!name_index::ready(&db.conn, &["remote_files"]).unwrap());
    assert!(
        !name_index::ready(&db.conn, &["remote_dirs"]).unwrap(),
        "an empty table of folders isn't ready while there are cached files to work them out from"
    );
    assert_eq!(described(&find("moretts").0), ["file Backups/Moretts.tar"]);
    assert!(db.missing_later_indexes().unwrap().contains(&"names"));
    db.build_later_indexes(&mut |_| {}).unwrap();
    assert!(name_index::ready(&db.conn, name_index::REMOTE).unwrap());
    assert_eq!(described(&find("moretts").0).len(), 3);
    assert_eq!(find("moretts").1, 3);

    // the files of a folder all gone there: the folder isn't given any more
    fs::remove_dir_all(vol.join("Backups/old")).unwrap();
    scan(&mut there, &vol);
    sync(&db, &m, &mut peer, &SyncOptions::default(), &mut |_| {}).unwrap();
    assert_eq!(
        described(&find("moretts").0),
        ["dir Music/Moretts", "file Backups/Moretts.tar"]
    );
}
