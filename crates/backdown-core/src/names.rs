//! Names that are one name to some filesystems and two to others.
//!
//! macOS's HFS+ stores names decomposed (NFD: "e" and a combining accent), nearly everything else
//! composed (NFC: "é"); APFS, HFS+, NTFS, FAT and exFAT usually find a name whatever its case, and
//! APFS and HFS+ whatever its normalisation. So two files that are distinct on a Linux drive
//! ("Photo.jpg" and "photo.jpg", or "café" spelt both ways) are one name on an archive drive, and
//! the same folder can be spelt one way in the index and another way where a person typed it.
//!
//! - A destination is probed once (`probe`) for what it takes for the same name; a `Renamer` gives
//!   the second of two colliding names a " (2)" suffix instead of letting it overwrite or merge
//!   with the first (callers journal the rename); `variant_there` spots a name already at a
//!   destination under another spelling. On Windows, and on drives that refuse `:` (FAT, exFAT),
//!   names those systems can't hold are renamed the same way (`windows_name`).
//! - Paths compared across drives, or with what a person typed, match whatever their Unicode
//!   form (`within`, `nfc_path`, `as_indexed`), never whatever their case: on Linux "Photos" and
//!   "photos" are two folders.
//! - Names are shown composed (`nfc`).

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use unicode_normalization::{UnicodeNormalization, is_nfc};

use crate::{Db, paths};

/// `s` composed (NFC), as most systems write names; borrowed when it already is.
pub fn nfc(s: &str) -> Cow<'_, str> {
    if s.is_ascii() || is_nfc(s) {
        Cow::Borrowed(s)
    } else {
        Cow::Owned(s.nfc().collect())
    }
}

/// A path's stored bytes with its names composed (NFC): the same for both spellings of a name.
pub fn nfc_path(b: &[u8]) -> Cow<'_, [u8]> {
    if b.is_ascii() {
        return Cow::Borrowed(b);
    }
    let p = paths::from_bytes(b);
    match p.to_str() {
        Some(s) if !is_nfc(s) => Cow::Owned(paths::to_bytes(Path::new(&nfc(s).into_owned()))),
        _ => Cow::Borrowed(b),
    }
}

/// Whether `p` is `base` or inside it, a name matching whatever its Unicode form.
pub fn within(p: &Path, base: &Path) -> bool {
    let mut a = p.components();
    base.components()
        .all(|b| a.next().is_some_and(|x| same(x, b)))
}

fn same(a: Component, b: Component) -> bool {
    a == b
        || match (a, b) {
            (Component::Normal(x), Component::Normal(y)) => match (x.to_str(), y.to_str()) {
                (Some(x), Some(y)) => nfc(x) == nfc(y),
                _ => false,
            },
            _ => false,
        }
}

/// A folder or file as the index spells it. A path a person typed, or one spelt by another
/// system, can name a folder the index recorded in the other Unicode form (a Mac drive read on
/// Linux keeps its names decomposed): each name that isn't in the index as it is, but is there in
/// the other form, is replaced by the index's spelling. The rest is left as given.
pub fn as_indexed(db: &Db, p: &Path) -> Result<PathBuf> {
    let mut stmt = db.conn.prepare_cached("SELECT path FROM roots")?;
    let roots: Vec<Vec<u8>> = stmt
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let Some(root) = roots
        .into_iter()
        .map(|r| paths::from_bytes(&r))
        .filter(|r| within(p, r))
        .max_by_key(|r| r.components().count())
    else {
        return Ok(p.to_path_buf());
    };
    let rest: Vec<Component> = p.components().skip(root.components().count()).collect();
    let mut exact = db.conn.prepare_cached(
        "SELECT EXISTS (SELECT 1 FROM dirs WHERE path = ?1 AND current = 1)
             OR EXISTS (SELECT 1 FROM files WHERE path = ?1 AND current = 1)",
    )?;
    let mut sub_dirs = db
        .conn
        .prepare_cached("SELECT path FROM dirs WHERE parent = ?1 AND current = 1")?;
    let mut sub_files = db
        .conn
        .prepare_cached("SELECT path FROM files WHERE dir = ?1 AND current = 1")?;
    let mut at = root;
    for (i, c) in rest.iter().enumerate() {
        let Component::Normal(name) = c else {
            return Ok(p.to_path_buf());
        };
        let cand = at.join(name);
        let cb = paths::to_bytes(&cand);
        if exact.query_row(params![cb], |r| r.get::<_, bool>(0))? {
            at = cand;
            continue;
        }
        let want = nfc_path(&cb).into_owned();
        let parent = paths::to_bytes(&at);
        let mut found = None;
        for q in [&mut sub_dirs, &mut sub_files] {
            found = q
                .query_map(params![parent], |r| r.get::<_, Vec<u8>>(0))?
                .filter_map(Result::ok)
                .find(|x| *nfc_path(x) == *want);
            if found.is_some() {
                break;
            }
        }
        match found {
            Some(f) => at = paths::from_bytes(&f),
            None => {
                // not in the index at all: the rest as given
                for c in &rest[i..] {
                    at.push(c.as_os_str());
                }
                return Ok(at);
            }
        }
    }
    Ok(at)
}

// ---------- what a destination takes for the same name

/// What a destination folder's filesystem treats as one name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Folding {
    /// "Photo.jpg" and "photo.jpg" are one name (APFS and HFS+ by default, NTFS, FAT, exFAT)
    pub case: bool,
    /// "café" composed and decomposed are one name (APFS, HFS+)
    pub normalization: bool,
    /// only names Windows can hold: no `:*?"<>|\`, no trailing dot or space, no `CON` or `LPT1`
    /// (Windows itself, and FAT and exFAT drives anywhere)
    pub windows: bool,
}

impl Folding {
    /// Every name is its own (ext4, Btrfs, ZFS, XFS).
    pub const EXACT: Folding = Folding {
        case: false,
        normalization: false,
        windows: false,
    };
    /// When a destination can't be probed: the most forgiving (a rename that wasn't needed is
    /// harmless; a merge isn't).
    pub const ANY: Folding = Folding {
        case: true,
        normalization: true,
        windows: true,
    };

    pub fn is_exact(&self) -> bool {
        *self == Folding::EXACT
    }

    /// What two names that are one name here have in common.
    pub fn key(&self, name: &OsStr) -> Vec<u8> {
        match name.to_str() {
            Some(s) => {
                let mut k: String = if self.normalization {
                    s.nfd().collect()
                } else {
                    s.to_string()
                };
                if self.case {
                    k = k.chars().flat_map(char::to_lowercase).collect();
                    if self.normalization {
                        k = k.nfd().collect();
                    }
                }
                let mut v = b"s".to_vec();
                v.extend_from_slice(k.as_bytes());
                v
            }
            None => {
                let raw = paths::to_bytes(Path::new(name));
                let mut v = b"b".to_vec();
                v.extend(if self.case {
                    raw.to_ascii_lowercase()
                } else {
                    raw
                });
                v
            }
        }
    }

    /// `key` of every name of a relative path, separated.
    pub fn path_key(&self, rel: &Path) -> Vec<u8> {
        let mut out = Vec::new();
        for c in rel.components() {
            out.extend(self.key(c.as_os_str()));
            out.push(0);
        }
        out
    }
}

#[cfg(test)]
thread_local! {
    /// Tests on a case-sensitive drive stand in another drive's answer here.
    pub(crate) static ASSUME: std::cell::Cell<Option<Folding>> = const { std::cell::Cell::new(None) };
}

/// What `dir` takes for the same name, found by creating one small hidden file in it and looking
/// it up by other spellings (then removing it). `dir` must exist.
pub fn probe(dir: &Path) -> io::Result<Folding> {
    #[cfg(test)]
    if let Some(f) = ASSUME.with(|a| a.get()) {
        return Ok(f);
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let base = format!(".backdown-probe-{}-{nanos}-", std::process::id());
    let made = dir.join(format!("{base}Ab\u{e9}"));
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(paths::os(&made))?;
    let there = |name: String| fs::symlink_metadata(paths::os(&dir.join(name))).is_ok();
    let case = there(format!("{base}aB\u{e9}"));
    let normalization = there(format!("{base}Abe\u{301}"));
    let _ = fs::remove_file(paths::os(&made));
    // `a:b` is a stream of `a` on Windows, so that one isn't tried there
    let windows = cfg!(windows) || {
        let colon = dir.join(format!("{base}x:y"));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&colon)
        {
            Ok(_) => {
                let _ = fs::remove_file(&colon);
                false
            }
            Err(_) => true,
        }
    };
    Ok(Folding {
        case,
        normalization,
        windows,
    })
}

/// `probe`, or the most forgiving answer when `dir` can't be written to.
pub fn probe_or_assume(dir: &Path) -> Folding {
    probe(dir).unwrap_or(Folding::ANY)
}

// ---------- names Windows can hold

const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "COM¹", "COM²", "COM³", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8",
    "LPT9", "LPT¹", "LPT²", "LPT³",
];

/// `name` changed so Windows (and FAT or exFAT) can hold it: `:*?"<>|\` and control characters
/// become `-`, a trailing dot or space goes, and a device name (`CON`, `aux.txt`) gets a `_`
/// in front. `None` when it can already hold it.
pub fn windows_name(name: &str) -> Option<String> {
    let mut s: String = name
        .chars()
        .map(|c| {
            if matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\\') || c.is_control() {
                '-'
            } else {
                c
            }
        })
        .collect();
    while s.ends_with('.') || s.ends_with(' ') {
        s.pop();
    }
    if s.is_empty() {
        s = "_".into();
    }
    let stem = s.split('.').next().unwrap_or("").trim_end();
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
        s.insert(0, '_');
    }
    (s != name).then_some(s)
}

// ---------- telling colliding names apart

/// `name` with " (n)" before its extension (a folder's name gets it at the end).
pub fn numbered(name: &OsStr, n: u32, is_dir: bool) -> OsString {
    let Some(s) = name.to_str() else {
        let mut o = name.to_os_string();
        o.push(format!(" ({n})"));
        return o;
    };
    let split = if is_dir {
        None
    } else {
        s.rfind('.').filter(|&i| i > 0 && s.len() - i <= 12)
    };
    OsString::from(match split {
        Some(i) => format!("{} ({n}){}", &s[..i], &s[i..]),
        None => format!("{s} ({n})"),
    })
}

/// New names for the entries of one folder that would collide under `folding`: of the names that
/// are one name there, the first (in byte order) keeps it and the others are numbered; a name
/// the destination can't hold is replaced (and numbered if that collides too). Only the renamed
/// entries are returned.
pub fn assign(entries: &[(OsString, bool)], folding: Folding) -> HashMap<OsString, OsString> {
    let mut sorted: Vec<&(OsString, bool)> = entries.iter().collect();
    sorted.sort();
    let portable = |n: &OsStr| -> Option<OsString> {
        if !folding.windows {
            return None;
        }
        windows_name(n.to_str()?).map(OsString::from)
    };
    let mut used: HashSet<Vec<u8>> = HashSet::new();
    let mut later: Vec<(&OsString, bool, OsString)> = Vec::new();
    for (name, is_dir) in &sorted {
        match portable(name) {
            None if used.insert(folding.key(name)) => {}
            None => later.push((name, *is_dir, name.clone())),
            Some(p) => later.push((name, *is_dir, p)),
        }
    }
    let mut out = HashMap::new();
    for (name, is_dir, base) in later {
        let mut cand = base.clone();
        let mut n = 2;
        while !used.insert(folding.key(&cand)) {
            cand = numbered(&base, n, is_dir);
            n += 1;
        }
        out.insert(name.clone(), cand);
    }
    out
}

/// Maps paths below a source folder to paths below its copy, so that names the destination takes
/// for one never meet: each source folder's entries are listed (read-only) once, when the first
/// path through it is mapped, and `assign`ed new names where needed. Folders are remembered from
/// the root down to the last one mapped, so a depth-first walk lists each folder once.
pub struct Renamer {
    src_root: PathBuf,
    folding: Folding,
    /// (a source folder relative to the root, the renamed entries in it)
    stack: Vec<(PathBuf, HashMap<OsString, OsString>)>,
}

/// One rename: the source name, and the name it has at the destination.
pub type Renamed = (OsString, OsString);

impl Renamer {
    pub fn new(src_root: &Path, folding: Folding) -> Renamer {
        Renamer {
            src_root: src_root.to_path_buf(),
            folding,
            stack: Vec::new(),
        }
    }

    /// Where `rel` (a path below the source root) goes below the destination root, and the names
    /// renamed on the way.
    pub fn map(&mut self, rel: &Path) -> (PathBuf, Vec<Renamed>) {
        if self.folding.is_exact() {
            return (rel.to_path_buf(), Vec::new());
        }
        let mut out = PathBuf::new();
        let mut renamed = Vec::new();
        let mut dir = PathBuf::new();
        for (depth, c) in rel.components().enumerate() {
            let name = c.as_os_str();
            if !matches!(c, Component::Normal(_)) {
                out.push(name);
                dir.push(name);
                continue;
            }
            match self.renames_in(&dir, depth).get(name) {
                Some(to) => {
                    out.push(to);
                    renamed.push((name.to_os_string(), to.clone()));
                }
                None => out.push(name),
            }
            dir.push(name);
        }
        (out, renamed)
    }

    fn renames_in(&mut self, dir: &Path, depth: usize) -> &HashMap<OsString, OsString> {
        if self.stack.get(depth).is_none_or(|(d, _)| d != dir) {
            self.stack.truncate(depth);
            let entries: Vec<(OsString, bool)> =
                match fs::read_dir(paths::os(&self.src_root.join(dir))) {
                    Ok(rd) => rd
                        .filter_map(Result::ok)
                        .map(|e| (e.file_name(), e.file_type().is_ok_and(|t| t.is_dir())))
                        .collect(),
                    Err(_) => Vec::new(),
                };
            self.stack
                .push((dir.to_path_buf(), assign(&entries, self.folding)));
        }
        let i = depth.min(self.stack.len() - 1);
        &self.stack[i].1
    }
}

/// "notes.txt → notes (2).txt (…why)" for the journal.
pub fn describe(renamed: &[Renamed]) -> String {
    renamed
        .iter()
        .map(|(a, b)| {
            let a = a.to_string_lossy();
            let why = if windows_name(&a).is_some() {
                "a name the drive can't hold"
            } else {
                "the drive takes it for another name there"
            };
            format!("{} → {} ({why})", nfc(&a), nfc(&b.to_string_lossy()))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// When something is at `dest` under another spelling (another case, or another name that this
/// filesystem takes for it): that name. `None` when `dest` itself is there (or nothing is, or
/// what's there is the same name in the other Unicode form, which is the same name to people).
pub fn variant_there(dest: &Path) -> Option<OsString> {
    Listing::default().variant_there(dest)
}

/// Which of `names` (a folder's entries) is `name` under another spelling, as `variant_there`.
pub fn variant_among(names: &[OsString], name: &OsStr) -> Option<OsString> {
    let want = name.to_str().map(|s| nfc(s).into_owned());
    let key = Folding::ANY.key(name);
    let mut variant = None;
    for n in names {
        if n == name || (want.is_some() && n.to_str().map(nfc).as_deref() == want.as_deref()) {
            return None;
        }
        if variant.is_none() && Folding::ANY.key(n) == key {
            variant = Some(n.clone());
        }
    }
    variant
}

/// The entries of the destination folder last looked at, for `variant_there` on many files of
/// one folder. Names added to it since don't matter: a `Renamer` keeps new names apart.
#[derive(Default)]
pub struct Listing {
    dir: Option<PathBuf>,
    names: Vec<OsString>,
}

impl Listing {
    pub fn variant_there(&mut self, dest: &Path) -> Option<OsString> {
        let name = dest.file_name()?;
        let parent = dest.parent()?;
        if self.dir.as_deref() != Some(parent) {
            self.names = fs::read_dir(paths::os(parent))
                .ok()?
                .filter_map(Result::ok)
                .map(|e| e.file_name())
                .collect();
            self.dir = Some(parent.to_path_buf());
        }
        variant_among(&self.names, name)
    }
}

/// The first "name (n).ext" beside `dest` that nothing is at (under any spelling this filesystem
/// takes for it).
pub fn free_name(dest: &Path) -> PathBuf {
    let name = dest.file_name().unwrap_or_default();
    let mut n = 2;
    loop {
        let cand = dest.with_file_name(numbered(name, n, false));
        if fs::symlink_metadata(paths::os(&cand)).is_err() || n > 10_000 {
            return cand;
        }
        n += 1;
    }
}

/// `as_indexed` for a path that may not be inside any root (then it's returned as is), without
/// failing: for callers that only use it to look things up.
pub fn as_indexed_or_same(db: &Db, p: &Path) -> PathBuf {
    as_indexed(db, p).unwrap_or_else(|_| p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    #[test]
    fn composed_for_people() {
        assert_eq!(nfc("cafe\u{301}"), "caf\u{e9}");
        assert!(matches!(nfc("caf\u{e9}"), Cow::Borrowed(_)));
        assert!(matches!(nfc("plain"), Cow::Borrowed(_)));
        let d = paths::to_bytes(Path::new("/x/Cafe\u{301}/Re\u{301}sume\u{301}.txt"));
        let c = paths::to_bytes(Path::new("/x/Caf\u{e9}/R\u{e9}sum\u{e9}.txt"));
        assert_eq!(nfc_path(&d), nfc_path(&c));
        assert_eq!(*nfc_path(&c), *c);
    }

    #[test]
    fn keys_follow_the_filesystem() {
        let nfc_name = os("Caf\u{e9}.txt");
        let nfd_name = os("Cafe\u{301}.txt");
        let lower = os("caf\u{e9}.TXT");
        let ext4 = Folding::EXACT;
        assert_ne!(ext4.key(&nfc_name), ext4.key(&nfd_name));
        assert_ne!(ext4.key(&nfc_name), ext4.key(&lower));
        let ntfs = Folding {
            case: true,
            normalization: false,
            windows: true,
        };
        assert_eq!(ntfs.key(&nfc_name), ntfs.key(&lower));
        assert_ne!(ntfs.key(&nfc_name), ntfs.key(&nfd_name));
        let apfs = Folding {
            case: true,
            normalization: true,
            windows: false,
        };
        assert_eq!(apfs.key(&nfc_name), apfs.key(&nfd_name));
        assert_eq!(apfs.key(&nfd_name), apfs.key(&lower));
        assert_eq!(
            apfs.key(&os("\u{c9}T\u{c9}")),
            apfs.key(&os("e\u{301}te\u{301}"))
        );
    }

    #[test]
    fn colliding_names_are_numbered_in_a_stable_order() {
        let apfs = Folding {
            case: true,
            normalization: true,
            windows: false,
        };
        let entries = vec![
            (os("photo.jpg"), false),
            (os("Photo.jpg"), false),
            (os("Caf\u{e9}"), true),
            (os("Cafe\u{301}"), true),
            (os("notes.txt"), false),
            (os("Photo (2).jpg"), false), // already taken: the renamed one skips it
        ];
        let r = assign(&entries, apfs);
        assert_eq!(r.len(), 2, "{r:?}");
        // byte order: "Photo.jpg" < "photo.jpg"; NFD "Cafe\u{301}" < NFC "Caf\u{e9}"
        assert_eq!(r[&os("photo.jpg")], os("photo (3).jpg"));
        assert_eq!(r[&os("Caf\u{e9}")], os("Caf\u{e9} (2)"));
        // the same on a case-sensitive drive: nothing to do; the same entries in another order
        assert!(assign(&entries, Folding::EXACT).is_empty());
        let mut rev = entries.clone();
        rev.reverse();
        assert_eq!(assign(&rev, apfs), r);
        // NTFS: case collides, the two Unicode forms don't
        let ntfs = Folding {
            case: true,
            normalization: false,
            windows: false,
        };
        let r = assign(&entries, ntfs);
        assert_eq!(r.len(), 1);
        assert_eq!(r[&os("photo.jpg")], os("photo (3).jpg"));
    }

    #[test]
    fn names_windows_cant_hold() {
        assert_eq!(
            windows_name("12:30 meeting?.txt").as_deref(),
            Some("12-30 meeting-.txt")
        );
        assert_eq!(windows_name("draft. ").as_deref(), Some("draft"));
        assert_eq!(windows_name("...").as_deref(), Some("_"));
        assert_eq!(windows_name("aux.txt").as_deref(), Some("_aux.txt"));
        assert_eq!(windows_name("CON").as_deref(), Some("_CON"));
        assert_eq!(windows_name("console.txt"), None);
        assert_eq!(windows_name("Résumé (final).docx"), None);
        let fat = Folding {
            case: true,
            normalization: false,
            windows: true,
        };
        let r = assign(
            &[
                (os("a:b.txt"), false),
                (os("a-b.txt"), false),
                (os("x."), true),
            ],
            fat,
        );
        assert_eq!(r[&os("a:b.txt")], os("a-b (2).txt"));
        assert_eq!(r[&os("x.")], os("x"));
    }

    #[test]
    fn numbering_keeps_the_extension() {
        assert_eq!(numbered(&os("a.tar.gz"), 2, false), os("a.tar (2).gz"));
        assert_eq!(numbered(&os(".bashrc"), 2, false), os(".bashrc (2)"));
        assert_eq!(numbered(&os("Photos.2019"), 3, true), os("Photos.2019 (3)"));
        assert_eq!(numbered(&os("noext"), 2, false), os("noext (2)"));
    }

    #[test]
    fn paths_match_whatever_their_form() {
        let a = Path::new("/media/old/Cafe\u{301}/2019");
        assert!(within(a, Path::new("/media/old/Caf\u{e9}")));
        assert!(within(a, Path::new("/media/old/Cafe\u{301}/2019")));
        assert!(
            !within(a, Path::new("/media/old/caf\u{e9}")),
            "case still counts"
        );
        assert!(!within(Path::new("/media/old"), a));
    }

    #[test]
    fn the_renamer_reads_each_folder_once() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path();
        for p in ["A/x.txt", "a/x.txt", "a/X.txt", "b/deep/y.txt"] {
            let f = src.join(p);
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(&f, p).unwrap();
        }
        let apfs = Folding {
            case: true,
            normalization: true,
            windows: false,
        };
        let mut r = Renamer::new(src, apfs);
        let (d, n) = r.map(Path::new("A/x.txt"));
        assert_eq!((d, n.len()), (PathBuf::from("A/x.txt"), 0));
        // "a" meets "A", and inside it "x.txt" meets "X.txt" (which comes first in byte order)
        let (d, n) = r.map(Path::new("a/x.txt"));
        assert_eq!(d, PathBuf::from("a (2)/x (2).txt"));
        assert_eq!(
            describe(&n),
            "a → a (2) (the drive takes it for another name there), \
             x.txt → x (2).txt (the drive takes it for another name there)"
        );
        let (d, n) = r.map(Path::new("a/X.txt"));
        assert_eq!(d, PathBuf::from("a (2)/X.txt"));
        assert_eq!(n.len(), 1);
        let (d, n) = r.map(Path::new("b/deep/y.txt"));
        assert_eq!((d, n.len()), (PathBuf::from("b/deep/y.txt"), 0));
        // out of order still maps the same way
        let (d, _) = r.map(Path::new("a/x.txt"));
        assert_eq!(d, PathBuf::from("a (2)/x (2).txt"));
        let mut exact = Renamer::new(src, Folding::EXACT);
        assert_eq!(exact.map(Path::new("a/X.txt")).0, PathBuf::from("a/X.txt"));
    }

    #[test]
    fn probing_matches_what_the_filesystem_does() {
        let dir = tempfile::tempdir().unwrap();
        let f = probe(dir.path()).unwrap();
        // whatever this machine's temp folder is, the probe leaves nothing behind and agrees with it
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
        fs::write(dir.path().join("Probe"), "x").unwrap();
        assert_eq!(f.case, dir.path().join("pROBE").exists());
        fs::write(dir.path().join("caf\u{e9}"), "x").unwrap();
        assert_eq!(f.normalization, dir.path().join("cafe\u{301}").exists());
    }

    fn write(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn walk(p: &Path) -> Vec<String> {
        let mut v: Vec<String> = walkdir::WalkDir::new(p)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
            .map(|e| {
                e.path()
                    .strip_prefix(p)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        v.sort();
        v
    }

    fn scan(db: &mut Db, root: &Path) {
        crate::index::index(
            db,
            root,
            &crate::index::Options {
                one_file_system: false,
                allow_empty: false,
            },
            &mut |_| {},
        )
        .unwrap();
    }

    const APFS: Folding = Folding {
        case: true,
        normalization: true,
        windows: true,
    };

    #[test]
    fn apply_keeps_colliding_names_apart() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("old");
        write(&src.join("Photo.jpg"), b"one photo");
        write(&src.join("photo.jpg"), b"another photo");
        write(&src.join("Caf\u{e9}/menu.txt"), b"composed");
        write(&src.join("Cafe\u{301}/menu.txt"), b"decomposed");
        write(&src.join("12:30 call.txt"), b"a name Windows can't hold");
        let mut db = Db::open(&dir.path().join("i.db")).unwrap();
        scan(&mut db, &src);
        let src = paths::canonical(&src).unwrap();
        let target = dir.path().join("archive");
        let p = crate::plan::Plan {
            target: target.display().to_string(),
            created_at: 0,
            schema: String::new(),
            items: vec![crate::plan::Item {
                what: "Folder".into(),
                kind: "unsorted".into(),
                source: src.display().to_string(),
                source_key: crate::browse::key(&src),
                drive: "old".into(),
                is_file: false,
                only_files: false,
                dest: "Unsorted/old".into(),
                files: 5,
                bytes: 60,
                action: "copy".into(),
                why: String::new(),
                exclude: Vec::new(),
                merge: Vec::new(),
                twin: None,
            }],
            drives: Vec::new(),
            copy_files: 5,
            copy_bytes: 60,
        };
        let id = crate::plan::save(&db, &p).unwrap();
        ASSUME.with(|a| a.set(Some(APFS)));
        let st = crate::apply::apply(&mut db, id, false, &mut |_| {}).unwrap();
        assert_eq!((st.copied, st.renamed, st.failed), (5, 3, 0), "{st:?}");
        assert_eq!(
            walk(&target.join("Unsorted/old")),
            vec![
                "12-30 call.txt",
                "Cafe\u{301}/menu.txt",
                "Caf\u{e9} (2)/menu.txt",
                "Photo.jpg",
                "photo (2).jpg",
            ]
        );
        let notes = crate::apply::journal_notes(&db, id, 10).unwrap();
        assert_eq!(notes.len(), 3);
        assert!(
            notes.iter().any(|(s, d, n)| s.ends_with("photo.jpg")
                && d.ends_with("photo (2).jpg")
                && n.contains("renamed photo.jpg → photo (2).jpg")),
            "{notes:?}"
        );
        // resumed as it was: nothing copied twice, nothing renamed differently
        let again = crate::apply::apply(&mut db, id, false, &mut |_| {}).unwrap();
        assert_eq!((again.resumed, again.copied), (5, 0));
        ASSUME.with(|a| a.set(None));
    }

    #[test]
    fn mirrors_keep_colliding_names_apart() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("docs");
        write(&src.join("Notes/A.txt"), b"upper");
        write(&src.join("Notes/a.txt"), b"lower");
        write(&src.join("notes/b.txt"), b"another folder");
        let db = Db::open(&dir.path().join("i.db")).unwrap();
        let dst = dir.path().join("mirror");
        let id = crate::mirror::add(&db, &src, &dst, 30).unwrap();
        ASSUME.with(|a| a.set(Some(APFS)));
        let st = crate::mirror::sync(&db, id, &mut |_, _| {}).unwrap();
        assert_eq!((st.copied, st.renamed, st.failed), (3, 2, 0), "{st:?}");
        assert_eq!(
            walk(&dst),
            vec!["Notes/A.txt", "Notes/a (2).txt", "notes (2)/b.txt"]
        );
        let st = crate::mirror::sync(&db, id, &mut |_, _| {}).unwrap();
        assert_eq!((st.unchanged, st.copied, st.extra), (3, 0, 0), "{st:?}");
        let v = crate::mirror::verify(&db, id, 100, &mut |_, _| {}).unwrap();
        assert_eq!((v.ok, v.missing, v.differs), (3, 0, 0), "{v:?}");
        ASSUME.with(|a| a.set(None));
    }

    #[test]
    fn paths_typed_in_the_other_form_find_the_index() {
        let dir = tempfile::tempdir().unwrap();
        let drive = dir.path().join("mac");
        // as a Mac's HFS+ drive reads on Linux: names decomposed
        write(&drive.join("Re\u{301}sume\u{301}s/cv.txt"), b"cv");
        write(&drive.join("other.txt"), b"x");
        let mut db = Db::open(&dir.path().join("i.db")).unwrap();
        scan(&mut db, &drive);
        let drive = paths::canonical(&drive).unwrap();
        let typed = drive.join("R\u{e9}sum\u{e9}s");
        assert_eq!(
            as_indexed(&db, &typed).unwrap(),
            drive.join("Re\u{301}sume\u{301}s")
        );
        assert_eq!(
            as_indexed(&db, &typed.join("cv.txt")).unwrap(),
            drive.join("Re\u{301}sume\u{301}s/cv.txt")
        );
        // not in the index: as given
        assert_eq!(
            as_indexed(&db, &typed.join("new")).unwrap(),
            drive.join("Re\u{301}sume\u{301}s/new")
        );
        let elsewhere = dir.path().join("elsewhere");
        assert_eq!(as_indexed(&db, &elsewhere).unwrap(), elsewhere);
        let a = crate::wipe::assess(&db, std::slice::from_ref(&typed), None).unwrap();
        assert_eq!(a.items[0].files, 1, "{:?}", a.items[0]);
    }

    #[test]
    fn variants_already_there() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Report.pdf"), "a").unwrap();
        assert_eq!(variant_there(&dir.path().join("Report.pdf")), None);
        let v = variant_there(&dir.path().join("report.PDF"));
        if dir.path().join("report.PDF").exists() {
            // a case-insensitive filesystem: the other spelling is what's there
            assert_eq!(v, Some(os("Report.pdf")));
        }
        assert_eq!(
            free_name(&dir.path().join("Report.pdf")),
            dir.path().join("Report (2).pdf")
        );
        // what a case-insensitive drive would list
        let there = [os("Report.pdf"), os("Cafe\u{301}"), os("notes.txt")];
        assert_eq!(
            variant_among(&there, OsStr::new("report.PDF")),
            Some(os("Report.pdf"))
        );
        assert_eq!(variant_among(&there, OsStr::new("Report.pdf")), None);
        assert_eq!(
            variant_among(&there, OsStr::new("Caf\u{e9}")),
            None,
            "the other Unicode form is the same name to people"
        );
        assert_eq!(
            variant_among(&there, OsStr::new("CAF\u{c9}")),
            Some(os("Cafe\u{301}"))
        );
        assert_eq!(variant_among(&there, OsStr::new("other.txt")), None);
    }
}
