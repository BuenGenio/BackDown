//! Merging snapshots of one mailbox into one Maildir, and exporting a Maildir as an mbox.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use backdown_core::Db;
use backdown_core::mailmerge::{self, SEEN};

/// Maildir's info separator as a Maildir on this system has it: `:` can't be in a Windows file
/// name (`a:b` is a stream of `a` there), so `!` (what mailmerge writes there too).
const INFO: &str = if cfg!(windows) { "!2," } else { ":2," };

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn maildir(root: &Path, folder: &str) -> PathBuf {
    let d = if folder == "INBOX" {
        root.to_path_buf()
    } else {
        root.join(format!(".{folder}"))
    };
    for s in ["cur", "new", "tmp"] {
        fs::create_dir_all(d.join(s)).unwrap();
    }
    d
}

fn msg(id: Option<&str>, subject: &str, body: &str) -> String {
    let mut m = String::from("From: Ann <ann@example.org>\nTo: bob@example.org\n");
    if let Some(id) = id {
        m += &format!("Message-ID: <{id}>\n");
    }
    m += &format!("Date: Mon, 3 Jun 2019 10:00:00 +0000\nSubject: {subject}\n\n{body}\n");
    m
}

/// Put a message in a Maildir folder, delivered at `t`, with flags.
fn put(root: &Path, folder: &str, t: i64, n: u32, flags: &str, m: &str) -> PathBuf {
    let p = maildir(root, folder)
        .join("cur")
        .join(format!("{t}.M{n}P1.host.example.org{INFO}{flags}"));
    write(&p, m.as_bytes());
    p
}

/// Every message file under a merged Maildir: folder -> (name, bytes).
fn result(root: &Path) -> BTreeMap<String, Vec<(String, Vec<u8>)>> {
    let mut out: BTreeMap<String, Vec<(String, Vec<u8>)>> = BTreeMap::new();
    for e in walk(root) {
        let parent = e.parent().unwrap();
        if parent.file_name().unwrap() != "cur" && parent.file_name().unwrap() != "new" {
            continue;
        }
        let folder_dir = parent.parent().unwrap();
        let folder = if folder_dir == root {
            "INBOX".to_string()
        } else {
            folder_dir
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .trim_start_matches('.')
                .to_string()
        };
        out.entry(folder).or_default().push((
            e.file_name().unwrap().to_string_lossy().into_owned(),
            fs::read(&e).unwrap(),
        ));
    }
    out
}

fn walk(d: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(d) else { return out };
    for e in rd {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out.sort();
    out
}

fn subjects(msgs: &[(String, Vec<u8>)]) -> BTreeSet<String> {
    msgs.iter()
        .map(|(_, b)| {
            let s = String::from_utf8_lossy(b);
            s.lines()
                .find_map(|l| l.strip_prefix("Subject: "))
                .unwrap_or("")
                .to_string()
        })
        .collect()
}

fn flags_of(msgs: &[(String, Vec<u8>)], subject: &str) -> String {
    msgs.iter()
        .find(|(_, b)| String::from_utf8_lossy(b).contains(&format!("Subject: {subject}\n")))
        .map(|(n, _)| n.rsplit_once(INFO).unwrap().1.to_string())
        .unwrap_or_else(|| panic!("no {subject}"))
}

struct Fixture {
    older: PathBuf,
    newer: PathBuf,
    mbox: PathBuf,
    /// every source copy's bytes, as a message program would read them
    originals: BTreeSet<Vec<u8>>,
}

fn fixture(d: &Path) -> Fixture {
    let older = d.join("drive-2019/Maildir");
    let newer = d.join("drive-2021/mail/ann");
    let mut originals = BTreeSet::new();
    let mut keep = |p: PathBuf| {
        originals.insert(fs::read(p).unwrap());
    };
    // 2019: read "welcome"; an unread "invoice"; a note with no Message-ID; "trip", later filed
    // away; a sent message; and a folder the newer one no longer has
    keep(put(
        &older,
        "INBOX",
        1_559_000_000,
        1,
        "S",
        &msg(Some("welcome@example.org"), "welcome", "Hello Bob."),
    ));
    keep(put(
        &older,
        "INBOX",
        1_559_100_000,
        2,
        "",
        &msg(Some("invoice@example.org"), "invoice", "Pay by June."),
    ));
    keep(put(
        &older,
        "INBOX",
        1_559_200_000,
        3,
        "S",
        &msg(None, "no id", "A note without an ID."),
    ));
    keep(put(
        &older,
        "INBOX",
        1_559_300_000,
        4,
        "",
        &msg(Some("trip@example.org"), "trip", "Tickets attached."),
    ));
    keep(put(
        &older,
        "Sent",
        1_559_400_000,
        5,
        "S",
        &msg(Some("reply@example.org"), "reply", "Thanks!"),
    ));
    keep(put(
        &older,
        "Old",
        1_559_500_000,
        6,
        "S",
        &msg(Some("old@example.org"), "old", "From an old folder."),
    ));
    // 2021: "welcome" flagged but (on this copy) unread; "invoice" replied to, with a header the
    // server added; "trip" moved to Archive; the note deleted; something new
    keep(put(
        &newer,
        "INBOX",
        1_559_000_000,
        1,
        "F",
        &msg(Some("welcome@example.org"), "welcome", "Hello Bob."),
    ));
    let invoice = msg(Some("invoice@example.org"), "invoice", "Pay by June.").replacen(
        "Subject: invoice\n",
        "Subject: invoice\nX-UID: 17\n",
        1,
    );
    keep(put(&newer, "INBOX", 1_559_100_000, 2, "RS", &invoice));
    keep(put(
        &newer,
        "Archive",
        1_559_300_000,
        4,
        "S",
        &msg(Some("trip@example.org"), "trip", "Tickets attached."),
    ));
    keep(put(
        &newer,
        "Sent",
        1_559_400_000,
        5,
        "S",
        &msg(Some("reply@example.org"), "reply", "Thanks!"),
    ));
    keep(put(
        &newer,
        "INBOX",
        1_620_000_000,
        7,
        "",
        &msg(Some("new@example.org"), "new", "Brand new."),
    ));
    // an mbox export from 2020 (Thunderbird's Inbox): "welcome" (read: Status RO), a message
    // twice, a message with no ID twice, and one whose body has a line starting "From "
    let mut mbox = String::new();
    let mut add = |env: &str, m: &str| {
        mbox += &format!("From {env}\n");
        for line in m.split_inclusive('\n') {
            if line.trim_start_matches('>').starts_with("From ") {
                mbox.push('>');
            }
            mbox += line;
        }
        mbox += "\n";
    };
    let welcome = msg(Some("welcome@example.org"), "welcome", "Hello Bob.").replacen(
        "\n\n",
        "\nStatus: RO\n\n",
        1,
    );
    add("ann@example.org Mon Jun  3 10:00:00 2019", &welcome);
    let twice = msg(Some("twice@example.org"), "twice", "Same again.");
    add("ann@example.org Tue Jun  4 10:00:00 2019", &twice);
    add("ann@example.org Tue Jun  4 10:00:00 2019", &twice);
    let noid = msg(None, "no id either", "Nothing to go by.");
    add("ann@example.org Wed Jun  5 10:00:00 2019", &noid);
    add("ann@example.org Wed Jun  5 10:00:00 2019", &noid);
    let quoted = msg(
        Some("quoted@example.org"),
        "quoted",
        "From the start\n>From a quote",
    );
    add("ann@example.org Sat Feb  1 10:00:00 2020", &quoted);
    for m in [&welcome, &twice, &noid, &quoted] {
        originals.insert(m.as_bytes().to_vec());
    }
    let mbox_path = d.join("thunderbird-export/Inbox");
    write(&mbox_path, mbox.as_bytes());
    write(
        &d.join("thunderbird-export/Inbox.msf"),
        b"// <!-- <mdb:mork:z v=\"1.4\"/> -->",
    );
    Fixture {
        older,
        newer,
        mbox: mbox_path.parent().unwrap().to_path_buf(),
        originals,
    }
}

#[test]
fn snapshots_merge_into_one_maildir_every_message_once() {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let f = fixture(tmp.path());
    let dest = tmp.path().join("Archive/Mail/example.org/ann");
    let sources = vec![f.newer.clone(), f.mbox.clone(), f.older.clone()];
    let no = AtomicBool::new(false);

    // the dry run reads everything and writes nothing
    let dry = mailmerge::dry_run(&sources, &dest, &no, &mut |_, _, _| {}).unwrap();
    assert!(!dest.exists(), "a dry run writes nothing");
    assert!(dry.dry_run);
    // welcome, invoice, no id, trip, reply, old, new, twice, no id either, quoted
    assert_eq!(dry.distinct, 10, "{dry:#?}");
    assert_eq!(dry.messages, 10);
    assert_eq!(dry.without_id, 2);
    assert_eq!(dry.moved, 1, "trip: INBOX then, Archive now");
    assert_eq!(
        dry.differing_copies, 2,
        "invoice got a header on the server, welcome a Status line in the mbox"
    );
    // 6 + 5 + 6 copies read, 10 kept
    assert_eq!(dry.duplicates, 7);
    // oldest first: 2019's Maildir, the 2020 mbox, 2021's Maildir
    let order: Vec<&str> = dry.sources.iter().map(|s| s.kind.as_str()).collect();
    assert_eq!(order, ["Maildir", "mbox folder", "Maildir"]);
    assert!(dry.sources[0].path.ends_with("drive-2019/Maildir"));
    assert_eq!(dry.sources[0].folders_only_here, ["Old"]);
    assert_eq!(dry.sources[2].folders_only_here, ["Archive"]);
    assert_eq!(
        dry.sources[0].only_here, 2,
        "the note and the old folder's message"
    );
    assert_eq!(dry.sources[1].only_here, 3, "twice, no id either, quoted");
    assert_eq!(dry.sources[2].only_here, 1, "new");

    // the real thing, journaled
    let db = Db::open(&tmp.path().join("i.db")).unwrap();
    let id = mailmerge::record(&db, &sources, &dest, None).unwrap();
    let r = mailmerge::run(&db, id, &no, &mut |_, _, _| {}).unwrap();
    assert_eq!((r.written, r.failed, r.resumed), (10, 0, 0), "{r:#?}");
    let got = result(&dest);
    let folders: Vec<&str> = got.keys().map(|k| k.as_str()).collect();
    assert_eq!(folders, ["Archive", "INBOX", "Old", "Sent"]);
    assert_eq!(
        subjects(&got["INBOX"]),
        [
            "invoice",
            "new",
            "no id",
            "no id either",
            "quoted",
            "twice",
            "welcome"
        ]
        .into_iter()
        .map(String::from)
        .collect()
    );
    assert_eq!(subjects(&got["Archive"]), ["trip".to_string()].into());
    assert_eq!(subjects(&got["Old"]), ["old".to_string()].into());
    // flags: read anywhere, flagged anywhere
    assert_eq!(flags_of(&got["INBOX"], "welcome"), "FS");
    assert_eq!(flags_of(&got["INBOX"], "invoice"), "RS");
    assert_eq!(flags_of(&got["INBOX"], "twice"), "");
    // byte for byte a copy one of the snapshots had (the newest copy where they differ)
    for msgs in got.values() {
        for (name, bytes) in msgs {
            assert!(f.originals.contains(bytes), "{name} isn't a source's copy");
        }
    }
    let invoice = &got["INBOX"]
        .iter()
        .find(|(_, b)| String::from_utf8_lossy(b).contains("invoice"))
        .unwrap()
        .1;
    assert!(String::from_utf8_lossy(invoice).contains("X-UID: 17"));
    let quoted = &got["INBOX"]
        .iter()
        .find(|(_, b)| String::from_utf8_lossy(b).contains("quoted"))
        .unwrap()
        .1;
    assert!(
        String::from_utf8_lossy(quoted).contains("\nFrom the start\n>From a quote\n"),
        "mbox quoting undone once"
    );
    // subfolders are Maildir++ folders
    assert!(dest.join(".Archive/maildirfolder").exists());
    // delivery times kept as the files' dates
    let trip = walk(&dest.join(".Archive/cur"));
    assert!(file_name(&trip[0]).starts_with("1559300000."));
    let n: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM mail_merge_log WHERE merge_id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 10);
    let h = mailmerge::history(&db).unwrap();
    assert_eq!(h[0]["state"], "done");
    assert!(mailmerge::report_of(&db, id).unwrap().unwrap().written == 10);

    // the sources are as they were
    assert_eq!(walk(&f.older).len(), 6);
    assert_eq!(walk(&f.newer).len(), 5);

    // interrupted: a message lost from the destination is written again, the rest found there
    fs::remove_file(&trip[0]).unwrap();
    let r = mailmerge::run(&db, id, &no, &mut |_, _, _| {}).unwrap();
    assert_eq!((r.written, r.resumed), (1, 9), "{r:#?}");
    // and a new merge of the same into the same place finds everything there
    assert_eq!(mailmerge::unfinished(&db, &sources, &dest).unwrap(), None);
    let id2 = mailmerge::record(&db, &sources, &dest, None).unwrap();
    let r = mailmerge::run(&db, id2, &no, &mut |_, _, _| {}).unwrap();
    assert_eq!((r.written, r.resumed), (0, 10));

    // a stopped run is unfinished, and carries on
    let dest2 = tmp.path().join("Archive/Mail/stopped");
    let id3 = mailmerge::record(&db, &sources, &dest2, None).unwrap();
    assert!(mailmerge::run(&db, id3, &AtomicBool::new(true), &mut |_, _, _| {}).is_err());
    assert_eq!(
        mailmerge::unfinished(&db, &sources, &dest2).unwrap(),
        Some(id3)
    );
    let r = mailmerge::run(&db, id3, &no, &mut |_, _, _| {}).unwrap();
    assert_eq!(r.written, 10);

    // export INBOX as an mbox, and read it back through a merge: the same messages
    let out = tmp.path().join("exports/inbox.mbox");
    let e = mailmerge::export(Some(&db), &dest, &out, &mut |_, _| {}).unwrap();
    assert_eq!(e.messages, 7);
    assert_eq!(
        e.quoted_lines, 2,
        "\"From the start\" and \">From a quote\""
    );
    let text = fs::read_to_string(&out).unwrap();
    assert!(text.starts_with("From ann@example.org "));
    assert!(text.contains("\n>From the start\n>>From a quote\n"));
    assert!(
        mailmerge::export(None, &dest, &out, &mut |_, _| {}).is_err(),
        "never overwrites"
    );
    let back = tmp.path().join("roundtrip");
    let id4 = mailmerge::record(&db, std::slice::from_ref(&out), &back, None).unwrap();
    mailmerge::run(&db, id4, &no, &mut |_, _, _| {}).unwrap();
    let a: BTreeSet<Vec<u8>> = got["INBOX"].iter().map(|(_, b)| b.clone()).collect();
    let b: BTreeSet<Vec<u8>> = result(&back)
        .into_values()
        .flatten()
        .map(|(_, b)| b)
        .collect();
    assert_eq!(a, b, "an exported mbox reads back as the same messages");
}

fn file_name(p: &Path) -> String {
    p.file_name().unwrap().to_string_lossy().into_owned()
}

#[test]
fn stores_merge_mailbox_by_mailbox() {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let a = tmp.path().join("old-server/vmail");
    let b = tmp.path().join("backup-disk/vmail");
    let m1 = msg(Some("1@example.org"), "one", "1");
    let m2 = msg(Some("2@example.org"), "two", "2");
    let m3 = msg(Some("3@example.net"), "three", "3");
    put(
        &a.join("example.org/ann/Maildir"),
        "INBOX",
        1_500_000_000,
        1,
        "S",
        &m1,
    );
    put(
        &b.join("example.org/ann/Maildir"),
        "INBOX",
        1_500_000_000,
        1,
        "",
        &m1,
    );
    put(
        &b.join("example.org/ann/Maildir"),
        "INBOX",
        1_600_000_000,
        2,
        "",
        &m2,
    );
    put(
        &a.join("example.net/cy/Maildir"),
        "Sent",
        1_500_000_000,
        3,
        "S",
        &m3,
    );
    // the same message in another mailbox is another mailbox's message
    put(
        &b.join("example.net/cy/Maildir"),
        "INBOX",
        1_600_000_000,
        4,
        "",
        &m1,
    );
    let dest = tmp.path().join("Archive/Mail/server");
    let no = AtomicBool::new(false);
    let r = mailmerge::dry_run(&[a.clone(), b.clone()], &dest, &no, &mut |_, _, _| {}).unwrap();
    assert_eq!((r.mailboxes, r.distinct, r.duplicates), (2, 4, 1), "{r:#?}");
    let db = Db::open(&tmp.path().join("i.db")).unwrap();
    let id = mailmerge::record(&db, &[a.clone(), b.clone()], &dest, None).unwrap();
    mailmerge::run(&db, id, &no, &mut |_, _, _| {}).unwrap();
    let ann = result(&dest.join("example.org/ann/Maildir"));
    assert_eq!(subjects(&ann["INBOX"]), ["one".into(), "two".into()].into());
    assert_eq!(flags_of(&ann["INBOX"], "one"), "S");
    let cy = result(&dest.join("example.net/cy/Maildir"));
    assert_eq!(subjects(&cy["Sent"]), ["three".into()].into());
    assert_eq!(subjects(&cy["INBOX"]), ["one".into()].into());
    // stores and lone mailboxes don't mix
    let e = mailmerge::dry_run(
        &[a.clone(), a.join("example.org/ann/Maildir")],
        &tmp.path().join("x"),
        &no,
        &mut |_, _, _| {},
    )
    .unwrap_err();
    assert!(format!("{e:#}").contains("not both"), "{e:#}");
}

#[test]
fn never_into_a_source_nor_over_other_mail() {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let src = tmp.path().join("Maildir");
    put(
        &src,
        "INBOX",
        1_500_000_000,
        1,
        "S",
        &msg(Some("a@example.org"), "a", "a"),
    );
    let no = AtomicBool::new(false);
    let inside = src.join("merged");
    let e = mailmerge::dry_run(std::slice::from_ref(&src), &inside, &no, &mut |_, _, _| {})
        .unwrap_err();
    assert!(format!("{e:#}").contains("inside a source"), "{e:#}");
    let other = tmp.path().join("someone-elses");
    put(
        &other,
        "INBOX",
        1_500_000_000,
        9,
        "",
        &msg(Some("z@example.org"), "z", "z"),
    );
    let e =
        mailmerge::dry_run(std::slice::from_ref(&src), &other, &no, &mut |_, _, _| {}).unwrap_err();
    assert!(
        format!("{e:#}").contains("merge into a new folder"),
        "{e:#}"
    );
    let e = mailmerge::dry_run(
        &[tmp.path().join("nothing-here")],
        &tmp.path().join("d"),
        &no,
        &mut |_, _, _| {},
    );
    assert!(e.is_err());
    // a lone mbox file is a mailbox too, its name its folder ("mbox": INBOX)
    let mbox = tmp.path().join("mbox");
    write(
        &mbox,
        b"From x@example.org Mon Jan  1 00:00:00 2024\nMessage-ID: <m@example.org>\nStatus: O\n\nhi\n",
    );
    let r = mailmerge::dry_run(&[mbox], &tmp.path().join("new"), &no, &mut |_, _, _| {}).unwrap();
    assert_eq!(r.folders, [("INBOX".to_string(), 1)]);
    assert_eq!(SEEN, 1);
}
