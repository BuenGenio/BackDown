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
    let laptop = tmp.path().join("laptop");
    let usb = tmp.path().join("usb");
    let archive_drive = tmp.path().join("archive");
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
