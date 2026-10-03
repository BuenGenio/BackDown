//! Signed manifests of a drive: made from the index, checked against their listings, chained,
//! and compared with the drive as it is later.

use std::fs;
use std::io::{BufReader, Cursor};
use std::path::Path;

use backdown_core::manifest::{self, Entry};
use backdown_core::signing::Signer;
use backdown_core::{Db, hash, index};
use serde_json::{Value, json};

fn write(path: &Path, bytes: &[u8]) {
    write_at(path, bytes, 1_700_000_000);
}

/// With its modification time set: the index tells versions apart by size and mtime (to the
/// second), so a rewrite in the same second would look unchanged.
fn write_at(path: &Path, bytes: &[u8], mtime: u64) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(mtime))
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

fn scan(db: &mut Db, root: &Path) -> i64 {
    index::index(
        db,
        root,
        &index::Options {
            one_file_system: true,
            allow_empty: false,
        },
        &mut |_| {},
    )
    .unwrap()
    .scan
}

fn root_id(db: &Db) -> i64 {
    db.conn
        .query_row("SELECT id FROM roots", [], |r| r.get(0))
        .unwrap()
}

/// A manifest of the drive's latest scan, with its listing.
fn make(db: &Db, signer: &Signer) -> (manifest::Made, String) {
    let mut listing = Vec::new();
    let m = manifest::create(
        db,
        signer,
        root_id(db),
        None,
        Some(&mut listing),
        &mut |_| {},
    )
    .unwrap();
    (m, String::from_utf8(listing).unwrap())
}

fn listing_ok(doc: &Value, listing: &str) -> bool {
    let c = manifest::check(doc).unwrap();
    let r = manifest::verify_listing(&c, &mut Cursor::new(listing.as_bytes())).unwrap();
    r.ok
}

/// The same listing with line `i` changed by `f`.
fn tamper(listing: &str, i: usize, f: impl Fn(&mut Value)) -> String {
    let mut lines: Vec<String> = listing.lines().map(String::from).collect();
    let mut v: Value = serde_json::from_str(&lines[i]).unwrap();
    f(&mut v);
    lines[i] = v.to_string();
    lines.join("\n") + "\n"
}

fn drive(tmp: &Path) -> std::path::PathBuf {
    let d = tmp.join("old-drive");
    write(&d.join("docs/report.txt"), b"the first draft");
    write(&d.join("docs/notes.txt"), b"same size here!"); // 15 bytes, like report.txt: hashed
    write(&d.join("photos/beach.jpg"), &[7u8; 5000]);
    write(&d.join("empty.txt"), b"");
    d
}

#[test]
fn a_manifest_verifies_and_any_change_to_its_listing_shows() {
    let tmp = tempfile::tempdir().unwrap();
    let d = drive(tmp.path());
    let mut db = Db::open(&tmp.path().join("idx.db")).unwrap();
    let s = scan(&mut db, &d);
    hash_all(&mut db);
    let signer = Signer::load_or_create(&tmp.path().join("signing.key")).unwrap();
    let (m, listing) = make(&db, &signer);

    let c = manifest::check(&m.doc).unwrap();
    assert_eq!(c.key, signer.public_hex());
    assert_eq!(c.scan_id, s);
    assert_eq!((c.files, c.bytes), (4, 15 + 15 + 5000));
    // the two 15-byte files are hashed, and the empty one is known without reading it
    assert_eq!((c.hashed, c.unhashed), (3, 1));
    assert_eq!(m.doc["prev"], Value::Null);
    assert_eq!(m.doc["drive"]["root_id"], json!(root_id(&db)));
    assert!(m.doc["created"].as_str().unwrap().ends_with('Z'));

    // relative paths, sorted by their bytes
    let paths: Vec<String> = listing
        .lines()
        .map(|l| Entry::parse(l).unwrap().display())
        .collect();
    assert_eq!(
        paths,
        [
            "docs/notes.txt",
            "docs/report.txt",
            "empty.txt",
            "photos/beach.jpg"
        ]
    );
    assert!(listing.contains(
        r#""path":"empty.txt","size":0,"sha256":"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855""#
    ));
    assert!(listing.contains(r#""path":"photos/beach.jpg","size":5000,"sha256":null"#));
    assert!(listing_ok(&m.doc, &listing));

    // the same scan gives the same root every time
    let (again, listing2) = make(&db, &signer);
    assert_eq!(again.doc["merkle"], m.doc["merkle"]);
    assert_eq!(listing2, listing);

    // a changed path, size or hash, a file left out or added, or a reordering: refused
    let changed = [
        tamper(&listing, 0, |v| v["path"] = json!("docs/Notes.txt")),
        tamper(&listing, 1, |v| v["size"] = json!(16)),
        tamper(&listing, 0, |v| {
            v["sha256"] = json!("00000000000000000000000000000000000000000000000000000000000000ff")
        }),
        tamper(&listing, 3, |v| {
            v["sha256"] = json!("1111111111111111111111111111111111111111111111111111111111111111")
        }),
        tamper(&listing, 2, |v| v["sha256"] = Value::Null),
        listing.lines().skip(1).collect::<Vec<_>>().join("\n"),
        format!(
            "{listing}{}\n",
            r#"{"path":"zz.txt","size":0,"sha256":null}"#
        ),
        {
            let mut l: Vec<&str> = listing.lines().collect();
            l.swap(0, 1);
            l.join("\n")
        },
    ];
    for (i, bad) in changed.iter().enumerate() {
        let c = manifest::check(&m.doc).unwrap();
        let r = manifest::verify_listing(&c, &mut BufReader::new(bad.as_bytes())).unwrap();
        assert!(!r.ok, "change {i} went unnoticed");
        assert!(!r.problems.is_empty());
    }

    // a changed document fails its signature
    for change in [
        ("merkle", json!("00".repeat(32))),
        (
            "counts",
            json!({ "files": 3, "bytes": 5030, "hashed": 2, "unhashed": 1 }),
        ),
        ("created", json!("2020-01-01T00:00:00Z")),
        ("prev", json!("11".repeat(32))),
    ] {
        let mut doc = m.doc.clone();
        doc[change.0] = change.1;
        let e = manifest::check(&doc).unwrap_err().to_string();
        assert!(e.contains("signature"), "{}: {e}", change.0);
    }
    // so does one signed by another key
    let other = Signer::load_or_create(&tmp.path().join("other.key")).unwrap();
    let mut doc = m.doc.clone();
    doc["key"] = json!(other.public_hex());
    assert!(manifest::check(&doc).is_err());
    // and it's no manifest at all without its kind
    let mut doc = m.doc.clone();
    doc.as_object_mut().unwrap().remove("kind");
    other.sign_json(&mut doc).unwrap();
    assert!(manifest::check(&doc).is_err());
}

#[test]
fn manifests_chain_and_compare_with_the_drive_later() {
    let tmp = tempfile::tempdir().unwrap();
    let d = drive(tmp.path());
    let mut db = Db::open(&tmp.path().join("idx.db")).unwrap();
    scan(&mut db, &d);
    // not hashed yet: the first manifest attests every file by path and size
    let signer = Signer::load_or_create(&tmp.path().join("signing.key")).unwrap();
    let (first, first_listing) = make(&db, &signer);
    assert_eq!(first.doc["counts"]["unhashed"], json!(3));
    hash_all(&mut db);

    // the drive changes: one file edited (same size), one gone, one new
    write_at(
        &d.join("docs/report.txt"),
        b"the final draft",
        1_700_000_100,
    );
    fs::remove_file(d.join("photos/beach.jpg")).unwrap();
    write_at(
        &d.join("photos/sunset.jpg"),
        b"new photo bytes",
        1_700_000_100,
    );
    scan(&mut db, &d);
    hash_all(&mut db);
    let (second, _) = make(&db, &signer);
    assert_eq!(second.prev_id, Some(first.id));
    assert_eq!(
        second.doc["prev"].as_str().unwrap(),
        manifest::chain_hash(&first.doc)
    );
    let (third, _) = make(&db, &signer);
    assert_eq!(third.prev_id, Some(second.id));

    let kept = manifest::list(&db, Some(root_id(&db))).unwrap();
    assert_eq!(
        kept.iter().map(|k| k.id).collect::<Vec<_>>(),
        [first.id, second.id, third.id]
    );
    let docs: Vec<Value> = kept.iter().map(|k| k.doc.clone()).collect();
    assert!(manifest::check_chain(&docs).is_empty());
    // one left out, or two swapped: it shows
    let p = manifest::check_chain(&[docs[0].clone(), docs[2].clone()]);
    assert!(p.len() == 1 && p[0].what.contains("gap"), "{p:?}");
    let p = manifest::check_chain(&[docs[1].clone(), docs[0].clone(), docs[2].clone()]);
    assert!(
        !p.is_empty() && p.iter().all(|x| x.what.contains("out of order")),
        "{p:?}"
    );

    // the first manifest's listing, made again from the index although its files were hashed
    // since, is exactly the one signed
    let k = manifest::get(&db, first.id).unwrap();
    let mut lines = String::new();
    let root = manifest::replay(&db, &k, &mut |e| {
        lines.push_str(&e.line());
        lines.push('\n');
        Ok(())
    })
    .unwrap();
    assert_eq!(
        backdown_core::fmt::hex(&root),
        first.doc["merkle"].as_str().unwrap()
    );
    assert_eq!(lines, first_listing);

    // compared with the drive now: from the index's record, and from the listing
    let c = manifest::check(&first.doc).unwrap();
    for from_listing in [false, true] {
        let mut reader = Cursor::new(first_listing.as_bytes());
        let cmp = manifest::compare(
            &db,
            &first.doc,
            &c,
            from_listing.then_some(&mut reader as &mut dyn std::io::BufRead),
            10,
        )
        .unwrap();
        assert!(cmp.reproduced, "the manifest's side gives its signed root");
        assert_eq!(cmp.exact, !from_listing);
        assert_eq!(cmp.from, if from_listing { "listing" } else { "index" });
        let names = |t: &manifest::Tally| {
            t.examples
                .iter()
                .map(|e| e.path.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&cmp.added), ["photos/sunset.jpg"]);
        assert_eq!(names(&cmp.removed), ["photos/beach.jpg"]);
        assert_eq!(cmp.removed.bytes, 5000);
        // report.txt was edited but kept its size, and wasn't hashed when the first manifest was
        // signed: nothing proves it changed, so it counts as unchanged but unverified
        assert!(cmp.changed.examples.is_empty(), "{:?}", cmp.changed);
        assert_eq!(cmp.unchanged, 3);
        assert_eq!(
            cmp.unverified, 2,
            "notes.txt and report.txt had no hash then"
        );
    }
    // the second one had hashes: the edit shows as a change
    let c2 = manifest::check(&second.doc).unwrap();
    write_at(&d.join("docs/notes.txt"), b"SAME SIZE HERE!", 1_700_000_200);
    scan(&mut db, &d);
    hash_all(&mut db);
    let cmp = manifest::compare(&db, &second.doc, &c2, None, 10).unwrap();
    assert!(cmp.reproduced && cmp.exact);
    assert_eq!(
        cmp.changed
            .examples
            .iter()
            .map(|e| e.path.as_str())
            .collect::<Vec<_>>(),
        ["docs/notes.txt"]
    );
    let ch = &cmp.changed.examples[0];
    assert_eq!(ch.was.as_ref().unwrap().size, 15);
    assert_ne!(
        ch.was.as_ref().unwrap().sha256,
        ch.now.as_ref().unwrap().sha256
    );
    assert_eq!(cmp.added.files + cmp.removed.files, 0);
}

#[test]
fn a_manifest_needs_a_finished_scan_of_its_drive() {
    let tmp = tempfile::tempdir().unwrap();
    let d = drive(tmp.path());
    let mut db = Db::open(&tmp.path().join("idx.db")).unwrap();
    let s = scan(&mut db, &d);
    let signer = Signer::load_or_create(&tmp.path().join("signing.key")).unwrap();
    let id = root_id(&db);
    let e = manifest::create(&db, &signer, id, Some(s + 100), None, &mut |_| {}).unwrap_err();
    assert!(e.to_string().contains("isn't a finished scan"), "{e}");
    let e = manifest::create(&db, &signer, id + 1, None, None, &mut |_| {}).unwrap_err();
    assert!(e.to_string().contains("no drive"), "{e}");
    // an earlier scan can be attested too
    let m = manifest::create(&db, &signer, id, Some(s), None, &mut |_| {}).unwrap();
    assert_eq!(m.scan, s);
}

#[cfg(unix)]
#[test]
fn names_that_arent_utf8_still_verify() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join("old-drive");
    let latin1 = d.join(OsStr::from_bytes(b"caf\xe9.txt"));
    write(&latin1, b"une tasse");
    write(&d.join("plain.txt"), b"plain");
    // a folder too, with a file of the same size as one outside it, so it's hashed
    write(
        &d.join(OsStr::from_bytes(b"r\xe9sum\xe9s/cv.txt")),
        b"une tasse",
    );
    let mut db = Db::open(&tmp.path().join("idx.db")).unwrap();
    scan(&mut db, &d);
    hash_all(&mut db);
    let signer = Signer::load_or_create(&tmp.path().join("signing.key")).unwrap();
    let (m, listing) = make(&db, &signer);
    let entries: Vec<Entry> = listing.lines().map(|l| Entry::parse(l).unwrap()).collect();
    assert_eq!(entries.len(), 3);
    let odd = entries
        .iter()
        .find(|e| e.path == b"caf\xe9.txt")
        .expect("the exact bytes survive the listing");
    assert!(odd.path_b64().is_some());
    assert!(listing.contains(r#""path_b64":"Y2Fm6S50eHQ""#), "{listing}");
    assert!(
        entries
            .iter()
            .any(|e| e.path == b"r\xe9sum\xe9s/cv.txt" && e.sha256.is_some())
    );
    assert!(listing_ok(&m.doc, &listing));
    // a readable stand-in for the name isn't the name: changing the bytes is noticed
    let bad = listing.replace("Y2Fm6S50eHQ", "Y2Fmw6kudHh0"); // café.txt in UTF-8
    assert!(!listing_ok(&m.doc, &bad));
    // and the drive later: nothing changed
    let c = manifest::check(&m.doc).unwrap();
    let cmp = manifest::compare(&db, &m.doc, &c, None, 10).unwrap();
    assert!(cmp.reproduced);
    assert_eq!(
        (cmp.added.files, cmp.removed.files, cmp.changed.files),
        (0, 0, 0)
    );
}
