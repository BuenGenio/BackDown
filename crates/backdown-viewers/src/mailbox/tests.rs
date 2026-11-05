use std::path::Path;

use anyhow::Result;
use backdown_core::mailmerge::letters;
use backdown_core::{Db, browse, paths};
use rusqlite::params;
use serde_json::{Value, json};

use crate::peek;

use super::*;

fn msg(key: i64, id: &str, refs: &[&str], subject: &str, date: i64) -> Head {
    Head {
        key,
        size: 100,
        id: (!id.is_empty()).then(|| hash_id(id)),
        refs: refs.iter().map(|r| hash_id(r)).collect(),
        subject: subject.into(),
        from: format!("Person {key} <p{key}@example.org>"),
        date: Some(date),
        flags: 0,
    }
}

/// Each thread as (message key, depth) pairs, checking that every message is in exactly one.
fn shape(msgs: &[Head]) -> Vec<Vec<(i64, u32)>> {
    let threads = thread(msgs);
    let mut seen: Vec<usize> = threads
        .iter()
        .flat_map(|t| t.items.iter().map(|it| it.msg))
        .collect();
    seen.sort();
    assert_eq!(seen, (0..msgs.len()).collect::<Vec<_>>());
    threads
        .iter()
        .map(|t| {
            t.items
                .iter()
                .map(|it| (msgs[it.msg].key, it.depth))
                .collect()
        })
        .collect()
}

const DAY: i64 = 86_400;

#[test]
fn references_and_in_reply_to() {
    let msgs = vec![
        msg(1, "a@example.org", &[], "Plans", 10 * DAY),
        msg(
            2,
            "b@example.org",
            &["a@example.org"],
            "Re: Plans",
            11 * DAY,
        ),
        // In-Reply-To only (no References)
        msg(
            3,
            "c@example.org",
            &["b@example.org"],
            "Re: Plans",
            12 * DAY,
        ),
        // References to the whole chain, answering b too; earlier than c
        msg(
            4,
            "d@example.org",
            &["a@example.org", "b@example.org"],
            "Re: Plans",
            11 * DAY + 5,
        ),
        // something else, newer
        msg(5, "e@example.org", &[], "Other", 20 * DAY),
    ];
    assert_eq!(
        shape(&msgs),
        vec![vec![(5, 0)], vec![(1, 0), (2, 1), (4, 2), (3, 2)]]
    );
}

#[test]
fn replies_arriving_before_their_parent() {
    // the reply is read first; its parent turns up later in the file
    let msgs = vec![
        msg(1, "b@example.org", &["a@example.org"], "Re: Q", 2 * DAY),
        msg(2, "a@example.org", &[], "Q", DAY),
    ];
    let t = thread(&msgs);
    assert_eq!(shape(&msgs), vec![vec![(2, 0), (1, 1)]]);
    assert_eq!(t[0].first_read, 0); // the thread is still named by what was read first
    assert!(!t[0].items[1].orphan);
}

#[test]
fn missing_parents() {
    let msgs = vec![
        // two answers to a message that isn't here: kept together, side by side
        msg(1, "b@example.org", &["gone@example.org"], "Re: Lost", DAY),
        msg(
            2,
            "c@example.org",
            &["gone@example.org"],
            "Re: Lost",
            2 * DAY,
        ),
        // one answer to another missing one: it simply starts its thread
        msg(
            3,
            "d@example.org",
            &["other@example.org"],
            "Re: Alone",
            3 * DAY,
        ),
        // a missing grandparent: the chain holds below it
        msg(
            4,
            "f@example.org",
            &["root@example.org", "e@example.org"],
            "Re: Deep",
            4 * DAY,
        ),
        msg(
            5,
            "g@example.org",
            &["root@example.org", "e@example.org", "f@example.org"],
            "Re: Deep",
            5 * DAY,
        ),
    ];
    assert_eq!(
        shape(&msgs),
        vec![vec![(4, 0), (5, 1)], vec![(3, 0)], vec![(1, 0), (2, 0)]]
    );
    let t = thread(&msgs);
    let orphans: Vec<bool> = t
        .iter()
        .flat_map(|t| t.items.iter().map(|i| i.orphan))
        .collect();
    assert_eq!(orphans, [true, false, true, true, true]);
}

#[test]
fn subject_fallback() {
    let msgs = vec![
        msg(1, "a@example.org", &[], "Budget 2024", 10 * DAY),
        // a reply whose client dropped the references
        msg(2, "b@example.org", &[], "AW: Budget 2024", 12 * DAY),
        msg(3, "c@example.org", &[], "SV: Re: budget  2024", 13 * DAY),
        // the same subject but not a reply: a conversation of its own
        msg(4, "d@example.org", &[], "Budget 2024", 14 * DAY),
        // a reply long after: not joined
        msg(5, "e@example.org", &[], "Re: Budget 2024", 200 * DAY),
        // a forward with a list tag, soon after the second conversation
        msg(6, "f@example.org", &[], "[team] Fwd: Budget 2024", 15 * DAY),
        // no subject is never a reason to join
        msg(7, "g@example.org", &[], "", 10 * DAY),
        msg(8, "h@example.org", &[], "Re:", 10 * DAY + 1),
    ];
    assert_eq!(
        shape(&msgs),
        vec![
            vec![(5, 0)],
            vec![(4, 0), (6, 1)],
            vec![(1, 0), (2, 1), (3, 1)],
            vec![(8, 0)],
            vec![(7, 0)],
        ]
    );
}

#[test]
fn replies_without_their_original_join_by_subject() {
    let msgs = vec![
        msg(1, "b@example.org", &[], "Re: Trip", DAY),
        msg(2, "c@example.org", &[], "Re: Trip", 2 * DAY),
    ];
    assert_eq!(shape(&msgs), vec![vec![(1, 0), (2, 0)]]);
}

#[test]
fn loops_are_refused() {
    let msgs = vec![
        // each claims to answer the other
        msg(1, "a@example.org", &["b@example.org"], "x", DAY),
        msg(2, "b@example.org", &["a@example.org"], "y", 2 * DAY),
        // a message that references itself, and a three-way loop in References
        msg(3, "c@example.org", &["c@example.org"], "z", 3 * DAY),
        msg(
            4,
            "d@example.org",
            &["e@example.org", "f@example.org", "e@example.org"],
            "w",
            4 * DAY,
        ),
        msg(5, "e@example.org", &["d@example.org"], "v", 5 * DAY),
    ];
    let s = shape(&msgs);
    assert_eq!(s.iter().map(Vec::len).sum::<usize>(), 5);
    assert!(s.contains(&vec![(3, 0)]));
    // a and b end up in one thread, one below the other
    assert!(s.contains(&vec![(1, 0), (2, 1)]) || s.contains(&vec![(2, 0), (1, 1)]));
}

#[test]
fn duplicates_stay_with_their_original() {
    let msgs = vec![
        msg(1, "a@example.org", &[], "Hello", DAY),
        msg(2, "b@example.org", &["a@example.org"], "Re: Hello", 2 * DAY),
        // the same message again (a copy in another folder, or saved twice)
        msg(3, "a@example.org", &[], "Hello", DAY),
        msg(4, "b@example.org", &["a@example.org"], "Re: Hello", 2 * DAY),
        // and no Message-ID at all
        msg(5, "", &[], "Hello there", 3 * DAY),
    ];
    assert_eq!(
        shape(&msgs),
        vec![vec![(5, 0)], vec![(1, 0), (3, 1), (2, 1), (4, 1)]]
    );
    let t = thread(&msgs);
    let dups: Vec<(i64, bool)> = t[1]
        .items
        .iter()
        .map(|it| (msgs[it.msg].key, it.dup))
        .collect();
    assert_eq!(dups, [(1, false), (3, true), (2, false), (4, true)]);
}

#[test]
fn deep_chains_and_many_messages() {
    // a long conversation, each message answering the one before
    let mut msgs = Vec::new();
    for i in 0..3000i64 {
        let refs: Vec<String> = (i.saturating_sub(10)..i)
            .map(|j| format!("m{j}@example.org"))
            .collect();
        let refs: Vec<&str> = refs.iter().map(String::as_str).collect();
        msgs.push(msg(
            i,
            &format!("m{i}@example.org"),
            &refs,
            "Re: long",
            DAY + i,
        ));
    }
    let t = thread(&msgs);
    assert_eq!(t.len(), 1);
    assert_eq!(t[0].items.len(), 3000);
    assert_eq!(t[0].items[2999].depth, 2999);
}

#[test]
fn subjects() {
    for (s, want, reply) in [
        ("Re: Plans", "Plans", true),
        ("RE: re: Fwd: Plans", "Plans", true),
        ("AW: Plans", "Plans", true),
        ("SV: Plans", "Plans", true),
        ("Re[2]: Plans", "Plans", true),
        ("Re(3): Plans", "Plans", true),
        ("RE : Plans", "Plans", true),
        ("回复：Plans", "Plans", true),
        ("Plans", "Plans", false),
        ("Re-organising", "Re-organising", false),
        ("Regarding: Plans", "Regarding: Plans", false),
        ("Re:", "", true),
    ] {
        assert_eq!(strip_subject(s, false), (want, reply), "{s}");
    }
    assert_eq!(strip_subject("[dev] Re: Plans", true), ("Plans", true));
    assert_eq!(
        strip_subject("[dev] Re: Plans", false),
        ("[dev] Re: Plans", false)
    );
    assert_eq!(strip_subject("[only a tag]", true), ("[only a tag]", false));
    assert_eq!(subject_key("Re:  Budget\t2024 "), "budget 2024");
}

#[test]
fn maildir_names() {
    let seen = KNOWN | SEEN;
    for (name, want) in [
        ("1698765432.M5P6Q7.host,S=1234:2,S", seen),
        (
            "1698765432.M5P6Q7.host,S=1234:2,FRS",
            seen | FLAGGED | REPLIED,
        ),
        ("1698765432.M5P6Q7.host:2,T", KNOWN | TRASHED),
        ("1698765432.M5P6Q7.host:2,DP", KNOWN | DRAFT | PASSED),
        ("1698765432.M5P6Q7.host:2,", KNOWN),
        ("1698765432.M5P6Q7.host", KNOWN),    // new/: not read yet
        ("1698765432.M5P6Q7.host!2,S", seen), // where ':' can't be in a name
        ("1698765432.M5P6Q7.host:2,Sab", seen), // keywords
        ("1698765432.M5P6Q7.host:1,S", KNOWN), // experimental info: no flags
    ] {
        assert_eq!(maildir_flags(name), want, "{name}");
    }
    assert!(unread(maildir_flags("1698765432.x.host")));
    assert!(!unread(maildir_flags("1698765432.x.host:2,RS")));
    assert!(!unread(0)); // an mbox without status headers: unknown, so not shown as unread
    assert_eq!(letters(maildir_flags("1.x:2,SRF")), "FRS");
    assert_eq!(delivered("1698765432.M5P6Q7.host:2,S"), Some(1_698_765_432));
    assert_eq!(delivered("notes.txt"), None);
}

#[test]
fn envelope_dates() {
    assert_eq!(
        envelope_date(b"From ann@example.org Mon Jan  1 12:00:00 2024\n"),
        Some(1_704_110_400)
    );
    assert_eq!(
        envelope_date(b"From 1234@xxx Wed Jan 03 14:35:03 +0000 2024\n"),
        Some(1_704_292_503)
    );
    assert_eq!(envelope_date(b"From - \n"), None);
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn eml(id: &str, reply_to: &[&str], from: &str, subject: &str, date: &str) -> String {
    let refs = if reply_to.is_empty() {
        String::new()
    } else {
        let ids: Vec<String> = reply_to.iter().map(|r| format!("<{r}>")).collect();
        format!(
            "In-Reply-To: {}\nReferences: {}\n",
            ids.last().unwrap(),
            ids.join(" ")
        )
    };
    format!(
        "From: {from}\nTo: team@example.org\nSubject: {subject}\nDate: {date}\nMessage-ID: <{id}>\n{refs}\nHello from {from}.\n"
    )
}

fn call(db: &Db, route: &str, args: &[(&str, &str)]) -> Result<Value> {
    let args: Vec<(String, String)> = args
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    api(db, route, &|name| {
        args.iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    })
}

fn keys(v: &Value) -> Vec<i64> {
    v["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["key"].as_i64().unwrap())
        .collect()
}

#[test]
fn indexed_mbox_and_maildir() {
    use backdown_core::index;
    let tmp = tempfile::tempdir().unwrap();
    let drive = tmp.path().join("drive");
    // Thunderbird's way: an mbox called "Inbox", no extension
    let a = eml(
        "a@example.org",
        &[],
        "Ann <ann@example.org>",
        "Plans",
        "Mon, 1 Jan 2024 10:00:00 +0000",
    );
    let b = eml(
        "b@example.org",
        &["a@example.org"],
        "Bob <bob@example.org>",
        "Re: Plans",
        "Tue, 2 Jan 2024 10:00:00 +0000",
    );
    let c = eml(
        "c@example.org",
        &["a@example.org", "b@example.org"],
        "Ann <ann@example.org>",
        "Re: Plans",
        "Wed, 3 Jan 2024 10:00:00 +0000",
    );
    let d = eml(
        "d@example.org",
        &[],
        "Cy <cy@example.org>",
        "Invoice 42",
        "Thu, 4 Jan 2024 10:00:00 +0000",
    );
    let mbox: String = [&a, &b, &d, &c]
        .iter()
        .map(|m| format!("From - Mon Jan  1 10:00:00 2024\n{m}\n"))
        .collect();
    write(&drive.join("Mail/Inbox"), &mbox);
    // a Maildir++: the inbox in cur/ and new/, a Sent folder, and what isn't mail
    let md = drive.join("Maildir");
    write(&md.join("cur/1704103200.M1P1.mx.example.org,S=300:2,S"), &a);
    write(
        &md.join("cur/1704189600.M2P2.mx.example.org,S=300:2,FRS"),
        &b,
    );
    write(&md.join("new/1704276000.M3P3.mx.example.org"), &c);
    write(&md.join("tmp/1704362400.M4P4.mx.example.org"), &d);
    write(&md.join("dovecot-uidlist"), "3 V1 N4\n");
    write(&md.join(".Sent/cur/1704190000.M5P5.mx.example.org:2,S"), &b);
    write(&md.join(".Sent/maildirfolder"), "");
    // Dovecot's "fs" layout: Maildirs side by side in a folder that isn't one
    write(
        &drive.join("mail/INBOX/cur/1704103200.M6P6.mx.example.org:2,S"),
        &a,
    );
    write(
        &drive.join("mail/INBOX/new/1704189600.M7P7.mx.example.org"),
        &b,
    );
    write(
        &drive.join("mail/Sent/cur/1704276000.M8P8.mx.example.org:2,S"),
        &c,
    );

    let mut db = Db::open(&tmp.path().join("idx.db")).unwrap();
    let opts = index::Options {
        one_file_system: true,
        allow_empty: false,
        allow_other_drive: false,
    };
    index::index(&mut db, &drive, &opts, &mut |_| {}).unwrap();
    let root = dunce::canonicalize(&drive).unwrap();
    let file_id = |p: &str| -> String {
        db.conn
            .query_row(
                "SELECT id FROM files WHERE path = ?1 AND current = 1",
                params![paths::to_bytes(&root.join(p))],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
            .to_string()
    };

    // the mbox: recognised without an extension, threaded, searchable
    let inbox = file_id("Mail/Inbox");
    let k = call(&db, "/api/mail-kind", &[("id", &inbox)]).unwrap();
    assert_eq!(k["kind"], "mbox");
    let v = call(&db, "/api/threads", &[("id", &inbox)]).unwrap();
    assert_eq!(
        (v["kind"].as_str(), v["done"].as_bool()),
        (Some("mbox"), Some(true))
    );
    assert_eq!(
        (v["messages"].as_u64(), v["total"].as_u64()),
        (Some(4), Some(2))
    );
    let t = &v["threads"][0]; // newest activity first: Invoice (4 Jan) before Plans (3 Jan)
    assert_eq!(
        (t["subject"].as_str(), t["count"].as_u64()),
        (Some("Invoice 42"), Some(1))
    );
    let t = &v["threads"][1];
    assert_eq!(
        (t["subject"].as_str(), t["count"].as_u64()),
        (Some("Plans"), Some(3))
    );
    assert_eq!(t["who"], json!(["Ann", "Bob"]));
    assert_eq!(
        (t["first"].as_i64(), t["last"].as_i64()),
        (Some(1_704_103_200), Some(1_704_276_000))
    );
    let key = t["key"].as_i64().unwrap().to_string();
    let th = call(
        &db,
        "/api/thread",
        &[("id", &inbox), ("key", &key), ("q", "bob")],
    )
    .unwrap();
    let depths: Vec<u64> = th["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["depth"].as_u64().unwrap())
        .collect();
    assert_eq!(depths, [0, 1, 2]);
    let hits: Vec<bool> = th["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["hit"].as_bool().unwrap())
        .collect();
    assert_eq!(hits, [false, true, false]);
    // each message's key is where it starts, for /api/mail
    let path = root.join("Mail/Inbox");
    for (at, subject) in keys(&th)
        .into_iter()
        .zip(["Plans", "Re: Plans", "Re: Plans"])
    {
        assert_eq!(
            peek::mail(&path, Some(at as u64)).unwrap()["subject"],
            subject
        );
    }
    let s = call(&db, "/api/threads", &[("id", &inbox), ("q", "INVOICE")]).unwrap();
    assert_eq!(s["total"], 1);
    let f = call(&db, "/api/threads", &[("id", &inbox), ("flat", "1")]).unwrap();
    assert_eq!(f["total"], 4);
    assert_eq!(f["threads"][1]["subject"], "Re: Plans");
    let p = call(
        &db,
        "/api/threads",
        &[("id", &inbox), ("offset", "1"), ("limit", "1")],
    )
    .unwrap();
    assert_eq!(
        (p["threads"].as_array().unwrap().len(), p["total"].as_u64()),
        (1, Some(2))
    );

    // the Maildir: cur/ and new/ (not tmp/), flags from the names, its Sent folder alongside
    let dir = browse::key(&root.join("Maildir"));
    let v = call(&db, "/api/threads", &[("dir", &dir)]).unwrap();
    assert_eq!(
        (
            v["kind"].as_str(),
            v["messages"].as_u64(),
            v["total"].as_u64()
        ),
        (Some("maildir"), Some(3), Some(1))
    );
    assert_eq!(v["threads"][0]["unread"], 1);
    assert_eq!(v["threads"][0]["flags"], "FR");
    let folders: Vec<(&str, u64, bool)> = v["folders"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["name"].as_str().unwrap(),
                f["count"].as_u64().unwrap(),
                f["here"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(folders, [("Inbox", 3, true), ("Sent", 1, false)]);
    let key = v["threads"][0]["key"].as_i64().unwrap().to_string();
    let th = call(&db, "/api/thread", &[("dir", &dir), ("key", &key)]).unwrap();
    let msgs = th["messages"].as_array().unwrap();
    let flags: Vec<(&str, bool)> = msgs
        .iter()
        .map(|m| (m["flags"].as_str().unwrap(), m["unread"].as_bool().unwrap()))
        .collect();
    assert_eq!(flags, [("S", false), ("FRS", false), ("", true)]);
    // messages are index ids: /api/mail reads them like an .eml
    let (p, _) = source(&db, msgs[0]["key"].as_i64().unwrap()).unwrap();
    assert_eq!(peek::mail(&p, None).unwrap()["subject"], "Plans");
    let one = file_id("Maildir/new/1704276000.M3P3.mx.example.org");
    assert_eq!(
        call(&db, "/api/mail-kind", &[("id", &one)]).unwrap()["kind"],
        "message"
    );
    // a Maildir++ sub-folder lists its family too, from the inbox down
    let sent = browse::key(&root.join("Maildir/.Sent"));
    let v = call(&db, "/api/threads", &[("dir", &sent)]).unwrap();
    assert_eq!(v["total"], 1);
    assert_eq!(v["folders"][1]["here"], true);
    assert_eq!(v["name"], "Maildir"); // the mailbox is named by its top folder
    let inbox = browse::key(&root.join("mail/INBOX"));
    let v = call(&db, "/api/threads", &[("dir", &inbox)]).unwrap();
    assert_eq!(
        (v["messages"].as_u64(), v["name"].as_str()),
        (Some(2), Some("mail"))
    );
    let names: Vec<(&str, u64, bool)> = v["folders"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["name"].as_str().unwrap(),
                f["new"].as_u64().unwrap(),
                f["here"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(names, [("INBOX", 1, true), ("Sent", 0, false)]);
    // anything else isn't a mailbox
    assert!(
        call(
            &db,
            "/api/threads",
            &[("dir", &browse::key(&root.join("Mail")))]
        )
        .is_err()
    );
    assert!(call(&db, "/api/threads", &[("dir", &browse::key(tmp.path()))]).is_err());
    let uid = file_id("Maildir/dovecot-uidlist");
    assert_eq!(
        call(&db, "/api/mail-kind", &[("id", &uid)]).unwrap()["kind"],
        Value::Null
    );
}

#[test]
fn what_mail_looks_like() {
    let mbox = b"From ann@example.org Mon Jan  1 12:00:00 2024\nFrom: ann@example.org\nSubject: hi\n\nbody\n";
    assert!(looks_mbox(mbox));
    assert!(!looks_mbox(
        b"From my point of view, at 12:30 we left.\nIt was late.\n"
    ));
    let eml = b"Return-Path: <ann@example.org>\r\nReceived: from mx.example.org\r\n\tby mx.example.com\r\nFrom: Ann <ann@example.org>\r\nSubject: hi\r\n\r\nbody\r\n";
    assert!(looks_message(eml, true));
    assert!(!looks_message(b"title: notes\nauthor: me\n\ntext\n", true));
    assert!(!looks_message(b"just some text\nFrom: x\n", true));
    assert!(!looks_message(b"", true));
}
