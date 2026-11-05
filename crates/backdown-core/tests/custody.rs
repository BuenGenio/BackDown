//! The chain of custody: what each job records, that the chain and its seals check out, and that
//! any change to the log, in the index or in an export, shows.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use backdown_core::custody::{self, Action, Event, Filter, Report};
use backdown_core::signing::{self, Signer, canonical};
use backdown_core::{Db, apply, hash, index, mailmerge, mirror, paths, plan, wipe};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn blob(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(29).wrapping_add(seed))
        .collect()
}

fn sha(b: &[u8]) -> Vec<u8> {
    Sha256::digest(b).to_vec()
}

fn b(p: &Path) -> Vec<u8> {
    paths::to_bytes(p)
}

fn scan_all(db: &mut Db, drives: &[&Path]) {
    for d in drives {
        index::index(
            db,
            d,
            &index::Options {
                one_file_system: true,
                allow_empty: false,
                allow_other_drive: false,
            },
            &mut |_| {},
        )
        .unwrap();
    }
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

/// An event as the log has it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Ev {
    action: String,
    src: Vec<u8>,
    src_sha256: Option<Vec<u8>>,
    dest: Option<Vec<u8>>,
    dest_sha256: Option<Vec<u8>>,
    size: i64,
    job: String,
}

fn ev(
    action: &str,
    job: &str,
    src: &Path,
    dest: Option<impl AsRef<Path>>,
    content: Option<impl AsRef<[u8]>>,
    dest_read: bool,
    size: usize,
) -> Ev {
    let h = content.map(|c| sha(c.as_ref()));
    Ev {
        action: action.into(),
        src: b(src),
        src_sha256: h.clone(),
        dest: dest.map(|d| b(d.as_ref())),
        dest_sha256: h.filter(|_| dest_read),
        size: size as i64,
        job: job.into(),
    }
}

/// The events since `after`, sorted (a folder's files are walked in no particular order).
fn events_after(db: &Db, after: i64) -> Vec<Ev> {
    let mut stmt = db
        .conn
        .prepare("SELECT action, src, src_sha256, dest, dest_sha256, size, ref FROM custody WHERE seq > ?1 ORDER BY seq")
        .unwrap();
    let mut v: Vec<Ev> = stmt
        .query_map([after], |r| {
            Ok(Ev {
                action: r.get(0)?,
                src: r.get(1)?,
                src_sha256: r.get(2)?,
                dest: r.get(3)?,
                dest_sha256: r.get(4)?,
                size: r.get(5)?,
                job: r.get(6)?,
            })
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    v.sort();
    v
}

fn last_seq(db: &Db) -> i64 {
    custody::head(&db.conn).unwrap().map_or(0, |h| h.0)
}

fn signer(dir: &Path) -> Signer {
    Signer::load_or_create(&dir.join("signing.key")).unwrap()
}

fn export(db: &Db, s: &Signer, f: &Filter) -> Vec<u8> {
    let mut out = Vec::new();
    custody::export(&db.conn, s, f, &mut out).unwrap();
    out
}

fn verify(bytes: &[u8]) -> Report {
    custody::verify(bytes).unwrap()
}

fn has(rep: &Report, what: &str) -> bool {
    rep.problems.iter().any(|p| p.contains(what))
}

fn sorted(mut v: Vec<Ev>) -> Vec<Ev> {
    v.sort();
    v
}

fn maildir_message(root: &Path, n: u32, id: &str, subject: &str) -> Vec<u8> {
    for s in ["cur", "new", "tmp"] {
        fs::create_dir_all(root.join(s)).unwrap();
    }
    let m = format!(
        "From: Ann <ann@example.org>\nTo: bob@example.org\nMessage-ID: <{id}>\nDate: Mon, 3 Jun 2019 10:00:00 +0000\nSubject: {subject}\n\nHello.\n"
    );
    write(
        &root
            .join("cur")
            .join(format!("15595{n}.M{n}P1.host.example.org:2,S")),
        m.as_bytes(),
    );
    m.into_bytes()
}

#[test]
fn each_job_records_what_it_did() {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let base = dunce::canonicalize(tmp.path()).unwrap();
    let old = base.join("old-drive");
    let drive = base.join("archive");
    let archive = drive.join("Archive");
    let (a, bb, only) = (blob(1, 40_000), blob(2, 30_000), blob(3, 20_000));
    write(&old.join("photos/a.jpg"), &a);
    write(&old.join("photos/b.jpg"), &bb);
    write(&old.join("docs/only.odt"), &only);
    write(&drive.join("readme.txt"), b"the archive drive");
    let mut db = Db::open(&base.join("i.db")).unwrap();
    assert!(custody::check(&db.conn).unwrap().ok, "an empty log checks");
    assert!(custody::started(&db.conn).unwrap().is_some());
    scan_all(&mut db, &[&old, &drive]);

    // a plan's copies
    let item = plan::Item {
        what: "Folder".into(),
        kind: "unsorted".into(),
        source: old.join("photos").display().to_string(),
        source_key: String::new(),
        drive: old.display().to_string(),
        is_file: false,
        only_files: false,
        dest: "Photos".into(),
        files: 2,
        bytes: 70_000,
        action: "copy".into(),
        why: String::new(),
        exclude: vec![],
        twin: None,
        merge: vec![],
    };
    let pid = plan::save(
        &db,
        &plan::Plan {
            target: archive.display().to_string(),
            created_at: 0,
            schema: String::new(),
            items: vec![item],
            drives: vec![],
            copy_files: 2,
            copy_bytes: 70_000,
        },
    )
    .unwrap();
    // a dry run records nothing
    apply::apply(&mut db, pid, true, &mut |_| {}).unwrap();
    assert_eq!(last_seq(&db), 0);
    let st = apply::apply(&mut db, pid, false, &mut |_| {}).unwrap();
    assert_eq!(st.copied, 2, "{st:?}");
    let plan_ref = format!("plan:{pid}");
    let (a_old, b_old) = (old.join("photos/a.jpg"), old.join("photos/b.jpg"));
    let (a_new, b_new) = (archive.join("Photos/a.jpg"), archive.join("Photos/b.jpg"));
    assert_eq!(
        events_after(&db, 0),
        sorted(vec![
            ev(
                "copy",
                &plan_ref,
                &a_old,
                Some(&a_new),
                Some(&a),
                true,
                a.len()
            ),
            ev(
                "copy",
                &plan_ref,
                &b_old,
                Some(&b_new),
                Some(&bb),
                true,
                bb.len()
            ),
        ])
    );
    // applied again: nothing new was copied, so nothing new is recorded
    apply::apply(&mut db, pid, false, &mut |_| {}).unwrap();
    assert_eq!(last_seq(&db), 2);

    // a wipe: the photos have copies in the archive, the document is moved first
    scan_all(&mut db, &[&old, &drive]);
    let rescued = drive.join("Rescued");
    let wid = wipe::record(&db, &[old.join("photos"), old.join("docs")], Some(&rescued)).unwrap();
    let st = wipe::wipe(&mut db, wid, false, None, &mut |_, _| {}).unwrap();
    assert_eq!((st.deleted, st.moved), (3, 1), "{st:?}");
    let wipe_ref = format!("wipe:{wid}");
    let moved_to = rescued.join("old-drive/docs/only.odt");
    assert_eq!(
        events_after(&db, 2),
        sorted(vec![
            ev(
                "delete",
                &wipe_ref,
                &a_old,
                Some(&a_new),
                Some(&a),
                true,
                a.len()
            ),
            ev(
                "delete",
                &wipe_ref,
                &b_old,
                Some(&b_new),
                Some(&bb),
                true,
                bb.len()
            ),
            ev(
                "move",
                &wipe_ref,
                &old.join("docs/only.odt"),
                Some(&moved_to),
                Some(&only),
                true,
                only.len()
            ),
        ])
    );

    // tidying: a duplicate becomes a hardlink to its twin
    #[cfg(unix)]
    {
        use backdown_core::dedup::{self, Link};
        let dup = blob(4, 50_000);
        let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
        for n in ["Dups/one.bin", "Dups/two.bin"] {
            write(&archive.join(n), &dup);
            fs::File::options()
                .write(true)
                .open(archive.join(n))
                .unwrap()
                .set_modified(when)
                .unwrap();
        }
        scan_all(&mut db, &[&drive]);
        let before = last_seq(&db);
        let id = dedup::record(&db, &archive, Link::Hard, 4096).unwrap();
        let r = dedup::run(&db, id, None, &AtomicBool::new(false), &mut |_, _| {}).unwrap();
        assert_eq!(r.linked, 1, "{r:?}");
        let got = events_after(&db, before);
        assert_eq!(got.len(), 1, "{got:?}");
        let (one, two) = (
            b(&archive.join("Dups/one.bin")),
            b(&archive.join("Dups/two.bin")),
        );
        let e = &got[0];
        assert_eq!(
            (e.action.as_str(), e.job.clone()),
            ("link", format!("dedup:{id}"))
        );
        assert!(
            (e.src == one && e.dest.as_ref() == Some(&two))
                || (e.src == two && e.dest.as_ref() == Some(&one)),
            "{e:?}"
        );
        assert_eq!(e.src_sha256, Some(sha(&dup)));
        assert_eq!(e.dest_sha256, Some(sha(&dup)));
        assert_eq!(e.size, dup.len() as i64);
    }

    // a mirror: each copy, and an old copy set aside when the source changed
    let photos = archive.join("Photos");
    let mirror_dir = base.join("mirror-drive/Photos");
    let mid = mirror::add(&db, &photos, &mirror_dir, 30).unwrap();
    let before = last_seq(&db);
    mirror::sync(&db, mid, &mut |_, _| {}).unwrap();
    let mirror_ref = format!("mirror:{mid}");
    assert_eq!(
        events_after(&db, before),
        sorted(vec![
            ev(
                "copy",
                &mirror_ref,
                &a_new,
                Some(&mirror_dir.join("a.jpg")),
                Some(&a),
                true,
                a.len()
            ),
            ev(
                "copy",
                &mirror_ref,
                &b_new,
                Some(&mirror_dir.join("b.jpg")),
                Some(&bb),
                true,
                bb.len()
            ),
        ])
    );
    let a2 = blob(9, 41_000);
    write(&a_new, &a2);
    let before = last_seq(&db);
    mirror::sync(&db, mid, &mut |_, _| {}).unwrap();
    let got = events_after(&db, before);
    assert_eq!(got.len(), 2, "{got:?}");
    assert_eq!(
        got[0],
        ev(
            "copy",
            &mirror_ref,
            &a_new,
            Some(&mirror_dir.join("a.jpg")),
            Some(&a2),
            true,
            a2.len()
        )
    );
    let aside = &got[1];
    assert_eq!(
        (aside.action.as_str(), &aside.src, aside.size),
        ("move", &b(&mirror_dir.join("a.jpg")), a.len() as i64)
    );
    let to = paths::from_bytes(aside.dest.as_ref().unwrap());
    assert!(to.starts_with(mirror_dir.join(mirror::VERSIONS)), "{to:?}");
    assert_eq!(fs::read(&to).unwrap(), a, "the old copy is where it says");
    assert_eq!((&aside.src_sha256, &aside.dest_sha256), (&None, &None));

    // merging two snapshots of a mailbox: each message written, once
    let (s1, s2) = (
        base.join("old-drive/mail-2019"),
        base.join("old-drive/mail-2020"),
    );
    let m1 = maildir_message(&s1, 1, "one@example.org", "first");
    maildir_message(&s2, 1, "one@example.org", "first");
    let m2 = maildir_message(&s2, 2, "two@example.org", "second");
    let merged = base.join("merged");
    let id = mailmerge::record(&db, &[s1.clone(), s2.clone()], &merged, None).unwrap();
    let before = last_seq(&db);
    let r = mailmerge::run(&db, id, &AtomicBool::new(false), &mut |_, _, _| {}).unwrap();
    assert_eq!(r.written, 2, "{r:?}");
    let got = events_after(&db, before);
    assert_eq!(got.len(), 2, "{got:?}");
    for e in &got {
        assert_eq!(
            (e.action.as_str(), e.job.clone()),
            ("merge", format!("merge:{id}"))
        );
        let dest = paths::from_bytes(e.dest.as_ref().unwrap());
        assert!(dest.starts_with(&merged), "{dest:?}");
        let bytes = fs::read(&dest).unwrap();
        assert!(bytes == m1 || bytes == m2);
        assert_eq!(e.src_sha256, Some(sha(&bytes)));
        assert_eq!(e.dest_sha256, e.src_sha256);
        assert!(paths::from_bytes(&e.src).starts_with(base.join("old-drive")));
    }

    // the chain checks; sealed, it's covered to the end
    let s = signer(&base);
    let n = last_seq(&db);
    let rep = custody::check(&db.conn).unwrap();
    assert!(rep.ok, "{rep:?}");
    assert_eq!(
        (rep.events as i64, rep.unsealed as i64, rep.sealed_to),
        (n, n, 0)
    );
    let seal = custody::seal(&db.conn, &s).unwrap().expect("a seal");
    assert_eq!(seal["seq"], json!(n));
    assert!(
        custody::seal(&db.conn, &s).unwrap().is_none(),
        "nothing new to seal"
    );
    assert!(custody::sealed(&db.conn, &s).unwrap());
    let rep = custody::check(&db.conn).unwrap();
    assert!(rep.ok && rep.unsealed == 0 && rep.sealed_to == n, "{rep:?}");
    assert_eq!(rep.seal_keys, vec![s.fingerprint()]);

    // the export verifies, and is a document signing.rs checks too
    let bundle = export(&db, &s, &Filter::default());
    let rep = verify(&bundle);
    assert!(rep.ok, "{rep:?}");
    assert_eq!((rep.events as i64, rep.elided), (n, 0));
    assert_eq!(rep.key, Some(s.fingerprint()));
    let doc: Value = serde_json::from_slice(&bundle).unwrap();
    assert_eq!(signing::verify_json(&doc).unwrap(), s.public_hex());
    assert!(doc["note"].as_str().unwrap().contains("The log starts on"));
}

/// Six events by four jobs, sealed after the third and the sixth.
fn sample_log(dir: &Path) -> (Db, Signer) {
    let db = Db::open(&dir.join("i.db")).unwrap();
    let s = signer(dir);
    let h = |n: u8| sha(&[n]);
    let p = |s: &str| PathBuf::from(s);
    let copy = |src: &str, dest: &str, n: u8, size: u64, job: (&str, i64), act: Action| {
        Event::new(act, job, &b(&p(src)), Some(h(n).as_slice()), size)
            .to(&b(&p(dest)), Some(h(n).as_slice()))
    };
    let evs = [
        copy(
            "/media/old-drive/DCIM/a.jpg",
            "/media/archive/Photos/a.jpg",
            1,
            100,
            ("plan", 1),
            Action::Copy,
        ),
        copy(
            "/media/old-drive/DCIM/b.jpg",
            "/media/archive/Photos/b.jpg",
            2,
            200,
            ("plan", 1),
            Action::Copy,
        ),
        copy(
            "/media/old-drive/DCIM/a.jpg",
            "/media/archive/Photos/a.jpg",
            1,
            100,
            ("wipe", 1),
            Action::Delete,
        ),
        copy(
            "/media/old-drive/notes.txt",
            "/media/archive/Rescued/old-drive/notes.txt",
            3,
            300,
            ("wipe", 1),
            Action::Move,
        ),
        copy(
            "/media/archive/Photos/b copy.jpg",
            "/media/archive/Photos/b.jpg",
            2,
            200,
            ("dedup", 2),
            Action::Link,
        ),
        Event::new(
            Action::Delete,
            ("wipe", 1),
            &b(&p("/media/old-drive/empty.txt")),
            Some(sha(b"").as_slice()),
            0,
        ),
    ];
    for (i, e) in evs.iter().enumerate() {
        assert_eq!(custody::append(&db.conn, e).unwrap(), i as i64 + 1);
        if i == 2 || i == 5 {
            custody::seal(&db.conn, &s).unwrap().expect("sealed");
        }
    }
    (db, s)
}

#[test]
fn changing_the_log_in_the_index_shows() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, s) = sample_log(tmp.path());
    let rep = custody::check(&db.conn).unwrap();
    assert!(rep.ok, "{rep:?}");
    assert_eq!((rep.events, rep.seals, rep.sealed_to), (6, 2, 6));
    assert!(verify(&export(&db, &s, &Filter::default())).ok);

    let edits: &[(&str, &str, &str)] = &[
        (
            "its time",
            "UPDATE custody SET at = at + 1 WHERE seq = 4",
            "event 4 was changed after it was recorded",
        ),
        (
            "its action",
            "UPDATE custody SET action = 'copy' WHERE seq = 4",
            "event 4 was changed",
        ),
        (
            "its source",
            "UPDATE custody SET src = CAST('/media/elsewhere/notes.txt' AS BLOB) WHERE seq = 4",
            "event 4 was changed",
        ),
        (
            "its source's hash",
            "UPDATE custody SET src_sha256 = zeroblob(32) WHERE seq = 4",
            "event 4 was changed",
        ),
        (
            "its destination",
            "UPDATE custody SET dest = NULL WHERE seq = 4",
            "event 4 was changed",
        ),
        (
            "its destination's hash",
            "UPDATE custody SET dest_sha256 = NULL WHERE seq = 4",
            "event 4 was changed",
        ),
        (
            "its size",
            "UPDATE custody SET size = 301 WHERE seq = 4",
            "event 4 was changed",
        ),
        (
            "its job",
            "UPDATE custody SET ref = 'wipe:2' WHERE seq = 4",
            "event 4 was changed",
        ),
        (
            "its link",
            "UPDATE custody SET prev = zeroblob(32) WHERE seq = 4",
            "event 4 doesn't follow event 3",
        ),
        (
            "its hash",
            "UPDATE custody SET hash = zeroblob(32) WHERE seq = 4",
            "event 4 was changed",
        ),
        (
            "its number",
            "UPDATE custody SET seq = 40 WHERE seq = 4",
            "event 4 is missing",
        ),
        (
            "one taken out",
            "DELETE FROM custody WHERE seq = 4",
            "event 4 is missing",
        ),
        (
            "the first taken out",
            "DELETE FROM custody WHERE seq = 1",
            "event 1 is missing",
        ),
        (
            "the last taken out",
            "DELETE FROM custody WHERE seq = 6",
            "goes further than the log, which ends at event 5",
        ),
        (
            "two swapped",
            "UPDATE custody SET seq = 100 WHERE seq = 2; UPDATE custody SET seq = 2 WHERE seq = 3; UPDATE custody SET seq = 3 WHERE seq = 100",
            "event 2 was changed",
        ),
        (
            "a seal changed",
            "UPDATE custody_seals SET doc = replace(doc, '\"at\":', '\"at\":1') WHERE seq = 3",
            "doesn't verify",
        ),
        (
            "a seal damaged",
            "UPDATE custody_seals SET doc = 'not a seal' WHERE seq = 6",
            "doesn't verify",
        ),
    ];
    for (what, sql, says) in edits {
        db.conn.execute_batch("SAVEPOINT edit").unwrap();
        db.conn.execute_batch(sql).unwrap();
        let rep = custody::check(&db.conn).unwrap();
        assert!(!rep.ok, "{what}: {rep:?}");
        assert!(has(&rep, says), "{what}: {:?}", rep.problems);
        // an export of it says so too, and gets no new seal
        let bundle = export(&db, &s, &Filter::default());
        let v = verify(&bundle);
        assert!(!v.ok && has(&v, says), "{what}: {:?}", v.problems);
        // nor does sealing vouch for it
        assert!(
            !matches!(custody::seal(&db.conn, &s), Ok(Some(_))),
            "{what}"
        );
        db.conn
            .execute_batch("ROLLBACK TO edit; RELEASE edit")
            .unwrap();
    }
    assert!(custody::check(&db.conn).unwrap().ok, "all undone");

    // a careful rewrite, every hash recomputed after the change: the seals don't match it
    db.conn.execute_batch("SAVEPOINT edit").unwrap();
    let mut doc: Value = serde_json::from_slice(&export(&db, &s, &Filter::default())).unwrap();
    let events = doc["events"].as_array_mut().unwrap();
    events[1]["size"] = json!(201);
    rehash(events, 1);
    db.conn
        .execute("UPDATE custody SET size = 201 WHERE seq = 2", [])
        .unwrap();
    for e in events.iter().skip(1) {
        db.conn
            .execute(
                "UPDATE custody SET prev = ?2, hash = ?3 WHERE seq = ?1",
                rusqlite::params![
                    e["seq"].as_i64(),
                    unhex(e["prev"].as_str().unwrap()),
                    unhex(e["hash"].as_str().unwrap())
                ],
            )
            .unwrap();
    }
    let rep = custody::check(&db.conn).unwrap();
    assert!(!rep.ok, "{rep:?}");
    assert!(
        has(&rep, "event 3 isn't what was sealed on") && has(&rep, "event 6 isn't what was sealed"),
        "{:?}",
        rep.problems
    );
    assert!(
        !rep.problems
            .iter()
            .any(|p| p.contains("was changed after it was recorded"))
    );
    db.conn
        .execute_batch("ROLLBACK TO edit; RELEASE edit")
        .unwrap();

    // a seal taken away leaves events unsealed (an export seals them again)
    db.conn
        .execute("DELETE FROM custody_seals WHERE seq = 6", [])
        .unwrap();
    let rep = custody::check(&db.conn).unwrap();
    assert!(rep.ok && rep.unsealed == 3 && rep.sealed_to == 3, "{rep:?}");
}

/// Recompute `prev` and `hash` from event `from` on, as a forger would.
fn rehash(events: &mut [Value], from: usize) {
    for i in from..events.len() {
        let prev = events[i - 1]["hash"].as_str().unwrap().to_string();
        let e = &mut events[i];
        let mut fields = e.as_object().unwrap().clone();
        fields.remove("prev");
        fields.remove("hash");
        let digest = signing::sha256_hex(&canonical(&Value::Object(fields)));
        e["prev"] = json!(prev);
        e["hash"] = json!(signing::sha256_hex(format!("{prev}{digest}").as_bytes()));
    }
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn resign(doc: &mut Value, s: &Signer) -> Vec<u8> {
    s.sign_json(doc).unwrap();
    serde_json::to_vec_pretty(doc).unwrap()
}

#[test]
fn changing_an_export_shows() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, s) = sample_log(tmp.path());
    let bundle = export(&db, &s, &Filter::default());
    let rep = verify(&bundle);
    assert!(rep.ok, "{rep:?}");
    assert_eq!((rep.events, rep.seals, rep.sealed_to), (6, 2, 6));
    let doc: Value = serde_json::from_slice(&bundle).unwrap();
    // reformatted, it's the same document
    assert!(verify(&serde_json::to_vec_pretty(&doc).unwrap()).ok);
    // not a bundle at all
    assert!(custody::verify(&b"{\"kind\":\"something else\",\"events\":[]}"[..]).is_err());
    assert!(custody::verify(&b"not json"[..]).is_err());

    // changed and not signed again: the export's own signature says so
    let mut d = doc.clone();
    d["events"][3]["size"] = json!(301);
    let rep = verify(&serde_json::to_vec(&d).unwrap());
    assert!(
        has(&rep, "the export itself was changed"),
        "{:?}",
        rep.problems
    );
    assert!(has(&rep, "event 4 was changed after it was recorded"));

    // changed and signed again (by whoever has a key): the chain says what
    type Change = Box<dyn Fn(&mut Value)>;
    let cases: Vec<(&str, Change, &str)> = vec![
        (
            "a field",
            Box::new(|d| d["events"][3]["src"] = json!("/media/elsewhere/notes.txt")),
            "event 4 was changed after it was recorded",
        ),
        (
            "an event taken out",
            Box::new(|d| {
                d["events"].as_array_mut().unwrap().remove(3);
            }),
            "event 4 is missing",
        ),
        (
            "two swapped",
            Box::new(|d| d["events"].as_array_mut().unwrap().swap(1, 2)),
            "is out of place",
        ),
        (
            "the last seal taken out",
            Box::new(|d| {
                d["seals"].as_array_mut().unwrap().pop();
            }),
            "events after seq 3 aren't covered by a seal",
        ),
        (
            "every seal taken out",
            Box::new(|d| d["seals"] = json!([])),
            "no event is covered by a seal",
        ),
        (
            "the last event taken out",
            Box::new(|d| {
                d["events"].as_array_mut().unwrap().pop();
            }),
            "goes further than the log",
        ),
        (
            "a seal's signature",
            Box::new(|d| d["seals"][0]["at"] = json!(1)),
            "doesn't verify",
        ),
        (
            "a seal for another event",
            Box::new(|d| d["seals"][1]["hash"] = d["events"][4]["hash"].clone()),
            "doesn't verify",
        ),
    ];
    for (what, change, says) in &cases {
        let mut d = doc.clone();
        change(&mut d);
        let rep = verify(&resign(&mut d, &s));
        assert!(!rep.ok, "{what}: {rep:?}");
        assert!(has(&rep, says), "{what}: {:?}", rep.problems);
        assert!(!has(&rep, "the export itself"), "{what}: signed again");
    }

    // a careful rewrite (every hash recomputed), signed with another key: the seals don't match
    let other = Signer::load_or_create(&tmp.path().join("forger.key")).unwrap();
    let mut d = doc.clone();
    let events = d["events"].as_array_mut().unwrap();
    events[1]["size"] = json!(201);
    rehash(events, 1);
    let rep = verify(&resign(&mut d, &other));
    assert!(!rep.ok);
    assert!(
        has(&rep, "event 3 isn't what was sealed on"),
        "{:?}",
        rep.problems
    );
    assert_eq!(rep.key, Some(other.fingerprint()));
    assert_eq!(rep.seal_keys, Vec::<String>::new(), "no seal covers it");
}

#[test]
fn an_elided_export_verifies_without_showing_other_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, s) = sample_log(tmp.path());
    let only = |f: Filter| {
        let bytes = export(&db, &s, &f);
        let rep = verify(&bytes);
        assert!(rep.ok, "{f:?}: {rep:?}");
        let doc: Value = serde_json::from_slice(&bytes).unwrap();
        (String::from_utf8(bytes).unwrap(), doc, rep)
    };
    let (text, doc, rep) = only(Filter {
        job: Some(custody::parse_ref("plan 1").unwrap()),
        ..Filter::default()
    });
    assert_eq!((rep.events, rep.elided), (6, 4));
    assert_eq!(doc["filter"], json!({ "ref": "plan:1" }));
    assert!(doc["note"].as_str().unwrap().contains("elided"));
    for e in doc["events"].as_array().unwrap() {
        let shown = e["ref"] == "plan:1";
        assert_eq!(e.get("elided").is_none(), shown, "{e}");
        if !shown {
            let mut keys: Vec<&str> = e.as_object().unwrap().keys().map(String::as_str).collect();
            keys.sort();
            assert_eq!(keys, ["digest", "elided", "hash", "seq"], "{e}");
        }
    }
    for hidden in [
        "notes.txt",
        "b copy.jpg",
        "empty.txt",
        "Rescued",
        "wipe:1",
        "dedup:2",
    ] {
        assert!(!text.contains(hidden), "{hidden} shows in an elided export");
    }
    assert!(
        !text.contains(&signing::sha256_hex(&[3])),
        "nor do their files' hashes"
    );

    // by folder: whole names only (/media/archive/Pho isn't /media/archive/Photos)
    let (_, _, rep) = only(Filter {
        path: Some(b(Path::new("/media/archive/Photos"))),
        ..Filter::default()
    });
    assert_eq!(rep.elided, 2, "{rep:?}");
    let (_, _, rep) = only(Filter {
        path: Some(b(Path::new("/media/archive/Pho"))),
        ..Filter::default()
    });
    assert_eq!(rep.elided, 6);
    // by date: nothing since tomorrow
    let (_, doc, rep) = only(Filter {
        since: Some(custody::parse_date("2999-01-01").unwrap()),
        ..Filter::default()
    });
    assert_eq!((rep.events, rep.elided), (6, 6));
    assert_eq!(rep.sealed_to, 6, "sealed to the end all the same");

    // an elided event changed: the chain notices
    let mut d = doc.clone();
    d["events"][2]["digest"] = d["events"][1]["digest"].clone();
    let rep = verify(&resign(&mut d, &s));
    assert!(
        has(&rep, "event 3 was changed after it was recorded"),
        "{:?}",
        rep.problems
    );
    // and one left out without its digest can't be checked through
    let mut d = doc.clone();
    d["events"][2].as_object_mut().unwrap().remove("digest");
    let rep = verify(&resign(&mut d, &s));
    assert!(has(&rep, "without its digest"), "{:?}", rep.problems);
}

#[test]
fn an_export_seals_what_isnt_sealed_yet() {
    let tmp = tempfile::tempdir().unwrap();
    let (db, s) = sample_log(tmp.path());
    custody::append(
        &db.conn,
        &Event::new(
            Action::Copy,
            ("plan", 2),
            &b(Path::new("/media/x")),
            None,
            1,
        ),
    )
    .unwrap();
    let rep = custody::check(&db.conn).unwrap();
    assert!(rep.ok && rep.unsealed == 1 && rep.sealed_to == 6, "{rep:?}");
    assert!(!custody::sealed(&db.conn, &s).unwrap());
    // the export carries a seal made for it (and the index is left as it was)
    let mut out = Vec::new();
    let ex = custody::export(&db.conn, &s, &Filter::default(), &mut out).unwrap();
    assert!(
        ex.sealed_now && ex.sealed_to == 7 && ex.problems.is_empty(),
        "{ex:?}"
    );
    let rep = verify(&out);
    assert!(rep.ok && rep.seals == 3, "{rep:?}");
    assert!(!custody::sealed(&db.conn, &s).unwrap());
    // without that seal, the last event isn't covered
    let mut d: Value = serde_json::from_slice(&out).unwrap();
    d["seals"].as_array_mut().unwrap().pop();
    let rep = verify(&resign(&mut d, &s));
    assert!(
        has(&rep, "events after seq 6 aren't covered by a seal"),
        "{:?}",
        rep.problems
    );
}

#[cfg(unix)]
#[test]
fn paths_that_arent_utf8_are_kept_exactly() {
    use base64::Engine;
    use std::os::unix::ffi::OsStrExt;
    let tmp = tempfile::tempdir().unwrap();
    let (db, s) = sample_log(tmp.path());
    let raw = b"/media/old-drive/caf\xe9.jpg".to_vec();
    let to = b"/media/archive/caf\xe9.jpg".to_vec();
    custody::append(
        &db.conn,
        &Event::new(
            Action::Copy,
            ("plan", 3),
            &raw,
            Some(sha(b"x").as_slice()),
            1,
        )
        .to(&to, None),
    )
    .unwrap();
    custody::seal(&db.conn, &s).unwrap();
    assert!(custody::check(&db.conn).unwrap().ok);
    let bundle = export(&db, &s, &Filter::default());
    assert!(verify(&bundle).ok);
    let doc: Value = serde_json::from_slice(&bundle).unwrap();
    let e = &doc["events"][6];
    assert!(e.get("src").is_none() && e.get("dest").is_none(), "{e}");
    let decode = |k: &str| {
        base64::engine::general_purpose::STANDARD
            .decode(e[k].as_str().unwrap())
            .unwrap()
    };
    assert_eq!(
        (decode("src_base64"), decode("dest_base64")),
        (raw.clone(), to)
    );
    // and a filter by such a folder finds it
    let f = Filter {
        path: Some(custody::path_filter(Path::new(
            std::ffi::OsStr::from_bytes(b"/media/old-drive"),
        ))),
        ..Filter::default()
    };
    let rep = verify(&export(&db, &s, &f));
    assert!(rep.ok);
    assert_eq!(rep.events - rep.elided, 6, "{rep:?}");
}

/// A real file with such a name, copied by a mirror (Linux: macOS's file systems don't take them).
#[cfg(target_os = "linux")]
#[test]
fn a_file_named_in_no_encoding_is_recorded_as_it_is() {
    use std::os::unix::ffi::OsStrExt;
    let tmp = tempfile::tempdir().unwrap();
    let base = dunce::canonicalize(tmp.path()).unwrap();
    let src = base.join("Photos");
    let name = std::ffi::OsStr::from_bytes(b"caf\xe9.jpg");
    write(&src.join(name), b"bytes");
    let db = Db::open(&base.join("i.db")).unwrap();
    let id = mirror::add(&db, &src, &base.join("mirror"), 30).unwrap();
    mirror::sync(&db, id, &mut |_, _| {}).unwrap();
    let got = events_after(&db, 0);
    assert_eq!(
        got,
        vec![ev(
            "copy",
            &format!("mirror:{id}"),
            &src.join(name),
            Some(&base.join("mirror").join(name)),
            Some(b"bytes"),
            true,
            5
        )]
    );
    let s = signer(&base);
    let bundle = export(&db, &s, &Filter::default());
    assert!(verify(&bundle).ok);
    let doc: Value = serde_json::from_slice(&bundle).unwrap();
    assert!(doc["events"][0]["src_base64"].is_string());
}
