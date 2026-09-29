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
                "SELECT sha256 FROM remote_files WHERE CAST(path AS TEXT) LIKE ?1",
                [format!("%/{name}")],
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
            "SELECT rid FROM remote_files WHERE CAST(path AS TEXT) LIKE '%/b.txt'",
            [],
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
                "SELECT sha256 FROM files WHERE CAST(path AS TEXT) LIKE ?1",
                [format!("%/{name}")],
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
        copies[0].path.ends_with("/backup/photos/a copy.jpg")
            || copies[0].path.ends_with("/backup/photos/a.jpg")
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
    assert!(drive.containers.iter().any(|c| c.label == "the machine nas"));
    assert!(ov.iter().all(|o| o.unit.level != "machine"));

    // duplicates across machines
    let x = across(&db, 1, 10).unwrap();
    assert_eq!(x.files, 3);
    assert_eq!(x.proven_files, 3);
    assert_eq!(x.largest[0].size, 320_000);
    let mut d = serde_json::to_value(report::dupes(&db, 1, 10).unwrap()).unwrap();
    annotate_dupes(&db, &mut d, 1, 10).unwrap();
    assert_eq!(d["across"]["files"], 3);

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
        proof.starts_with("ssh://nas.example.org/") && proof.ends_with("/backup/big.bin"),
        "{proof}"
    );
    // and without a verifier (an older caller), a remote copy is never enough
    write(&laptop.join("big/big.bin"), &blob(3, 320_000));
    scan(&mut db, &laptop);
    let id = wipe::record(&db, &[laptop.join("big")], None).unwrap();
    let st = wipe::wipe(&mut db, id, false, None, &mut |_, _| {}).unwrap();
    assert_eq!(st.deleted, 0);
}
