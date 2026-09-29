//! End-to-end: names that differ only by case or Unicode form, across drives. The plan must not
//! let two drives' folders merge unseen on an archive drive that ignores case, nor take the
//! archive's own drive for a source because its path was typed in the other Unicode form.

use std::fs;
use std::path::Path;

use backdown_core::{Db, index, names, paths, plan, recognise};

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

#[test]
fn places_that_one_drive_takes_for_one() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("Backup");
    let b = dir.path().join("BACKUP");
    // the archive's drive, its name decomposed (a Mac drive read on Linux)
    let archive = dir.path().join("Archi\u{301}v");
    write(&a.join("Docs/a.txt"), b"from one drive");
    write(&b.join("Docs/b.txt"), b"from another");
    write(&archive.join("old/c.txt"), b"already filed");
    let mut db = Db::open(&dir.path().join("index.db")).unwrap();
    for d in [&a, &b, &archive] {
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
    let rec: Vec<_> = recognise::root_ids(&db)
        .unwrap()
        .into_iter()
        .map(|id| recognise::recognise(&db, id, true, &mut |_| {}).unwrap())
        .collect();
    // the archive folder typed composed
    let target = paths::canonical(dir.path())
        .unwrap()
        .join("Arch\u{ed}v/Filed");
    let p = plan::make(&db, &target, None, &plan::Schema::default(), &rec, &[]).unwrap();
    let drives: Vec<&str> = p.drives.iter().map(|d| d.drive.as_str()).collect();
    assert_eq!(
        drives,
        ["BACKUP", "Backup"],
        "the archive's drive isn't a source"
    );
    let mut dests: Vec<&str> = p.items.iter().map(|i| i.dest.as_str()).collect();
    dests.sort();
    // "Unsorted/Backup" takes the spelling "Unsorted/BACKUP" was given first, and the second
    // "Docs" there gets the drive's name rather than merging with the first
    assert_eq!(
        dests,
        ["Unsorted/BACKUP/Docs", "Unsorted/BACKUP/Docs (Backup)"]
    );
    // and they're told apart the same way on the archive's drive, whatever it is
    let key = |s: &str| names::Folding::ANY.key(std::ffi::OsStr::new(s));
    assert_ne!(key(dests[0]), key(dests[1]));
}
