//! What things are: recognisers over the index.
//!
//! A drive isn't a heap of files to the person who owns it: it's "the mail server's maildir backup",
//! "my old MacBook's Time Machine", "Dad's iPhone backup from 2016". This module finds those
//! *entities* (APP-PLAN §6) with rules over the index, adds what the drive itself says about where
//! they came from (a backup script's rsync line, an iPhone backup's Info.plist, a system's
//! /etc/hostname, a repository's remote), and marks the parts that are regenerable junk (caches,
//! previews, trash) so they don't count as data that must be kept.
//!
//! It scales with the index, not with memory: two streaming passes collect only "marker" names
//! (`cur`, `Manifest.db`, `.git`, …), per-folder media counts and a few file lists; rules then decide
//! what the marked folders are, and each entity's dates come from a range scan of the path index.
//! Provenance needs the drive to be connected (it reads small text files, read-only); without it,
//! entities are still found, just with less said about them.
//!
//! Results are stored per root (table `entities`, one JSON document per root) with the scan they
//! describe, so the browser and reports show them at once and know when they're out of date.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{MAIN_SEPARATOR_STR, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{Db, browse, fmt, paths};

/// A recognised thing: a folder (or, for disk images and dumps, a file) with what it is.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entity {
    /// stable id of the kind: "mail-store", "timemachine", "git", … (see `KINDS`)
    pub kind: String,
    /// the kind for people: "Mail server store"
    pub label: String,
    /// one line about this one: "3 domains, 142 mailboxes"
    pub title: String,
    pub path: String,
    /// the browser's key for `path`
    pub key: String,
    pub is_file: bool,
    /// 0..1: how sure the rule is
    pub confidence: f32,
    pub files: u64,
    pub bytes: u64,
    /// modification times of the files inside (not when they were copied here)
    pub oldest: Option<i64>,
    pub newest: Option<i64>,
    /// the facts behind the title, in order ("Mailboxes" → "142")
    pub facts: Vec<(String, String)>,
    /// where it came from, as sentences ("Copied from mail.example.com:/var/vmail with rsync (backup-mail.sh).")
    pub provenance: Vec<String>,
    /// regenerable parts inside it
    pub junk: Vec<Junk>,
    /// the entity it sits in, if any (by key)
    pub inside: Option<String>,
    /// probably not worth keeping as a unit (a downloads pile, recovered fragments)
    pub low_value: bool,
}

/// A regenerable subtree: caches, thumbnails, previews, build output, trash.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Junk {
    pub path: String,
    pub key: String,
    pub why: String,
    pub files: u64,
    pub bytes: u64,
}

/// Everything recognised on one root, as of one scan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RootEntities {
    pub root_id: i64,
    pub root: String,
    pub scan: i64,
    pub computed_at: i64,
    /// the drive was connected, so provenance files could be read
    pub read_drive: bool,
    pub entities: Vec<Entity>,
    /// junk that isn't inside any entity
    pub junk: Vec<Junk>,
}

/// Kinds, their labels, and the order they're listed in.
pub const KINDS: &[(&str, &str)] = &[
    ("mail-store", "Mail server store"),
    ("maildir", "Mailbox (Maildir)"),
    ("mdbox", "Mail store (Dovecot mdbox)"),
    ("thunderbird", "Thunderbird profile"),
    ("nextcloud", "Nextcloud data"),
    ("nextcloud-server", "Nextcloud server"),
    ("timemachine", "Time Machine backup"),
    ("ios-backup", "iPhone or iPad backup"),
    ("android-backup", "Android backup"),
    ("system", "Operating system"),
    ("home-mac", "Mac home folder"),
    ("home-linux", "Linux home folder"),
    ("home-windows", "Windows user profile"),
    ("website", "Website"),
    ("git", "Git repository"),
    ("sql-dump", "Database dump"),
    ("disk-image", "Disk image"),
    ("photos", "Photo library"),
    ("music", "Music library"),
    ("videos", "Video library"),
    ("export", "Data export"),
    ("downloads", "Downloads pile"),
    ("recovery", "Recovered files"),
];

fn label(kind: &str) -> String {
    KINDS
        .iter()
        .find(|(k, _)| *k == kind)
        .map_or(kind, |(_, l)| l)
        .to_string()
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

// ---------- names

fn sep() -> Vec<u8> {
    paths::to_bytes(Path::new(MAIN_SEPARATOR_STR))
}

/// The last component of `path` below `parent`, as text (lossy for odd bytes).
fn child_name(parent: &[u8], path: &[u8], sep: &[u8]) -> Option<String> {
    let rest = path.strip_prefix(parent)?.strip_prefix(sep)?;
    Some(paths::from_bytes(rest).to_string_lossy().into_owned())
}

fn ext(name: &str) -> String {
    name.rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default()
}

/// Folder names that mean something on their own or next to their siblings.
fn dir_marker(lower: &str) -> bool {
    matches!(
        lower,
        "cur"
            | "new"
            | "tmp"
            | "maildir"
            | "storage"
            | "imapmail"
            | "mail"
            | ".git"
            | "objects"
            | "refs"
            | "backups.backupdb"
            | "dcim"
            | "appdata"
            | "library"
            | "desktop"
            | "documents"
            | "downloads"
            | "movies"
            | "music"
            | "pictures"
            | "public"
            | "videos"
            | "etc"
            | "usr"
            | "var"
            | "bin"
            | "sbin"
            | "lib"
            | "boot"
            | "opt"
            | "srv"
            | "root"
            | "home"
            | "proc"
            | "dev"
            | "windows"
            | "program files"
            | "users"
            | ".config"
            | ".local"
            | ".ssh"
            | ".cache"
            | "node_modules"
            | "__pycache__"
            | ".npm"
            | ".gradle"
            | ".m2"
            | "target"
            | ".venv"
            | "venv"
            | ".thumbnails"
            | "thumbnails"
            | "$recycle.bin"
            | "recycler"
            | "system volume information"
            | ".spotlight-v100"
            | ".fseventsd"
            | ".documentrevisions-v100"
            | ".temporaryitems"
            | ".trashes"
            | "caches"
            | "temp"
            | "preview"
            | "files_trashbin"
            | "files"
            | "wp-content"
            | "wp-admin"
            | "administrator"
            | "apps"
            | "core"
            | "config"
            | "twrp"
            | "titaniumbackup"
            | "android"
            | "takeout"
            | "lost+found"
            | "incoming"
            | ".stfolder"
            | ".sync"
            | ".dropbox.cache"
            | "vhosts"
            | "sites"
    ) || lower.starts_with("appdata_")
        || lower.starts_with("updater-")
        || lower.starts_with(".trash")
        || lower.starts_with("found.")
        || lower.starts_with("recup_dir")
        || lower.ends_with(".photoslibrary")
        || lower.ends_with(".sparsebundle")
        || lower.starts_with("takeout-")
        || lower.starts_with("facebook-")
        || lower.starts_with("instagram-")
        || lower.starts_with("twitter-")
        || lower.contains("linkedindataexport")
}

/// File names that mean something next to their siblings.
fn file_marker(lower: &str) -> bool {
    matches!(
        lower,
        "dovecot-uidlist"
            | "dovecot.index"
            | "dovecot.index.log"
            | "dovecot.map.index"
            | "dovecot.list.index"
            | "subscriptions"
            | "maildirfolder"
            | "manifest.db"
            | "manifest.mbdb"
            | "info.plist"
            | "status.plist"
            | ".ocdata"
            | "ntuser.dat"
            | "prefs.js"
            | ".bashrc"
            | ".zshrc"
            | ".profile"
            | ".bash_history"
            | ".zsh_history"
            | ".cfusertextencoding"
            | "wp-config.php"
            | "configuration.php"
            | "composer.json"
            | "artisan"
            | "index.php"
            | ".htaccess"
            | "head"
            | "config"
            | "description"
            | "hostname"
            | "os-release"
            | "archive_browser.html"
            | "cargo.toml"
            | "package.json"
            | "config.php"
            | "nextcloud.log"
            | ".nextcloudsync.log"
            | ".owncloudsync.log"
            | ".dropbox"
            | "id"
            | "thumbs.db"
            | ".ds_store"
    ) || lower.starts_with("._sync_")
        || lower.ends_with(".lrcat")
        || lower.ends_with(".msf")
}

const DISK_IMAGE: &[&str] = &[
    "img",
    "iso",
    "dd",
    "raw",
    "dmg",
    "vdi",
    "vmdk",
    "qcow2",
    "vhd",
    "vhdx",
    "sparseimage",
    "wim",
    "e01",
    "toast",
];
const AUDIO: &[&str] = &[
    "mp3", "m4a", "aac", "flac", "wav", "ogg", "oga", "opus", "wma", "aiff", "aif", "alac", "ape",
];
const VIDEO: &[&str] = &[
    "mp4", "m4v", "mov", "mkv", "webm", "avi", "wmv", "mpg", "mpeg", "3gp", "mts", "m2ts", "vob",
    "ts",
];
/// photographs: camera and phone formats (icons, screenshots and web graphics are PNG, GIF, SVG…)
const IMAGE: &[&str] = &[
    "jpg", "jpeg", "heic", "heif", "tif", "tiff", "raw", "cr2", "cr3", "nef", "arw", "dng", "orf",
    "rw2", "raf", "srw", "pef",
];
/// a library's files are real media, not icons or ringtones: average size at least this (audio, video, photos)
const MIN_AVERAGE: [u64; 3] = [1 << 20, 2 << 20, 50 << 10];
const INSTALLER: &[&str] = &[
    "exe", "msi", "dmg", "pkg", "deb", "rpm", "zip", "iso", "7z", "rar", "appimage", "apk", "tgz",
    "gz", "xz", "bz2", "tar",
];
const SCRIPT: &[&str] = &["sh", "bash", "bat", "cmd", "ps1", "cron", "command"];

fn is_sql_dump(lower: &str) -> bool {
    lower.ends_with(".sql")
        || lower.ends_with(".sql.gz")
        || lower.ends_with(".sql.bz2")
        || lower.ends_with(".sql.xz")
        || lower.ends_with(".sql.zst")
        || lower.ends_with(".dump")
        || lower.ends_with(".pgdump")
}

// ---------- what the passes collect

#[derive(Default)]
struct Markers {
    /// parent folder -> lower-case names of marker folders directly in it
    dirs: HashMap<Vec<u8>, HashSet<String>>,
    /// folder -> lower-case names of marker files directly in it
    files: HashMap<Vec<u8>, HashSet<String>>,
    /// folder -> [audio, video, image] files directly in it, and their bytes
    media: HashMap<Vec<u8>, [(u64, u64); 3]>,
    /// folder -> (files directly in it, of those: installers/archives)
    downloads: HashMap<Vec<u8>, (u64, u64)>,
    disk_images: Vec<(Vec<u8>, u64)>,
    sql_dumps: Vec<(Vec<u8>, u64)>,
    /// small scripts and crontabs, for provenance
    scripts: Vec<Vec<u8>>,
    /// possible ddrescue mapfiles (small .log/.map/.mapfile next to images)
    mapfiles: Vec<Vec<u8>>,
}

impl Markers {
    fn has_dir(&self, dir: &[u8], name: &str) -> bool {
        self.dirs.get(dir).is_some_and(|s| s.contains(name))
    }
    fn has_file(&self, dir: &[u8], name: &str) -> bool {
        self.files.get(dir).is_some_and(|s| s.contains(name))
    }
    fn dir_names(&self, dir: &[u8]) -> HashSet<String> {
        self.dirs.get(dir).cloned().unwrap_or_default()
    }
}

fn join(dir: &[u8], name: &str) -> Vec<u8> {
    paths::to_bytes(&paths::from_bytes(dir).join(name))
}

/// Folder rows directly inside `dir` (current), by name.
fn children(db: &Db, dir: &[u8]) -> Result<Vec<String>> {
    let sep = sep();
    let mut stmt = db
        .conn
        .prepare_cached("SELECT path FROM dirs WHERE parent = ?1 AND current = 1")?;
    let rows = stmt.query_map(params![dir], |r| r.get::<_, Vec<u8>>(0))?;
    let mut out = Vec::new();
    for p in rows {
        if let Some(n) = child_name(dir, &p?, &sep) {
            out.push(n);
        }
    }
    out.sort();
    Ok(out)
}

fn collect(db: &Db, root_id: i64, progress: &mut dyn FnMut(&str)) -> Result<Markers> {
    let sep = sep();
    let mut m = Markers::default();
    progress("reading folders");
    {
        let mut stmt = db
            .conn
            .prepare("SELECT path, parent FROM dirs WHERE root_id = ?1 AND current = 1 AND parent IS NOT NULL")?;
        let rows = stmt.query_map(params![root_id], |r| {
            Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        for row in rows {
            let (path, parent) = row?;
            let Some(name) = child_name(&parent, &path, &sep) else {
                continue;
            };
            let lower = name.to_lowercase();
            if dir_marker(&lower) {
                m.dirs.entry(parent).or_default().insert(lower);
            }
        }
    }
    progress("reading files");
    let mut stmt = db
        .conn
        .prepare("SELECT dir, path, size FROM files WHERE root_id = ?1 AND current = 1")?;
    let rows = stmt.query_map(params![root_id], |r| {
        Ok((
            r.get::<_, Option<Vec<u8>>>(0)?,
            r.get::<_, Vec<u8>>(1)?,
            r.get::<_, i64>(2)? as u64,
        ))
    })?;
    let mut n = 0u64;
    for row in rows {
        let (dir, path, size) = row?;
        n += 1;
        if n.is_multiple_of(500_000) {
            progress(&format!("reading files: {}", fmt::count(n)));
        }
        let Some(dir) = dir else { continue };
        let Some(name) = child_name(&dir, &path, &sep) else {
            continue;
        };
        let lower = name.to_lowercase();
        let e = ext(&lower);
        if file_marker(&lower) {
            m.files
                .entry(dir.clone())
                .or_default()
                .insert(lower.clone());
        }
        let kind = if AUDIO.contains(&e.as_str()) {
            Some(0)
        } else if VIDEO.contains(&e.as_str()) {
            Some(1)
        } else if IMAGE.contains(&e.as_str()) {
            Some(2)
        } else {
            None
        };
        if let Some(k) = kind {
            let e = &mut m.media.entry(dir.clone()).or_default()[k];
            e.0 += 1;
            e.1 += size;
        }
        let dl = m.downloads.entry(dir.clone()).or_default();
        dl.0 += 1;
        if INSTALLER.contains(&e.as_str()) {
            dl.1 += 1;
        }
        if DISK_IMAGE.contains(&e.as_str()) && size >= 1 << 20 {
            m.disk_images.push((path.clone(), size));
        }
        if is_sql_dump(&lower) && size >= 1024 {
            m.sql_dumps.push((path.clone(), size));
        }
        if (SCRIPT.contains(&e.as_str()) || lower == "crontab" || lower.starts_with("crontab"))
            && size <= 256 * 1024
        {
            m.scripts.push(path.clone());
        }
        if (e == "log" || e == "map" || e == "mapfile") && size <= 4 << 20 {
            m.mapfiles.push(path);
        }
    }
    // the downloads tally only matters for folders that could be a downloads pile
    m.downloads.retain(|dir, (files, _)| {
        *files >= 20 && {
            let name = paths::from_bytes(dir)
                .file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            matches!(
                name.as_str(),
                "downloads"
                    | "download"
                    | "incoming"
                    | "téléchargements"
                    | "descargas"
                    | "downloads (old)"
            )
        }
    });
    Ok(m)
}

// ---------- rules

struct Candidate {
    path: Vec<u8>,
    kind: &'static str,
    is_file: bool,
    confidence: f32,
    facts: Vec<(String, String)>,
    title: String,
    low_value: bool,
}

fn cand(path: Vec<u8>, kind: &'static str, confidence: f32) -> Candidate {
    Candidate {
        path,
        kind,
        is_file: false,
        confidence,
        facts: Vec::new(),
        title: String::new(),
        low_value: false,
    }
}

fn name_of(path: &[u8]) -> String {
    paths::from_bytes(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| paths::from_bytes(path).display().to_string())
}

fn parent_of(path: &[u8]) -> Option<Vec<u8>> {
    paths::from_bytes(path).parent().map(paths::to_bytes)
}

fn looks_like_domain(s: &str) -> bool {
    s.contains('.')
        && !s.starts_with('.')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

fn rules(db: &Db, root: &[u8], m: &Markers) -> Result<Vec<Candidate>> {
    let mut out = Vec::new();

    // ---- mail: Maildir folders (cur/new/tmp), grouped into stores
    let maildirs: HashSet<Vec<u8>> = m
        .dirs
        .iter()
        .filter(|(_, s)| s.contains("cur") && s.contains("new") && s.contains("tmp"))
        .map(|(d, _)| d.clone())
        .collect();
    // a mailbox is a Maildir that isn't a folder inside another one (Maildir++ subfolders are)
    let mut mailboxes: Vec<Vec<u8>> = maildirs
        .iter()
        .filter(|d| {
            let mut p = parent_of(d);
            while let Some(x) = p {
                if maildirs.contains(&x) {
                    return false;
                }
                if x.len() <= root.len() {
                    break;
                }
                p = parent_of(&x);
            }
            true
        })
        .cloned()
        .collect();
    mailboxes.sort();
    // the user's folder: "user/Maildir" layouts put the mailbox one level down
    let user_of = |mb: &[u8]| -> Vec<u8> {
        if name_of(mb).eq_ignore_ascii_case("maildir") {
            parent_of(mb).unwrap_or_else(|| mb.to_vec())
        } else {
            mb.to_vec()
        }
    };
    let mut by_store: BTreeMap<Vec<u8>, Vec<Vec<u8>>> = BTreeMap::new();
    let mut by_domain: BTreeMap<Vec<u8>, Vec<Vec<u8>>> = BTreeMap::new();
    for mb in &mailboxes {
        let user = user_of(mb);
        if let Some(domain) = parent_of(&user).filter(|d| d.len() > root.len()) {
            by_domain
                .entry(domain.clone())
                .or_default()
                .push(mb.clone());
            if let Some(store) = parent_of(&domain).filter(|s| s.len() >= root.len()) {
                by_store.entry(store).or_default().push(mb.clone());
            }
        }
    }
    let mut placed: HashSet<Vec<u8>> = HashSet::new();
    for (store, mbs) in &by_store {
        let domains: BTreeMap<String, usize> = mbs.iter().fold(BTreeMap::new(), |mut acc, mb| {
            if let Some(d) = parent_of(&user_of(mb)) {
                *acc.entry(name_of(&d)).or_default() += 1;
            }
            acc
        });
        if domains.len() >= 2 || (mbs.len() >= 5 && domains.keys().any(|d| looks_like_domain(d))) {
            let mut c = cand(store.clone(), "mail-store", 0.9);
            let named: Vec<&String> = domains.keys().filter(|d| looks_like_domain(d)).collect();
            c.title = format!(
                "{} mailbox{} in {} domain{}",
                mbs.len(),
                if mbs.len() == 1 { "" } else { "es" },
                domains.len(),
                if domains.len() == 1 { "" } else { "s" }
            );
            c.facts.push(("Mailboxes".into(), mbs.len().to_string()));
            c.facts.push((
                "Domains".into(),
                if named.is_empty() {
                    domains
                        .keys()
                        .take(8)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                } else {
                    named
                        .iter()
                        .take(8)
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                },
            ));
            placed.extend(mbs.iter().cloned());
            out.push(c);
        }
    }
    for (domain, mbs) in &by_domain {
        let rest: Vec<&Vec<u8>> = mbs.iter().filter(|mb| !placed.contains(*mb)).collect();
        if rest.len() >= 2 {
            let mut c = cand(domain.clone(), "mail-store", 0.85);
            let d = name_of(domain);
            c.title = format!(
                "{} mailboxes{}",
                rest.len(),
                if looks_like_domain(&d) {
                    format!(" for {d}")
                } else {
                    String::new()
                }
            );
            c.facts.push(("Mailboxes".into(), rest.len().to_string()));
            placed.extend(rest.into_iter().cloned());
            out.push(c);
        }
    }
    for mb in &mailboxes {
        if placed.contains(mb) {
            continue;
        }
        let user = user_of(mb);
        let dovecot = m.has_file(mb, "dovecot-uidlist") || m.has_file(mb, "dovecot.index");
        let mut c = cand(user.clone(), "maildir", if dovecot { 0.95 } else { 0.85 });
        let folders = maildirs
            .iter()
            .filter(|d| d.starts_with(mb) && d.len() > mb.len())
            .count();
        c.title = format!(
            "{} folder{}",
            folders + 1,
            if folders == 0 { "" } else { "s" }
        );
        c.facts.push(("Folders".into(), (folders + 1).to_string()));
        if dovecot {
            c.facts.push(("Server".into(), "Dovecot".into()));
        }
        out.push(c);
    }
    // Dovecot mdbox/sdbox
    for (dir, files) in &m.files {
        if files.contains("dovecot.map.index") && m.has_dir(dir, "storage") {
            let mut c = cand(dir.clone(), "mdbox", 0.9);
            c.title = "Dovecot mdbox storage".into();
            out.push(c);
        }
    }
    // Thunderbird profiles
    for (dir, files) in &m.files {
        if files.contains("prefs.js") && (m.has_dir(dir, "imapmail") || m.has_dir(dir, "mail")) {
            let mut c = cand(dir.clone(), "thunderbird", 0.9);
            let hosts: Vec<String> = children(db, &join(dir, "ImapMail"))?
                .into_iter()
                .chain(children(db, &join(dir, "Mail"))?)
                .filter(|h| h != "Local Folders" && h != "smart mailboxes")
                .collect();
            c.title = if hosts.is_empty() {
                "mail profile".into()
            } else {
                format!("mail from {}", hosts.join(", "))
            };
            if !hosts.is_empty() {
                c.facts.push(("Accounts".into(), hosts.join(", ")));
            }
            out.push(c);
        }
    }

    // ---- Nextcloud: the data directory, and the server's web root
    for (dir, files) in &m.files {
        if files.contains(".ocdata") {
            let mut c = cand(dir.clone(), "nextcloud", 0.95);
            let users: Vec<String> = children(db, dir)?
                .into_iter()
                .filter(|u| {
                    !u.starts_with("appdata_")
                        && !u.starts_with("updater-")
                        && !u.starts_with('.')
                        && !u.starts_with("files_external")
                })
                .filter(|u| m.has_dir(&join(dir, u), "files"))
                .collect();
            c.title = format!(
                "{} user{}",
                users.len(),
                if users.len() == 1 { "" } else { "s" }
            );
            c.facts.push((
                "Users".into(),
                users
                    .iter()
                    .take(12)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
            out.push(c);
        }
    }
    for (dir, names) in &m.dirs {
        if names.contains("apps")
            && names.contains("core")
            && names.contains("config")
            && m.has_file(&join(dir, "config"), "config.php")
        {
            let mut c = cand(dir.clone(), "nextcloud-server", 0.85);
            c.title = "Nextcloud or ownCloud web root".into();
            out.push(c);
        }
    }

    // ---- Time Machine
    for (dir, names) in &m.dirs {
        if names.contains("backups.backupdb") {
            let db_dir = join(dir, "Backups.backupdb");
            let hosts = children(db, &db_dir)?;
            let mut c = cand(db_dir, "timemachine", 0.99);
            let mut snaps = 0;
            let mut first: Option<String> = None;
            let mut last: Option<String> = None;
            for h in &hosts {
                for s in children(db, &join(&c.path, h))? {
                    if s.len() >= 17
                        && s.as_bytes()[4] == b'-'
                        && s[..4].chars().all(|ch| ch.is_ascii_digit())
                    {
                        snaps += 1;
                        let day = s[..10].to_string();
                        if first.as_ref().is_none_or(|f| &day < f) {
                            first = Some(day.clone());
                        }
                        if last.as_ref().is_none_or(|l| &day > l) {
                            last = Some(day);
                        }
                    }
                }
            }
            c.title = format!(
                "{} from {}",
                if snaps == 1 {
                    "1 snapshot".to_string()
                } else {
                    format!("{snaps} snapshots")
                },
                hosts.join(", ")
            );
            c.facts.push(("Computers".into(), hosts.join(", ")));
            c.facts.push(("Snapshots".into(), snaps.to_string()));
            if let (Some(f), Some(l)) = (first, last) {
                c.facts.push(("Span".into(), format!("{f} – {l}")));
            }
            out.push(c);
        }
        for n in names.iter().filter(|n| n.ends_with(".sparsebundle")) {
            let mut c = cand(join(dir, n), "disk-image", 0.9);
            c.title = "sparse bundle (network Time Machine or disk image)".into();
            c.facts
                .push(("Format".into(), "Apple sparse bundle".into()));
            out.push(c);
        }
    }

    // ---- phones
    for (dir, files) in &m.files {
        if (files.contains("manifest.db") || files.contains("manifest.mbdb"))
            && files.contains("info.plist")
        {
            let mut c = cand(
                dir.clone(),
                "ios-backup",
                if files.contains("status.plist") {
                    0.97
                } else {
                    0.9
                },
            );
            c.title = "device backup".into();
            out.push(c);
        }
    }
    for (dir, names) in &m.dirs {
        if names.contains("twrp") {
            let mut c = cand(join(dir, "TWRP"), "android-backup", 0.85);
            c.title = "TWRP recovery backups".into();
            out.push(c);
        }
        if names.contains("titaniumbackup") {
            let mut c = cand(join(dir, "TitaniumBackup"), "android-backup", 0.8);
            c.title = "Titanium Backup".into();
            out.push(c);
        }
    }

    // ---- systems and home folders
    const SYS: &[&str] = &[
        "bin", "boot", "etc", "home", "lib", "opt", "root", "sbin", "srv", "usr", "var", "proc",
        "dev",
    ];
    for (dir, names) in &m.dirs {
        let hits = SYS.iter().filter(|s| names.contains(**s)).count();
        if names.contains("etc") && names.contains("usr") && hits >= 5 {
            let mut c = cand(
                dir.clone(),
                "system",
                if m.has_file(&join(dir, "etc"), "hostname") {
                    0.95
                } else {
                    0.85
                },
            );
            c.title = "Linux or Unix system files".into();
            out.push(c);
        }
        if names.contains("windows") && (names.contains("program files") || names.contains("users"))
        {
            let mut c = cand(dir.clone(), "system", 0.9);
            c.title = "Windows installation".into();
            out.push(c);
        }
        let mac = [
            "desktop",
            "documents",
            "downloads",
            "movies",
            "music",
            "pictures",
            "public",
        ]
        .iter()
        .filter(|s| names.contains(**s))
        .count();
        if names.contains("library") && mac >= 3 {
            let mut c = cand(
                dir.clone(),
                "home-mac",
                if m.has_file(dir, ".cfusertextencoding") {
                    0.95
                } else {
                    0.8
                },
            );
            c.title = format!("user {}", name_of(dir));
            out.push(c);
        } else if names.contains("appdata")
            && (names.contains("desktop") || names.contains("documents"))
            || m.has_file(dir, "ntuser.dat")
        {
            let mut c = cand(
                dir.clone(),
                "home-windows",
                if m.has_file(dir, "ntuser.dat") {
                    0.95
                } else {
                    0.8
                },
            );
            c.title = format!("user {}", name_of(dir));
            out.push(c);
        }
    }
    for (dir, files) in &m.files {
        let dots = [
            ".bashrc",
            ".zshrc",
            ".profile",
            ".bash_history",
            ".zsh_history",
        ]
        .iter()
        .filter(|f| files.contains(**f))
        .count();
        let dirs = m.dir_names(dir);
        let cfg = [".config", ".local", ".ssh", ".cache"]
            .iter()
            .filter(|d| dirs.contains(**d))
            .count();
        if dots >= 1 && dots + cfg >= 3 && !dirs.contains("library") {
            let mut c = cand(
                dir.clone(),
                "home-linux",
                (0.6 + 0.08 * (dots + cfg) as f32).min(0.95),
            );
            c.title = format!("user {}", name_of(dir));
            out.push(c);
        }
    }

    // ---- websites
    for (dir, files) in &m.files {
        let dirs = m.dir_names(dir);
        let (cms, conf) = if files.contains("wp-config.php")
            || (dirs.contains("wp-content") && dirs.contains("wp-admin"))
        {
            ("WordPress", 0.95)
        } else if files.contains("configuration.php") && dirs.contains("administrator") {
            ("Joomla", 0.9)
        } else if files.contains("artisan") && files.contains("composer.json") {
            ("Laravel", 0.85)
        } else if files.contains("index.php") && files.contains(".htaccess") {
            ("PHP", 0.6)
        } else {
            continue;
        };
        let mut c = cand(dir.clone(), "website", conf);
        c.title = format!("{cms} site {}", name_of(dir));
        c.facts.push(("Software".into(), cms.into()));
        out.push(c);
    }

    // ---- git repositories (a working copy's .git, or a bare repository)
    for (dir, names) in &m.dirs {
        if names.contains(".git") && m.has_file(&join(dir, ".git"), "head") {
            let mut c = cand(dir.clone(), "git", 0.99);
            c.title = name_of(dir);
            out.push(c);
        } else if names.contains("objects")
            && names.contains("refs")
            && m.has_file(dir, "head")
            && m.has_file(dir, "config")
        {
            let mut c = cand(dir.clone(), "git", 0.9);
            c.title = format!("{} (bare)", name_of(dir));
            out.push(c);
        }
    }

    // ---- files that are things: disk images, database dumps
    for (path, size) in &m.disk_images {
        let mut c = cand(path.clone(), "disk-image", 0.85);
        c.is_file = true;
        c.title = format!("{} image", ext(&name_of(path)).to_uppercase());
        c.facts.push(("Size".into(), fmt::bytes(*size)));
        out.push(c);
    }
    let mut dumps_by_dir: BTreeMap<Vec<u8>, Vec<(Vec<u8>, u64)>> = BTreeMap::new();
    for (path, size) in &m.sql_dumps {
        if let Some(d) = parent_of(path) {
            dumps_by_dir
                .entry(d)
                .or_default()
                .push((path.clone(), *size));
        }
    }
    for (dir, dumps) in dumps_by_dir {
        if dumps.len() >= 3 {
            let mut c = cand(dir.clone(), "sql-dump", 0.85);
            c.title = format!("{} database dumps", dumps.len());
            c.facts.push(("Dumps".into(), dumps.len().to_string()));
            out.push(c);
        } else {
            for (path, size) in dumps {
                let mut c = cand(path.clone(), "sql-dump", 0.75);
                c.is_file = true;
                c.title = name_of(&path);
                c.facts.push(("Size".into(), fmt::bytes(size)));
                out.push(c);
            }
        }
    }

    // ---- exports, downloads, recovered files
    for (dir, names) in &m.dirs {
        for n in names {
            let (what, conf) = if n == "takeout" || n.starts_with("takeout-") {
                (
                    "Google Takeout",
                    if m.has_file(&join(dir, n), "archive_browser.html") {
                        0.95
                    } else {
                        0.8
                    },
                )
            } else if n.starts_with("facebook-") {
                ("Facebook export", 0.8)
            } else if n.starts_with("instagram-") {
                ("Instagram export", 0.75)
            } else if n.starts_with("twitter-") {
                ("Twitter archive", 0.75)
            } else if n.contains("linkedindataexport") {
                ("LinkedIn export", 0.85)
            } else {
                continue;
            };
            let real = children(db, dir)?
                .into_iter()
                .find(|c| c.to_lowercase() == *n)
                .unwrap_or_else(|| n.clone());
            let mut c = cand(join(dir, &real), "export", conf);
            c.title = what.into();
            out.push(c);
        }
        for n in names.iter().filter(|n| {
            n.starts_with("found.") || n.starts_with("recup_dir") || n.as_str() == "lost+found"
        }) {
            let real = children(db, dir)?
                .into_iter()
                .find(|c| c.to_lowercase() == *n)
                .unwrap_or_else(|| n.clone());
            let mut c = cand(join(dir, &real), "recovery", 0.8);
            c.title = if n.starts_with("recup_dir") {
                "PhotoRec output".into()
            } else if n.starts_with("found.") {
                "chkdsk fragments".into()
            } else {
                "fsck fragments".into()
            };
            c.low_value = true;
            out.push(c);
        }
    }
    for (dir, (files, installers)) in &m.downloads {
        if *installers * 10 >= *files * 3 {
            let mut c = cand(dir.clone(), "downloads", 0.6);
            c.title = format!("{installers} installers and archives among {files} files");
            c.low_value = true;
            out.push(c);
        }
    }
    Ok(out)
}

/// Photo, music and video libraries: the highest folders where one kind of media makes up most
/// of the files, with at least a few hundred of them (or a camera's DCIM / an Apple Photos library).
fn media_libraries(db: &Db, root: &[u8], m: &Markers) -> Result<Vec<Candidate>> {
    let mut rolled: HashMap<Vec<u8>, [(u64, u64); 3]> = HashMap::new();
    for (dir, counts) in &m.media {
        let mut d = Some(dir.clone());
        while let Some(x) = d {
            let e = rolled.entry(x.clone()).or_default();
            for (total, n) in e.iter_mut().zip(counts) {
                total.0 += n.0;
                total.1 += n.1;
            }
            if x.len() <= root.len() {
                break;
            }
            d = parent_of(&x);
        }
    }
    let mut totals = db
        .conn
        .prepare_cached("SELECT files FROM dirs WHERE path = ?1 AND current = 1")?;
    let mut qualifying: HashMap<Vec<u8>, (usize, f32)> = HashMap::new();
    for (dir, c) in &rolled {
        let files: i64 = totals
            .query_row(params![dir], |r| r.get(0))
            .optional()?
            .unwrap_or(0);
        if files == 0 {
            continue;
        }
        for (k, &(n, bytes)) in c.iter().enumerate() {
            let share = n as f32 / files as f32;
            if n >= 200 && share >= 0.6 && bytes / n >= MIN_AVERAGE[k] {
                qualifying.insert(dir.clone(), (k, share));
            }
        }
    }
    let mut out = Vec::new();
    let kinds = [
        ("music", "tracks"),
        ("videos", "videos"),
        ("photos", "photos"),
    ];
    let mut chosen: HashSet<Vec<u8>> = HashSet::new();
    for (dir, (k, _)) in &qualifying {
        // start from the highest qualifying folder (its parent doesn't qualify for the same kind)…
        if parent_of(dir).is_some_and(|p| {
            p.len() >= root.len() && qualifying.get(&p).is_some_and(|(pk, _)| pk == k)
        }) {
            continue;
        }
        // …and while it's still a mix, go down into the child that holds nearly all of this media:
        // a drive that is 70% photos isn't a photo library, its Photos folder is
        let mut at = dir.clone();
        while qualifying[&at].1 < 0.9 {
            let Some(child) = qualifying.iter().find(|(c, (ck, _))| {
                ck == k
                    && parent_of(c).as_deref() == Some(&at[..])
                    && rolled[*c][*k].0 * 10 >= rolled[&at][*k].0 * 9
            }) else {
                break;
            };
            at = child.0.clone();
        }
        if !chosen.insert(at.clone()) {
            continue;
        }
        let (kind, noun) = kinds[*k];
        let share = qualifying[&at].1;
        let mut c = cand(at.clone(), kind, (0.5 + share * 0.45).min(0.95));
        c.title = format!("{} {noun}", fmt::count(rolled[&at][*k].0));
        c.facts.push((
            noun[..1].to_uppercase() + &noun[1..],
            rolled[&at][*k].0.to_string(),
        ));
        out.push(c);
    }
    // camera card dumps and photo apps' libraries, whatever their size
    for (dir, names) in &m.dirs {
        if names.contains("dcim") && !qualifying.contains_key(&join(dir, "DCIM")) {
            let mut c = cand(join(dir, "DCIM"), "photos", 0.8);
            c.title = "camera or phone photos (DCIM)".into();
            out.push(c);
        }
        for n in names.iter().filter(|n| n.ends_with(".photoslibrary")) {
            let real = children(db, dir)?
                .into_iter()
                .find(|c| c.to_lowercase() == *n)
                .unwrap_or_else(|| n.clone());
            let mut c = cand(join(dir, &real), "photos", 0.95);
            c.title = "Apple Photos library".into();
            out.push(c);
        }
    }
    for (dir, files) in &m.files {
        if files.iter().any(|f| f.ends_with(".lrcat")) {
            let mut c = cand(dir.clone(), "photos", 0.85);
            c.title = "Lightroom catalogue".into();
            out.push(c);
        }
    }
    Ok(out)
}

/// Regenerable subtrees, by name (and for build output, by what's next to it).
fn junk_dirs(m: &Markers) -> Vec<(Vec<u8>, &'static str)> {
    let mut out = Vec::new();
    for (parent, names) in &m.dirs {
        let files = m.files.get(parent);
        for n in names {
            let why = match n.as_str() {
                "node_modules" => "npm packages (reinstallable)",
                "__pycache__" => "Python bytecode cache",
                ".cache" => "application caches",
                ".npm" => "npm cache",
                ".gradle" => "Gradle caches",
                ".thumbnails" | "thumbnails" => "thumbnail cache",
                "$recycle.bin" | "recycler" => "Windows recycle bin",
                ".spotlight-v100" => "macOS Spotlight index",
                ".fseventsd" => "macOS file event log",
                ".documentrevisions-v100" => "macOS document versions store",
                ".temporaryitems" => "macOS temporary items",
                ".trashes" => "macOS trash",
                "system volume information" => "Windows system restore data",
                "caches" if name_of(parent).eq_ignore_ascii_case("library") => "macOS app caches",
                "temp" if name_of(parent).eq_ignore_ascii_case("local") => {
                    "Windows temporary files"
                }
                "target" if files.is_some_and(|f| f.contains("cargo.toml")) => "Rust build output",
                ".venv" | "venv" => "Python virtual environment",
                "preview" if name_of(parent).starts_with("appdata_") => "Nextcloud previews",
                "files_trashbin" => "Nextcloud trash",
                n if n.starts_with("updater-") => "Nextcloud updater backups",
                n if n.starts_with(".trash") => "desktop trash",
                _ => continue,
            };
            out.push((join(parent, n), why));
        }
    }
    out
}

// ---------- provenance

/// A copy the drive's own scripts describe: "rsync -a host:/var/vmail/ /mnt/backup/mail/".
#[derive(Debug, Clone, PartialEq)]
pub struct Transfer {
    pub tool: &'static str,
    pub sources: Vec<String>,
    pub dest: String,
}

/// Copy commands in a shell script, batch file or crontab: rsync, scp, rclone, cp -a, robocopy,
/// tar -c. Variables assigned earlier in the script (`DEST=/mnt/x`) are substituted.
pub fn parse_script(text: &str) -> Vec<Transfer> {
    // in batch files and PowerShell a backslash is a path separator, not an escape
    let lower = text.to_ascii_lowercase();
    let windows = text.contains("\r\n")
        || lower.contains("@echo")
        || lower.contains("robocopy")
        || lower.contains("set ");
    let mut vars: HashMap<String, String> = HashMap::new();
    let mut out = Vec::new();
    let joined = text.replace("\\\r\n", " ").replace("\\\n", " ");
    for raw in joined.lines() {
        let line = raw.trim();
        if line.starts_with('#')
            || line.starts_with("::")
            || line.to_ascii_lowercase().starts_with("rem ")
        {
            continue;
        }
        // NAME=value, export NAME=value, set NAME=value (batch)
        let assign = line
            .strip_prefix("export ")
            .or_else(|| line.strip_prefix("set "))
            .unwrap_or(line);
        if let Some((k, v)) = assign.split_once('=')
            && !k.is_empty()
            && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !v.starts_with('=')
        {
            let v = substitute(unquote(v.trim()), &vars);
            vars.insert(k.to_string(), v);
            continue;
        }
        let line = substitute(line.to_string(), &vars);
        for part in line
            .split(['|', ';'])
            .flat_map(|p| p.split("&&"))
            .flat_map(|p| p.split("||"))
        {
            let words = shell_words(part, !windows);
            if let Some(t) = transfer(&words) {
                out.push(t);
            }
        }
    }
    out
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

fn substitute(mut s: String, vars: &HashMap<String, String>) -> String {
    for (k, v) in vars {
        s = s
            .replace(&format!("${{{k}}}"), v)
            .replace(&format!("${k}"), v)
            .replace(&format!("%{k}%"), v);
    }
    s
}

fn shell_words(s: &str, escapes: bool) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                any = true;
            }
            (None, '\\') if escapes => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            (None, c) if c.is_whitespace() => {
                if !cur.is_empty() || any {
                    words.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if !cur.is_empty() || any {
        words.push(cur);
    }
    words
}

/// rsync options that take the next word as their argument
const RSYNC_ARG: &[&str] = &[
    "-e",
    "--rsh",
    "-f",
    "--filter",
    "--exclude",
    "--include",
    "--exclude-from",
    "--include-from",
    "--files-from",
    "--rsync-path",
    "--log-file",
    "-T",
    "--temp-dir",
    "--password-file",
    "--port",
    "--bwlimit",
    "--timeout",
    "--contimeout",
    "--link-dest",
    "--compare-dest",
    "--copy-dest",
    "--backup-dir",
    "--suffix",
    "--chmod",
    "--chown",
    "--info",
    "--debug",
    "--max-size",
    "--min-size",
    "--partial-dir",
    "--out-format",
    "--log-file-format",
    "--iconv",
    "-M",
    "--remote-option",
    "--usermap",
    "--groupmap",
    "--max-delete",
    "--modify-window",
    "-B",
    "--block-size",
    "--address",
    "--compress-level",
    "--skip-compress",
];

fn transfer(words: &[String]) -> Option<Transfer> {
    let pos = words.iter().position(|w| {
        let base = w
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(w)
            .to_ascii_lowercase();
        matches!(
            base.as_str(),
            "rsync" | "scp" | "rclone" | "cp" | "robocopy" | "robocopy.exe" | "tar" | "gtar"
        )
    })?;
    let tool_word = words[pos]
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(&words[pos])
        .to_ascii_lowercase();
    let rest = &words[pos + 1..];
    let (tool, args): (&'static str, Vec<String>) = match tool_word.as_str() {
        "rsync" => {
            let mut args = Vec::new();
            let mut skip = false;
            for w in rest {
                if skip {
                    skip = false;
                } else if RSYNC_ARG.contains(&w.as_str()) {
                    skip = true;
                } else if !w.starts_with('-') {
                    args.push(w.clone());
                }
            }
            ("rsync", args)
        }
        "scp" => {
            let mut args = Vec::new();
            let mut skip = false;
            for w in rest {
                if skip {
                    skip = false;
                } else if ["-P", "-i", "-o", "-F", "-c", "-l", "-S", "-J"].contains(&w.as_str()) {
                    skip = true;
                } else if !w.starts_with('-') {
                    args.push(w.clone());
                }
            }
            ("scp", args)
        }
        "rclone" => {
            let sub = rest.first()?;
            if !["sync", "copy", "move", "copyto"].contains(&sub.as_str()) {
                return None;
            }
            (
                "rclone",
                rest[1..]
                    .iter()
                    .filter(|w| !w.starts_with('-'))
                    .cloned()
                    .collect(),
            )
        }
        "cp" => {
            if !rest.iter().any(|w| {
                w.starts_with('-')
                    && !w.starts_with("--")
                    && (w.contains('a') || w.contains('r') || w.contains('R'))
                    || w == "--archive"
                    || w == "--recursive"
            }) {
                return None;
            }
            (
                "cp",
                rest.iter()
                    .filter(|w| !w.starts_with('-'))
                    .cloned()
                    .collect(),
            )
        }
        "robocopy" | "robocopy.exe" => (
            "robocopy",
            rest.iter()
                .filter(|w| !w.starts_with('/'))
                .take(2)
                .cloned()
                .collect(),
        ),
        _ => {
            // tar: only creating an archive (-c / c…), its -f argument is where it goes
            let flags = rest.first()?;
            if !(flags.contains('c')
                && (flags.starts_with('-') || flags.chars().all(|c| c.is_ascii_alphabetic())))
            {
                return None;
            }
            let mut dest = None;
            let mut sources = Vec::new();
            let mut f_next = flags.ends_with('f') && !flags.starts_with("--");
            for w in &rest[1..] {
                if f_next {
                    dest = Some(w.clone());
                    f_next = false;
                } else if w == "-f" || w == "--file" {
                    f_next = true;
                } else if let Some(v) = w.strip_prefix("--file=") {
                    dest = Some(v.to_string());
                } else if !w.starts_with('-') {
                    sources.push(w.clone());
                }
            }
            return Some(Transfer {
                tool: "tar",
                sources,
                dest: dest?,
            });
        }
    };
    if args.len() < 2 {
        return None;
    }
    let (dest, sources) = args.split_last()?;
    Some(Transfer {
        tool,
        sources: sources.to_vec(),
        dest: dest.clone(),
    })
}

/// "user@host:/path" -> host; a local path -> None
fn remote_host(src: &str) -> Option<String> {
    if src.contains("://") {
        return src
            .split("://")
            .nth(1)?
            .split(['/', ':'])
            .next()
            .map(|h| h.rsplit('@').next().unwrap_or(h).to_string());
    }
    let (host, _) = src.split_once(':')?;
    if host.len() <= 1 || host.contains('/') || host.contains('\\') {
        return None; // C:\… or a path with a colon
    }
    Some(host.rsplit('@').next().unwrap_or(host).to_string())
}

/// The last `n` components of a slash- or backslash-separated path, lower-cased.
fn tail(p: &str, n: usize) -> Vec<String> {
    let parts: Vec<String> = p
        .split(['/', '\\'])
        .filter(|s| !s.is_empty() && *s != ".")
        .map(|s| s.to_lowercase())
        .collect();
    parts[parts.len().saturating_sub(n)..].to_vec()
}

fn read_small(path: &Path, max: u64) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > max {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// A value from an XML property list (iOS backups' Info.plist): the text after `<key>k</key>`.
fn plist_value(xml: &str, key: &str) -> Option<String> {
    let at = xml.find(&format!("<key>{key}</key>"))? + key.len() + 11;
    let rest = xml[at..].trim_start();
    let open = rest.find('>')? + 1;
    let close = rest[open..].find('<')?;
    let v = rest[open..open + close].trim();
    (!v.is_empty()).then(|| v.to_string())
}

fn provenance(
    e: &mut Entity,
    root: &Path,
    path: &Path,
    scripts: &[(PathBuf, Vec<Transfer>)],
    sync_markers: &[(Vec<u8>, &'static str)],
) {
    match e.kind.as_str() {
        "ios-backup" => {
            if let Some(x) = read_small(&path.join("Info.plist"), 2 << 20) {
                if x.starts_with("bplist") {
                    e.provenance.push(
                        "Device details are in a binary property list (not read yet).".into(),
                    );
                } else {
                    let device = plist_value(&x, "Device Name");
                    let product =
                        plist_value(&x, "Product Name").or_else(|| plist_value(&x, "Product Type"));
                    let date = plist_value(&x, "Last Backup Date");
                    if let Some(d) = &device {
                        e.title = format!("backup of {d}");
                        e.facts.push(("Device".into(), d.clone()));
                    }
                    if let Some(p) = product {
                        e.facts.push(("Model".into(), p));
                    }
                    if let Some(d) = date {
                        e.facts
                            .push(("Last backup".into(), d.chars().take(10).collect()));
                    }
                }
            }
        }
        "system" => {
            let host = read_small(&path.join("etc").join("hostname"), 4096)
                .map(|h| h.trim().to_string())
                .filter(|h| !h.is_empty());
            let os = read_small(&path.join("etc").join("os-release"), 64 * 1024).and_then(|t| {
                t.lines()
                    .find_map(|l| l.strip_prefix("PRETTY_NAME=").map(unquote))
            });
            if let Some(h) = &host {
                e.facts.push(("Host name".into(), h.clone()));
            }
            if let Some(o) = &os {
                e.facts.push(("System".into(), o.clone()));
            }
            if host.is_some() || os.is_some() {
                e.title = match (host, os) {
                    (Some(h), Some(o)) => format!("{o} ({h})"),
                    (Some(h), None) => format!("system of {h}"),
                    (None, Some(o)) => o,
                    _ => unreachable!(),
                };
            }
        }
        "git" => {
            let cfg = if e.title.ends_with("(bare)") {
                path.join("config")
            } else {
                path.join(".git").join("config")
            };
            if let Some(t) = read_small(&cfg, 256 * 1024) {
                let mut in_origin = false;
                for l in t.lines() {
                    let l = l.trim();
                    if l.starts_with('[') {
                        in_origin = l.contains("remote \"origin\"");
                    } else if in_origin
                        && let Some(url) = l
                            .strip_prefix("url")
                            .map(|r| r.trim_start_matches([' ', '=']).trim())
                    {
                        e.facts.push(("Remote".into(), url.to_string()));
                        e.provenance.push(format!("Clone of {url}."));
                    }
                }
            }
            let head = if e.title.ends_with("(bare)") {
                path.join("HEAD")
            } else {
                path.join(".git").join("HEAD")
            };
            if let Some(h) = read_small(&head, 4096).and_then(|h| {
                h.trim()
                    .strip_prefix("ref: refs/heads/")
                    .map(str::to_string)
            }) {
                e.facts.push(("Branch".into(), h));
            }
        }
        "disk-image" if e.is_file => {
            // a ddrescue mapfile next to the image says which device it was read from
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            if let Some(dir) = path.parent() {
                for ext in ["log", "map", "mapfile"] {
                    let cands = [
                        dir.join(format!("{stem}.{ext}")),
                        dir.join(format!(
                            "{}.{ext}",
                            path.file_name().unwrap_or_default().to_string_lossy()
                        )),
                    ];
                    for c in cands {
                        if let Some(t) = read_small(&c, 4 << 20)
                            && t.contains("ddrescue")
                        {
                            let cmd = t
                                .lines()
                                .find_map(|l| l.strip_prefix("# Command line:").map(str::trim));
                            let dev = cmd.and_then(|c| {
                                c.split_whitespace()
                                    .skip(1)
                                    .find(|w| !w.starts_with('-'))
                                    .map(str::to_string)
                            });
                            e.provenance.push(match dev {
                                Some(d) => format!(
                                    "Rescued from {d} with GNU ddrescue ({}).",
                                    c.file_name().unwrap_or_default().to_string_lossy()
                                ),
                                None => format!(
                                    "Made with GNU ddrescue ({}).",
                                    c.file_name().unwrap_or_default().to_string_lossy()
                                ),
                            });
                            if let Some(d) = cmd {
                                e.facts.push(("ddrescue".into(), d.to_string()));
                            }
                            return;
                        }
                    }
                }
            }
        }
        "sql-dump" if e.is_file => {
            if let Ok(mut f) = std::fs::File::open(path) {
                use std::io::Read;
                let mut head = vec![0u8; 4096];
                let n = f.read(&mut head).unwrap_or(0);
                let t = String::from_utf8_lossy(&head[..n]);
                let engine = if t.contains("MariaDB dump") {
                    Some("MariaDB")
                } else if t.contains("MySQL dump") {
                    Some("MySQL")
                } else if t.contains("PostgreSQL database dump") {
                    Some("PostgreSQL")
                } else if t.starts_with("PGDMP") {
                    Some("PostgreSQL (custom format)")
                } else {
                    None
                };
                if let Some(en) = engine {
                    e.confidence = 0.95;
                    e.facts.push(("Engine".into(), en.into()));
                }
                if let Some(l) = t
                    .lines()
                    .find(|l| l.contains("Host:") && l.contains("Database:"))
                {
                    let host = l
                        .split("Host:")
                        .nth(1)
                        .and_then(|r| r.split_whitespace().next())
                        .unwrap_or("");
                    let dbn = l.split("Database:").nth(1).map(str::trim).unwrap_or("");
                    if !dbn.is_empty() {
                        e.facts.push(("Database".into(), dbn.to_string()));
                        e.title = format!("{dbn} ({})", engine.unwrap_or("SQL"));
                    }
                    if !host.is_empty() {
                        e.provenance.push(format!("Dumped from {host}."));
                    }
                }
            }
        }
        _ => {}
    }
    // sync tools that leave a marker in the folder
    for (dir, tool) in sync_markers {
        if paths::from_bytes(dir) == path {
            e.provenance.push(format!("Kept in sync by {tool}."));
        }
    }
    // backup scripts on the same drive that copy something to (or from) a folder with this name
    // Compare the script's destination with this thing's place *on the drive*: the drive was
    // mounted somewhere else when the script ran (/mnt/backup), so only the part below the drive's
    // root still means anything. "/mnt/backup/vmail/" matches "vmail" at the drive's top level,
    // and "…/backups/mail/vmail" matches "backups/mail/vmail".
    let on_drive: Vec<String> = path
        .strip_prefix(root)
        .map(|r| tail(&r.display().to_string(), usize::MAX))
        .unwrap_or_default();
    for (script, transfers) in scripts {
        for t in transfers {
            let dest = tail(&t.dest, usize::MAX);
            let n = on_drive.len().min(dest.len());
            if n == 0 || on_drive[on_drive.len() - n..] != dest[dest.len() - n..] {
                continue;
            }
            let from = t
                .sources
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let host = t.sources.iter().find_map(|s| remote_host(s));
            let s = format!(
                "Copied from {from} with {} ({}).",
                t.tool,
                script.file_name().unwrap_or_default().to_string_lossy()
            );
            if !e.provenance.contains(&s) {
                e.provenance.push(s);
            }
            if let Some(h) = host
                && !e.facts.iter().any(|(k, _)| k == "Source host")
            {
                e.facts.push(("Source host".into(), h));
            }
        }
    }
}

// ---------- putting it together

/// Recognise everything on one root, as of its latest scan. `read_drive` lets it open small files
/// on the drive for provenance (skipped anyway when the drive isn't there).
pub fn recognise(
    db: &Db,
    root_id: i64,
    read_drive: bool,
    progress: &mut dyn FnMut(&str),
) -> Result<RootEntities> {
    let (root_bytes, scan): (Vec<u8>, Option<i64>) = db.conn.query_row(
        "SELECT path, last_scan FROM roots WHERE id = ?1",
        params![root_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let scan = scan.context("this root has no finished scan yet")?;
    let root_path = paths::from_bytes(&root_bytes);
    let read_drive = read_drive && root_path.is_dir();
    let m = collect(db, root_id, progress)?;
    progress("recognising");
    let mut cands = rules(db, &root_bytes, &m)?;
    cands.extend(media_libraries(db, &root_bytes, &m)?);
    let junk_list = junk_dirs(&m);
    let junk_prefixes: Vec<Vec<u8>> = junk_list
        .iter()
        .map(|(p, _)| paths::dir_prefix(&paths::from_bytes(p)))
        .collect();
    let in_junk = |p: &[u8]| junk_prefixes.iter().any(|j| p.starts_with(j));

    // one entity per path (the more certain rule wins), none inside junk
    cands.retain(|c| !in_junk(&c.path));
    cands.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then(b.confidence.total_cmp(&a.confidence))
    });
    cands.dedup_by(|b, a| a.path == b.path);
    // containers whose insides aren't separate things: snapshots in a Time Machine set, mailboxes
    // in a store, libraries inside backups, submodules inside repositories
    let swallows = |outer: &str, inner: &str| match outer {
        "timemachine" | "ios-backup" | "android-backup" => true,
        "mail-store" | "mdbox" => inner == "maildir",
        "git" => inner == "git",
        "system" => matches!(
            inner,
            "photos" | "music" | "videos" | "sql-dump" | "downloads"
        ),
        "nextcloud" => matches!(inner, "photos" | "music" | "videos" | "maildir" | "git"),
        "website" => matches!(inner, "photos" | "git" | "website"),
        "export" => matches!(inner, "photos" | "videos" | "music"),
        _ => false,
    };
    let dir_prefix_of = |c: &Candidate| {
        if c.is_file {
            c.path.clone()
        } else {
            paths::dir_prefix(&paths::from_bytes(&c.path))
        }
    };
    let prefixes: Vec<(Vec<u8>, &'static str, bool)> = cands
        .iter()
        .map(|c| (dir_prefix_of(c), c.kind, c.is_file))
        .collect();
    // (a folder's prefix ends with a separator, so starting with it means being strictly inside)
    let keep: Vec<bool> = cands
        .iter()
        .map(|c| {
            !prefixes
                .iter()
                .any(|(p, k, is_file)| !is_file && c.path.starts_with(p) && swallows(k, c.kind))
        })
        .collect();
    let cands: Vec<Candidate> = cands
        .into_iter()
        .zip(keep)
        .filter_map(|(c, k)| k.then_some(c))
        .collect();

    // scripts and sync markers, for provenance
    let mut scripts: Vec<(PathBuf, Vec<Transfer>)> = Vec::new();
    let mut sync_markers: Vec<(Vec<u8>, &'static str)> = Vec::new();
    if read_drive {
        progress("reading backup scripts");
        for s in &m.scripts {
            let p = paths::from_bytes(s);
            if let Some(t) = read_small(&p, 256 * 1024) {
                let ts = parse_script(&t);
                if !ts.is_empty() {
                    scripts.push((p, ts));
                }
            }
        }
    }
    for (dir, names) in &m.dirs {
        if names.contains(".stfolder") {
            sync_markers.push((dir.clone(), "Syncthing"));
        }
        if names.contains(".sync") {
            sync_markers.push((dir.clone(), "Resilio Sync"));
        }
        if names.contains(".dropbox.cache") {
            sync_markers.push((dir.clone(), "Dropbox"));
        }
    }
    for (dir, files) in &m.files {
        if files.contains(".dropbox") {
            sync_markers.push((dir.clone(), "Dropbox"));
        }
        if files.contains(".nextcloudsync.log") || files.iter().any(|f| f.starts_with("._sync_")) {
            sync_markers.push((dir.clone(), "the Nextcloud desktop client"));
        }
        if files.contains(".owncloudsync.log") {
            sync_markers.push((dir.clone(), "the ownCloud desktop client"));
        }
    }

    // sizes, dates and the rest, per entity
    progress("measuring");
    let sep = sep();
    let mut totals = db
        .conn
        .prepare_cached("SELECT files, bytes FROM dirs WHERE path = ?1 AND current = 1")?;
    let mut file_row = db
        .conn
        .prepare_cached("SELECT size, mtime FROM files WHERE path = ?1 AND current = 1")?;
    let mut range = db.conn.prepare_cached(
        "SELECT dir, mtime FROM files WHERE current = 1 AND path >= ?1 AND path < ?2",
    )?;
    let cur = [&sep[..], &paths::to_bytes(Path::new("cur"))[..]].concat();
    let new = [&sep[..], &paths::to_bytes(Path::new("new"))[..]].concat();
    let mut entities = Vec::new();
    for c in &cands {
        let p = paths::from_bytes(&c.path);
        let mut e = Entity {
            kind: c.kind.to_string(),
            label: label(c.kind),
            title: c.title.clone(),
            path: p.display().to_string(),
            key: browse::key(&p),
            is_file: c.is_file,
            confidence: c.confidence,
            files: 0,
            bytes: 0,
            oldest: None,
            newest: None,
            facts: c.facts.clone(),
            provenance: Vec::new(),
            junk: Vec::new(),
            inside: None,
            low_value: c.low_value,
        };
        if c.is_file {
            if let Some((size, mtime)) = file_row
                .query_row(params![c.path], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
                })
                .optional()?
            {
                e.files = 1;
                e.bytes = size as u64;
                e.oldest = Some(mtime);
                e.newest = Some(mtime);
            }
        } else {
            if let Some((files, bytes)) = totals
                .query_row(params![c.path], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
                })
                .optional()?
            {
                e.files = files as u64;
                e.bytes = bytes as u64;
            }
            let lo = paths::dir_prefix(&p);
            let mut hi = lo.clone();
            if let Some(last) = hi.last_mut() {
                *last += 1; // a separator is never 0xFF
            }
            let mail = matches!(c.kind, "mail-store" | "maildir");
            let mut messages = 0u64;
            let rows = range.query_map(params![lo, hi], |r| {
                Ok((r.get::<_, Option<Vec<u8>>>(0)?, r.get::<_, i64>(1)?))
            })?;
            for row in rows {
                let (dir, mtime) = row?;
                if mtime > 0 {
                    e.oldest = Some(e.oldest.map_or(mtime, |o| o.min(mtime)));
                    e.newest = Some(e.newest.map_or(mtime, |n| n.max(mtime)));
                }
                if mail && dir.is_some_and(|d| d.ends_with(&cur) || d.ends_with(&new)) {
                    messages += 1;
                }
            }
            if mail {
                e.facts.push(("Messages".into(), fmt::count(messages)));
            }
        }
        if read_drive {
            provenance(&mut e, &root_path, &p, &scripts, &sync_markers);
        }
        entities.push(e);
    }

    // nesting: each entity's closest container
    let dirs_by_len: Vec<(Vec<u8>, String)> = entities
        .iter()
        .filter(|e| !e.is_file)
        .map(|e| (paths::dir_prefix(Path::new(&e.path)), e.key.clone()))
        .collect();
    for e in &mut entities {
        let me = paths::to_bytes(Path::new(&e.path));
        e.inside = dirs_by_len
            .iter()
            .filter(|(p, k)| me.starts_with(p) && *k != e.key)
            .max_by_key(|(p, _)| p.len())
            .map(|(_, k)| k.clone());
    }

    // junk: its size, and the entity it belongs to
    let mut loose = Vec::new();
    for (path, why) in &junk_list {
        let Some((files, bytes)) = totals
            .query_row(params![path], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
            })
            .optional()?
        else {
            continue;
        };
        if bytes == 0 {
            continue;
        }
        let p = paths::from_bytes(path);
        let j = Junk {
            path: p.display().to_string(),
            key: browse::key(&p),
            why: (*why).to_string(),
            files: files as u64,
            bytes: bytes as u64,
        };
        let owner = entities
            .iter_mut()
            .filter(|e| !e.is_file && path.starts_with(&paths::dir_prefix(Path::new(&e.path))))
            .max_by_key(|e| e.path.len());
        match owner {
            Some(e) => e.junk.push(j),
            None => loose.push(j),
        }
    }
    for e in &mut entities {
        e.junk.sort_by_key(|j| std::cmp::Reverse(j.bytes));
    }
    loose.sort_by_key(|j| std::cmp::Reverse(j.bytes));
    let order = |k: &str| {
        KINDS
            .iter()
            .position(|(x, _)| *x == k)
            .unwrap_or(usize::MAX)
    };
    entities.sort_by(|a, b| {
        order(&a.kind)
            .cmp(&order(&b.kind))
            .then(b.bytes.cmp(&a.bytes))
            .then(a.path.cmp(&b.path))
    });
    Ok(RootEntities {
        root_id,
        root: root_path.display().to_string(),
        scan,
        computed_at: now(),
        read_drive,
        entities,
        junk: loose,
    })
}

// ---------- storage

/// Keep the result for the browser and reports. Needs a writable connection.
pub fn store(db: &Db, r: &RootEntities) -> Result<()> {
    db.conn.execute(
        "INSERT INTO entities(root_id, scan, computed_at, data) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(root_id) DO UPDATE SET scan = excluded.scan, computed_at = excluded.computed_at, data = excluded.data",
        params![r.root_id, r.scan, r.computed_at, serde_json::to_string(r)?],
    )?;
    Ok(())
}

/// What's stored for a root, and whether it describes the root's latest scan.
pub fn load(db: &Db, root_id: i64) -> Result<Option<(RootEntities, bool)>> {
    let row: Option<(String, i64)> = db
        .conn
        .query_row(
            "SELECT e.data, r.last_scan FROM entities e JOIN roots r ON r.id = e.root_id WHERE e.root_id = ?1",
            params![root_id],
            |r| Ok((r.get(0)?, r.get::<_, Option<i64>>(1)?.unwrap_or(0))),
        )
        .optional()?;
    Ok(match row {
        Some((data, last)) => {
            let r: RootEntities = serde_json::from_str(&data)?;
            let fresh = r.scan == last;
            Some((r, fresh))
        }
        None => None,
    })
}

/// Ids of roots with a finished scan.
pub fn root_ids(db: &Db) -> Result<Vec<i64>> {
    let mut stmt = db
        .conn
        .prepare("SELECT id FROM roots WHERE last_scan IS NOT NULL ORDER BY path")?;
    let ids = stmt
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_describe_their_copies() {
        let t = parse_script(
            "#!/bin/sh\nDEST=/mnt/usb/backups\nexport SRC=root@mail.example.org:/var/vmail/\n\
             rsync -aHAX --delete -e 'ssh -p 2222' --exclude=.cache $SRC \"$DEST/mail/\"\n\
             tar -czf ${DEST}/etc.tgz /etc /root\nscp -P 22 user@web.example.org:/srv/www/site.sql $DEST/sql/\n\
             echo done && cp -a /home/alex/Photos /mnt/usb/backups/photos\n\
             0 3 * * * rclone sync --fast-list gdrive:Documents /mnt/usb/backups/gdrive\n",
        );
        assert_eq!(t.len(), 5, "{t:?}");
        assert_eq!(t[0].tool, "rsync");
        assert_eq!(
            t[0].sources,
            vec!["root@mail.example.org:/var/vmail/".to_string()]
        );
        assert_eq!(t[0].dest, "/mnt/usb/backups/mail/");
        assert_eq!(t[1].tool, "tar");
        assert_eq!(t[1].dest, "/mnt/usb/backups/etc.tgz");
        assert_eq!(t[1].sources, vec!["/etc".to_string(), "/root".to_string()]);
        assert_eq!(t[2].tool, "scp");
        assert_eq!(t[2].dest, "/mnt/usb/backups/sql/");
        assert_eq!(t[3].tool, "cp");
        assert_eq!(t[4].tool, "rclone");
        assert_eq!(
            remote_host(&t[0].sources[0]).as_deref(),
            Some("mail.example.org")
        );
        assert_eq!(remote_host("C:\\Users\\alex"), None);
        assert_eq!(remote_host("/home/alex"), None);
    }

    #[test]
    fn batch_files_too() {
        let t = parse_script(
            "@echo off\r\nset TARGET=E:\\Backup\r\nrem nightly\r\nrobocopy C:\\Users\\Alex\\Documents %TARGET%\\Documents /MIR /R:1\r\n",
        );
        assert_eq!(t.len(), 1, "{t:?}");
        assert_eq!(t[0].tool, "robocopy");
        assert_eq!(t[0].dest, "E:\\Backup\\Documents");
    }

    #[test]
    fn plist_values() {
        let x = "<plist><dict><key>Device Name</key>\n\t<string>Alex’s iPhone</string><key>Last Backup Date</key><date>2016-05-01T10:00:00Z</date></dict></plist>";
        assert_eq!(
            plist_value(x, "Device Name").as_deref(),
            Some("Alex’s iPhone")
        );
        assert_eq!(
            plist_value(x, "Last Backup Date").as_deref(),
            Some("2016-05-01T10:00:00Z")
        );
        assert_eq!(plist_value(x, "Product Type"), None);
    }
}
