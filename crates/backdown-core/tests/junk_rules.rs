//! Folder names added in Settings count as regenerable from the next recognition on, beside the
//! built-in rules, and stop counting when they're taken out again. Names match whatever their case,
//! and the folder found keeps its real name (a mixed-case one used to be looked up lowercased, and
//! missed, on case-sensitive filesystems).

use std::fs;
use std::path::Path;

use backdown_core::recognise::{self, EXTRA_JUNK_WHY, JUNK_NAMES};
use backdown_core::{Db, index};

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

/// Every regenerable folder recognised on the drive: (folder name, why).
fn junk(db: &Db) -> Vec<(String, String)> {
    let root = recognise::root_ids(db).unwrap()[0];
    let r = recognise::recognise(db, root, true, &mut |_| {}).unwrap();
    let mut v: Vec<(String, String)> = r
        .junk
        .iter()
        .chain(r.entities.iter().flat_map(|e| e.junk.iter()))
        .map(|j| {
            let name = Path::new(&j.path)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string();
            (name, j.why.clone())
        })
        .collect();
    v.sort();
    v
}

#[test]
fn added_folder_names_count_as_junk_until_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join("drive");
    write(
        &d.join("site/node_modules/left-pad/index.js"),
        b"module.exports = 1;\n",
    );
    write(&d.join("site/.terraform/providers/lock"), b"lock\n");
    write(&d.join("app/DerivedData/Build/x.o"), b"\x7fELF");
    write(&d.join("app/main.swift"), b"print(1)\n");
    write(&d.join("System Volume Information/tracking.log"), b"x");
    let mut db = Db::open(&tmp.path().join("i.db")).unwrap();
    let opts = index::Options {
        one_file_system: true,
        allow_empty: false,
    };
    index::index(&mut db, &d, &opts, &mut |_| {}).unwrap();

    let why = |name: &str| {
        JUNK_NAMES
            .iter()
            .find(|(n, _)| *n == name)
            .unwrap()
            .1
            .to_string()
    };
    let (npm, svi) = (why("node_modules"), why("system volume information"));
    assert_eq!(
        junk(&db),
        vec![
            ("System Volume Information".into(), svi.clone()),
            ("node_modules".into(), npm.clone())
        ]
    );

    // names are matched the way the built-in ones are: whatever their case
    recognise::set_extra_junk(&[".terraform".into(), "deriveddata".into(), "  ".into()]);
    assert_eq!(
        junk(&db),
        vec![
            (".terraform".into(), EXTRA_JUNK_WHY.into()),
            ("DerivedData".into(), EXTRA_JUNK_WHY.into()),
            ("System Volume Information".into(), svi),
            ("node_modules".into(), npm),
        ]
    );

    recognise::set_extra_junk(&[]);
    assert_eq!(junk(&db).len(), 2);
}
