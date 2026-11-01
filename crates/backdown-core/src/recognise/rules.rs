//! The rules: what the markers make of a folder (each a candidate with a kind and a confidence),
//! media libraries, and what's regenerable junk.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::Result;
use rusqlite::{OptionalExtension, params};

use crate::{Db, fmt, names, paths, timemachine};

use super::*;

// ---------- rules

pub(super) struct Candidate {
    pub(super) path: Vec<u8>,
    pub(super) kind: &'static str,
    pub(super) is_file: bool,
    pub(super) confidence: f32,
    pub(super) facts: Vec<(String, String)>,
    pub(super) title: String,
    pub(super) low_value: bool,
}

pub(super) fn cand(path: Vec<u8>, kind: &'static str, confidence: f32) -> Candidate {
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

pub(crate) fn name_of(path: &[u8]) -> String {
    paths::from_bytes(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| paths::from_bytes(path).display().to_string())
}

pub(crate) fn parent_of(path: &[u8]) -> Option<Vec<u8>> {
    paths::from_bytes(path).parent().map(paths::to_bytes)
}

/// A folder holding APFS Time Machine snapshots (`YYYY-MM-DD-HHMMSS.backup`): the backup volume's
/// root, or where macOS mounts its snapshots. The computer is in the volume's name when it's the
/// default "Backups of …"; the records (`timemachine`) may say more once the drive is read.
pub(super) fn apfs_time_machine(db: &Db, dir: &[u8]) -> Result<Candidate> {
    let snaps: Vec<(String, timemachine::Snapshot)> = children(db, dir)?
        .into_iter()
        .filter(|n| timemachine::apfs_snapshot(n))
        .filter_map(|n| timemachine::snapshot(&n).map(|s| (n, s)))
        .collect();
    let done: Vec<&(String, timemachine::Snapshot)> = snaps
        .iter()
        .filter(|(_, s)| s.state == timemachine::State::Done)
        .collect();
    let mut c = cand(dir.to_vec(), "timemachine", 0.95);
    let own = name_of(dir);
    let computer = timemachine::computer_of_volume(&own).map(|c| names::nfc(c).into_owned());
    let n = done.len();
    let count = if n == 1 {
        "1 snapshot".to_string()
    } else {
        format!("{n} snapshots")
    };
    c.title = match &computer {
        Some(m) => format!("{count} from {m}"),
        None => format!("{count} (APFS)"),
    };
    if let Some(m) = &computer {
        c.facts.push(("Computers".into(), m.clone()));
    }
    c.facts.push(("Snapshots".into(), n.to_string()));
    if let (Some(first), Some(last)) = (done.first(), done.last()) {
        c.facts
            .push(("Span".into(), format!("{} – {}", first.1.day, last.1.day)));
        // what was backed up: the volumes in the newest snapshot ("Macintosh HD - Data"); where
        // macOS mounts a snapshot, the folder holds its own `….backup` folder once more
        let mut at = join(dir, &last.0);
        let mut vols = children(db, &at)?;
        if vols.len() == 1 && vols[0] == last.0 {
            at = join(&at, &last.0);
            vols = children(db, &at)?;
        }
        let vols: Vec<String> = vols
            .into_iter()
            .filter(|v| !v.starts_with('.') && !timemachine::apfs_snapshot(v))
            .map(|v| names::nfc(&v).into_owned())
            .collect();
        if !vols.is_empty() {
            c.facts.push(("Volumes".into(), vols.join(", ")));
        }
    }
    let unfinished = snaps.len() - n;
    if unfinished > 0 {
        c.facts.push((
            "Unfinished".into(),
            format!("{unfinished} (in progress or interrupted)"),
        ));
    }
    c.facts
        .push(("Layout".into(), "APFS snapshots (macOS 11 or later)".into()));
    Ok(c)
}

pub(crate) fn looks_like_domain(s: &str) -> bool {
    s.contains('.')
        && !s.starts_with('.')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

pub(super) fn rules(db: &Db, root: &[u8], m: &Markers) -> Result<Vec<Candidate>> {
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
            // the domain with the most mailboxes names the store when nothing better does
            if let Some((main, _)) = domains
                .iter()
                .filter(|(d, _)| looks_like_domain(d))
                .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
            {
                c.facts.push(("Main domain".into(), main.clone()));
            }
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
    // Dovecot dbox: mdbox keeps messages in storage/m.N (its map index in there too) beside
    // mailboxes/<name>/dbox-Mails/; sdbox has a u.N file per message in each dbox-Mails
    let mut dbox_roots: BTreeMap<Vec<u8>, &str> = BTreeMap::new();
    for (dir, names) in &m.dirs {
        if names.contains("storage") && names.contains("mailboxes") {
            let st = join(dir, "storage");
            if m.has_file(&st, "dovecot.map.index") || m.dbox.get(&st).is_some_and(|c| c.0 > 0) {
                dbox_roots.insert(dir.clone(), "mdbox");
            }
        }
    }
    for (dir, (_, singles)) in &m.dbox {
        if *singles == 0 || !name_of(dir).eq_ignore_ascii_case("dbox-mails") {
            continue;
        }
        let mut p = parent_of(dir);
        while let Some(x) = p {
            if x.len() <= root.len() {
                break;
            }
            if name_of(&x) == "mailboxes" {
                if let Some(r) = parent_of(&x) {
                    dbox_roots.entry(r).or_insert("sdbox");
                }
                break;
            }
            p = parent_of(&x);
        }
    }
    // an older layout: the map index beside storage/
    for (dir, files) in &m.files {
        if files.contains("dovecot.map.index") && m.has_dir(dir, "storage") {
            dbox_roots.entry(dir.clone()).or_insert("mdbox");
        }
    }
    for (dir, format) in dbox_roots {
        let mut c = cand(dir, "mdbox", 0.9);
        c.title = format!("Dovecot {format} storage");
        c.facts.push(("Format".into(), format.into()));
        out.push(c);
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
            c.facts
                .push(("Layout".into(), "HFS+ (Backups.backupdb)".into()));
            out.push(c);
        }
        // APFS (macOS 11 and later): each backup a snapshot, seen as a `….backup` folder
        if names.iter().any(|n| timemachine::apfs_snapshot(n)) {
            out.push(apfs_time_machine(db, dir)?);
        }
        // network backups, and other sparse bundles; the marker names are lower case, so the
        // bundle's own spelling comes from the index
        let bundles: Vec<&String> = names
            .iter()
            .filter(|n| n.ends_with(".sparsebundle") || n.ends_with(".backupbundle"))
            .collect();
        if bundles.is_empty() {
            continue;
        }
        let kids = children(db, dir)?;
        for n in bundles {
            let own = kids
                .iter()
                .find(|k| names::nfc(k).to_lowercase() == **n)
                .unwrap_or(n);
            let path = join(dir, own);
            let apfs = n.ends_with(".backupbundle");
            let has_id = [
                "com.apple.timemachine.machineid.plist",
                ".com.apple.timemachine.machineid.plist",
                "com.apple.timemachine.machineid.bckup",
            ]
            .iter()
            .any(|f| m.has_file(&path, f));
            if apfs || has_id {
                let computer = timemachine::bundle_computer(own).unwrap_or(own).to_string();
                let mut c = cand(path, "timemachine", if has_id { 0.98 } else { 0.9 });
                c.title = format!("network backup of {}", names::nfc(&computer));
                c.facts
                    .push(("Computers".into(), names::nfc(&computer).into_owned()));
                c.facts.push((
                    "Layout".into(),
                    if apfs {
                        "APFS backup bundle (network, macOS 11 or later)"
                    } else {
                        "sparse bundle (network)"
                    }
                    .into(),
                ));
                out.push(c);
            } else {
                let mut c = cand(path, "disk-image", 0.9);
                c.title = "sparse bundle (network Time Machine or disk image)".into();
                c.facts
                    .push(("Format".into(), "Apple sparse bundle".into()));
                out.push(c);
            }
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

    // ---- databases: mongodump output, SQLite files, and the data directories of servers
    // mongodump writes dump/<database>/<collection>.bson with a .metadata.json beside each
    let db_dirs: Vec<&Vec<u8>> = m
        .bson
        .iter()
        .filter(|(_, (bson, _, meta))| *bson >= 1 && *meta >= 1)
        .map(|(d, _)| d)
        .collect();
    let mut by_parent: BTreeMap<Vec<u8>, Vec<&Vec<u8>>> = BTreeMap::new();
    let mut alone: Vec<&Vec<u8>> = Vec::new();
    for d in db_dirs {
        match parent_of(d).filter(|p| p.len() >= root.len()) {
            Some(p) => by_parent.entry(p).or_default().push(d),
            None => alone.push(d),
        }
    }
    for (parent, dbs) in by_parent {
        // the whole dump when the folder is mongodump's own or holds several databases
        if dbs.len() >= 2 || name_of(&parent).eq_ignore_ascii_case("dump") {
            let mut c = cand(parent.clone(), "mongo-dump", 0.9);
            let (colls, bytes) = dbs.iter().fold((0, 0), |(n, b), d| {
                let x = m.bson[*d];
                (n + x.0, b + x.1)
            });
            let mut names: Vec<String> = dbs.iter().map(|d| name_of(d)).collect();
            names.sort();
            c.title = format!(
                "{} database{}, {} collection{}",
                dbs.len(),
                if dbs.len() == 1 { "" } else { "s" },
                colls,
                if colls == 1 { "" } else { "s" }
            );
            c.facts.push(("Engine".into(), "MongoDB".into()));
            c.facts.push((
                "Databases".into(),
                names
                    .iter()
                    .take(12)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
            c.facts.push(("Collections".into(), colls.to_string()));
            c.facts.push(("Data".into(), fmt::bytes(bytes)));
            out.push(c);
        } else {
            alone.extend(dbs);
        }
    }
    for d in alone {
        let (colls, bytes, _) = m.bson[d];
        let mut c = cand(d.clone(), "mongo-dump", 0.85);
        c.title = format!(
            "{} ({} collection{})",
            name_of(d),
            colls,
            if colls == 1 { "" } else { "s" }
        );
        c.facts.push(("Engine".into(), "MongoDB".into()));
        c.facts.push(("Databases".into(), name_of(d)));
        c.facts.push(("Collections".into(), colls.to_string()));
        c.facts.push(("Data".into(), fmt::bytes(bytes)));
        out.push(c);
    }
    // SQLite files: the largest first, and no more than a drive-full of opens
    let mut sqlite = m.sqlite.clone();
    sqlite.sort_by(|a, b| b.1.cmp(&a.1));
    for (path, size, certain) in sqlite.into_iter().take(2000) {
        // with the drive here, the magic bytes decide; without it, the name does
        let magic = first_bytes(&paths::from_bytes(&path), 16).map(|b| b.starts_with(b"SQLite format 3\0"));
        let confidence = match (magic, certain) {
            (Some(false), _) => continue,
            (Some(true), _) => 0.95,
            (None, true) => 0.8,
            (None, false) => 0.5,
        };
        let mut c = cand(path.clone(), "sqlite-db", confidence);
        c.is_file = true;
        c.title = name_of(&path);
        c.facts.push(("Engine".into(), "SQLite".into()));
        c.facts.push(("Size".into(), fmt::bytes(size)));
        out.push(c);
    }
    // MySQL / MariaDB: the data directory has the InnoDB system files and a mysql/ schema folder;
    // each other folder with table files is a database
    for (dir, files) in &m.files {
        let dirs = m.dir_names(dir);
        if !((files.contains("ibdata1") || files.contains("ib_logfile0")) && dirs.contains("mysql"))
            && !(dirs.contains("mysql") && dirs.contains("performance_schema"))
        {
            continue;
        }
        let mut dbs: Vec<(String, u64)> = children(db, dir)?
            .into_iter()
            .filter(|n| {
                !matches!(
                    n.to_ascii_lowercase().as_str(),
                    "mysql" | "performance_schema" | "sys" | "information_schema" | "#innodb_redo" | "#innodb_temp"
                ) && !n.starts_with('#')
                    && !n.starts_with('.')
            })
            .filter_map(|n| {
                let (frm, data) = m.tables.get(&join(dir, &n)).copied()?;
                Some((n, frm.max(data)))
            })
            .collect();
        dbs.sort();
        let engine = if files.contains("aria_log_control") {
            "MariaDB"
        } else {
            "MySQL or MariaDB"
        };
        let tables: u64 = dbs.iter().map(|(_, t)| t).sum();
        let mut c = cand(dir.clone(), "mysql-data", 0.95);
        c.title = format!(
            "{} database{}, {} table{}",
            dbs.len(),
            if dbs.len() == 1 { "" } else { "s" },
            tables,
            if tables == 1 { "" } else { "s" }
        );
        c.facts.push(("Engine".into(), engine.into()));
        c.facts.push((
            "Databases".into(),
            dbs.iter()
                .take(12)
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        ));
        c.facts.push(("Tables".into(), tables.to_string()));
        out.push(c);
    }
    // PostgreSQL: PG_VERSION beside base/ (one folder per database, by OID) and global/
    for (dir, files) in &m.files {
        if files.contains("pg_version") {
            let dirs = m.dir_names(dir);
            if dirs.contains("base") && dirs.contains("global") {
                let dbs = children(db, &join(dir, "base"))?
                    .iter()
                    .filter(|n| n.bytes().all(|b| b.is_ascii_digit()))
                    .count();
                let mut c = cand(dir.clone(), "postgres-data", 0.95);
                c.title = format!(
                    "{} database{}",
                    dbs,
                    if dbs == 1 { "" } else { "s" }
                );
                c.facts.push(("Engine".into(), "PostgreSQL".into()));
                c.facts.push(("Databases".into(), dbs.to_string()));
                out.push(c);
            }
        }
    }
    // MongoDB: WiredTiger's own files and the catalogue
    for (dir, files) in &m.files {
        if files.contains("wiredtiger") && (files.contains("storage.bson") || files.contains("_mdb_catalog.wt")) {
            let mut c = cand(dir.clone(), "mongo-data", 0.95);
            c.title = "MongoDB data (WiredTiger)".into();
            c.facts.push(("Engine".into(), "MongoDB".into()));
            out.push(c);
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
pub(super) fn media_libraries(db: &Db, root: &[u8], m: &Markers) -> Result<Vec<Candidate>> {
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

/// Folders that are regenerable by their (lowercase) name alone, and why.
pub const JUNK_NAMES: &[(&str, &str)] = &[
    ("node_modules", "npm packages (reinstallable)"),
    ("__pycache__", "Python bytecode cache"),
    (".cache", "application caches"),
    (".npm", "npm cache"),
    (".gradle", "Gradle caches"),
    (".thumbnails", "thumbnail cache"),
    ("thumbnails", "thumbnail cache"),
    ("$recycle.bin", "Windows recycle bin"),
    ("recycler", "Windows recycle bin"),
    (".spotlight-v100", "macOS Spotlight index"),
    (".fseventsd", "macOS file event log"),
    (".documentrevisions-v100", "macOS document versions store"),
    (".temporaryitems", "macOS temporary items"),
    (".trashes", "macOS trash"),
    ("system volume information", "Windows system restore data"),
    (".venv", "Python virtual environment"),
    ("venv", "Python virtual environment"),
    ("files_trashbin", "Nextcloud trash"),
];

/// The rules that also look at where a folder is, in words (`junk_dirs` has them in code).
pub const JUNK_PLACES: &[(&str, &str)] = &[
    ("Caches in a Library folder", "macOS app caches"),
    ("Temp in AppData\\Local", "Windows temporary files"),
    ("target beside a Cargo.toml", "Rust build output"),
    (
        "preview in a Nextcloud appdata_ folder",
        "Nextcloud previews",
    ),
    ("updater-… folders", "Nextcloud updater backups"),
    (".Trash… folders", "desktop trash"),
];

/// Why a folder named with `set_extra_junk` counts as regenerable.
pub const EXTRA_JUNK_WHY: &str = "counted as regenerable in Settings";

pub(super) static EXTRA_JUNK: std::sync::RwLock<Vec<String>> = std::sync::RwLock::new(Vec::new());

/// More folder names to treat as regenerable besides `JUNK_NAMES` (the Settings view's list),
/// from the next recognition on.
pub fn set_extra_junk(names: &[String]) {
    let names = names
        .iter()
        .map(|n| n.trim().to_lowercase())
        .filter(|n| !n.is_empty())
        .collect();
    *EXTRA_JUNK.write().unwrap_or_else(|e| e.into_inner()) = names;
}

/// Regenerable subtrees, by name (and for build output, by what's next to it).
pub(super) fn junk_dirs(m: &Markers) -> Vec<(Vec<u8>, &'static str)> {
    let extra = EXTRA_JUNK.read().unwrap_or_else(|e| e.into_inner());
    let mut out = Vec::new();
    for (parent, names) in &m.dirs {
        let files = m.files.get(parent);
        for n in names {
            let named = JUNK_NAMES.iter().find(|(k, _)| k == n).map(|(_, why)| *why);
            let why = match n.as_str() {
                _ if named.is_some() => named.unwrap_or_default(),
                "caches" if name_of(parent).eq_ignore_ascii_case("library") => "macOS app caches",
                "temp" if name_of(parent).eq_ignore_ascii_case("local") => {
                    "Windows temporary files"
                }
                "target" if files.is_some_and(|f| f.contains("cargo.toml")) => "Rust build output",
                "preview" if name_of(parent).starts_with("appdata_") => "Nextcloud previews",
                n if n.starts_with("updater-") => "Nextcloud updater backups",
                n if n.starts_with(".trash") => "desktop trash",
                n if extra.iter().any(|x| x == n) => EXTRA_JUNK_WHY,
                _ => continue,
            };
            let real = m.dir_case.get(&(parent.clone(), n.clone())).unwrap_or(n);
            out.push((join(parent, real), why));
        }
    }
    out
}
