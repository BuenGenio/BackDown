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
    let mut f = String::from("2 M1e C5c000000\n");
    for (i, r) in received.iter().enumerate() {
        let body = format!("Subject: {i}\n\nhello {i}");
        f += &format!(
            "\x01\x02N 00000000 {:016x}\n{body}\n\x01\x03\nGguid{i}\nR{r:x}\n\n",
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

// ---------- databases, and who and what things are about (subjects.rs)

fn fopt<'a>(e: &'a Entity, k: &str) -> Option<&'a str> {
    e.facts
        .iter()
        .find(|(key, _)| key == k)
        .map(|(_, v)| v.as_str())
}

fn sqlite_file(d: &Path, size: usize) {
    let mut b = b"SQLite format 3\0".to_vec();
    b.resize(size, 0);
    write(d, &b);
}

/// A drive with databases in every form, two sites, and a Nextcloud server with its data.
fn data_drive(d: &Path) {
    // three dumps in one folder: two nights of the shop (MariaDB), one of the wiki (pg_dumpall)
    for day in ["2021-03-04", "2021-03-05"] {
        write(
            &d.join(format!("backups/db/shop_{day}.sql")),
            format!(
                "-- MariaDB dump 10.19  Distrib 10.5.9-MariaDB, for Linux (x86_64)\n--\n-- Host: db1.example.org    Database: shop\n\
                 -- ------------------------------------------------------\n-- Server version\t10.5.9-MariaDB\n\n\
                 CREATE TABLE `orders` (`id` int NOT NULL);\nINSERT INTO `orders` VALUES (1),(2),(3);\n{}\n-- Dump completed on {day} 03:00:12\n",
                "-- filler\n".repeat(200)
            )
            .as_bytes(),
        );
    }
    write(
        &d.join("backups/db/wiki.sql"),
        format!(
            "--\n-- PostgreSQL database cluster dump\n--\n\n\\connect template1\n\n--\n-- Name: wiki; Type: DATABASE; Schema: -; Owner: wiki\n--\n\nCREATE DATABASE wiki;\n\\connect wiki\n{}",
            "SELECT 1;\n".repeat(200)
        )
        .as_bytes(),
    );
    // one phpMyAdmin export on its own
    write(
        &d.join("old/site.sql"),
        format!(
            "-- phpMyAdmin SQL Dump\n-- version 5.1.0\n-- https://www.phpmyadmin.net/\n--\n-- Host: localhost:3306\n-- Generation Time: Mar 04, 2021 at 10:22 AM\n-- Server version: 10.5.9-MariaDB\n\n--\n-- Database: `wp_prod`\n--\n{}",
            "-- filler\n".repeat(200)
        )
        .as_bytes(),
    );
    // mongodump: dump/<database>/<collection>.bson + .metadata.json
    for (db, coll) in [("app", "users"), ("app", "orders"), ("logs", "events")] {
        write(
            &d.join(format!("backups/mongo/dump/{db}/{coll}.bson")),
            &[7u8; 4096],
        );
        write(
            &d.join(format!("backups/mongo/dump/{db}/{coll}.metadata.json")),
            b"{\"indexes\":[]}",
        );
    }
    // a SQLite file, and a .db that isn't one (a program's own file)
    sqlite_file(&d.join("data/notes.sqlite"), 8192);
    write(&d.join("data/cache.db"), &[1u8; 5000]);
    // a MariaDB data directory: the system files, the mysql schema, and the shop
    let my = d.join("servers/db1/var/lib/mysql");
    write(&my.join("ibdata1"), &[0u8; 1024]);
    write(&my.join("aria_log_control"), &[0u8; 52]);
    write(&my.join("mysql/user.frm"), b"frm");
    write(&my.join("mysql/user.MYD"), b"myd");
    for t in ["orders", "customers"] {
        write(&my.join(format!("shop/{t}.frm")), b"frm");
        write(&my.join(format!("shop/{t}.ibd")), &[0u8; 2048]);
    }
    // a PostgreSQL data directory with two databases
    let pg = d.join("servers/db1/var/lib/postgresql/13/main");
    write(&pg.join("PG_VERSION"), b"13\n");
    write(&pg.join("base/1/1259"), b"x");
    write(&pg.join("base/13394/1259"), b"x");
    write(&pg.join("global/pg_control"), b"x");
    // a WordPress site using the shop database, and a Joomla site with a contact
    let wp = d.join("sites/shop.example.org");
    write(
        &wp.join("wp-config.php"),
        b"<?php\ndefine( 'DB_NAME', 'shop' );\ndefine( 'DB_USER', 'shop' );\ndefine( 'DB_PASSWORD', 'secret' );\ndefine( 'DB_HOST', 'db1.example.org' );\ndefine('WP_HOME','https://shop.example.org');\n$table_prefix = 'wp_';\n",
    );
    write(&wp.join("wp-content/index.php"), b"<?php");
    write(&wp.join("wp-admin/index.php"), b"<?php");
    let jo = d.join("sites/club");
    write(
        &jo.join("configuration.php"),
        b"<?php\nclass JConfig {\n\tpublic $sitename = 'The Club';\n\tpublic $dbtype = 'mysqli';\n\tpublic $host = 'localhost';\n\tpublic $db = 'club';\n\tpublic $dbprefix = 'j_';\n\tpublic $mailfrom = 'alice@example.org';\n\tpublic $fromname = 'Alice Example';\n}\n",
    );
    write(&jo.join("administrator/index.php"), b"<?php");
    // Nextcloud: the server's web root with its config, and the data directory with two users
    let nc = d.join("cloud/nextcloud");
    for sub in ["apps", "core"] {
        write(&nc.join(sub).join("x.php"), b"<?php");
    }
    write(
        &nc.join("config/config.php"),
        b"<?php\n$CONFIG = array (\n  'instanceid' => 'oc1',\n  'trusted_domains' => \n  array (\n    0 => 'cloud.example.org',\n  ),\n  'datadirectory' => '/var/www/data',\n  'dbtype' => 'pgsql',\n  'version' => '27.1.0.0',\n  'dbname' => 'nextcloud',\n  'dbhost' => 'localhost',\n  'mail_domain' => 'example.org',\n);\n",
    );
    write(&d.join("cloud/data/.ocdata"), b"");
    write(&d.join("cloud/data/alice/files/doc.txt"), b"hello");
    write(&d.join("cloud/data/bob/files/notes.txt"), b"hi");
    write(&d.join("cloud/data/appdata_oc1/preview/1.png"), b"png");
}

#[test]
fn databases_and_who_things_are_about() {
    use backdown_core::subjects;
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join("drive");
    drive(&d); // the mail store with alice, bob and carol, among the rest
    data_drive(&d);
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

    // ---- the things
    let dumps = find(es, "sql-dump", "backups/db");
    assert!(!dumps.is_file);
    assert_eq!(fopt(dumps, "Dumps"), Some("3"));
    let site = find(es, "sql-dump", "old/site.sql");
    assert_eq!(fopt(site, "Engine"), Some("MariaDB"), "{:?}", site.facts);
    assert_eq!(fopt(site, "Database"), Some("wp_prod"));
    assert_eq!(fopt(site, "Host"), Some("localhost"));
    assert_eq!(site.title, "wp_prod (MariaDB)");
    let mongo = find(es, "mongo-dump", "mongo/dump");
    assert_eq!(fopt(mongo, "Databases"), Some("app, logs"));
    assert_eq!(fopt(mongo, "Collections"), Some("3"));
    assert!(
        !es.iter()
            .any(|e| e.kind == "mongo-dump" && e.path.ends_with("dump/app")),
        "a database's folder isn't a thing of its own inside the dump"
    );
    let lite = find(es, "sqlite-db", "notes.sqlite");
    assert!(lite.is_file && lite.confidence >= 0.9);
    assert!(
        !es.iter().any(|e| e.path.ends_with("cache.db")),
        "a .db without SQLite's magic isn't one"
    );
    let my = find(es, "mysql-data", "lib/mysql");
    assert_eq!(fopt(my, "Engine"), Some("MariaDB"));
    assert_eq!(fopt(my, "Databases"), Some("shop"));
    assert_eq!(fopt(my, "Tables"), Some("2"));
    let pg = find(es, "postgres-data", "13/main");
    assert_eq!(fopt(pg, "Databases"), Some("2"));
    assert_eq!(fopt(pg, "Version"), Some("13"));
    let wp = find(es, "website", "shop.example.org");
    assert_eq!(fopt(wp, "Database"), Some("shop"), "{:?}", wp.facts);
    assert_eq!(fopt(wp, "Database host"), Some("db1.example.org"));
    assert_eq!(fopt(wp, "Address"), Some("shop.example.org"));
    assert_eq!(wp.title, "WordPress site shop.example.org");
    let jo = find(es, "website", "sites/club");
    assert_eq!(fopt(jo, "Database"), Some("club"));
    assert_eq!(fopt(jo, "Database engine"), Some("MySQL or MariaDB"));
    assert_eq!(fopt(jo, "Contact"), Some("alice@example.org"));
    assert_eq!(fopt(jo, "Contact name"), Some("Alice Example"));
    assert_eq!(fopt(jo, "Site name"), Some("The Club"));
    let ncs = find(es, "nextcloud-server", "cloud/nextcloud");
    assert_eq!(
        fopt(ncs, "Address"),
        Some("cloud.example.org"),
        "{:?}",
        ncs.facts
    );
    assert_eq!(fopt(ncs, "Database engine"), Some("PostgreSQL"));
    assert_eq!(fopt(ncs, "Database"), Some("nextcloud"));
    assert_eq!(fopt(ncs, "Mail domain"), Some("example.org"));
    assert_eq!(ncs.title, "Nextcloud at cloud.example.org");
    assert_eq!(
        fopt(find(es, "nextcloud", "cloud/data"), "Users"),
        Some("alice, bob")
    );

    // ---- who and what they're about
    let s = subjects::compute(&db, &r, true).unwrap();
    let get = |kind: &str, name: &str| {
        s.subjects
            .iter()
            .find(|x| x.kind == kind && x.name == name)
            .unwrap_or_else(|| {
                panic!(
                    "no {kind} {name}; got {:#?}",
                    s.subjects
                        .iter()
                        .map(|x| (&x.kind, &x.name, x.mentions.len()))
                        .collect::<Vec<_>>()
                )
            })
    };
    fn roles(x: &subjects::Subject) -> Vec<&str> {
        let mut r: Vec<&str> = x.mentions.iter().map(|m| m.role.as_str()).collect();
        r.sort();
        r
    }
    // Alice: a mailbox in the store, a Nextcloud account (whose address the server's mail domain
    // gives), and the Joomla site's contact, which also names her
    let alice = get("person", "Alice Example");
    // (the drive's other Nextcloud data directory is another instance's, appdata_oc123 where the
    // server here is oc1: its alice is a login nobody can place, and stays apart)
    assert_eq!(
        roles(alice),
        ["Mailbox", "Nextcloud account", "Website contact"],
        "{:#?}",
        alice.mentions
    );
    assert!(
        alice.aliases.iter().any(|a| a.kind == "login"
            && a.value == "alice"
            && a.scope.as_deref() == Some("cloud.example.org")),
        "{:?}",
        alice.aliases
    );
    let sfact = |x: &subjects::Subject, k: &str| {
        x.facts
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.clone())
    };
    assert_eq!(
        sfact(alice, "Email addresses").as_deref(),
        Some("alice@example.org")
    );
    assert!(
        sfact(alice, "Mailboxes").is_some_and(|v| v.starts_with("1, 4 messages")),
        "{:?}",
        alice.facts
    );
    let bob = get("person", "bob@example.org");
    assert_eq!(roles(bob), ["Mailbox", "Nextcloud account"]);
    // the other instance's accounts: a login with no known host never merges with anyone
    let unplaced: Vec<&subjects::Subject> = s
        .subjects
        .iter()
        .filter(|x| {
            x.mentions
                .iter()
                .any(|m| m.role == "Nextcloud account" && m.path.contains("nextcloud-data"))
        })
        .collect();
    assert_eq!(unplaced.len(), 2, "{unplaced:#?}");
    assert!(unplaced.iter().all(|x| x.mentions.len() == 1));
    assert_eq!(roles(get("person", "carol@example.net")), ["Mailbox"]);
    // the shop: two dumps, its live data, and the site that uses it, all one database
    let shop = get("database", "shop");
    // (the shared fixture has an older MySQL dump of the shop as well: three dumps in all)
    assert_eq!(
        roles(shop),
        ["Dump", "Dump", "Dump", "Live data", "Used by"],
        "{:#?}",
        shop.mentions
    );
    // MySQL and MariaDB both said it; "MySQL or MariaDB" (the site's config) says less and goes
    assert_eq!(sfact(shop, "Engine").as_deref(), Some("MariaDB, MySQL"));
    assert!(
        sfact(shop, "Dumps").is_some_and(|v| v.starts_with("3, ")),
        "{:?}",
        shop.facts
    );
    assert!(sfact(shop, "Used by").is_some_and(|v| v.contains("shop.example.org")));
    assert_eq!(roles(get("database", "wiki")), ["Dump"]);
    assert_eq!(roles(get("database", "wp_prod")), ["Dump"]);
    assert_eq!(roles(get("database", "club")), ["Used by"]);
    assert_eq!(roles(get("database", "nextcloud")), ["Used by"]);
    let app = get("database", "app");
    assert_eq!(roles(app), ["Dump"]);
    assert!(
        app.mentions[0].detail.starts_with("MongoDB, 2 collections"),
        "{}",
        app.mentions[0].detail
    );
    // a SQLite file goes by its folder and name: every app's `db.sqlite3` isn't one database
    assert_eq!(
        roles(get("database", "data/notes.sqlite")),
        ["Database file"]
    );

    // stored per root, read back merged
    subjects::store(&db, &s).unwrap();
    let (back, fresh) = subjects::load(&db, root).unwrap().unwrap();
    assert!(fresh);
    assert_eq!(back.subjects.len(), s.subjects.len());
    let all = subjects::all(&db).unwrap();
    assert!(all.stale.is_empty() && all.missing.is_empty());
    assert_eq!(all.subjects.len(), s.subjects.len());
}
