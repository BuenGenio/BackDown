//! `backdown dedup` (identical files in an archive share their storage) and `backdown mail
//! merge|export` (snapshots of a mailbox made into one Maildir; a Maildir folder as an mbox).

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result};
use backdown_core::dedup::{self, Link};
use backdown_core::{Db, fmt, mailmerge};

use crate::daemon::{Counts, Daemon, Job};
use crate::{clear, done, live};

fn absolute(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(p)
    }
}

/// A running daemon for this index, unless told to work alone.
fn daemon(db_path: &Path, use_daemon: bool) -> Option<crate::client::Client> {
    if use_daemon {
        crate::client::Client::connect(Some(db_path))
    } else {
        None
    }
}

// ---------- dedup

fn print_dedup(r: &dedup::Report, dry: bool) {
    let link = r.link.unwrap_or(Link::Hard);
    let fs = r
        .filesystem
        .as_deref()
        .map(|f| format!(" ({f})"))
        .unwrap_or_default();
    println!(
        "{}{fs}: {}",
        r.folder,
        match link {
            Link::Hard => "hardlinks",
            Link::Clone => "clones",
        }
    );
    let would = if dry { "would be" } else { "were" };
    println!(
        "  {} set{} of identical files: {} files {would} linked, saving {}",
        fmt::count(r.groups),
        if r.groups == 1 { "" } else { "s" },
        fmt::count(r.linked),
        fmt::bytes(r.saved_bytes)
    );
    if r.already > 0 {
        println!("  {} files already share storage", fmt::count(r.already));
    }
    if r.kept_apart > 0 {
        println!(
            "  {} identical files kept apart: their permissions, owner or extended attributes{} differ from every other copy's",
            fmt::count(r.kept_apart),
            if link == Link::Hard {
                ", or modification time,"
            } else {
                ""
            }
        );
    }
    if r.elsewhere > 0 {
        println!(
            "  {} files have other names outside this folder: left alone",
            fmt::count(r.elsewhere)
        );
    }
    if r.changed > 0 {
        println!(
            "  {} files changed since the last scan (or can't be read): left alone",
            fmt::count(r.changed)
        );
    }
    if r.failed > 0 {
        println!("  {} failed", fmt::count(r.failed));
    }
    if r.unhashed > 0 {
        println!(
            "  {} files aren't hashed yet, so their duplicates aren't known: `backdown hash` first",
            fmt::count(r.unhashed)
        );
    }
    if link == Link::Clone && !r.clones_possible {
        println!(
            "  This folder is on {}, which can't share blocks between files: clones need btrfs, XFS or APFS (--link hard works anywhere).",
            r.filesystem.as_deref().unwrap_or("a filesystem")
        );
    }
    if !r.examples.is_empty() {
        println!("Biggest savings:");
        for g in r.examples.iter().take(10) {
            let rel = |p: &str| {
                p.strip_prefix(&r.folder)
                    .map(|x| x.trim_start_matches(['/', '\\']).to_string())
                    .unwrap_or_else(|| p.to_string())
            };
            println!(
                "  {:>9}  {} ← {}",
                fmt::bytes(g.saves),
                rel(&g.keeper),
                g.others
                    .iter()
                    .map(|o| rel(o))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    for p in &r.problems {
        eprintln!("  {p}");
    }
    println!("{}", r.warning);
}

pub struct DedupArgs<'a> {
    pub folder: &'a Path,
    pub link: &'a str,
    pub min_size: u64,
    pub go: bool,
    pub json: bool,
    pub use_daemon: bool,
}

pub fn dedup(db: &Db, db_path: &Path, a: DedupArgs) -> Result<ExitCode> {
    let link = Link::parse(a.link)?;
    let folder = dedup::check_folder(&absolute(a.folder), Some(db_path))?;
    if !a.go {
        let r = dedup::assess(db, &folder, link, a.min_size)?;
        if a.json {
            println!("{}", serde_json::to_string_pretty(&r)?);
            return Ok(ExitCode::SUCCESS);
        }
        print_dedup(&r, true);
        if r.linked > 0 && (link == Link::Hard || r.clones_possible) {
            println!(
                "\nNothing was changed. To link them: backdown dedup \"{}\" --link {}{} --go",
                folder.display(),
                link.name(),
                if Some(a.min_size) == fmt::parse_size("4K").ok() {
                    String::new()
                } else {
                    format!(" --min-size {}", a.min_size)
                }
            );
        } else {
            println!("\nNothing was changed.");
        }
        return Ok(ExitCode::SUCCESS);
    }
    let id = dedup::record(db, &folder, link, a.min_size)?;
    // a running daemon owns the index: it does the linking, then rescans the folder's drive
    if let Some(mut c) = daemon(db_path, a.use_daemon) {
        let r =
            c.request(&serde_json::json!({ "cmd": "enqueue", "kind": "dedup", "dedup": id }))?;
        let j = c.follow(r["job"].as_u64().unwrap_or(0), |j| {
            live(&format!(
                "linking: {}",
                j["progress"].as_str().unwrap_or("")
            ))
        })?;
        clear();
        done(j["summary"].as_str().unwrap_or(""));
        println!("{}", dedup::warning(link));
        return Ok(ExitCode::SUCCESS);
    }
    let r = dedup::run(
        db,
        id,
        Some(db_path),
        &AtomicBool::new(false),
        &mut |n, b| {
            live(&format!(
                "re-reading and linking: {} files, {}",
                fmt::count(n),
                fmt::bytes(b)
            ))
        },
    )?;
    clear();
    if a.json {
        println!("{}", serde_json::to_string_pretty(&r)?);
    } else {
        print_dedup(&r, false);
        eprintln!(
            "Index the drive again so BackDown sees the links: backdown index <the folder's drive>"
        );
    }
    Ok(if r.failed > 0 {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    })
}

// ---------- mail

fn print_merge(r: &mailmerge::Report) {
    println!("Snapshots, oldest first:");
    for (i, s) in r.sources.iter().enumerate() {
        println!(
            "  {}. {} ({}): {} messages{}",
            i + 1,
            s.path,
            s.kind,
            fmt::count(s.messages),
            s.as_of
                .map(|t| format!(", the newest from {}", fmt::day(t)))
                .unwrap_or_default()
        );
        if s.only_here > 0 {
            println!(
                "     only in this one: {} message{}",
                fmt::count(s.only_here),
                if s.only_here == 1 { "" } else { "s" }
            );
        }
        if !s.folders_only_here.is_empty() {
            println!("     folders only here: {}", s.folders_only_here.join(", "));
        }
    }
    println!(
        "{} {} distinct messages ({}){}, in {} folders; {} duplicate copies left out",
        if r.dry_run { "Would merge" } else { "Merged" },
        fmt::count(r.distinct),
        fmt::bytes(r.bytes),
        if r.mailboxes > 1 {
            format!(" in {} mailboxes", fmt::count(r.mailboxes))
        } else {
            String::new()
        },
        r.folders.len(),
        fmt::count(r.duplicates)
    );
    let line = |n: u64, what: &str| {
        if n > 0 {
            println!("  {what}: {}", fmt::count(n));
        }
    };
    line(
        r.messages.saturating_sub(r.distinct),
        "extra copies kept because the newest snapshot has a message in more than one folder",
    );
    line(
        r.without_id,
        "without a Message-ID (matched by headers and body)",
    );
    line(
        r.moved,
        "in another folder in an older snapshot (placed where the newest has them)",
    );
    line(
        r.differing_copies,
        "copies differing only in headers a server or mail program added (the newest kept)",
    );
    line(
        r.flags_merged,
        "flags combined from several copies (read, replied or flagged in any)",
    );
    let folders: Vec<String> = r
        .folders
        .iter()
        .take(12)
        .map(|(f, n)| format!("{f} {}", fmt::count(*n)))
        .collect();
    if !folders.is_empty() {
        println!(
            "  Folders: {}{}",
            folders.join(" · "),
            if r.folders.len() > 12 { " · …" } else { "" }
        );
    }
    if !r.dry_run {
        println!(
            "Written and verified: {} ({}){}",
            fmt::count(r.written),
            fmt::bytes(r.written_bytes),
            if r.resumed > 0 {
                format!("; {} were there already", fmt::count(r.resumed))
            } else {
                String::new()
            }
        );
    }
    if r.failed + r.unreadable > 0 {
        println!(
            "  {} failed or couldn't be read:",
            fmt::count(r.failed + r.unreadable)
        );
    }
    for p in &r.problems {
        eprintln!("  {p}");
    }
}

pub fn mail_merge(
    db: &Db,
    db_path: &Path,
    sources: &[PathBuf],
    to: &Path,
    dry_run: bool,
    json: bool,
    use_daemon: bool,
) -> Result<ExitCode> {
    let to = absolute(to);
    let sources: Vec<PathBuf> = sources.iter().map(|s| absolute(s)).collect();
    let no = AtomicBool::new(false);
    let mut tick = |what: &str, n: u64, b: u64| {
        live(&format!(
            "{}: {} messages, {}",
            if what.is_empty() { "writing" } else { what },
            fmt::count(n),
            fmt::bytes(b)
        ))
    };
    if dry_run {
        let r = mailmerge::dry_run(&sources, &to, &no, &mut tick)?;
        clear();
        if json {
            println!("{}", serde_json::to_string_pretty(&r)?);
        } else {
            print_merge(&r);
            println!(
                "\nNothing was written. Without --dry-run, the merged mailbox goes to {}",
                to.display()
            );
        }
        return Ok(ExitCode::SUCCESS);
    }
    let id = match mailmerge::unfinished(db, &sources, &to)? {
        Some(id) => {
            eprintln!("carrying on with merge {id}, which didn't finish");
            id
        }
        None => mailmerge::record(db, &sources, &to, None)?,
    };
    let r = if let Some(mut c) = daemon(db_path, use_daemon) {
        let q =
            c.request(&serde_json::json!({ "cmd": "enqueue", "kind": "mail-merge", "merge": id }))?;
        c.follow(q["job"].as_u64().unwrap_or(0), |j| {
            live(&format!(
                "merging: {}",
                j["progress"].as_str().unwrap_or("")
            ))
        })?;
        clear();
        mailmerge::report_of(db, id)?.context("the merge left no report")?
    } else {
        let r = mailmerge::run(db, id, &no, &mut tick)?;
        clear();
        r
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&r)?);
    } else {
        print_merge(&r);
        println!(
            "The sources were only read. Each message was written to {}, read back and journaled (merge {id}).",
            to.display()
        );
    }
    Ok(if r.failed + r.unreadable > 0 {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    })
}

// ---------- as the daemon's jobs

/// Carry out a recorded dedup run (the daemon's "dedup" job).
pub fn run_dedup_job(d: &Daemon, db: &Db, job: &Job, stop: &AtomicBool) -> Result<String> {
    let id = job.opts.dedup.context("dedup without a recorded run")?;
    let r = dedup::run(db, id, Some(&d.db), stop, &mut |n, b| {
        d.progress(
            job.id,
            format!(
                "{} files re-read and linked, {}",
                fmt::count(n),
                fmt::bytes(b)
            ),
            Counts {
                stage: "linking",
                files: n,
                bytes: b,
                ..Counts::default()
            },
        )
    })?;
    Ok(format!("dedup {id}: {}", dedup::summary(&r)))
}

/// Carry out a recorded mail merge (the daemon's "mail-merge" job).
pub fn run_merge_job(d: &Daemon, db: &Db, job: &Job, stop: &AtomicBool) -> Result<String> {
    let id = job
        .opts
        .merge
        .context("mail-merge without a recorded merge")?;
    let r = mailmerge::run(db, id, stop, &mut |what, n, b| {
        d.progress(
            job.id,
            format!(
                "{}: {} messages, {}",
                if what.is_empty() { "writing" } else { what },
                fmt::count(n),
                fmt::bytes(b)
            ),
            Counts {
                stage: "merging",
                files: n,
                bytes: b,
                ..Counts::default()
            },
        )
    });
    match r {
        Ok(r) => Ok(format!("merge {id}: {}", mailmerge::summary(&r))),
        // stopped while reading the snapshots: nothing written yet, nothing lost
        Err(_) if stop.load(std::sync::atomic::Ordering::Relaxed) => {
            Ok(format!("merge {id}: stopped; run it again to carry on"))
        }
        Err(e) => Err(e),
    }
}

/// Where a finished dedup or merge changed files (to rescan there), and why.
pub fn changed_place(db: &Db, job: &Job) -> Option<(PathBuf, &'static str)> {
    match job.kind {
        "dedup" => dedup::load(db, job.opts.dedup?)
            .ok()
            .map(|(f, ..)| (f, "after-dedup")),
        "mail-merge" => mailmerge::load(db, job.opts.merge?)
            .ok()
            .map(|(_, dest, _)| (dest, "after-merge")),
        _ => None,
    }
}

pub fn mail_export(db: &Db, maildir: &Path, mbox: &Path) -> Result<ExitCode> {
    let r = mailmerge::export(
        Some(db),
        &absolute(maildir),
        &absolute(mbox),
        &mut |n, b| {
            live(&format!(
                "exporting: {} messages, {}",
                fmt::count(n),
                fmt::bytes(b)
            ))
        },
    )?;
    done(&format!(
        "exported {} messages ({}) to {} as mboxrd, read back and verified",
        fmt::count(r.messages),
        fmt::bytes(r.bytes),
        r.mbox
    ));
    if r.quoted_lines > 0 {
        eprintln!(
            "  {} lines starting with \"From \" were quoted (\">From \"), as mbox needs",
            fmt::count(r.quoted_lines)
        );
    }
    if r.newline_added > 0 {
        eprintln!(
            "  {} messages didn't end with a line break and were given one",
            fmt::count(r.newline_added)
        );
    }
    eprintln!(
        "  Only this folder's messages (not its subfolders); read, replied and flagged marks aren't kept: mbox has no standard place for them."
    );
    Ok(ExitCode::SUCCESS)
}
