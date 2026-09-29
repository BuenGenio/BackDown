//! End-to-end: a drive with one of almost everything on it, indexed, then recognised.

use std::fs;
use std::path::Path;

use backdown_core::recognise::{self, Entity};
use backdown_core::{Db, index};

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn find<'a>(es: &'a [Entity], kind: &str, ends: &str) -> &'a Entity {
    es.iter()
        .find(|e| e.kind == kind && e.path.ends_with(ends))
        .unwrap_or_else(|| {
            panic!(
                "no {kind} at …{ends}; got {:#?}",
                es.iter().map(|e| (&e.kind, &e.path)).collect::<Vec<_>>()
            )
        })
}

fn drive(d: &Path) {
    // a mail server's store: two domains, three mailboxes, each with subfolders, and the script that copied it
    for (dom, user) in [
        ("example.org", "alice"),
        ("example.org", "bob"),
        ("example.net", "carol"),
    ] {
        let mb = d
            .join("backups/mail/vmail")
            .join(dom)
            .join(user)
            .join("Maildir");
        for sub in ["", ".Sent", ".Drafts"] {
            for f in ["cur", "new", "tmp"] {
                fs::create_dir_all(mb.join(sub).join(f)).unwrap();
            }
            write(
                &mb.join(sub)
                    .join("cur")
                    .join(format!("1500000000.M1P1.host:2,S{user}{sub}")),
                b"From: x\n\nhello",
            );
        }
        write(&mb.join("new/1600000000.M2P2.host"), b"From: y\n\nnew one");
        write(&mb.join("dovecot-uidlist"), b"3 V1 N2\n");
    }
    write(
        &d.join("backups/backup-mail.sh"),
        b"#!/bin/sh\nDEST=/mnt/usb/backups/mail\nrsync -aH --delete root@mail.example.org:/var/vmail/ $DEST/vmail/\n",
    );
    // a Time Machine set with two snapshots of one Mac
    for snap in ["2019-03-01-120000", "2020-06-30-083000"] {
        write(
            &d.join(format!(
                "Backups.backupdb/Alexs-MacBook/{snap}/Macintosh HD/Users/alex/notes.txt"
            )),
            snap.as_bytes(),
        );
        write(
            &d.join(format!(
                "Backups.backupdb/Alexs-MacBook/{snap}/Macintosh HD/Users/alex/Pictures/p{snap}.jpg"
            )),
            b"jpg",
        );
    }
    // an iPhone backup
    let ios = d.join("MobileSync/Backup/00008030-000A");
    write(&ios.join("Manifest.db"), b"SQLite format 3\0");
    write(&ios.join("Status.plist"), b"<plist/>");
    write(
        &ios.join("Info.plist"),
        b"<plist><dict><key>Device Name</key><string>Alex's iPhone</string><key>Product Type</key><string>iPhone12,1</string><key>Last Backup Date</key><date>2021-02-03T04:05:06Z</date></dict></plist>",
    );
    write(&ios.join("ab/ab1234"), b"blob");
    // a Linux system image with a home inside
    let sys = d.join("images/old-server-root");
    for dir in ["bin", "boot", "usr/lib", "var/log", "srv", "opt"] {
        write(&sys.join(dir).join("x"), b"x");
    }
    write(&sys.join("etc/hostname"), b"webhost\n");
    write(
        &sys.join("etc/os-release"),
        b"NAME=Debian\nPRETTY_NAME=\"Debian GNU/Linux 11 (bullseye)\"\n",
    );
    let home = sys.join("home/dana");
    write(&home.join(".bashrc"), b"alias ll='ls -l'");
    write(&home.join(".profile"), b"");
    write(&home.join(".config/app.conf"), b"x");
    write(&home.join(".ssh/known_hosts"), b"x");
    // a git repository inside the home, with a remote; and node_modules junk inside it
    let repo = home.join("src/website");
    write(&repo.join(".git/HEAD"), b"ref: refs/heads/main\n");
    write(&repo.join(".git/config"), b"[core]\n\tbare = false\n[remote \"origin\"]\n\turl = git@git.example.org:dana/website.git\n");
    write(&repo.join("index.html"), b"<html>");
    write(
        &repo.join("node_modules/left-pad/index.js"),
        b"module.exports = 1;",
    );
    // a Nextcloud data directory, with previews (junk)
    let nc = d.join("nextcloud-data");
    write(&nc.join(".ocdata"), b"");
    write(&nc.join("alice/files/Documents/a.odt"), b"odt");
    write(&nc.join("bob/files/b.txt"), b"b");
    write(
        &nc.join("appdata_oc123/preview/1/2/3.png"),
        b"preview bytes",
    );
    // a disk image rescued with ddrescue, and database dumps
    write(&d.join("rescue/sdf.img"), &vec![0u8; 1 << 20]);
    write(&d.join("rescue/sdf.log"), b"# Rescue Logfile. Created by GNU ddrescue version 1.25\n# Command line: ddrescue -d -r3 /dev/sdf sdf.img sdf.log\n");
    write(
        &d.join("dumps/shop.sql"),
        format!(
            "-- MySQL dump 10.13  Distrib 8.0\n--\n-- Host: db.example.org    Database: shop\n{}",
            "x".repeat(2000)
        )
        .as_bytes(),
    );
    // a photo library big enough to count, and a camera card dump
    for i in 0..220 {
        write(
            &d.join(format!("Photos/2018/IMG_{i:04}.JPG")),
            &vec![i as u8; 60 << 10],
        );
    }
    write(&d.join("camera-card/DCIM/100CANON/IMG_0001.CR2"), b"raw");
    // icons are not a photo library, however many there are
    for i in 0..300 {
        write(
            &d.join(format!("share/icons/hicolor/icon{i}.png")),
            b"small png",
        );
    }
    // a Google Takeout and chkdsk fragments
    write(&d.join("exports/Takeout/archive_browser.html"), b"<html>");
    write(&d.join("exports/Takeout/Drive/doc.pdf"), b"%PDF");
    write(&d.join("FOUND.000/FILE0000.CHK"), b"frag");
}

#[test]
fn recognises_what_things_are() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join("drive");
    drive(&d);
    let mut db = Db::open(&tmp.path().join("i.db")).unwrap();
    index::index(
        &mut db,
        &d,
        &index::Options {
            one_file_system: true,
            allow_empty: false,
        },
        &mut |_| {},
    )
    .unwrap();
    let root = recognise::root_ids(&db).unwrap()[0];
    let r = recognise::recognise(&db, root, true, &mut |_| {}).unwrap();
    let es = &r.entities;

    let store = find(es, "mail-store", "vmail");
    assert!(
        store.title.contains("3 mailboxes") && store.title.contains("2 domains"),
        "{}",
        store.title
    );
    assert!(
        store
            .facts
            .iter()
            .any(|(k, v)| k == "Messages" && v == "12"),
        "{:?}",
        store.facts
    );
    assert!(
        store
            .provenance
            .iter()
            .any(|p| p.contains("root@mail.example.org:/var/vmail/")
                && p.contains("rsync")
                && p.contains("backup-mail.sh")
                && p.contains("last on or after ")),
        "{:?}",
        store.provenance
    );
    assert!(
        store
            .facts
            .iter()
            .any(|(k, v)| k == "Source host" && v == "mail.example.org")
    );
    assert!(
        !es.iter().any(|e| e.kind == "maildir"),
        "mailboxes belong to the store"
    );

    let tm = find(es, "timemachine", "Backups.backupdb");
    assert!(
        tm.title.contains("2 snapshots") && tm.title.contains("Alexs-MacBook"),
        "{}",
        tm.title
    );
    assert!(
        tm.facts
            .iter()
            .any(|(k, v)| k == "Span" && v == "2019-03-01 – 2020-06-30")
    );
    assert!(
        tm.provenance.contains(
            &"Backed up by Time Machine from Alexs-MacBook, last on 30 Jun 2020.".to_string()
        ),
        "{:?}",
        tm.provenance
    );
    assert!(
        !es.iter()
            .any(|e| e.kind == "photos" && e.path.contains("Backups.backupdb")),
        "nothing inside a Time Machine set is listed separately"
    );

    let ios = find(es, "ios-backup", "00008030-000A");
    assert_eq!(ios.title, "backup of Alex's iPhone");
    assert!(
        ios.facts
            .iter()
            .any(|(k, v)| k == "Last backup" && v == "3 Feb 2021")
    );
    assert!(
        ios.provenance
            .iter()
            .any(|p| p.starts_with("Backed up from Alex's iPhone") && p.ends_with("on 3 Feb 2021.")),
        "{:?}",
        ios.provenance
    );

    let sys = find(es, "system", "old-server-root");
    assert_eq!(sys.title, "Debian GNU/Linux 11 (bullseye) (webhost)");
    let home = find(es, "home-linux", "dana");
    assert_eq!(home.inside.as_deref(), Some(sys.key.as_str()));
    let git = find(es, "git", "website");
    assert!(
        git.provenance
            .iter()
            .any(|p| p.contains("git@git.example.org:dana/website.git"))
    );
    assert!(git.facts.iter().any(|(k, v)| k == "Branch" && v == "main"));
    assert!(
        git.junk
            .iter()
            .any(|j| j.path.ends_with("node_modules") && j.why.contains("npm")),
        "{:?}",
        git.junk
    );

    let nc = find(es, "nextcloud", "nextcloud-data");
    assert_eq!(nc.title, "2 users");
    assert!(
        nc.junk
            .iter()
            .any(|j| j.why == "Nextcloud previews" && j.bytes == 13),
        "{:?}",
        nc.junk
    );

    let img = find(es, "disk-image", "sdf.img");
    assert!(img.is_file);
    assert!(
        img.provenance
            .iter()
            .any(|p| p.contains("/dev/sdf") && p.contains("ddrescue")),
        "{:?}",
        img.provenance
    );
    let dump = find(es, "sql-dump", "shop.sql");
    assert_eq!(dump.title, "shop (MySQL)");
    assert!(dump.provenance.iter().any(|p| p.contains("db.example.org")));

    let photos = find(es, "photos", "Photos");
    assert_eq!(photos.title, "220 photos");
    find(es, "photos", "DCIM");
    assert!(
        !es.iter().any(|e| e.path.contains("icons")),
        "icons aren't photos"
    );
    find(es, "export", "Takeout");
    let rec = find(es, "recovery", "FOUND.000");
    assert!(rec.low_value);

    // stored and read back, fresh for the latest scan
    recognise::store(&db, &r).unwrap();
    let (back, fresh) = recognise::load(&db, root).unwrap().unwrap();
    assert!(fresh);
    assert_eq!(back.entities, r.entities);
    // a new scan makes it stale
    write(&d.join("new-file.txt"), b"x");
    index::index(
        &mut db,
        &d,
        &index::Options {
            one_file_system: true,
            allow_empty: false,
        },
        &mut |_| {},
    )
    .unwrap();
    assert!(!recognise::load(&db, root).unwrap().unwrap().1);
}

#[test]
fn without_the_drive_it_still_recognises_but_says_less() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join("drive");
    drive(&d);
    let mut db = Db::open(&tmp.path().join("i.db")).unwrap();
    index::index(
        &mut db,
        &d,
        &index::Options {
            one_file_system: true,
            allow_empty: false,
        },
        &mut |_| {},
    )
    .unwrap();
    fs::rename(&d, tmp.path().join("unplugged")).unwrap();
    let root = recognise::root_ids(&db).unwrap()[0];
    let r = recognise::recognise(&db, root, true, &mut |_| {}).unwrap();
    assert!(!r.read_drive);
    let store = find(&r.entities, "mail-store", "vmail");
    assert!(store.provenance.is_empty());
    assert_eq!(
        find(&r.entities, "ios-backup", "00008030-000A").title,
        "device backup"
    );
}

/// A Dovecot index header saying how many messages a mailbox holds.
fn dovecot_index(n: u32) -> Vec<u8> {
    let mut h = vec![0u8; 120];
    h[0] = 7; // major version
    h[1] = 3;
    h[2] = 120; // base header size
    h[12] = 1; // little-endian
    h[32..36].copy_from_slice(&n.to_le_bytes());
    h
}

/// A dbox file of messages received at the given times.
fn dbox_file(received: &[u32]) -> Vec<u8> {
    let mut f = String::from("2 M15 C5c000000\n");
    for (i, r) in received.iter().enumerate() {
        let body = format!("Subject: {i}\n\nhello {i}");
        f += &format!(
            "\x01\x02N {:016x}\n{body}\n\x01\x03\nGguid{i}\nR{r:x}\n\n",
            body.len()
        );
    }
    f.into_bytes()
}

fn mail_drive(d: &Path) {
    // mdbox: messages in storage/m.N, three mailboxes (one nested)
    let md = d.join("vmail/ann/mdbox");
    write(
        &md.join("storage/m.1"),
        &dbox_file(&[1_400_000_000, 1_400_100_000]),
    );
    write(
        &md.join("storage/m.2"),
        &dbox_file(&[1_500_000_000, 1_600_000_000]),
    );
    write(&md.join("storage/dovecot.map.index"), &dovecot_index(4));
    for (b, n) in [("INBOX", 2), ("Sent", 1), ("Archive/2019", 1)] {
        write(
            &md.join("mailboxes")
                .join(b)
                .join("dbox-Mails/dovecot.index"),
            &dovecot_index(n),
        );
    }
    // sdbox: a file per message
    let sd = d.join("vmail/bob/sdbox");
    for (b, n) in [("INBOX", 3), ("Sent", 1)] {
        for i in 1..=n {
            write(
                &sd.join("mailboxes")
                    .join(b)
                    .join(format!("dbox-Mails/u.{i}")),
                &dbox_file(&[1_450_000_000]),
            );
        }
    }
    // a Thunderbird profile: an IMAP account (a cache of server mail), a POP account and Local
    // Folders (kept only here)
    let tb = d.join("thunderbird/abcd1234.default");
    write(
        &tb.join("prefs.js"),
        br#"user_pref("mail.accountmanager.accounts", "account1,account2,account3");
user_pref("mail.account.account1.identities", "id1");
user_pref("mail.account.account1.server", "server1");
user_pref("mail.account.account2.identities", "id2");
user_pref("mail.account.account2.server", "server2");
user_pref("mail.account.account3.server", "server3");
user_pref("mail.identity.id1.fullName", "Ann Example");
user_pref("mail.identity.id1.useremail", "ann@example.org");
user_pref("mail.identity.id2.useremail", "bob@example.net");
user_pref("mail.server.server1.directory-rel", "[ProfD]ImapMail/imap.example.org");
user_pref("mail.server.server1.hostname", "imap.example.org");
user_pref("mail.server.server1.name", "ann@example.org");
user_pref("mail.server.server1.type", "imap");
user_pref("mail.server.server1.userName", "ann");
user_pref("mail.server.server2.directory-rel", "[ProfD]Mail/pop.example.net");
user_pref("mail.server.server2.hostname", "pop.example.net");
user_pref("mail.server.server2.name", "Bob at work");
user_pref("mail.server.server2.type", "pop3");
user_pref("mail.server.server2.userName", "bob");
user_pref("mail.server.server3.directory-rel", "[ProfD]Mail/Local Folders");
user_pref("mail.server.server3.hostname", "Local Folders");
user_pref("mail.server.server3.name", "Local Folders");
user_pref("mail.server.server3.type", "none");
"#,
    );
    let mbox = |n: usize| {
        (0..n)
            .map(|i| format!("From - Mon Jan  1 00:00:00 2024\nSubject: {i}\n\nbody {i}\n\n"))
            .collect::<String>()
    };
    for (f, n) in [
        ("ImapMail/imap.example.org/INBOX", 5),
        ("ImapMail/imap.example.org/Archives.sbd/2019", 2),
        ("Mail/pop.example.net/Inbox", 3),
        ("Mail/Local Folders/Unsent Messages", 1),
        ("Mail/Local Folders/Trash", 1),
    ] {
        write(&tb.join(f), mbox(n).as_bytes());
        write(
            &tb.join(format!("{f}.msf")),
            b"// <!-- <mdb:mork:z v=\"1.4\"/> -->",
        );
    }
    write(
        &tb.join("ImapMail/imap.example.org/msgFilterRules.dat"),
        b"version=\"9\"",
    );
}

fn fact<'a>(e: &'a Entity, k: &str) -> &'a str {
    e.facts
        .iter()
        .find(|(x, _)| x == k)
        .map(|(_, v)| v.as_str())
        .unwrap_or_else(|| panic!("no {k} in {:#?}", e.facts))
}

#[test]
fn mail_stores_in_depth() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join("drive");
    mail_drive(&d);
    let mut db = Db::open(&tmp.path().join("i.db")).unwrap();
    index::index(
        &mut db,
        &d,
        &index::Options {
            one_file_system: true,
            allow_empty: false,
        },
        &mut |_| {},
    )
    .unwrap();
    let root = recognise::root_ids(&db).unwrap()[0];
    let r = recognise::recognise(&db, root, true, &mut |_| {}).unwrap();
    let es = &r.entities;

    let md = find(es, "mdbox", "ann/mdbox");
    assert_eq!(fact(md, "Format"), "mdbox");
    assert_eq!(fact(md, "Mailboxes"), "3");
    assert_eq!(fact(md, "Mailbox names"), "Archive/2019, INBOX, Sent");
    assert_eq!(
        fact(md, "Messages"),
        "4",
        "from the mailboxes' index headers"
    );
    assert_eq!(fact(md, "Oldest message"), "13 May 2014");
    assert_eq!(fact(md, "Newest message"), "13 Sep 2020");
    assert_eq!(
        md.title,
        "Dovecot mdbox: 3 mailboxes, 4 messages (2014–2020)"
    );

    let sd = find(es, "mdbox", "bob/sdbox");
    assert_eq!(fact(sd, "Format"), "sdbox");
    assert_eq!(fact(sd, "Messages"), "4", "a file per message");
    assert_eq!(fact(sd, "Mailboxes"), "2");

    let tb = find(es, "thunderbird", "abcd1234.default");
    assert_eq!(tb.title, "mail of ann@example.org, bob@example.net");
    assert_eq!(
        fact(tb, "Account names"),
        "ann@example.org, Bob at work, Local Folders"
    );
    assert_eq!(
        fact(tb, "Email addresses"),
        "Ann Example <ann@example.org>, bob@example.net"
    );
    assert_eq!(
        fact(tb, "Servers"),
        "imap.example.org (IMAP, as ann), pop.example.net (POP3, as bob)"
    );
    assert_eq!(fact(tb, "Main domain"), "example.org");
    assert!(fact(tb, "Copies of server mail (IMAP)").starts_with("ann@example.org ("));
    let local = fact(tb, "Kept here (Local Folders, POP)");
    assert!(
        local.contains("Local Folders (") && local.contains("Bob at work ("),
        "{local}"
    );
    let folders = fact(tb, "Folders in ann@example.org");
    assert!(
        folders.starts_with("INBOX (") && folders.contains("Archives/2019 ("),
        "which mbox file is which folder: {folders}"
    );
    assert!(!folders.contains("msgFilterRules"));
    assert!(fact(tb, "Folders in Local Folders").contains("Unsent Messages"));
    let (rec, ov) = {
        let o =
            backdown_core::overlap::overlaps(&db, std::slice::from_ref(&r), &mut |_| {}).unwrap();
        (vec![r.clone()], o)
    };
    let p = backdown_core::plan::make(
        &db,
        &tmp.path().join("Archive"),
        None,
        &backdown_core::plan::Schema::default(),
        &rec,
        &ov,
    )
    .unwrap();
    assert!(
        p.items
            .iter()
            .any(|i| i.kind == "thunderbird" && i.dest == "Mail/example.org/Thunderbird"),
        "filed by the domain its mail is for: {:#?}",
        p.items
    );

    // without the drive: what the index alone says
    fs::rename(&d, tmp.path().join("unplugged")).unwrap();
    let r = recognise::recognise(&db, root, true, &mut |_| {}).unwrap();
    let md = find(&r.entities, "mdbox", "ann/mdbox");
    assert_eq!(fact(md, "Mailboxes"), "3");
    assert!(!md.facts.iter().any(|(k, _)| k == "Messages"));
    assert_eq!(md.title, "Dovecot mdbox: 3 mailboxes");
    let tb = find(&r.entities, "thunderbird", "abcd1234.default");
    assert!(fact(tb, "Folders in imap.example.org").contains("Archives/2019"));
    assert!(!tb.facts.iter().any(|(k, _)| k == "Email addresses"));
}

fn blob(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

#[test]
fn overlaps_and_the_report() {
    use backdown_core::{hash, overlap, summary};
    let tmp = tempfile::tempdir().unwrap();
    let laptop = tmp.path().join("old-laptop");
    let archive = tmp.path().join("archive");
    // laptop: a mail store (fully archived), photos (half archived), and papers (nowhere else)
    for (i, (dom, user)) in [("example.org", "ann"), ("example.net", "ben")]
        .iter()
        .enumerate()
    {
        for f in ["cur", "new", "tmp"] {
            fs::create_dir_all(laptop.join("mail").join(dom).join(user).join(f)).unwrap();
        }
        let msg = blob(i as u8, 5000);
        write(
            &laptop.join("mail").join(dom).join(user).join("cur/1.eml"),
            &msg,
        );
        write(
            &archive.join("Mail").join(dom).join(user).join("cur/1.eml"),
            &msg,
        );
    }
    for i in 0..4u8 {
        let p = blob(40 + i, 10_000);
        write(&laptop.join(format!("photos/p{i}.jpg")), &p);
        if i < 2 {
            write(&archive.join(format!("Pictures/p{i}.jpg")), &p);
        }
    }
    write(&laptop.join("papers/thesis.pdf"), &blob(99, 7000));
    let mut db = Db::open(&tmp.path().join("i.db")).unwrap();
    for d in [&laptop, &archive] {
        index::index(
            &mut db,
            d,
            &index::Options {
                one_file_system: true,
                allow_empty: false,
            },
            &mut |_| {},
        )
        .unwrap();
    }
    hash::hash(
        &mut db,
        &hash::Options {
            jobs_per_device: 1,
            retry_errors: false,
        },
        &mut |_| {},
    )
    .unwrap();
    let mut rec = Vec::new();
    for id in recognise::root_ids(&db).unwrap() {
        let r = recognise::recognise(&db, id, true, &mut |_| {}).unwrap();
        recognise::store(&db, &r).unwrap();
        rec.push(r);
    }
    let ov = overlap::overlaps(&db, &rec, &mut |_| {}).unwrap();
    let get = |level: &str, ends: &str| {
        ov.iter()
            .find(|o| o.unit.level == level && o.unit.path.ends_with(ends))
            .unwrap_or_else(|| panic!("no {level} …{ends}"))
    };

    let drive = get("drive", "old-laptop");
    assert_eq!(drive.unit.bytes, 2 * 5000 + 4 * 10_000 + 7000);
    assert_eq!(
        drive.elsewhere,
        2 * 5000 + 2 * 10_000,
        "mail and two photos are archived"
    );
    assert!(drive.containers[0].path.ends_with("archive"));
    assert_eq!(get("folder", "old-laptop/mail").share(), 1.0);
    assert_eq!(get("folder", "old-laptop/photos").share(), 0.5);
    assert_eq!(get("folder", "old-laptop/papers").elsewhere, 0);
    let store = get("entity", "old-laptop/mail");
    assert_eq!(store.elsewhere, 10_000);
    assert!(
        store.containers[0].path.ends_with("archive/Mail"),
        "the most specific place: {:?}",
        store.containers
    );
    // the archive's copies all exist on the laptop too
    assert_eq!(get("drive", "archive").share(), 1.0);

    let fresh: Vec<_> = recognise::root_ids(&db)
        .unwrap()
        .into_iter()
        .map(|id| recognise::load(&db, id).unwrap().unwrap())
        .collect();
    let s = summary::build(&db, &fresh, &ov, &mut |_| {}).unwrap();
    let laptop_s = s.drives.iter().find(|d| d.name == "old-laptop").unwrap();
    assert!(!laptop_s.verdict.safe);
    assert_eq!(laptop_s.verdict.unique_bytes, 2 * 10_000 + 7000);
    assert!(
        s.drives
            .iter()
            .find(|d| d.name == "archive")
            .unwrap()
            .verdict
            .safe
    );
    let md = summary::markdown(&s);
    assert!(md.contains("| Safe to wipe |"), "{md}");
    assert!(
        md.contains("Copy 27 KB (3 files) somewhere else before wiping"),
        "{md}"
    );
    assert!(
        md.contains("**Mail server store**: 2 mailboxes in 2 domains"),
        "{md}"
    );
    let html = summary::html(&s);
    assert!(
        html.contains("<h2>old-laptop</h2>") && !html.contains("<script"),
        "report is static HTML"
    );
}
