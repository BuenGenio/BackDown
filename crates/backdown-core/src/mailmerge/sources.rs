//! Where the mail is: mbox files (separators and quoting, one message at a time), the mail stores
//! in a source, and reading every message once.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

// where an mbox's messages start: the same rule as the browser's mbox viewers
pub(super) use crate::mbox::{blank, is_from_line};

use super::*;

// ---------- mbox files

/// Undo mbox quoting: one '>' off every line that is '>'s then "From " (mboxrd; for the older
/// mboxo, which quoted only "From ", it's the same except for ">>From " lines, which are rare).
pub(super) fn unquote(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    for line in raw.split_inclusive(|&b| b == b'\n') {
        let q = line.iter().position(|&b| b != b'>').unwrap_or(line.len());
        if q > 0 && line[q..].starts_with(b"From ") {
            out.extend_from_slice(&line[1..]);
        } else {
            out.extend_from_slice(line);
        }
    }
    out
}

/// Quote for mboxrd: one more '>' on every line that is '>'s then "From ".
pub(super) fn quote(msg: &[u8], out: &mut Vec<u8>) {
    for line in msg.split_inclusive(|&b| b == b'\n') {
        let q = line.iter().position(|&b| b != b'>').unwrap_or(line.len());
        if line[q..].starts_with(b"From ") {
            out.push(b'>');
        }
        out.extend_from_slice(line);
    }
}

/// One message of an mbox: its "From " line and where its content is (quoted, as stored).
pub(super) struct MboxMsg {
    pub(super) envelope: Vec<u8>,
    /// the first byte after the "From " line, and the stored length (without the blank line
    /// that separates it from the next message)
    pub(super) start: u64,
    pub(super) len: u64,
    /// the content, unquoted
    pub(super) content: Vec<u8>,
}

/// Reads an mbox one message at a time.
pub(super) struct MboxReader<R: BufRead> {
    pub(super) r: R,
    pub(super) pos: u64,
    pub(super) next_from: Option<(Vec<u8>, u64)>,
    pub(super) started: bool,
    pub(super) line: Vec<u8>,
}

impl<R: BufRead> MboxReader<R> {
    pub(super) fn new(r: R) -> Self {
        MboxReader {
            r,
            pos: 0,
            next_from: None,
            started: false,
            line: Vec::new(),
        }
    }

    pub(super) fn read_line(&mut self) -> io::Result<usize> {
        self.line.clear();
        let n = self.r.read_until(b'\n', &mut self.line)?;
        self.pos += n as u64;
        Ok(n)
    }

    pub(super) fn next_msg(&mut self) -> io::Result<Option<MboxMsg>> {
        if !self.started {
            self.started = true;
            // the first line must be a "From " line: anything else isn't an mbox
            if self.read_line()? == 0 {
                return Ok(None);
            }
            if !self.line.starts_with(b"From ") {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "not an mbox file (it doesn't start with a \"From \" line)",
                ));
            }
            self.next_from = Some((self.line.clone(), self.pos));
        }
        let Some((envelope, start)) = self.next_from.take() else {
            return Ok(None);
        };
        let mut raw = Vec::new();
        let mut prev_blank = true;
        loop {
            if self.read_line()? == 0 {
                break;
            }
            if is_from_line(&self.line, prev_blank) {
                self.next_from = Some((self.line.clone(), self.pos));
                break;
            }
            prev_blank = blank(&self.line);
            raw.extend_from_slice(&self.line);
        }
        // the blank line before the next "From " belongs to the separator, not the message
        if raw.ends_with(b"\r\n\r\n") {
            raw.truncate(raw.len() - 2);
        } else if raw.ends_with(b"\n\n") || (self.next_from.is_some() && raw == b"\n") {
            raw.truncate(raw.len() - 1);
        }
        Ok(Some(MboxMsg {
            envelope,
            start,
            len: raw.len() as u64,
            content: unquote(&raw),
        }))
    }
}

/// The content of the mbox message stored at `start` for `len` bytes, unquoted.
pub(super) fn mbox_content(path: &Path, start: u64, len: u64) -> io::Result<Vec<u8>> {
    let mut f = File::open(path)?;
    f.seek(SeekFrom::Start(start))?;
    let mut raw = vec![0u8; len as usize];
    f.read_exact(&mut raw)?;
    Ok(unquote(&raw))
}

pub(super) fn looks_mbox(path: &Path) -> bool {
    let mut head = [0u8; 5];
    File::open(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok()
        && &head == b"From "
}

// ---------- finding the mail in a source

/// Where one folder's messages are.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Loc {
    /// a Maildir folder: its cur/ and new/
    Maildir(PathBuf),
    Mbox(PathBuf),
}

#[derive(Debug, Clone)]
pub(super) struct FolderAt {
    /// "INBOX", "Sent", "Archive.2019" (Maildir++ names, '.' between levels)
    pub(super) name: String,
    pub(super) loc: Loc,
}

/// One mailbox of a source: its folders. `key` places it in a store ("" for a lone mailbox).
#[derive(Debug, Clone)]
pub(super) struct MailboxAt {
    pub(super) key: String,
    pub(super) folders: Vec<FolderAt>,
}

#[derive(Debug, Clone)]
pub(super) struct SourceAt {
    pub(super) path: PathBuf,
    /// "Maildir", "mbox", "mbox folder", "mail store"
    pub(super) kind: &'static str,
    pub(super) mailboxes: Vec<MailboxAt>,
}

pub(super) fn is_maildir(p: &Path) -> bool {
    p.join("cur").is_dir() || p.join("new").is_dir()
}

/// A mailbox's top: a Maildir, or a folder holding Maildir++ ".Folder"s (an INBOX that was
/// never delivered to may lack its own cur/).
pub(super) fn is_mailbox_root(p: &Path) -> bool {
    is_maildir(p)
        || fs::read_dir(p).is_ok_and(|rd| {
            rd.filter_map(|e| e.ok()).any(|e| {
                e.file_name().to_string_lossy().starts_with('.')
                    && e.file_type().is_ok_and(|t| t.is_dir())
                    && is_maildir(&e.path())
            })
        })
}

/// A merged folder's name: composed (NFC), so a mailbox a Mac spelt decomposed merges with the
/// same mailbox from elsewhere instead of becoming a second folder.
pub(super) fn folder_name(parts: &[String]) -> String {
    let n = crate::names::nfc(&parts.join(".")).into_owned();
    if n.eq_ignore_ascii_case("inbox") {
        "INBOX".into()
    } else {
        n
    }
}

pub(super) fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A Maildir's folders: itself (INBOX), Maildir++ ".Folder" subfolders, and nested folders
/// (Dovecot's LAYOUT=fs).
pub(super) fn maildir_folders(root: &Path) -> io::Result<Vec<FolderAt>> {
    let mut out = vec![FolderAt {
        name: "INBOX".into(),
        loc: Loc::Maildir(root.to_path_buf()),
    }];
    fn nested(dir: &Path, parts: &mut Vec<String>, out: &mut Vec<FolderAt>, depth: usize) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        let mut kids: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
        kids.sort();
        for k in kids {
            let n = file_name(&k);
            if n.starts_with('.') || matches!(n.as_str(), "cur" | "new" | "tmp") || !k.is_dir() {
                continue;
            }
            parts.push(n);
            if is_maildir(&k) {
                out.push(FolderAt {
                    name: folder_name(parts),
                    loc: Loc::Maildir(k.clone()),
                });
            }
            if depth < 8 {
                nested(&k, parts, out, depth + 1);
            }
            parts.pop();
        }
    }
    let mut kids: Vec<PathBuf> = fs::read_dir(root)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    kids.sort();
    for k in &kids {
        let n = file_name(k);
        if let Some(sub) = n.strip_prefix('.')
            && !sub.is_empty()
            && sub != "."
            && k.is_dir()
            && is_maildir(k)
        {
            out.push(FolderAt {
                name: folder_name(&[sub.to_string()]),
                loc: Loc::Maildir(k.clone()),
            });
        }
    }
    nested(root, &mut Vec::new(), &mut out, 0);
    Ok(out)
}

/// A folder of mbox files, Thunderbird's way: each file a folder, "Name.sbd/" holding Name's
/// subfolders; the .msf files beside them are Thunderbird's indexes.
pub(super) fn mbox_folders(
    dir: &Path,
    parts: &mut Vec<String>,
    out: &mut Vec<FolderAt>,
    depth: usize,
) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut kids: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    kids.sort();
    for k in kids {
        let n = file_name(&k);
        if n.starts_with('.') {
            continue;
        }
        if k.is_file() && looks_mbox(&k) {
            let stem = n.strip_suffix(".mbox").unwrap_or(&n).to_string();
            parts.push(stem);
            out.push(FolderAt {
                name: folder_name(parts),
                loc: Loc::Mbox(k.clone()),
            });
            parts.pop();
        } else if k.is_dir()
            && let Some(sub) = n.strip_suffix(".sbd")
            && depth < 8
        {
            parts.push(sub.to_string());
            mbox_folders(&k, parts, out, depth + 1);
            parts.pop();
        }
    }
}

/// Mailboxes (Maildirs not inside another) below a store's folder, keyed by their path in it.
pub(super) fn store_mailboxes(root: &Path) -> io::Result<Vec<MailboxAt>> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<MailboxAt>, depth: usize) -> io::Result<()> {
        let mut kids: Vec<PathBuf> = fs::read_dir(dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        kids.sort();
        for k in kids {
            if !k.is_dir() || k.is_symlink() {
                continue;
            }
            if is_mailbox_root(&k) {
                let key = k
                    .strip_prefix(root)
                    .map(|r| r.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default();
                out.push(MailboxAt {
                    key,
                    folders: maildir_folders(&k)?,
                });
            } else if depth < 6 && !file_name(&k).starts_with('.') {
                walk(root, &k, out, depth + 1)?;
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(root, root, &mut out, 0)?;
    Ok(out)
}

/// What a source path holds.
pub(super) fn discover(path: &Path) -> Result<SourceAt> {
    let meta = fs::metadata(path).with_context(|| format!("can't open {}", path.display()))?;
    if meta.is_file() {
        if !looks_mbox(path) {
            bail!(
                "{} isn't an mbox file (it doesn't start with a \"From \" line)",
                path.display()
            );
        }
        let n = file_name(path);
        let stem = n
            .strip_suffix(".mbox")
            .or_else(|| n.strip_suffix(".mbx"))
            .unwrap_or(&n);
        let name = if stem.eq_ignore_ascii_case("mbox") {
            "INBOX".to_string()
        } else {
            folder_name(&[stem.to_string()])
        };
        return Ok(SourceAt {
            path: path.to_path_buf(),
            kind: "mbox",
            mailboxes: vec![MailboxAt {
                key: String::new(),
                folders: vec![FolderAt {
                    name,
                    loc: Loc::Mbox(path.to_path_buf()),
                }],
            }],
        });
    }
    if is_mailbox_root(path) {
        return Ok(SourceAt {
            path: path.to_path_buf(),
            kind: "Maildir",
            mailboxes: vec![MailboxAt {
                key: String::new(),
                folders: maildir_folders(path)?,
            }],
        });
    }
    // user/Maildir
    let inner = path.join("Maildir");
    if is_mailbox_root(&inner) {
        return Ok(SourceAt {
            path: path.to_path_buf(),
            kind: "Maildir",
            mailboxes: vec![MailboxAt {
                key: String::new(),
                folders: maildir_folders(&inner)?,
            }],
        });
    }
    let mut folders = Vec::new();
    mbox_folders(path, &mut Vec::new(), &mut folders, 0);
    if !folders.is_empty() {
        return Ok(SourceAt {
            path: path.to_path_buf(),
            kind: "mbox folder",
            mailboxes: vec![MailboxAt {
                key: String::new(),
                folders,
            }],
        });
    }
    let boxes = store_mailboxes(path)?;
    if boxes.is_empty() {
        bail!(
            "no mail found in {} (a Maildir, mbox files or a store of Maildirs)",
            path.display()
        );
    }
    Ok(SourceAt {
        path: path.to_path_buf(),
        kind: "mail store",
        mailboxes: boxes,
    })
}

// ---------- reading every message once

/// Where a message's bytes are.
#[derive(Debug, Clone)]
pub(super) enum At {
    File(PathBuf),
    Mbox { file: u32, start: u64, len: u64 },
}

/// One copy of a message, in one folder of one source.
#[derive(Debug, Clone)]
pub(super) struct Occ {
    pub(super) src: u16,
    pub(super) folder: u32,
    pub(super) at: At,
    pub(super) size: u64,
    pub(super) sha: [u8; 32],
    pub(super) flags: u8,
    pub(super) time: i64,
}

/// Everything read from the sources, by mailbox and identity.
#[derive(Default)]
pub(super) struct Catalog {
    pub(super) folders: Vec<String>,
    pub(super) mboxes: Vec<PathBuf>,
    pub(super) occs: Vec<Occ>,
    /// (mailbox key, identity) -> copies
    pub(super) ids: BTreeMap<(String, [u8; 32]), Vec<u32>>,
    pub(super) without_id: BTreeSet<(String, [u8; 32])>,
    /// per source: its newest message, and messages read
    pub(super) as_of: Vec<Option<i64>>,
    pub(super) messages: Vec<u64>,
    /// per source: (mailbox key, folder) it has
    pub(super) has_folder: Vec<BTreeSet<(String, String)>>,
    pub(super) unreadable: u64,
    pub(super) problems: Vec<String>,
}

impl Catalog {
    pub(super) fn folder_id(&mut self, name: &str) -> u32 {
        match self.folders.iter().position(|f| f == name) {
            Some(i) => i as u32,
            None => {
                self.folders.push(name.to_string());
                (self.folders.len() - 1) as u32
            }
        }
    }

    pub(super) fn add(&mut self, key: &str, occ: Occ, content: &[u8]) {
        let h = parse_headers(&content[..body_start(content)]);
        let (id, has_id) = identity(content, &h);
        let s = occ.src as usize;
        self.messages[s] += 1;
        if occ.time > 0 {
            self.as_of[s] = Some(self.as_of[s].map_or(occ.time, |t: i64| t.max(occ.time)));
        }
        let k = (key.to_string(), id);
        if !has_id {
            self.without_id.insert(k.clone());
        }
        self.occs.push(occ);
        self.ids
            .entry(k)
            .or_default()
            .push((self.occs.len() - 1) as u32);
    }
}

pub(super) fn read_maildir_folder(
    cat: &mut Catalog,
    src: u16,
    key: &str,
    folder: u32,
    dir: &Path,
    stop: &AtomicBool,
    tick: &mut dyn FnMut(u64, u64),
) -> Result<()> {
    for sub in ["cur", "new"] {
        let d = dir.join(sub);
        let Ok(rd) = fs::read_dir(&d) else { continue };
        let mut names: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
        names.sort();
        for p in names {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            let n = file_name(&p);
            if n.starts_with('.') {
                continue;
            }
            let Ok(meta) = fs::symlink_metadata(&p) else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let content = match fs::read(&p) {
                Ok(c) => c,
                Err(e) => {
                    cat.unreadable += 1;
                    push(
                        &mut cat.problems,
                        format!("can't read {}: {e}", p.display()),
                    );
                    continue;
                }
            };
            let time = delivered(&n)
                .or_else(|| {
                    meta.modified()
                        .ok()
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64)
                })
                .unwrap_or(0);
            let occ = Occ {
                src,
                folder,
                at: At::File(p.clone()),
                size: content.len() as u64,
                sha: Sha256::digest(&content).into(),
                flags: maildir_flags(&n),
                time,
            };
            tick(1, occ.size);
            cat.add(key, occ, &content);
        }
    }
    Ok(())
}

pub(super) fn read_mbox_folder(
    cat: &mut Catalog,
    src: u16,
    key: &str,
    folder: u32,
    path: &Path,
    stop: &AtomicBool,
    tick: &mut dyn FnMut(u64, u64),
) -> Result<()> {
    let f = File::open(path).with_context(|| format!("can't read {}", path.display()))?;
    let file = match cat.mboxes.iter().position(|m| m == path) {
        Some(i) => i as u32,
        None => {
            cat.mboxes.push(path.to_path_buf());
            (cat.mboxes.len() - 1) as u32
        }
    };
    let mut r = MboxReader::new(BufReader::with_capacity(1 << 18, f));
    while let Some(m) = r
        .next_msg()
        .with_context(|| format!("reading {}", path.display()))?
    {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let h = parse_headers(&m.content[..body_start(&m.content)]);
        let time = envelope_date(&m.envelope)
            .or_else(|| h.get("date").and_then(rfc2822))
            .unwrap_or(0);
        let occ = Occ {
            src,
            folder,
            at: At::Mbox {
                file,
                start: m.start,
                len: m.len,
            },
            size: m.content.len() as u64,
            sha: Sha256::digest(&m.content).into(),
            flags: mbox_flags(&h),
            time,
        };
        tick(1, occ.size);
        cat.add(key, occ, &m.content);
    }
    Ok(())
}
