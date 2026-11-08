//! End-to-end: three drives, a plan to file two of them into an archive on the third, applied,
//! resumed, and proved.

use std::fs;
use std::path::Path;

use backdown_core::{Db, apply, hash, index, overlap, plan, recognise, report};

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn blob(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

fn mail_store(root: &Path, extra: bool) {
    for (i, (dom, user)) in [
        ("example.org", "ann"),
        ("example.org", "ben"),
        ("example.net", "cy"),
    ]
    .iter()
    .enumerate()
    {
        let mb = root.join(dom).join(user);
        for f in ["cur", "new", "tmp"] {
            fs::create_dir_all(mb.join(f)).unwrap();
        }
        for m in 0..10u8 {
            write(
                &mb.join(format!("cur/{m}.eml")),
                &blob(i as u8 * 16 + m, 2000),
            );
        }
    }
    // each copy has one message the other hasn't
    write(
        &root.join(format!("example.org/ann/cur/only-{extra}.eml")),
        &blob(if extra { 200 } else { 201 }, 2000),
    );
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

fn analyse(db: &Db) -> (Vec<recognise::RootEntities>, Vec<overlap::Overlap>) {
    let rec: Vec<_> = recognise::root_ids(db)
        .unwrap()
        .into_iter()
        .map(|id| recognise::recognise(db, id, true, &mut |_| {}).unwrap())
        .collect();
    let ov = overlap::overlaps(db, &rec, &mut |_| {}).unwrap();
    (rec, ov)
}

#[test]
fn plan_apply_resume_and_prove() {
    let tmp = tempfile::tempdir().unwrap();
    // the index canonicalises the scanned roots (on macOS /var → /private/var); build the drive
    // paths the same way so the plan recognises the archive's own drive and the path checks match
    let base = tmp.path().canonicalize().unwrap();
    let laptop = base.join("laptop");
    let usb = base.join("usb");
    let archive_drive = base.join("archive");
    let archive = archive_drive.join("Archive");

    mail_store(&laptop.join("srv/vmail"), false);
    mail_store(&usb.join("mail-backup/vmail"), true); // the same store, plus one message
    let repo = laptop.join("code/tool");
    write(&repo.join(".git/HEAD"), b"ref: refs/heads/main\n");
    write(&repo.join("main.rs"), &blob(90, 3000));
    // the repository is already in the archive
    write(
        &archive.join("Code/local/tool/.git/HEAD"),
        b"ref: refs/heads/main\n",
    );
    write(&archive.join("Code/local/tool/main.rs"), &blob(90, 3000));
    let ios = laptop.join("MobileSync/Backup/0000-AAAA");
    write(&ios.join("Manifest.db"), &blob(91, 4096));
    write(&ios.join("Status.plist"), b"<plist/>");
    write(
        &ios.join("Info.plist"),
        b"<plist><dict><key>Device Name</key><string>Test iPhone</string></dict></plist>",
    );
    write(&laptop.join("papers/thesis.pdf"), &blob(92, 5000));
    for i in 0..25u8 {
        write(
            &laptop.join(format!("Downloads/setup-{i}.exe")),
            &blob(100 + i, 1500),
        );
    }
    write(&laptop.join("readme.txt"), b"loose");

    let mut db = Db::open(&tmp.path().join("i.db")).unwrap();
    scan_all(&mut db, &[&laptop, &usb, &archive_drive]);
    let (rec, ov) = analyse(&db);
    let p = plan::make(&db, &archive, None, &plan::Schema::default(), &rec, &ov).unwrap();
    let item = |src: &str| {
        p.items
            .iter()
            .find(|i| i.source.ends_with(src))
            .unwrap_or_else(|| panic!("no item for …{src}: {:#?}", p.items))
    };

    assert!(
        !p.items.iter().any(|i| i.source.contains("/archive")),
        "the archive's own drive isn't a source"
    );
    let store = item("laptop/srv/vmail");
    assert_eq!(
        (store.action.as_str(), store.dest.as_str()),
        ("copy", "Mail/example.org"),
        "{store:#?}"
    );
    let twin = item("usb/mail-backup/vmail");
    assert_eq!(twin.action, "copy-unique", "{twin:#?}");
    assert_eq!(twin.dest, "Mail/example.org/Also from usb/vmail");
    assert!(twin.why.contains("the same as"), "{}", twin.why);
    let git = item("laptop/code/tool");
    assert_eq!(git.action, "skip");
    assert!(git.why.contains("already in the archive"), "{}", git.why);
    assert_eq!(
        item("0000-AAAA").dest,
        "Phones/Test iPhone/".to_string() + &git_year(&item("0000-AAAA").dest)
    );
    assert_eq!(item("laptop/papers").dest, "Unsorted/laptop/papers");
    let dl = item("laptop/Downloads");
    assert_eq!(dl.action, "skip");
    let loose = p
        .items
        .iter()
        .find(|i| i.only_files && i.drive == "laptop")
        .unwrap();
    assert_eq!(loose.dest, "Unsorted/laptop/(top level)");
    let lap = p.drives.iter().find(|d| d.drive == "laptop").unwrap();
    assert!(!lap.covered, "the skipped downloads aren't copied");

    let id = plan::save(&db, &p).unwrap();
    // a different file already where one copy should go: never overwritten
    write(
        &archive.join("Unsorted/laptop/papers/thesis.pdf"),
        b"someone else's thesis",
    );

    let dry = apply::apply(&mut db, id, true, &mut |_| {}).unwrap();
    assert_eq!(dry.copied, 0);
    assert!(dry.would_copy > 30 && dry.conflicts == 1, "{dry:?}");
    assert!(!archive.join("Mail").exists(), "a dry run writes nothing");

    let run = apply::apply(&mut db, id, false, &mut |_| {}).unwrap();
    assert_eq!(run.failed, 0, "{run:?}");
    assert_eq!(run.conflicts, 1);
    assert_eq!(
        fs::read(archive.join("Unsorted/laptop/papers/thesis.pdf")).unwrap(),
        b"someone else's thesis"
    );
    assert_eq!(
        run.duplicates, 30,
        "the usb store's 30 shared messages stay put: {run:?}"
    );
    assert_eq!(
        fs::read(
            archive.join("Mail/example.org/Also from usb/vmail/example.org/ann/cur/only-true.eml")
        )
        .unwrap(),
        blob(200, 2000)
    );
    assert!(
        !archive
            .join("Mail/example.org/Also from usb/vmail/example.org/ann/cur/3.eml")
            .exists(),
        "shared messages aren't copied twice"
    );
    assert_eq!(
        fs::read(archive.join("Mail/example.org/example.org/ann/cur/3.eml")).unwrap(),
        blob(3, 2000)
    );
    let src_mtime = fs::metadata(laptop.join("srv/vmail/example.org/ann/cur/3.eml"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(
        fs::metadata(archive.join("Mail/example.org/example.org/ann/cur/3.eml"))
            .unwrap()
            .modified()
            .unwrap(),
        src_mtime
    );
    assert!(
        !walk(&archive)
            .iter()
            .any(|p| p.contains("backdown-partial")),
        "no temporary files left"
    );

    // run again: everything is resumed from the journal, nothing copied twice
    let again = apply::apply(&mut db, id, false, &mut |_| {}).unwrap();
    assert_eq!(again.copied, 0, "{again:?}");
    assert_eq!(
        again.resumed,
        run.copied + run.already_there + run.duplicates
    );
    let summary = apply::journal_summary(&db, id).unwrap();
    assert!(
        summary
            .iter()
            .any(|(s, n, _)| s == "copied" && *n == run.copied),
        "{summary:?}"
    );

    // proof: index the archive; the usb drive is now provably safe to wipe, the laptop isn't (downloads)
    fs::remove_file(archive.join("Unsorted/laptop/papers/thesis.pdf")).unwrap();
    apply::apply(&mut db, id, false, &mut |_| {}).unwrap();
    scan_all(&mut db, &[&archive_drive]);
    assert!(
        report::check(&db, &usb).unwrap().safe,
        "every usb file has a verified copy in the archive"
    );
    let c = report::check(&db, &laptop).unwrap();
    assert!(!c.safe);
    assert_eq!(
        c.unique.files, 25,
        "only the skipped downloads are left: {:?}",
        c.unique_by_dir
    );
}

/// A mail server's store with real messages: `extra` adds one message, `read` marks all read.
fn real_store(root: &Path, extra: bool, read: bool) {
    for (dom, user) in [("example.org", "ann"), ("example.net", "cy")] {
        let mb = root.join(dom).join(user);
        for f in ["cur", "new", "tmp"] {
            fs::create_dir_all(mb.join(f)).unwrap();
        }
        let n = if extra && user == "ann" { 4 } else { 3 };
        for m in 0..n {
            write(
                &mb.join(format!(
                    "cur/{}.M{m}P1.mail.example.org:2,{}",
                    1_500_000_000 + m * 1000,
                    if read { "S" } else { "" }
                )),
                format!(
                    "From: {user}@{dom}\nMessage-ID: <{m}.{user}@{dom}>\nSubject: message {m}\n\n{}\n",
                    "text ".repeat(200 + m as usize)
                )
                .as_bytes(),
            );
        }
    }
}

#[test]
fn overlapping_mailboxes_are_merged_instead_of_copied_twice() {
    let tmp = tempfile::tempdir().unwrap();
    let (laptop, usb) = (tmp.path().join("laptop"), tmp.path().join("usb"));
    let archive_drive = tmp.path().join("archive");
    let archive = archive_drive.join("Archive");
    real_store(&laptop.join("srv/vmail"), false, true);
    real_store(&usb.join("backup/vmail"), true, false);
    fs::create_dir_all(&archive).unwrap();
    let mut db = Db::open(&tmp.path().join("i.db")).unwrap();
    scan_all(&mut db, &[&laptop, &usb, &archive_drive]);
    let (rec, ov) = analyse(&db);
    let mut p = plan::make(&db, &archive, None, &plan::Schema::default(), &rec, &ov).unwrap();
    let i = p
        .items
        .iter()
        .position(|x| x.source.ends_with("laptop/srv/vmail"))
        .unwrap();
    let u = p
        .items
        .iter()
        .position(|x| x.source.ends_with("usb/backup/vmail"))
        .unwrap();
    // the larger (usb, one more message) is filed, the laptop's is its twin
    assert_eq!(p.items[u].action, "copy", "{:#?}", p.items);
    assert_eq!(p.items[i].twin.as_deref(), Some(p.items[u].source.as_str()));
    assert_eq!(plan::merge_target(&p, i), Some(u));
    assert_eq!(
        plan::merge_target(&p, u),
        None,
        "the filed one merges the others in"
    );
    let id = plan::save(&db, &p).unwrap();
    let merged = plan::merge_mail(&db, id, i).unwrap();
    assert_eq!(merged.items[u].action, "merge");
    assert_eq!(merged.items[u].merge, vec![p.items[i].source.clone()]);
    assert_eq!(merged.items[i].action, "skip");
    assert!(merged.items[i].why.starts_with("merged by Message-ID"));
    p = plan::load(&db, id).unwrap().0;
    assert_eq!(p, merged, "saved");
    assert_eq!(
        plan::merge_all_mail(&mut p.clone()),
        0,
        "nothing more to merge"
    );

    let dry = apply::apply(&mut db, id, true, &mut |_| {}).unwrap();
    assert_eq!(dry.would_merge, 1);
    assert!(!archive.join("Mail").exists(), "a dry run writes nothing");
    let run = apply::apply(&mut db, id, false, &mut |_| {}).unwrap();
    assert_eq!((run.merged, run.failed), (7, 0), "{run:?}");
    let ann = archive.join(&p.items[u].dest).join("example.org/ann/cur");
    let mut names: Vec<String> = fs::read_dir(&ann)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names.len(), 4, "{names:?}");
    assert!(
        names[..3].iter().all(|n| n.ends_with(":2,S")),
        "read on the laptop: read in the merge ({names:?})"
    );
    assert!(names[3].ends_with(":2,"), "only on the usb stick, unread");
    let j = apply::journal_summary(&db, id).unwrap();
    assert!(j.iter().any(|(s, n, _)| s == "merged" && *n == 1), "{j:?}");
    // again: carried on, nothing written twice
    let again = apply::apply(&mut db, id, false, &mut |_| {}).unwrap();
    assert_eq!((again.merged, again.failed), (7, 0));
    // once the archive is indexed, both drives' mail is provably kept
    scan_all(&mut db, &[&archive_drive]);
    assert!(report::check(&db, &usb.join("backup/vmail")).unwrap().safe);
    assert!(report::check(&db, &laptop.join("srv/vmail")).unwrap().safe);
}

fn git_year(dest: &str) -> String {
    dest.rsplit('/').next().unwrap().to_string()
}

fn walk(p: &Path) -> Vec<String> {
    walkdir::WalkDir::new(p)
        .into_iter()
        .filter_map(Result::ok)
        .map(|e| e.path().display().to_string())
        .collect()
}

#[test]
fn mirrors_copy_keep_old_versions_and_notice_rot() {
    use backdown_core::mirror;
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap(); // canonicalise (macOS /var → /private/var)
    let src = base.join("archive/Mail");
    let dst = base.join("mirror-drive/Mail");
    write(&src.join("a/1.eml"), &blob(1, 3000));
    write(&src.join("b/2.eml"), &blob(2, 3000));
    let db = Db::open(&tmp.path().join("i.db")).unwrap();
    assert!(
        mirror::add(&db, &src, &src.join("inside"), 30).is_err(),
        "not inside itself"
    );
    let id = mirror::add(&db, &src, &dst, 30).unwrap();
    assert_eq!(
        mirror::due(&db).unwrap(),
        Vec::<i64>::new(),
        "the mirror drive isn't there yet"
    );

    let s = mirror::sync(&db, id, &mut |_, _| {}).unwrap();
    assert_eq!((s.copied, s.updated, s.unchanged, s.failed), (2, 0, 0, 0));
    assert_eq!(fs::read(dst.join("b/2.eml")).unwrap(), blob(2, 3000));
    assert_eq!(mirror::due(&db).unwrap(), vec![id], "never verified: due");
    let v = mirror::verify(&db, id, 100, &mut |_, _| {}).unwrap();
    assert_eq!((v.checked, v.ok), (2, 2));
    assert!(mirror::get(&db, id).unwrap().last_verify_ok.unwrap());
    assert!(mirror::due(&db).unwrap().is_empty());

    // a changed source file: the mirror's old copy is kept aside, never overwritten
    std::thread::sleep(std::time::Duration::from_millis(1100));
    write(&src.join("a/1.eml"), &blob(9, 3100));
    fs::remove_file(src.join("b/2.eml")).unwrap();
    let s = mirror::sync(&db, id, &mut |_, _| {}).unwrap();
    assert_eq!((s.copied, s.updated, s.extra), (1, 1, 1), "{s:?}");
    assert_eq!(fs::read(dst.join("a/1.eml")).unwrap(), blob(9, 3100));
    let kept: Vec<_> = walkdir::WalkDir::new(dst.join(mirror::VERSIONS))
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .collect();
    assert_eq!(kept.len(), 1);
    assert_eq!(fs::read(kept[0].path()).unwrap(), blob(1, 3000));
    assert!(
        dst.join("b/2.eml").exists(),
        "deleted from the source, kept on the mirror"
    );

    // bit rot on the mirror is noticed
    let mut bad = blob(9, 3100);
    bad[100] ^= 0xff;
    fs::write(dst.join("a/1.eml"), &bad).unwrap();
    let v = mirror::verify(&db, id, 100, &mut |_, _| {}).unwrap();
    assert_eq!(v.differs, 1, "{v:?}");
    assert!(!mirror::get(&db, id).unwrap().last_verify_ok.unwrap());
}
