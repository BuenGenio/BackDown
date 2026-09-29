//! Scans from NTFS's master file table must record what a walk records. An NTFS image is made
//! with mkntfs and filled through ntfs-3g (a FUSE mount of the image file, which needs no root
//! where ntfs-3g may be used by everyone): thousands of files in nested folders, a folder big
//! enough for a multi-level index, Unicode names, empty, resident, non-resident, big, sparse and
//! compressed files, hard links (one file with enough names to need an attribute list and
//! extension records), symlinks and a pipe (ntfs-3g stores them Interix-style), system and hidden
//! files, times before 1970 and far in the future. Then the image is mounted read-only and the
//! same root is scanned twice: walked, and from the table (the real way: mountinfo, the device,
//! the checks), and every row compared. The table is also checked against fsread's own folder
//! listing and ntfs-3g's `ntfsls`.
//!
//! Without mkntfs or a usable ntfs-3g the tests say so and pass. Test data lives under the
//! target directory. The benchmark (200,000 files, release mode):
//! cargo test --release --test mft -- --ignored --nocapture

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, UNIX_EPOCH};

use backdown_core::fsread::{self, mft_scan};
use backdown_core::{Db, index, paths};

fn tool(name: &str) -> Option<PathBuf> {
    ["/usr/sbin", "/sbin", "/usr/bin", "/bin", "/usr/local/bin"]
        .iter()
        .map(|d| Path::new(d).join(name))
        .find(|p| p.exists())
}

fn run(cmd: &mut Command) -> String {
    let out = cmd.output().expect("run a tool");
    assert!(
        out.status.success(),
        "{cmd:?} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn base_dir(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("mft")
        .join(name);
    let mnt = d.join("mnt");
    // left mounted by an interrupted run
    if fs::read_to_string("/proc/self/mountinfo")
        .is_ok_and(|m| m.contains(&format!(" {} ", mnt.display())))
    {
        let _ = Command::new("fusermount").arg("-u").arg(&mnt).status();
    }
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(d.join("mnt")).unwrap();
    d
}

/// An ntfs-3g mount of an image, unmounted when dropped.
struct Mounted(PathBuf);

impl Mounted {
    fn new(img: &Path, mnt: &Path, ro: bool) -> Option<Mounted> {
        let ntfs3g = tool("ntfs-3g")?;
        let mut cmd = Command::new(ntfs3g);
        if ro {
            cmd.args(["-o", "ro"]);
        }
        let ok = cmd.arg(img).arg(mnt).status().ok()?.success();
        // it has to be a mount now, not just the empty folder
        let mounted = ok
            && fs::read_to_string("/proc/self/mountinfo")
                .is_ok_and(|m| m.contains(&format!(" {} ", mnt.display())));
        mounted.then(|| Mounted(mnt.to_path_buf()))
    }
}

impl Drop for Mounted {
    fn drop(&mut self) {
        for _ in 0..20 {
            if Command::new("fusermount")
                .arg("-u")
                .arg(&self.0)
                .status()
                .is_ok_and(|s| s.success())
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

fn write(root: &Path, rel: &str, bytes: &[u8]) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, bytes).unwrap();
}

fn attrib(p: &Path, bits: u32) {
    // ntfs-3g's extended attribute for the DOS/Windows attributes
    run(Command::new("python3").arg("-c").arg(format!(
        "import os; os.setxattr({:?}, 'system.ntfs_attrib_be', ({bits}).to_bytes(4, 'big'))",
        p.display().to_string()
    )));
}

/// Everything worth checking, in a mounted NTFS volume.
fn populate(m: &Path, files: usize) {
    for i in 0..files {
        write(
            m,
            &format!("tree/{}/{}/file {i}.txt", i % 7, i % 13),
            &vec![b'x'; i % 900],
        );
    }
    // a folder whose index needs INDX blocks and more than one level
    for i in 0..1500 {
        write(
            m,
            &format!("many/entry number {i:05} with a longer name.dat"),
            b"m",
        );
    }
    write(
        m,
        "d1/d2/d3/d4/d5/d6/d7/d8/d9/d10/d11/d12/deep.txt",
        b"deep",
    );
    write(m, "Ünïcødé ✓/Ω 名前.txt", "ω".as_bytes());
    write(m, "sizes/empty", b"");
    write(m, "sizes/resident", &[1; 200]);
    write(m, "sizes/nonresident", &[2; 5000]);
    write(m, "sizes/big", &vec![3; 3_000_000]);
    File::create(m.join("sizes/sparse"))
        .unwrap()
        .set_len(50 << 20)
        .unwrap();
    // hard links, and a file with so many names its record overflows (an attribute list)
    write(m, "links/a.txt", b"linked");
    fs::create_dir_all(m.join("other")).unwrap();
    fs::hard_link(m.join("links/a.txt"), m.join("other/a-link.txt")).unwrap();
    write(m, "multi/f", b"many names");
    for i in 0..150 {
        fs::hard_link(m.join("multi/f"), m.join(format!("multi/name {i:03}"))).unwrap();
    }
    // links and a pipe: not recorded by a walk
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("links", m.join("sym-dir")).unwrap();
        std::os::unix::fs::symlink("links/a.txt", m.join("sym-file")).unwrap();
    }
    run(Command::new("mkfifo").arg(m.join("pipe")));
    // a compressed folder
    fs::create_dir_all(m.join("comp")).unwrap();
    attrib(&m.join("comp"), 0x800);
    let text: Vec<u8> = b"compressible text "
        .iter()
        .cycle()
        .take(400_000)
        .copied()
        .collect();
    fs::write(m.join("comp/c.txt"), &text).unwrap();
    // system files: an empty one is a pipe to ntfs-3g, hidden or longer ones are files
    for (name, bits, bytes) in [
        ("sysfile", 0x4, &b""[..]),
        ("hidsys", 0x6, &b""[..]),
        ("sysdata", 0x4, &b"desktop settings"[..]),
    ] {
        fs::write(m.join(name), bytes).unwrap();
        attrib(&m.join(name), bits);
    }
    // times
    for (rel, secs) in [
        ("d1/old.txt", -315_619_200i64),
        ("d1/future.txt", 5_680_281_600),
    ] {
        write(m, rel, b"t");
        let t = if secs < 0 {
            UNIX_EPOCH - Duration::from_secs(secs.unsigned_abs())
        } else {
            UNIX_EPOCH + Duration::from_secs(secs as u64)
        };
        File::options()
            .write(true)
            .open(m.join(rel))
            .unwrap()
            .set_modified(t)
            .unwrap();
    }
}

/// Every row a scan decides (files and folders), comparable across two indexes.
fn rows(db: &Db) -> Vec<String> {
    let mut out = Vec::new();
    for sql in [
        "SELECT 'file', path, dir, size, mtime, dev, ino, first_scan, last_scan, current, lname FROM files",
        "SELECT 'dir', path, parent, files, bytes, newest, dev, ino, first_scan, last_scan, current, lname FROM dirs",
    ] {
        let mut stmt = db.conn.prepare(sql).unwrap();
        let n = stmt.column_count();
        let it = stmt
            .query_map([], |r| {
                let mut s = String::new();
                for i in 0..n {
                    let v: rusqlite::types::Value = r.get(i)?;
                    match v {
                        rusqlite::types::Value::Blob(b) => {
                            s.push_str(&paths::from_bytes(&b).display().to_string())
                        }
                        other => s.push_str(&format!("{other:?}")),
                    }
                    s.push_str(" | ");
                }
                Ok(s)
            })
            .unwrap();
        out.extend(it.map(Result::unwrap));
    }
    out.sort();
    out
}

fn same(want: &[String], got: &[String]) {
    let w: BTreeSet<&String> = want.iter().collect();
    let g: BTreeSet<&String> = got.iter().collect();
    let missing: Vec<_> = w.difference(&g).take(10).collect();
    let extra: Vec<_> = g.difference(&w).take(10).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "the table's rows differ from a walk's\nonly in the walk: {missing:#?}\nonly from the table: {extra:#?}"
    );
    assert_eq!(want.len(), got.len());
}

fn opts() -> index::Options {
    index::Options {
        one_file_system: true,
        allow_empty: false,
    }
}

/// Makes and fills an image; None (after saying why) where the tools or FUSE aren't there.
fn image(base: &Path, size: u64, files: usize) -> Option<PathBuf> {
    let Some(mkntfs) = tool("mkntfs") else {
        eprintln!("skipping: mkntfs isn't installed");
        return None;
    };
    tool("python3")?;
    let img = base.join("ntfs.img");
    File::create(&img).unwrap().set_len(size).unwrap();
    run(Command::new(mkntfs)
        .args(["-F", "-Q", "-q", "-L", "BD"])
        .arg(&img));
    let mnt = base.join("mnt");
    let Some(m) = Mounted::new(&img, &mnt, false) else {
        eprintln!("skipping: can't mount an image with ntfs-3g here (no FUSE?)");
        return None;
    };
    populate(&mnt, files);
    drop(m);
    Some(img)
}

#[test]
fn table_scan_equals_walk_on_ntfs3g() {
    let base = base_dir("equal");
    let Some(img) = image(&base, 256 << 20, 3000) else {
        return;
    };
    let mnt = base.join("mnt");
    let Some(m) = Mounted::new(&img, &mnt, true) else {
        return;
    };
    let mut walked = Db::open(&base.join("walk.db")).unwrap();
    let w = index::index(&mut walked, &mnt, &opts(), &mut |_| {}).unwrap();
    assert!(w.files > 3000 + 1500 + 150, "{}", w.files);
    let mut table = Db::open(&base.join("table.db")).unwrap();
    let t = index::scan(&mut table, &mnt, &opts(), index::Method::Table, &mut |_| {}).unwrap();
    assert_eq!(t.method, "table");
    assert_eq!((t.files, t.bytes, t.errors), (w.files, w.bytes, w.errors));
    same(&rows(&walked), &rows(&table));
    // left to choose, it reads the table too (there's no change log on Linux)
    let mut auto = Db::open(&base.join("auto.db")).unwrap();
    let a = index::scan(&mut auto, &mnt, &opts(), index::Method::Auto, &mut |_| {}).unwrap();
    assert_eq!(a.method, "table", "{}", a.note);

    // a folder of the drive as the root, then changes, and a second scan over the first
    let sub = mnt.join("tree");
    let mut w2 = Db::open(&base.join("walk2.db")).unwrap();
    let mut t2 = Db::open(&base.join("table2.db")).unwrap();
    index::index(&mut w2, &sub, &opts(), &mut |_| {}).unwrap();
    index::scan(&mut t2, &sub, &opts(), index::Method::Table, &mut |_| {}).unwrap();
    same(&rows(&w2), &rows(&t2));
    drop(m);
    let m = Mounted::new(&img, &mnt, false).expect("mounts again");
    fs::remove_dir_all(mnt.join("tree/3")).unwrap();
    write(&mnt, "tree/new/n.txt", b"new");
    write(&mnt, "tree/0/0/file 0.txt", b"changed");
    fs::rename(mnt.join("tree/4"), mnt.join("tree/moved")).unwrap();
    drop(m);
    let m = Mounted::new(&img, &mnt, true).expect("mounts again");
    index::index(&mut w2, &sub, &opts(), &mut |_| {}).unwrap();
    let st = index::scan(&mut t2, &sub, &opts(), index::Method::Table, &mut |_| {}).unwrap();
    assert!(st.new > 0 && st.changed == 1 && st.removed > 0, "{st:?}");
    same(&rows(&w2), &rows(&t2));

    // without read access to the device, the table is refused and says what would allow it
    drop(m);
    let m = Mounted::new(&img, &mnt, true).expect("mounts again");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&img, fs::Permissions::from_mode(0o000)).unwrap();
        let e = index::scan(&mut t2, &sub, &opts(), index::Method::Table, &mut |_| {});
        fs::set_permissions(&img, fs::Permissions::from_mode(0o644)).unwrap();
        let e = format!("{:#}", e.unwrap_err());
        assert!(e.contains("can't be read directly"), "{e}");
        let st = index::scan(&mut t2, &sub, &opts(), index::Method::Auto, &mut |_| {}).unwrap();
        assert_eq!(st.method, "table");
    }
    drop(m);
    let _ = fs::remove_dir_all(&base);
}

/// The table, read as Windows shows files, against fsread's own folder listing (from each
/// folder's index) and ntfs-3g's ntfsls.
#[test]
fn table_agrees_with_folder_listings() {
    let base = base_dir("listings");
    let Some(img) = image(&base, 128 << 20, 800) else {
        return;
    };
    let f = File::open(&img).unwrap();
    let len = f.metadata().unwrap().len();
    let mut vol = fsread::Volume::open(f, 0, len).unwrap();
    let t = mft_scan::read(&mut vol, mft_scan::Flavour::Windows, None, &mut |_, _| {}).unwrap();
    assert!(t.in_use > 2300, "{}", t.in_use);
    assert_eq!(t.label.as_deref(), Some("BD"));
    // the table's tree
    let mut from_table: BTreeMap<String, (bool, u64, i64)> = BTreeMap::new();
    let mut stack: Vec<String> = Vec::new();
    for (depth, name, rec) in t.walk(5) {
        stack.truncate(depth);
        let name = String::from_utf16(name).unwrap();
        let path = if depth == 0 {
            String::new()
        } else {
            format!("{}/{name}", stack[depth - 1])
        };
        if rec.kind != mft_scan::Kind::Link {
            from_table.insert(
                path.clone(),
                (rec.kind == mft_scan::Kind::Dir, rec.size, rec.mtime),
            );
        }
        if rec.kind == mft_scan::Kind::Dir {
            stack.push(path);
        }
    }
    // fsread's, folder by folder
    let mut listed: BTreeMap<String, (bool, u64, i64)> = BTreeMap::new();
    let mut todo = vec![String::new()];
    while let Some(dir) = todo.pop() {
        let l = vol.list_path(&dir, fsread::MAX_LIST).unwrap();
        assert!(!l.truncated);
        for e in l.entries {
            if e.link.is_some() {
                continue;
            }
            let p = format!("{dir}/{}", e.name);
            listed.insert(p.clone(), (e.dir, e.size, e.mtime.unwrap_or(0).max(0)));
            if e.dir {
                todo.push(p);
            }
        }
    }
    from_table.remove("");
    assert_eq!(from_table.len(), listed.len());
    for (p, want) in &listed {
        assert_eq!(from_table.get(p), Some(want), "{p}");
    }
    // ntfsls's names and record numbers, folder by folder
    if let Some(ntfsls) = tool("ntfsls") {
        let out = run(Command::new(ntfsls).args(["-R", "-l", "-i"]).arg(&img));
        let mut dir = String::new();
        let mut seen = 0;
        for line in out.lines() {
            if let Some(d) = line.strip_suffix(':').filter(|d| d.starts_with('/')) {
                dir = d.trim_end_matches('/').to_string();
                continue;
            }
            // inode size month day time year name (names here have single spaces)
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 7 {
                continue;
            }
            let name = f[6..].join(" ");
            if name == "." || name == ".." {
                continue;
            }
            let rec: u64 = f[0].parse().unwrap();
            let p = format!("{dir}/{name}");
            let in_table = t.find(
                &p.split('/')
                    .filter(|s| !s.is_empty())
                    .map(|s| s.encode_utf16().collect())
                    .collect::<Vec<_>>(),
            );
            assert_eq!(in_table.map(|r| r.record), Some(rec), "{p}");
            seen += 1;
        }
        assert!(seen > 2300, "{seen}");
    }
    let _ = fs::remove_dir_all(&base);
}

/// How long the table takes against a walk of the same volume's folders (fsread) and a walk of
/// the ntfs-3g mount, on 200,000 files.
#[test]
#[ignore]
fn bench_table_against_walks() {
    let base = base_dir("bench");
    let n = std::env::var("BACKDOWN_MFT_FILES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200_000);
    let t0 = Instant::now();
    let Some(img) = image(&base, 2 << 30, n) else {
        return;
    };
    eprintln!("made {n} files in {:.1?}", t0.elapsed());
    let len = fs::metadata(&img).unwrap().len();
    // the table
    let t0 = Instant::now();
    let mut vol = fsread::Volume::open(File::open(&img).unwrap(), 0, len).unwrap();
    let t = mft_scan::read(&mut vol, mft_scan::Flavour::Ntfs3g, None, &mut |_, _| {}).unwrap();
    let mut count = 0u64;
    for (_, _, rec) in t.walk(5) {
        count += u64::from(rec.kind == mft_scan::Kind::File);
    }
    let table = t0.elapsed();
    // fsread's folder-by-folder walk (reads each folder's index, then each file's record)
    let t0 = Instant::now();
    let mut vol = fsread::Volume::open(File::open(&img).unwrap(), 0, len).unwrap();
    let mut todo = vec![String::new()];
    let mut walked = 0u64;
    while let Some(dir) = todo.pop() {
        for e in vol.list_path(&dir, fsread::MAX_LIST).unwrap().entries {
            if e.dir {
                todo.push(format!("{dir}/{}", e.name));
            } else {
                walked += 1;
            }
        }
    }
    let listing = t0.elapsed();
    // the whole scan both ways, through the ntfs-3g mount
    let mnt = base.join("mnt");
    let m = Mounted::new(&img, &mnt, true).expect("mounts");
    let t0 = Instant::now();
    let mut a = Db::open(&base.join("walk.db")).unwrap();
    let w = index::index(&mut a, &mnt, &opts(), &mut |_| {}).unwrap();
    let walk_scan = t0.elapsed();
    let t0 = Instant::now();
    let mut b = Db::open(&base.join("table.db")).unwrap();
    let s = index::scan(&mut b, &mnt, &opts(), index::Method::Table, &mut |_| {}).unwrap();
    let table_scan = t0.elapsed();
    // and again, over the first (the rows are there: most time is the reading)
    let t0 = Instant::now();
    index::index(&mut a, &mnt, &opts(), &mut |_| {}).unwrap();
    let walk_rescan = t0.elapsed();
    let t0 = Instant::now();
    index::scan(&mut b, &mnt, &opts(), index::Method::Table, &mut |_| {}).unwrap();
    let table_rescan = t0.elapsed();
    drop(m);
    eprintln!(
        "{} records, {count} files in the table ({walked} by listing folders)\n\
         reading the table: {table:.2?}\n\
         listing every folder (fsread): {listing:.2?}\n\
         first scan, walking the ntfs-3g mount: {walk_scan:.2?} ({} files)\n\
         first scan, from the table: {table_scan:.2?} ({} files)\n\
         rescan, walking: {walk_rescan:.2?}\n\
         rescan, from the table: {table_rescan:.2?}",
        t.in_use, w.files, s.files
    );
    assert_eq!(count, walked);
    assert_eq!(w.files, s.files);
    let _ = fs::remove_dir_all(&base);
}
