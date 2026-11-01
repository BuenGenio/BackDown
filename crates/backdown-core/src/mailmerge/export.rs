//! A Maildir folder exported as one mbox file.

use std::fs::{self, File};
use std::io::{self, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result, bail};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::clock::now;
use crate::{Db, fastcopy, fmt};

use super::*;

// ---------- mbox export

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExportReport {
    pub maildir: String,
    pub mbox: String,
    pub messages: u64,
    pub bytes: u64,
    /// messages that didn't end in a line break, given one (mbox can't store them otherwise)
    pub newline_added: u64,
    /// ">From " quoting added (lines in bodies that start with "From ")
    pub quoted_lines: u64,
}

/// The envelope sender for a "From " line: Return-Path, else the From address, else
/// MAILER-DAEMON (as mail programs write it).
pub(super) fn envelope_sender(h: &Headers) -> String {
    let pick = |v: &str| -> Option<String> {
        let a = match (v.find('<'), v.rfind('>')) {
            (Some(i), Some(j)) if j > i + 1 => &v[i + 1..j],
            _ => v.split_whitespace().find(|w| w.contains('@'))?,
        };
        let a = a.trim_matches(|c: char| c == '"' || c == '<' || c == '>');
        (!a.is_empty() && !a.contains(char::is_whitespace)).then(|| a.to_string())
    };
    h.get("return-path")
        .and_then(pick)
        .or_else(|| h.get("from").and_then(pick))
        .unwrap_or_else(|| "MAILER-DAEMON".into())
}

/// Export one Maildir folder (its cur/ and new/: not its subfolders) as one mboxrd file, oldest
/// message first. Written beside `out` under a temporary name, flushed, read back and compared
/// message by message, then renamed into place. Never overwrites a file.
pub fn export(
    db: Option<&Db>,
    maildir: &Path,
    out: &Path,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<ExportReport> {
    if !is_maildir(maildir) {
        bail!(
            "{} isn't a Maildir folder (no cur/ or new/)",
            maildir.display()
        );
    }
    if out.exists() {
        bail!("{} exists: pick a new file name", out.display());
    }
    let parent = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if parent.starts_with(maildir) {
        bail!("write the mbox outside the Maildir");
    }
    fs::create_dir_all(parent)?;
    let mut msgs: Vec<(i64, PathBuf)> = Vec::new();
    for sub in ["cur", "new"] {
        let Ok(rd) = fs::read_dir(maildir.join(sub)) else {
            continue;
        };
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            let n = file_name(&p);
            if n.starts_with('.') || !e.file_type().is_ok_and(|t| t.is_file()) {
                continue;
            }
            let t = delivered(&n)
                .or_else(|| {
                    e.metadata()
                        .ok()?
                        .modified()
                        .ok()?
                        .duration_since(UNIX_EPOCH)
                        .ok()
                        .map(|d| d.as_secs() as i64)
                })
                .unwrap_or(0);
            msgs.push((t, p));
        }
    }
    msgs.sort();
    let tmp = parent.join(format!(".{}.backdown-partial", file_name(out)));
    let mut rep = ExportReport {
        maildir: maildir.display().to_string(),
        mbox: out.display().to_string(),
        ..ExportReport::default()
    };
    // what each message should read back as
    let mut expect: Vec<[u8; 32]> = Vec::with_capacity(msgs.len());
    let result = (|| -> Result<()> {
        let mut w = io::BufWriter::with_capacity(1 << 20, File::create(&tmp)?);
        let mut buf = Vec::new();
        for (t, p) in &msgs {
            let mut content = fs::read(p).with_context(|| format!("can't read {}", p.display()))?;
            if !content.ends_with(b"\n") {
                content.push(b'\n');
                rep.newline_added += 1;
            }
            let h = parse_headers(&content[..body_start(&content)]);
            let when = if *t > 0 {
                *t
            } else {
                h.get("date").and_then(rfc2822).unwrap_or(0)
            };
            buf.clear();
            buf.extend_from_slice(
                format!("From {} {}\n", envelope_sender(&h), asctime(when)).as_bytes(),
            );
            let before = buf.len();
            quote(&content, &mut buf);
            rep.quoted_lines += (buf.len() - before - content.len()) as u64;
            buf.push(b'\n');
            w.write_all(&buf)?;
            expect.push(Sha256::digest(&content).into());
            rep.messages += 1;
            rep.bytes += content.len() as u64;
            progress(rep.messages, rep.bytes);
        }
        let f = w.into_inner().map_err(|e| e.into_error())?;
        f.sync_all()?;
        fastcopy::forget_cached(&f);
        drop(f);
        // read it back as any mbox reader would
        let mut r = MboxReader::new(BufReader::new(File::open(&tmp)?));
        let mut i = 0usize;
        while let Some(m) = r.next_msg()? {
            let sum: [u8; 32] = Sha256::digest(&m.content).into();
            if expect.get(i) != Some(&sum) {
                bail!(
                    "message {} of the mbox read back differently: the export was removed",
                    i + 1
                );
            }
            i += 1;
        }
        if i != expect.len() {
            bail!(
                "the mbox read back {} messages instead of {}: the export was removed",
                i,
                expect.len()
            );
        }
        if out.exists() {
            bail!("{} appeared while exporting", out.display());
        }
        fs::rename(&tmp, out)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result?;
    if let Some(db) = db {
        db.conn.execute(
            "INSERT INTO mail_merges(created_at, kind, sources, dest, state, summary, report) VALUES (?1, 'export', ?2, ?3, 'done', ?4, ?5)",
            params![
                now(),
                serde_json::to_string(&[maildir.display().to_string()])?,
                out.display().to_string(),
                format!(
                    "exported {} messages ({}) as mboxrd",
                    fmt::count(rep.messages),
                    fmt::bytes(rep.bytes)
                ),
                serde_json::to_string(&rep)?
            ],
        )?;
    }
    Ok(rep)
}
