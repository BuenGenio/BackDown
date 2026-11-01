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

pub fn read_small(path: &Path, max: u64) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > max {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// The first `n` bytes of a file of any size (fewer if it's shorter); `None` when it can't be opened.
pub(super) fn first_bytes(path: &Path, n: usize) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut f = paths::open_read(path).ok()?;
    let mut b = vec![0u8; n];
    let got = f.read(&mut b).ok()?;
    b.truncate(got);
    Some(b)
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
/// What the first few kilobytes of a dump say about it.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct DumpHead {
    /// "MySQL", "MariaDB", "PostgreSQL", "PostgreSQL (custom format)", "SQLite"
    pub engine: Option<&'static str>,
    /// mysqldump, phpMyAdmin, pg_dump, pg_dumpall…
    pub tool: Option<String>,
    pub database: Option<String>,
    pub host: Option<String>,
    pub date: Option<i64>,
}

/// `name` with no quotes or backticks around it, trimmed.
fn unquoted(s: &str) -> String {
    s.trim()
        .trim_end_matches(';')
        .trim_matches(|c: char| c == '`' || c == '"' || c == '\'' || c.is_whitespace())
        .to_string()
}

/// Read a dump's head (and tail, for the date) without decompressing anything: a compressed dump
/// says nothing here, and that's fine. `None` when the file can't be opened.
pub fn sniff_dump(path: &Path) -> Option<DumpHead> {
    use std::io::Read;
    let mut f = paths::open_read(path).ok()?;
    let mut head = vec![0u8; 8192];
    let n = f.read(&mut head).unwrap_or(0);
    let head = &head[..n];
    let mut h = DumpHead::default();
    if head.starts_with(b"PGDMP") {
        h.engine = Some("PostgreSQL (custom format)");
        h.tool = Some("pg_dump".into());
        return Some(h);
    }
    if head.starts_with(&[0x1f, 0x8b])
        || head.starts_with(b"BZh")
        || head.starts_with(&[0xfd, b'7', b'z', b'X', b'Z'])
        || head.starts_with(&[0x28, 0xb5, 0x2f, 0xfd])
    {
        return Some(h); // compressed: nothing to read here
    }
    let t = String::from_utf8_lossy(head);
    let server_line = t.lines().find(|l| l.contains("Server version"));
    let mariadb = server_line.is_some_and(|l| l.contains("MariaDB"));
    if t.contains("MariaDB dump") || (t.contains("MySQL dump") && mariadb) {
        h.engine = Some("MariaDB");
        h.tool = Some("mysqldump".into());
    } else if t.contains("MySQL dump") {
        h.engine = Some("MySQL");
        h.tool = Some("mysqldump".into());
    } else if t.contains("phpMyAdmin SQL Dump") {
        h.engine = Some(if mariadb { "MariaDB" } else { "MySQL" });
        h.tool = Some("phpMyAdmin".into());
    } else if t.contains("PostgreSQL database cluster dump") {
        h.engine = Some("PostgreSQL");
        h.tool = Some("pg_dumpall".into());
    } else if t.contains("PostgreSQL database dump") {
        h.engine = Some("PostgreSQL");
        h.tool = Some("pg_dump".into());
    } else if t.contains("PRAGMA foreign_keys=OFF;") && t.contains("BEGIN TRANSACTION;") {
        h.engine = Some("SQLite");
        h.tool = Some("sqlite3 .dump".into());
    } else if t.contains("-- Adminer ") {
        h.tool = Some("Adminer".into());
    } else if t.contains("HeidiSQL") {
        h.tool = Some("HeidiSQL".into());
        h.engine = Some(if mariadb { "MariaDB" } else { "MySQL" });
    }
    for l in t.lines() {
        let l = l.trim();
        // mysqldump: "-- Host: db1    Database: shop"; phpMyAdmin: "-- Host: localhost:3306"
        if let Some(rest) = l.strip_prefix("--").map(str::trim) {
            if let Some(r) = rest.strip_prefix("Host:") {
                let (host, rest) = match r.split_once("Database:") {
                    Some((a, b)) => (a, Some(b)),
                    None => (r, None),
                };
                let host = host.trim();
                let host = host
                    .rsplit_once(':')
                    .filter(|(_, p)| p.bytes().all(|b| b.is_ascii_digit()))
                    .map_or(host, |(a, _)| a);
                if !host.is_empty() && h.host.is_none() {
                    h.host = Some(host.to_string());
                }
                if let Some(d) = rest.map(unquoted).filter(|d| !d.is_empty()) {
                    h.database.get_or_insert(d);
                }
            } else if let Some(r) = rest.strip_prefix("Database:") {
                let d = unquoted(r);
                if !d.is_empty() {
                    h.database.get_or_insert(d);
                }
            } else if let Some(r) = rest.strip_prefix("Current Database:") {
                let d = unquoted(r);
                if !d.is_empty() {
                    h.database.get_or_insert(d);
                }
            } else if let Some(r) = rest
                .strip_prefix("Name:")
                .filter(|r| r.contains("Type: DATABASE"))
            {
                let d = unquoted(r.split(';').next().unwrap_or(""));
                if !d.is_empty() {
                    h.database.get_or_insert(d);
                }
            }
        } else if let Some(r) = l
            .strip_prefix("\\connect ")
            .or_else(|| l.strip_prefix("\\c "))
        {
            let d = unquoted(r.split_whitespace().next().unwrap_or(""));
            if !d.is_empty() && d != "postgres" && d != "template1" {
                h.database.get_or_insert(d);
            }
        } else if let Some(r) = l.strip_prefix("USE ") {
            let d = unquoted(r);
            if !d.is_empty() {
                h.database.get_or_insert(d);
            }
        } else if let Some(after) = l.strip_prefix("CREATE DATABASE") {
            // CREATE DATABASE /*!32312 IF NOT EXISTS*/ `shop` /*!40100 DEFAULT CHARACTER SET utf8 */;
            let after = after.to_string();
            let after = match after.find("*/") {
                Some(i) if after.trim_start().starts_with("/*") => after[i + 2..].to_string(),
                _ => after,
            };
            let d = unquoted(
                after
                    .split(|c: char| c.is_whitespace() || c == '/' || c == ';')
                    .find(|w| !w.is_empty() && !w.eq_ignore_ascii_case("IF"))
                    .unwrap_or(""),
            );
            if !d.is_empty() {
                h.database.get_or_insert(d);
            }
        }
    }
    if h.database
        .as_deref()
        .is_some_and(|d| d.eq_ignore_ascii_case("IF") || d.len() > 128)
    {
        h.database = None;
    }
    h.date = dump_date(&mut f);
    Some(h)
}

/// A database engine's name for people, from how configuration files spell it.
fn engine_name(d: &str) -> String {
    match d.trim().to_ascii_lowercase().as_str() {
        "mysql" | "mysqli" | "pdomysql" => "MySQL or MariaDB".into(),
        "mariadb" => "MariaDB".into(),
        "pgsql" | "postgres" | "postgresql" => "PostgreSQL".into(),
        "sqlite" | "sqlite3" => "SQLite".into(),
        "mongodb" | "mongo" => "MongoDB".into(),
        "sqlsrv" | "mssql" => "SQL Server".into(),
        "oci" | "oracle" => "Oracle".into(),
        other => other.to_string(),
    }
}

/// The host in a URL ("https://cloud.example.org/nc" → "cloud.example.org"), or the text itself
/// when it's just a host.
fn host_of(url: &str) -> Option<String> {
    let s = url.trim();
    let s = s.split_once("://").map_or(s, |(_, r)| r);
    let s = s.split(['/', '?', '#']).next()?;
    let s = s.rsplit_once('@').map_or(s, |(_, r)| r);
    let s = s
        .rsplit_once(':')
        .filter(|(_, p)| p.bytes().all(|b| b.is_ascii_digit()))
        .map_or(s, |(a, _)| a);
    (!s.is_empty() && s != "localhost").then(|| s.to_string())
}

/// The first quoted PHP string in `s`.
fn php_string(s: &str) -> Option<String> {
    let start = s.find(['\'', '"'])?;
    let q = s.as_bytes()[start];
    let rest = &s[start + 1..];
    let mut out = String::new();
    let mut esc = false;
    for c in rest.chars() {
        if esc {
            out.push(c);
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else if c as u32 == q as u32 {
            return Some(out);
        } else {
            out.push(c);
        }
    }
    None
}

/// `define('NAME', 'value')`, or `'name' => 'value'` in a config array: the value.
fn php_define(text: &str, name: &str) -> Option<String> {
    for q in ['\'', '"'] {
        let needle = format!("{q}{name}{q}");
        let Some(i) = text.find(&needle) else {
            continue;
        };
        let after = &text[i + needle.len()..];
        let after = after
            .trim_start_matches(|c: char| c.is_whitespace() || c == ',' || c == '=' || c == '>');
        // a plain value ends with its statement; an array ("trusted_domains") may put its first
        // string on a later line
        let end = after.find([')', ';', '\n']).unwrap_or(after.len());
        if let Some(v) = php_string(&after[..end]) {
            return (!v.is_empty()).then_some(v);
        }
        let block = &after[..after.len().min(2048)];
        if (block.starts_with("array") || block.starts_with('['))
            && let Some(v) = php_string(block)
        {
            return (!v.is_empty()).then_some(v);
        }
    }
    None
}

/// `public $name = 'value';` in a Joomla configuration class: the value.
fn php_prop(text: &str, name: &str) -> Option<String> {
    let needle = format!("${name}");
    let mut from = 0;
    while let Some(i) = text[from..].find(&needle) {
        let at = from + i + needle.len();
        from = at;
        let next = text[at..].chars().next();
        if next.is_some_and(|c| c.is_alphanumeric() || c == '_') {
            continue; // $dbtype when asked for $db
        }
        let rest = &text[at..];
        let stmt = &rest[..rest.find(';').unwrap_or(rest.len())];
        if !stmt.contains('=') {
            continue;
        }
        return php_string(stmt).filter(|v| !v.is_empty());
    }
    None
}

/// `KEY=value` in a .env file (quotes dropped, comments ignored).
fn env_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let l = l.trim();
        let l = l.strip_prefix("export ").unwrap_or(l);
        let (k, v) = l.split_once('=')?;
        if k.trim() != key {
            return None;
        }
        let v = v.trim();
        let v = if v.starts_with('"') || v.starts_with('\'') {
            php_string(v).unwrap_or_default()
        } else {
            v.split('#').next().unwrap_or("").trim().to_string()
        };
        (!v.is_empty()).then_some(v)
    })
}

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
            if let Some(h) = sniff_dump(path) {
                if let Some(en) = h.engine {
                    e.confidence = 0.95;
                    e.facts.push(("Engine".into(), en.into()));
                }
                if let Some(dbn) = &h.database {
                    e.facts.push(("Database".into(), dbn.clone()));
                    e.title = format!("{dbn} ({})", h.engine.unwrap_or("SQL"));
                }
                if let Some(host) = &h.host {
                    e.facts.push(("Host".into(), host.clone()));
                }
                if let Some(t) = h.date {
                    e.facts.push(("Dumped".into(), fmt::day(t)));
                }
                if let Some(host) = &h.host {
                    e.provenance.push(match h.date {
                        Some(t) => format!("Dumped from {host} on {}.", fmt::day(t)),
                        None => format!("Dumped from {host}."),
                    });
                }
            }
        }
        "postgres-data" => {
            if let Some(v) = read_small(&path.join("PG_VERSION"), 64) {
                let v = v.trim();
                if !v.is_empty() {
                    e.facts.push(("Version".into(), v.to_string()));
                    e.title = format!("{} (PostgreSQL {v})", e.title);
                }
            }
        }
        // a website's configuration says which database it uses and where it was served
        "website" => {
            let software = e
                .facts
                .iter()
                .find(|(k, _)| k == "Software")
                .map(|(_, v)| v.as_str())
                .unwrap_or("")
                .to_string();
            let mut found: Vec<(&str, Option<String>)> = Vec::new();
            match software.as_str() {
                "WordPress" => {
                    if let Some(t) = read_small(&path.join("wp-config.php"), 256 * 1024) {
                        found.push(("Database", php_define(&t, "DB_NAME")));
                        found.push(("Database engine", Some("MySQL or MariaDB".into())));
                        found.push(("Database host", php_define(&t, "DB_HOST")));
                        found.push((
                            "Address",
                            php_define(&t, "WP_HOME")
                                .or_else(|| php_define(&t, "WP_SITEURL"))
                                .and_then(|u| host_of(&u)),
                        ));
                    }
                }
                "Joomla" => {
                    if let Some(t) = read_small(&path.join("configuration.php"), 256 * 1024) {
                        found.push(("Database", php_prop(&t, "db")));
                        found.push((
                            "Database engine",
                            php_prop(&t, "dbtype").map(|d| engine_name(&d)),
                        ));
                        found.push(("Database host", php_prop(&t, "host")));
                        found.push(("Site name", php_prop(&t, "sitename")));
                        found.push((
                            "Address",
                            php_prop(&t, "live_site").and_then(|u| host_of(&u)),
                        ));
                        found.push((
                            "Contact",
                            php_prop(&t, "mailfrom").filter(|m| m.contains('@')),
                        ));
                        found.push(("Contact name", php_prop(&t, "fromname")));
                    }
                }
                "Laravel" => {
                    if let Some(t) = read_small(&path.join(".env"), 256 * 1024) {
                        found.push(("Database", env_value(&t, "DB_DATABASE")));
                        found.push((
                            "Database engine",
                            env_value(&t, "DB_CONNECTION").map(|d| engine_name(&d)),
                        ));
                        found.push(("Database host", env_value(&t, "DB_HOST")));
                        found.push(("Site name", env_value(&t, "APP_NAME")));
                        found.push((
                            "Address",
                            env_value(&t, "APP_URL").and_then(|u| host_of(&u)),
                        ));
                        found.push((
                            "Contact",
                            env_value(&t, "MAIL_FROM_ADDRESS").filter(|m| m.contains('@')),
                        ));
                        found.push(("Contact name", env_value(&t, "MAIL_FROM_NAME")));
                    }
                }
                _ => {}
            }
            // a database engine only means something with a database name
            let has_db = found
                .iter()
                .any(|(k, v)| *k == "Database" && v.as_deref().is_some_and(|v| !v.is_empty()));
            for (k, v) in found {
                let Some(v) = v.filter(|v| !v.is_empty() && v.len() <= 200) else {
                    continue;
                };
                if k.starts_with("Database") && !has_db {
                    continue;
                }
                if k == "Address" {
                    e.title = format!("{software} site {v}");
                }
                e.facts.push((k.into(), v));
            }
        }
        // config/config.php: the database behind it and the address it answered at
        "nextcloud-server" => {
            if let Some(t) = read_small(&path.join("config").join("config.php"), 256 * 1024) {
                let mut found: Vec<(&str, Option<String>)> = vec![
                    ("Version", php_define(&t, "version")),
                    ("Address", php_define(&t, "trusted_domains")),
                    ("Database", php_define(&t, "dbname")),
                    (
                        "Database engine",
                        php_define(&t, "dbtype").map(|d| engine_name(&d)),
                    ),
                    ("Database host", php_define(&t, "dbhost")),
                    ("Mail domain", php_define(&t, "mail_domain")),
                    ("Data directory", php_define(&t, "datadirectory")),
                ];
                if found.iter().any(|(k, v)| *k == "Database" && v.is_none()) {
                    found.retain(|(k, _)| !k.starts_with("Database"));
                }
                for (k, v) in found {
                    let Some(v) = v.filter(|v| !v.is_empty() && v.len() <= 200) else {
                        continue;
                    };
                    if k == "Address" {
                        e.title = format!("Nextcloud at {v}");
                    }
                    e.facts.push((k.into(), v));
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
