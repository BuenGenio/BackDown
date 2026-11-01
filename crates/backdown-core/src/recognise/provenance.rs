//! Where things came from: the copy scripts (rsync, cp, robocopy…) and dumps on a drive, read for
//! the machines and folders they copied from.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::plist::{self, Plist};
use crate::{fmt, paths, timemachine};

use super::*;

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

pub(super) fn unquote(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

pub(super) fn substitute(mut s: String, vars: &HashMap<String, String>) -> String {
    for (k, v) in vars {
        s = s
            .replace(&format!("${{{k}}}"), v)
            .replace(&format!("${k}"), v)
            .replace(&format!("%{k}%"), v);
    }
    s
}

pub(super) fn shell_words(s: &str, escapes: bool) -> Vec<String> {
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
pub(super) const RSYNC_ARG: &[&str] = &[
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

pub(super) fn transfer(words: &[String]) -> Option<Transfer> {
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
pub(super) fn remote_host(src: &str) -> Option<String> {
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
pub(super) fn tail(p: &str, n: usize) -> Vec<String> {
    let parts: Vec<String> = p
        .split(['/', '\\'])
        .filter(|s| !s.is_empty() && *s != ".")
        .map(|s| s.to_lowercase())
        .collect();
    parts[parts.len().saturating_sub(n)..].to_vec()
}

pub(super) fn read_small(path: &Path, max: u64) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > max {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

pub(super) fn read_small_bytes(path: &Path, max: u64) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > max {
        return None;
    }
    std::fs::read(path).ok()
}

/// When a dump was made, from its last lines: mysqldump and MariaDB end with
/// "-- Dump completed on 2020-05-31 12:00:00", pg_dump (verbose) with "-- Completed on …".
pub(super) fn dump_date(f: &mut std::fs::File) -> Option<i64> {
    use std::io::{Read, Seek, SeekFrom};
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(4096))).ok()?;
    let mut tail = Vec::new();
    f.take(4096).read_to_end(&mut tail).ok()?;
    let t = String::from_utf8_lossy(&tail);
    t.lines().rev().find_map(|l| {
        let rest = l
            .split_once("Dump completed on ")
            .or_else(|| l.split_once("Completed on "))?
            .1
            .trim();
        plist::iso_date(&rest.replacen(' ', "T", 1))
    })
}

pub(super) fn provenance(
    e: &mut Entity,
    root: &Path,
    path: &Path,
    scripts: &[(PathBuf, Vec<Transfer>)],
    sync_markers: &[(Vec<u8>, &'static str)],
) {
    match e.kind.as_str() {
        "ios-backup" => {
            // Info.plist (usually XML) and Manifest.plist (usually binary) say the same things
            // in different words; either is enough
            let read = |name: &str| {
                read_small_bytes(&path.join(name), 8 << 20).and_then(|b| plist::parse(&b))
            };
            let info = read("Info.plist");
            let manifest = read("Manifest.plist");
            let pick = |a: Option<String>, b: Option<String>| a.or(b);
            let device = pick(
                info.as_ref().and_then(|p| p.text(&["Device Name"])),
                manifest
                    .as_ref()
                    .and_then(|p| p.text(&["Lockdown", "DeviceName"])),
            );
            let model = pick(
                info.as_ref().and_then(|p| p.text(&["Product Name"])),
                pick(
                    info.as_ref().and_then(|p| p.text(&["Product Type"])),
                    manifest
                        .as_ref()
                        .and_then(|p| p.text(&["Lockdown", "ProductType"])),
                ),
            );
            let version = pick(
                info.as_ref().and_then(|p| p.text(&["Product Version"])),
                manifest
                    .as_ref()
                    .and_then(|p| p.text(&["Lockdown", "ProductVersion"])),
            );
            let when = info
                .as_ref()
                .and_then(|p| p.date(&["Last Backup Date"]))
                .or_else(|| manifest.as_ref().and_then(|p| p.date(&["Date"])));
            if let Some(d) = &device {
                e.title = format!("backup of {d}");
                e.facts.push(("Device".into(), d.clone()));
            }
            if let Some(m) = &model {
                e.facts.push(("Model".into(), m.clone()));
            }
            if let Some(v) = version {
                e.facts.push(("iOS".into(), v));
            }
            if let Some(Plist::Bool(enc)) = manifest.as_ref().and_then(|p| p.get(&["IsEncrypted"]))
            {
                e.facts
                    .push(("Encrypted".into(), if *enc { "yes" } else { "no" }.into()));
            }
            if let Some(t) = when {
                e.facts.push(("Last backup".into(), fmt::day(t)));
                e.provenance.push(format!(
                    "Backed up from {}{} on {}.",
                    device.as_deref().unwrap_or("an iPhone or iPad"),
                    model.map(|m| format!(" ({m})")).unwrap_or_default(),
                    fmt::day(t)
                ));
            }
        }
        "timemachine" => {
            // Time Machine's own records, where it keeps them (network bundles, APFS volumes)
            if let Some(m) = timemachine::read(path, timemachine::MACHINE_ID) {
                let m = timemachine::machine(&m);
                if let Some(model) = m.model {
                    e.facts.push(("Model".into(), model));
                }
                if let Some(t) = m.verified {
                    e.facts.push(("Last verified".into(), fmt::day(t)));
                }
            }
            let history = timemachine::read(path, timemachine::SNAPSHOT_HISTORY)
                .map(|h| timemachine::history(&h))
                .unwrap_or_default();
            let done: Vec<_> = history
                .iter()
                .filter(|(s, _)| s.state == timemachine::State::Done)
                .collect();
            if let (Some(first), Some(last)) = (done.first(), done.last())
                && !e.facts.iter().any(|(k, _)| k == "Span")
            {
                // a bundle's snapshots are inside its disk image: only the history tells them
                e.facts.retain(|(k, _)| k != "Snapshots");
                e.facts.push(("Snapshots".into(), done.len().to_string()));
                e.facts
                    .push(("Span".into(), format!("{} – {}", first.0.day, last.0.day)));
                e.title = format!(
                    "{}, {} snapshot{}",
                    e.title,
                    done.len(),
                    if done.len() == 1 { "" } else { "s" }
                );
            }
            // the snapshot folders are named by date: the newest one is the last backup
            let computers = e
                .facts
                .iter()
                .find(|(k, _)| k == "Computers")
                .map(|(_, v)| v.clone());
            let last = e
                .facts
                .iter()
                .find(|(k, _)| k == "Span")
                .and_then(|(_, v)| v.rsplit(' ').next())
                .and_then(plist::iso_date);
            match (computers.filter(|c| !c.is_empty()), last) {
                (Some(c), Some(t)) => e.provenance.push(format!(
                    "Backed up by Time Machine from {c}, last on {}.",
                    fmt::day(t)
                )),
                (None, Some(t)) => e.provenance.push(format!(
                    "Backed up by Time Machine, last on {}.",
                    fmt::day(t)
                )),
                _ => {}
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
                        e.provenance.push(match dump_date(&mut f) {
                            Some(t) => format!("Dumped from {host} on {}.", fmt::day(t)),
                            None => format!("Dumped from {host}."),
                        });
                    }
                }
            }
        }
        _ => {}
    }
    // sync tools that leave a marker in the folder
    // the newest file bounds when a copy last happened: copies keep the files' own dates, so the
    // last one was then or later
    let since = e
        .newest
        .map(|t| format!(", last on or after {}", fmt::day(t)))
        .unwrap_or_default();
    for (dir, tool) in sync_markers {
        if paths::from_bytes(dir) == path {
            e.provenance.push(format!("Kept in sync by {tool}{since}."));
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
                "Copied from {from} with {} ({}){since}.",
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
