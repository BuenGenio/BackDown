//! A closer look at two kinds of mail store the recogniser finds (recognise.rs calls `describe`):
//!
//! **Dovecot dbox.** mdbox keeps every mailbox's messages together in `storage/m.N` files, with
//! each mailbox's index under `mailboxes/<name>/dbox-Mails/`; sdbox keeps one `u.N` file per
//! message there. The index alone says how many mailboxes there are, what they're called, and (for
//! sdbox) how many messages. With the drive connected, each mailbox's `dovecot.index` header says
//! how many messages it holds, and the first and last storage files say when the oldest and newest
//! message arrived: a few hundred bytes read per mailbox, never the messages themselves.
//!
//! **Thunderbird profiles.** From the index: each account's folder (IMAP accounts under
//! `ImapMail/`, which is an offline copy of mail kept on the server; POP and Local Folders under
//! `Mail/`, which may be the only copy), and which mbox file is which folder (`Archives.sbd/2019`
//! is the folder Archives/2019) with its size. With the drive, `prefs.js` names the accounts, the
//! email addresses they send as, and their servers. These are the person's own details, read
//! locally and kept in their own index.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{MAIN_SEPARATOR_STR, Path, PathBuf};

use anyhow::Result;
use rusqlite::params;

use crate::recognise::Entity;
use crate::{Db, fmt, paths};

/// Add what can be said about a dbox store or a Thunderbird profile to its entity. `read_drive`:
/// small files on the (connected) drive may be read.
pub(crate) fn describe(db: &Db, e: &mut Entity, path: &Path, read_drive: bool) -> Result<()> {
    match e.kind.as_str() {
        "mdbox" => dbox(db, e, path, read_drive),
        "thunderbird" => thunderbird(db, e, path, read_drive),
        _ => Ok(()),
    }
}

fn fact(e: &mut Entity, k: &str, v: String) {
    match e.facts.iter_mut().find(|(x, _)| x == k) {
        Some(f) => f.1 = v,
        None => e.facts.push((k.to_string(), v)),
    }
}

fn range(dir: &Path) -> (Vec<u8>, Vec<u8>) {
    let lo = paths::dir_prefix(dir);
    let mut hi = lo.clone();
    if let Some(l) = hi.last_mut() {
        *l += 1; // a separator is never 0xFF
    }
    (lo, hi)
}

/// Current files below `dir`: (path, size, mtime).
fn files_below(db: &Db, dir: &Path) -> Result<Vec<(PathBuf, u64, i64)>> {
    let (lo, hi) = range(dir);
    let mut stmt = db.conn.prepare_cached(
        "SELECT path, size, mtime FROM files WHERE current = 1 AND path >= ?1 AND path < ?2",
    )?;
    let rows = stmt.query_map(params![lo, hi], |r| {
        Ok((
            paths::from_bytes(&r.get::<_, Vec<u8>>(0)?),
            r.get::<_, i64>(1)? as u64,
            r.get::<_, i64>(2)?,
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Current folders below `dir`.
fn dirs_below(db: &Db, dir: &Path) -> Result<Vec<PathBuf>> {
    let (lo, hi) = range(dir);
    let mut stmt = db
        .conn
        .prepare_cached("SELECT path FROM dirs WHERE current = 1 AND path >= ?1 AND path < ?2")?;
    let rows = stmt.query_map(params![lo, hi], |r| {
        Ok(paths::from_bytes(&r.get::<_, Vec<u8>>(0)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// "m.12" -> 12 (and "u.7" with prefix "u.")
fn numbered(n: &str, prefix: &str) -> Option<u64> {
    let d = n.strip_prefix(prefix)?;
    (!d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
        .then(|| d.parse().ok())
        .flatten()
}

fn list(names: &[String], max: usize) -> String {
    let mut s = names
        .iter()
        .take(max)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if names.len() > max {
        s += &format!(" and {} more", names.len() - max);
    }
    s
}

fn years(a: Option<i64>, b: Option<i64>) -> Option<String> {
    let y = |t: i64| fmt::day(t).rsplit(' ').next().unwrap_or("").to_string();
    match (a, b) {
        (Some(a), Some(b)) if y(a) != y(b) => Some(format!("{}–{}", y(a), y(b))),
        (Some(t), _) | (_, Some(t)) => Some(y(t)),
        _ => None,
    }
}

// ---------- Dovecot dbox

/// A Dovecot index file's message count, from its header (`struct mail_index_header`: major
/// version 7, `messages_count` at byte 32, in the byte order its compat flags say).
pub fn dovecot_index_messages(path: &Path) -> Option<u64> {
    let mut b = [0u8; 48];
    File::open(path).ok()?.read_exact(&mut b).ok()?;
    if b[0] != 7 {
        return None;
    }
    let little = b[12] & 1 == 1;
    let n = [b[32], b[33], b[34], b[35]];
    Some(if little {
        u32::from_le_bytes(n)
    } else {
        u32::from_be_bytes(n)
    } as u64)
}

/// The received time in a dbox message's metadata block ("R<hex>" lines after "\n\x01\x03\n").
fn dbox_received(meta: &[u8]) -> Option<i64> {
    let at = meta.windows(4).position(|w| w == b"\n\x01\x03\n")? + 4;
    for line in meta[at..].split(|&b| b == b'\n') {
        if line.is_empty() {
            break;
        }
        if let Some(hex) = line.strip_prefix(b"R") {
            return i64::from_str_radix(std::str::from_utf8(hex).ok()?.trim(), 16).ok();
        }
    }
    None
}

/// When the first message in a dbox file arrived: the file header ("2 M<header size> C<created>"),
/// the first message's header ("\x01\x02N <16 hex digits of size>"), then its metadata, whose
/// received time is it. The file's creation stamp if that can't be read.
pub fn dbox_first_received(path: &Path) -> Option<i64> {
    let mut f = File::open(path).ok()?;
    let mut head = [0u8; 256];
    let n = f.read(&mut head).ok()?;
    let head = &head[..n];
    let eol = head.iter().position(|&b| b == b'\n')?;
    let line = std::str::from_utf8(&head[..eol]).ok()?;
    let mut words = line.split(' ');
    if words.next()? != "2" {
        return None;
    }
    let (mut hdr, mut created) = (None, None);
    for w in words {
        if let Some(h) = w.strip_prefix('M') {
            hdr = u64::from_str_radix(h, 16).ok();
        } else if let Some(c) = w.strip_prefix('C') {
            created = i64::from_str_radix(c, 16).ok();
        }
    }
    let start = eol as u64 + 1;
    let msg = head.get(start as usize..)?;
    let size = (msg.len() >= 20 && &msg[..3] == b"\x01\x02N")
        .then(|| u64::from_str_radix(std::str::from_utf8(&msg[4..20]).ok()?, 16).ok())
        .flatten();
    let received = (|| {
        let at = start + hdr? + size?;
        f.seek(SeekFrom::Start(at)).ok()?;
        let mut meta = vec![0u8; 4096];
        let n = f.read(&mut meta).ok()?;
        dbox_received(&meta[..n])
    })();
    received.or(created)
}

/// When the last message in a dbox file arrived: the last metadata block, near its end.
pub fn dbox_last_received(path: &Path) -> Option<i64> {
    let mut f = File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(8192))).ok()?;
    let mut tail = Vec::new();
    f.read_to_end(&mut tail).ok()?;
    let at = tail.windows(4).rposition(|w| w == b"\n\x01\x03\n")?;
    dbox_received(&tail[at..])
}

fn dbox(db: &Db, e: &mut Entity, root: &Path, read_drive: bool) -> Result<()> {
    let boxes_dir = root.join("mailboxes");
    let sep = MAIN_SEPARATOR_STR;
    // every mailbox has a dbox-Mails folder; its name is the path between
    let mut boxes: Vec<(String, PathBuf)> = dirs_below(db, &boxes_dir)?
        .into_iter()
        .filter(|d| name(d).eq_ignore_ascii_case("dbox-Mails"))
        .filter_map(|d| {
            let mb = d.parent()?;
            let n = mb.strip_prefix(&boxes_dir).ok()?.display().to_string();
            Some((n.replace(sep, "/"), d))
        })
        .collect();
    boxes.sort();
    let storage: Vec<(u64, PathBuf, u64, i64)> = {
        let mut v: Vec<_> = files_below(db, &root.join("storage"))?
            .into_iter()
            .filter_map(|(p, s, t)| numbered(&name(&p), "m.").map(|n| (n, p, s, t)))
            .collect();
        v.sort();
        v
    };
    let singles: Vec<(PathBuf, i64)> = files_below(db, &boxes_dir)?
        .into_iter()
        .filter(|(p, _, _)| numbered(&name(p), "u.").is_some())
        .map(|(p, _, t)| (p, t))
        .collect();
    let format = if !storage.is_empty() {
        "mdbox"
    } else if !singles.is_empty() || e.facts.iter().any(|(k, v)| k == "Format" && v == "sdbox") {
        "sdbox"
    } else {
        "mdbox"
    };
    fact(e, "Format", format.to_string());
    fact(e, "Mailboxes", fmt::count(boxes.len() as u64));
    if !boxes.is_empty() {
        let names: Vec<String> = boxes.iter().map(|(n, _)| n.clone()).collect();
        fact(e, "Mailbox names", list(&names, 12));
    }
    let mut messages: Option<(u64, bool)> = None; // (count, exact)
    let (mut oldest, mut newest) = (None, None);
    if format == "sdbox" {
        messages = Some((singles.len() as u64, true));
        oldest = singles.iter().map(|(_, t)| *t).filter(|t| *t > 0).min();
        newest = singles.iter().map(|(_, t)| *t).max();
    } else if !storage.is_empty() {
        fact(
            e,
            "Storage files",
            format!(
                "{} ({})",
                fmt::count(storage.len() as u64),
                fmt::bytes(storage.iter().map(|s| s.2).sum())
            ),
        );
    }
    if read_drive && format == "mdbox" {
        // each mailbox's index keeps its message count in its header
        let (mut n, mut all) = (0u64, true);
        for (_, d) in &boxes {
            match dovecot_index_messages(&d.join("dovecot.index")) {
                Some(c) => n += c,
                None => all = false,
            }
        }
        if !boxes.is_empty() && (n > 0 || all) {
            messages = Some((n, all));
        }
        oldest = storage.first().and_then(|s| dbox_first_received(&s.1));
        newest = storage.last().and_then(|s| dbox_last_received(&s.1));
    }
    if let Some((n, exact)) = messages {
        fact(
            e,
            "Messages",
            if exact {
                fmt::count(n)
            } else {
                format!("at least {}", fmt::count(n))
            },
        );
    }
    if let Some(t) = oldest {
        fact(e, "Oldest message", fmt::day(t));
    }
    if let Some(t) = newest {
        fact(e, "Newest message", fmt::day(t));
    }
    e.title = format!(
        "Dovecot {format}: {} mailbox{}{}{}",
        fmt::count(boxes.len() as u64),
        if boxes.len() == 1 { "" } else { "es" },
        messages
            .map(|(n, exact)| format!(
                ", {}{} messages",
                if exact { "" } else { "at least " },
                fmt::count(n)
            ))
            .unwrap_or_default(),
        years(oldest, newest)
            .map(|y| format!(" ({y})"))
            .unwrap_or_default()
    );
    Ok(())
}

// ---------- Thunderbird

/// `user_pref("name", value);` lines of a prefs.js, values as text (strings unescaped).
pub fn parse_prefs(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("user_pref(") else {
            continue;
        };
        let Some((key, rest)) = js_string(rest.trim_start()) else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix(',') else {
            continue;
        };
        let rest = rest.trim_start();
        let value = if rest.starts_with('"') {
            match js_string(rest) {
                Some((v, _)) => v,
                None => continue,
            }
        } else {
            rest.split(')').next().unwrap_or("").trim().to_string()
        };
        out.insert(key, value);
    }
    out
}

/// A double-quoted JavaScript string at the start of `s`, and what follows it.
fn js_string(s: &str) -> Option<(String, &str)> {
    let mut chars = s.strip_prefix('"')?.char_indices();
    let mut out = String::new();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some((out, &s[i + 2..])),
            '\\' => {
                let (_, e) = chars.next()?;
                match e {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'u' => {
                        let hex: String =
                            (0..4).filter_map(|_| chars.next().map(|x| x.1)).collect();
                        out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                    }
                    'x' => {
                        let hex: String =
                            (0..2).filter_map(|_| chars.next().map(|x| x.1)).collect();
                        out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                    }
                    other => out.push(other),
                }
            }
            c => out.push(c),
        }
    }
    None
}

/// One Thunderbird account as prefs.js describes it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Account {
    pub name: String,
    /// imap, pop3, none (Local Folders), nntp, rss
    pub kind: String,
    pub host: String,
    pub user: String,
    /// its folder, relative to the profile ("ImapMail/imap.example.org")
    pub dir: String,
    /// "Name <address>" (or the address)
    pub identities: Vec<String>,
    pub emails: Vec<String>,
}

/// The accounts in a profile's prefs, in Thunderbird's own order.
pub fn accounts(prefs: &BTreeMap<String, String>) -> Vec<Account> {
    let get = |k: String| prefs.get(&k).cloned().unwrap_or_default();
    let mut ids: Vec<String> = get("mail.accountmanager.accounts".into())
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if ids.is_empty() {
        ids = prefs
            .keys()
            .filter_map(|k| {
                k.strip_prefix("mail.account.")?
                    .strip_suffix(".server")
                    .map(str::to_string)
            })
            .collect();
    }
    let mut out = Vec::new();
    for a in ids {
        let server = get(format!("mail.account.{a}.server"));
        if server.is_empty() {
            continue;
        }
        let s = |k: &str| get(format!("mail.server.{server}.{k}"));
        let dir = {
            let rel = s("directory-rel");
            match rel.strip_prefix("[ProfD]") {
                Some(r) => r.replace('\\', "/"),
                None => {
                    // an absolute path: its last two parts ("ImapMail/host")
                    let abs = s("directory").replace('\\', "/");
                    let parts: Vec<&str> = abs.split('/').filter(|p| !p.is_empty()).collect();
                    parts[parts.len().saturating_sub(2)..].join("/")
                }
            }
        };
        let mut acc = Account {
            name: s("name"),
            kind: s("type"),
            host: {
                let h = s("realhostname");
                if h.is_empty() { s("hostname") } else { h }
            },
            user: {
                let u = s("realuserName");
                if u.is_empty() { s("userName") } else { u }
            },
            dir,
            ..Account::default()
        };
        for id in get(format!("mail.account.{a}.identities")).split(',') {
            let id = id.trim();
            if id.is_empty() {
                continue;
            }
            let email = get(format!("mail.identity.{id}.useremail"));
            if email.is_empty() {
                continue;
            }
            let full = get(format!("mail.identity.{id}.fullName"));
            acc.identities.push(if full.is_empty() {
                email.clone()
            } else {
                format!("{full} <{email}>")
            });
            acc.emails.push(email);
        }
        out.push(acc);
    }
    out
}

/// Is this file in an account's folder one of its mbox folders? Thunderbird names them after the
/// folder (no extension) and keeps an index (.msf) beside each one it has opened.
fn is_mbox_folder(p: &Path, siblings: &HashMap<PathBuf, ()>) -> bool {
    let n = name(p);
    if n.starts_with('.') {
        return false;
    }
    let has_msf = siblings.contains_key(&p.with_file_name(format!("{n}.msf")));
    let ext = n.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    has_msf
        || ((ext.is_none() || ext.as_deref().is_some_and(|e| e.len() > 4))
            && !matches!(n.as_str(), "popstate" | "rules" | "Makefile"))
}

/// The folder an mbox file is: its path in the account's folder, ".sbd" left out ("Archives/2019").
fn folder_of(account_dir: &Path, mbox: &Path) -> String {
    mbox.strip_prefix(account_dir)
        .map(|r| {
            r.components()
                .map(|c| {
                    let s = c.as_os_str().to_string_lossy();
                    s.strip_suffix(".sbd").unwrap_or(&s).to_string()
                })
                .collect::<Vec<_>>()
                .join("/")
        })
        .unwrap_or_else(|_| name(mbox))
}

fn thunderbird(db: &Db, e: &mut Entity, profile: &Path, read_drive: bool) -> Result<()> {
    // each account's folder: ImapMail/<host> (a cache of server mail), Mail/<name> (local)
    let mut folders: BTreeMap<String, (bool, Vec<(String, u64)>)> = BTreeMap::new();
    for (area, imap) in [("ImapMail", true), ("Mail", false)] {
        let base = profile.join(area);
        let all = files_below(db, &base)?;
        let known: HashMap<PathBuf, ()> = all.iter().map(|(p, _, _)| (p.clone(), ())).collect();
        for (p, size, _) in &all {
            let Ok(rel) = p.strip_prefix(&base) else {
                continue;
            };
            let mut comps = rel.components();
            let Some(acct) = comps.next() else { continue };
            if comps.next().is_none() {
                continue; // a file directly in ImapMail/ or Mail/
            }
            let acct_dir = base.join(acct.as_os_str());
            if !is_mbox_folder(p, &known) {
                continue;
            }
            let key = format!("{area}/{}", acct.as_os_str().to_string_lossy());
            folders
                .entry(key)
                .or_insert((imap, Vec::new()))
                .1
                .push((folder_of(&acct_dir, p), *size));
        }
    }
    // prefs.js: names, addresses and servers
    let accts = if read_drive {
        std::fs::metadata(profile.join("prefs.js"))
            .ok()
            .filter(|m| m.len() <= 8 << 20)
            .and_then(|_| std::fs::read(profile.join("prefs.js")).ok())
            .map(|b| accounts(&parse_prefs(&String::from_utf8_lossy(&b))))
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let label = |dir: &str| -> String {
        accts
            .iter()
            .find(|a| a.dir.eq_ignore_ascii_case(dir))
            .map(|a| a.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| dir.rsplit('/').next().unwrap_or(dir).to_string())
    };
    let mut local = Vec::new();
    let mut cache = Vec::new();
    for (dir, (imap, list_)) in &folders {
        let total: u64 = list_.iter().map(|(_, s)| s).sum();
        let l = format!("{} ({})", label(dir), fmt::bytes(total));
        if *imap {
            cache.push(l);
        } else {
            local.push(l);
        }
    }
    if !accts.is_empty() {
        let names: Vec<String> = accts
            .iter()
            .filter(|a| !a.name.is_empty())
            .map(|a| a.name.clone())
            .collect();
        fact(e, "Account names", list(&names, 10));
        let mut people: Vec<String> = Vec::new();
        for i in accts.iter().flat_map(|a| a.identities.iter()) {
            if !people.contains(i) {
                people.push(i.clone());
            }
        }
        if !people.is_empty() {
            fact(e, "Email addresses", list(&people, 10));
        }
        let servers: Vec<String> = accts
            .iter()
            .filter(|a| !a.host.is_empty() && matches!(a.kind.as_str(), "imap" | "pop3" | "nntp"))
            .map(|a| {
                format!(
                    "{} ({}{})",
                    a.host,
                    match a.kind.as_str() {
                        "imap" => "IMAP",
                        "pop3" => "POP3",
                        _ => "news",
                    },
                    if a.user.is_empty() {
                        String::new()
                    } else {
                        format!(", as {}", a.user)
                    }
                )
            })
            .collect();
        if !servers.is_empty() {
            fact(e, "Servers", list(&servers, 10));
        }
        // mail is filed by the domain it's for
        if let Some(domain) = accts
            .iter()
            .flat_map(|a| a.emails.iter())
            .find_map(|m| m.rsplit_once('@').map(|(_, d)| d.to_ascii_lowercase()))
        {
            fact(e, "Main domain", domain);
        }
        let emails: Vec<String> = accts
            .iter()
            .flat_map(|a| a.emails.iter().cloned())
            .collect();
        if !emails.is_empty() {
            let mut uniq: Vec<String> = Vec::new();
            for m in emails {
                if !uniq.contains(&m) {
                    uniq.push(m);
                }
            }
            e.title = format!("mail of {}", list(&uniq, 3));
        }
    }
    if !local.is_empty() {
        fact(e, "Kept here (Local Folders, POP)", list(&local, 8));
    }
    if !cache.is_empty() {
        fact(e, "Copies of server mail (IMAP)", list(&cache, 8));
    }
    // which mbox file is which folder, biggest first, per account
    for (dir, (_, mut list_)) in folders {
        list_.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let shown: Vec<String> = list_
            .iter()
            .map(|(f, s)| format!("{f} ({})", fmt::bytes(*s)))
            .collect();
        fact(e, &format!("Folders in {}", label(&dir)), list(&shown, 8));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefs_and_accounts() {
        let p = parse_prefs(
            r#"// Mozilla User Preferences
user_pref("mail.accountmanager.accounts", "account1,account2,account3");
user_pref("mail.account.account1.identities", "id1");
user_pref("mail.account.account1.server", "server1");
user_pref("mail.account.account2.identities", "id2,id3");
user_pref("mail.account.account2.server", "server2");
user_pref("mail.account.account3.server", "server3");
user_pref("mail.identity.id1.fullName", "Ann \"Nan\" Example");
user_pref("mail.identity.id1.useremail", "ann@example.org");
user_pref("mail.identity.id2.useremail", "bob@example.net");
user_pref("mail.identity.id3.useremail", "b\u00f6b@example.net");
user_pref("mail.server.server1.directory-rel", "[ProfD]ImapMail/imap.example.org");
user_pref("mail.server.server1.hostname", "imap.example.org");
user_pref("mail.server.server1.name", "ann@example.org");
user_pref("mail.server.server1.type", "imap");
user_pref("mail.server.server1.userName", "ann");
user_pref("mail.server.server2.directory", "C:\\Users\\Ann\\AppData\\Roaming\\Thunderbird\\Profiles\\x.default\\Mail\\pop.example.net");
user_pref("mail.server.server2.hostname", "pop.example.net");
user_pref("mail.server.server2.name", "Bob at work");
user_pref("mail.server.server2.type", "pop3");
user_pref("mail.server.server3.directory-rel", "[ProfD]Mail/Local Folders");
user_pref("mail.server.server3.hostname", "Local Folders");
user_pref("mail.server.server3.name", "Local Folders");
user_pref("mail.server.server3.type", "none");
user_pref("mail.server.server3.check_new_mail", false);
"#,
        );
        assert_eq!(p["mail.server.server3.check_new_mail"], "false");
        let a = accounts(&p);
        assert_eq!(a.len(), 3);
        assert_eq!(a[0].identities, ["Ann \"Nan\" Example <ann@example.org>"]);
        assert_eq!(
            (a[0].kind.as_str(), a[0].dir.as_str()),
            ("imap", "ImapMail/imap.example.org")
        );
        assert_eq!(a[1].dir, "Mail/pop.example.net");
        assert_eq!(a[1].emails, ["bob@example.net", "böb@example.net"]);
        assert_eq!(a[2].dir, "Mail/Local Folders");
    }

    #[test]
    fn dbox_headers() {
        // struct mail_index_header: major 7, little-endian flag, 1234 messages
        let t = tempfile::tempdir().unwrap();
        let mut h = vec![0u8; 120];
        h[0] = 7;
        h[1] = 3;
        h[12] = 1;
        h[32..36].copy_from_slice(&1234u32.to_le_bytes());
        std::fs::write(t.path().join("dovecot.index"), &h).unwrap();
        assert_eq!(
            dovecot_index_messages(&t.path().join("dovecot.index")),
            Some(1234)
        );
        // an m.N file with two messages received at 0x5d000000 and 0x60000000
        let m = |body: &str, when: u32| {
            format!(
                "\x01\x02N {:016x}\n{body}\n\x01\x03\nGabc\nR{when:x}\nV{:x}\n\n",
                body.len(),
                body.len()
            )
        };
        let file = format!(
            "2 M15 C5c000000\n{}{}",
            m("Subject: one\n\nhello", 0x5d00_0000),
            m("Subject: two\n\nbye", 0x6000_0000)
        );
        std::fs::write(t.path().join("m.1"), file).unwrap();
        assert_eq!(
            dbox_first_received(&t.path().join("m.1")),
            Some(0x5d00_0000)
        );
        assert_eq!(dbox_last_received(&t.path().join("m.1")), Some(0x6000_0000));
    }
}
