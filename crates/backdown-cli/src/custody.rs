//! `backdown custody`: export the chain of custody (backdown_core::custody) as a signed bundle,
//! verify a bundle, check the log in the index; and sealing it, with this installation's key, at
//! the end of every job that added to it (the daemon) and after a command that did (the CLI).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use backdown_core::custody::{self, Filter, Report};
use backdown_core::signing::Signer;
use backdown_core::{Db, fmt};
use clap::Subcommand;

use crate::{client, locations};

#[derive(Subcommand)]
pub enum CustodyCmd {
    /// Write the chain of custody as a signed JSON bundle, sealing it first. With a filter, the
    /// events that don't match are elided: only their number, digest and hash are kept, so the
    /// chain still checks from end to end without showing their paths
    Export {
        /// The file to write (never overwritten), or - for standard output
        #[arg(long, value_name = "FILE")]
        out: PathBuf,
        /// Only show what was done from this day on (UTC): 2026-10-01, or 2026-10-01T14:30
        #[arg(long, value_name = "DATE")]
        since: Option<String>,
        /// Only show one job's events: plan:N, wipe:N, dedup:N, merge:N, rescue:N or mirror:N
        #[arg(long = "ref", value_name = "JOB")]
        job: Option<String>,
        /// Only show what was done to files in this folder (or to this file)
        #[arg(long, value_name = "PREFIX")]
        path: Option<PathBuf>,
    },
    /// Check an exported bundle: every event against its hash and the one before it, every
    /// seal's signature, that the seals reach the last event, and the bundle's own signature.
    /// Exits 1, saying what doesn't check out, if anything doesn't
    Verify {
        file: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Check the chain of custody in the index the same way: whether anyone changed the
    /// database behind BackDown's back. Exits 1 if anything doesn't check out
    Check {
        #[arg(long)]
        json: bool,
    },
}

/// This installation's signing key (made on first use): the one manifests are signed with too.
pub fn key_path() -> PathBuf {
    locations::state_dir().join("signing.key")
}

fn signer() -> Result<Signer> {
    Signer::load_or_create(&key_path())
}

/// This installation's key's fingerprint, if it has one (never makes one).
fn own_fingerprint() -> Option<String> {
    key_path()
        .is_file()
        .then(|| signer().ok().map(|s| s.fingerprint()))
        .flatten()
}

/// The index: `--db`, else the running daemon's, else the default.
fn index_path(db: Option<PathBuf>) -> PathBuf {
    db.or_else(|| client::Client::connect(None).map(|c| PathBuf::from(c.db)))
        .unwrap_or_else(locations::default_db)
}

/// Whether the index has the log (an index last opened by an older BackDown doesn't yet).
fn has_log(conn: &rusqlite::Connection) -> bool {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'custody')",
        [],
        |r| r.get(0),
    )
    .unwrap_or(false)
}

fn filter(since: Option<&str>, job: Option<&str>, path: Option<&Path>) -> Result<Filter> {
    Ok(Filter {
        since: since.map(custody::parse_date).transpose()?,
        job: job.map(custody::parse_ref).transpose()?,
        path: path.map(custody::path_filter),
    })
}

pub fn command(db: Option<PathBuf>, action: CustodyCmd) -> Result<ExitCode> {
    match action {
        CustodyCmd::Verify { file, json } => {
            let f = std::fs::File::open(&file)
                .with_context(|| format!("can't open {}", file.display()))?;
            let rep = custody::verify(f)?;
            print_report(&rep, &file.display().to_string(), json, true)
        }
        CustodyCmd::Check { json } => {
            let path = index_path(db);
            if !path.is_file() {
                bail!("there's no index at {}", path.display());
            }
            let db = Db::open_read_only(&path)?;
            let rep = if has_log(&db.conn) {
                custody::check(&db.conn)?
            } else {
                Report {
                    ok: true,
                    ..Report::default()
                }
            };
            print_report(
                &rep,
                &format!("The chain of custody in {}", path.display()),
                json,
                false,
            )
        }
        CustodyCmd::Export {
            out,
            since,
            job,
            path,
        } => export(
            &index_path(db),
            &out,
            filter(since.as_deref(), job.as_deref(), path.as_deref())?,
        ),
    }
}

fn export(db_path: &Path, out: &Path, filter: Filter) -> Result<ExitCode> {
    let to_stdout = out.as_os_str() == "-";
    if !to_stdout && out.exists() {
        bail!(
            "{} exists: pick a new file name (an export is never written over)",
            out.display()
        );
    }
    let db = Db::open(db_path)?;
    let signer = signer()?;
    // keep a seal of the head first; if the index is busy, the export carries one made for it
    let _ = db.conn.busy_timeout(Duration::from_secs(5));
    if let Err(e) = custody::seal(&db.conn, &signer) {
        eprintln!("note: couldn't seal the chain of custody: {e:#}");
    }
    let _ = db.conn.busy_timeout(Duration::from_secs(30));
    let ex = if to_stdout {
        let mut w = std::io::BufWriter::new(std::io::stdout().lock());
        custody::export(&db.conn, &signer, &filter, &mut w)?
    } else {
        // written beside it, then moved into place: never a half-written export
        let name = out.file_name().context("--out needs a file name")?;
        let dir = out
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let tmp = dir.join(format!(".{}.backdown-partial", name.to_string_lossy()));
        let r = (|| -> Result<custody::Exported> {
            let f = std::fs::File::create(&tmp)
                .with_context(|| format!("can't write {}", tmp.display()))?;
            let mut w = std::io::BufWriter::new(f);
            let ex = custody::export(&db.conn, &signer, &filter, &mut w)?;
            w.into_inner()
                .map_err(|e| e.into_error())?
                .sync_all()
                .context("can't write the export")?;
            if out.exists() {
                bail!("{} appeared meanwhile: left as it is", out.display());
            }
            std::fs::rename(&tmp, out)?;
            Ok(ex)
        })();
        if r.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        r?
    };
    let shown = if to_stdout {
        "the export".to_string()
    } else {
        out.display().to_string()
    };
    let mut err = std::io::stderr();
    let _ = writeln!(
        err,
        "Wrote {shown}: {} event{}{}.",
        ex.events,
        if ex.events == 1 { "" } else { "s" },
        if ex.elided > 0 {
            format!(
                ", {} shown and {} elided (only events {} are shown; the others keep their number, digest and hash, not their paths)",
                ex.shown,
                ex.elided,
                filter.describe()
            )
        } else {
            String::new()
        }
    );
    if ex.events > 0 && ex.problems.is_empty() {
        let _ = writeln!(
            err,
            "Sealed up to event {}{}; signed with this installation's key {}.",
            ex.sealed_to,
            if ex.sealed_now {
                " (a seal made for this export)"
            } else {
                ""
            },
            ex.key
        );
    } else {
        let _ = writeln!(err, "Signed with this installation's key {}.", ex.key);
    }
    let _ = writeln!(err, "{}", custody::start_note(ex.log_starts));
    if !ex.problems.is_empty() {
        let _ = writeln!(
            err,
            "\nThe log in the index DOESN'T CHECK OUT, so the export has no new seal:"
        );
        for p in &ex.problems {
            let _ = writeln!(err, "  - {p}");
        }
        return Ok(ExitCode::FAILURE);
    }
    if !to_stdout {
        let _ = writeln!(
            err,
            "Check it with: backdown custody verify {}",
            out.display()
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn print_report(rep: &Report, what: &str, json: bool, bundle: bool) -> Result<ExitCode> {
    let code = if rep.ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    };
    if json {
        println!("{}", serde_json::to_string_pretty(rep)?);
        return Ok(code);
    }
    let own = own_fingerprint();
    let whose = |k: &str| {
        if own.as_deref() == Some(k) {
            format!("{k} (this installation's)")
        } else {
            k.to_string()
        }
    };
    if rep.ok {
        println!("{what} checks out.");
    } else {
        println!("{what} DOESN'T CHECK OUT:");
        for p in &rep.problems {
            println!("  - {p}");
        }
        if rep.more_problems > 0 {
            println!("  … and {} more", rep.more_problems);
        }
        println!();
    }
    let span = match (rep.first_at, rep.last_at) {
        (Some(a), Some(b)) if fmt::day(a) == fmt::day(b) => format!(", done on {}", fmt::day(a)),
        (Some(a), Some(b)) => format!(", done {} to {}", fmt::day(a), fmt::day(b)),
        _ => String::new(),
    };
    println!(
        "  {} event{}{}{}",
        rep.events,
        if rep.events == 1 { "" } else { "s" },
        if rep.elided > 0 {
            format!(
                " ({} of them elided: just their number, digest and hash)",
                rep.elided
            )
        } else {
            String::new()
        },
        span
    );
    if rep.seals > 0 {
        println!(
            "  sealed up to event {} ({} seal{}), by {} {}",
            rep.sealed_to,
            rep.seals,
            if rep.seals == 1 { "" } else { "s" },
            if rep.seal_keys.len() == 1 {
                "key"
            } else {
                "keys"
            },
            rep.seal_keys
                .iter()
                .map(|k| whose(k))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if rep.unsealed > 0 {
        println!(
            "  {} event{} after the last seal: sealed when the job adding them ends, or by an export",
            rep.unsealed,
            if rep.unsealed == 1 { "" } else { "s" }
        );
    }
    if bundle {
        if let (Some(at), Some(k)) = (rep.created, &rep.key) {
            println!("  exported {}, signed by key {}", crate::utc(at), whose(k));
        }
        if rep.seal_keys.iter().any(|k| Some(k) != rep.key.as_ref()) {
            println!(
                "  sealed by a key other than the one that signed the export: check whose it is"
            );
        }
    }
    println!(
        "  {}",
        custody::start_note(rep.log_starts)
            .replace("The log starts", "the log starts")
            .replace("What happened", "what happened")
    );
    if !rep.ok {
        println!(
            "\nWhat a seal proves is that its key saw the log, as it was then, up to that event; it can't say what happened outside BackDown."
        );
    }
    Ok(code)
}

/// Seal the head if something was added since the last seal (the daemon, after each job). Says
/// so in the log; problems too (a log that doesn't check out isn't sealed).
pub fn seal_new(db: &Db) {
    if !matches!(custody::head(&db.conn), Ok(Some(_))) {
        return; // nothing recorded yet: no key is made before it's needed
    }
    let r = signer().and_then(|s| {
        if custody::sealed(&db.conn, &s)? {
            return Ok(None);
        }
        custody::seal(&db.conn, &s)
    });
    match r {
        Ok(Some(doc)) => eprintln!("sealed the chain of custody up to event {}", doc["seq"]),
        Ok(None) => {}
        Err(e) => eprintln!("couldn't seal the chain of custody: {e:#}"),
    }
}

/// After a command run here (not by the daemon): seal what it added. Quietly: a busy index (the
/// daemon's job) is left for the daemon to seal when that job ends.
pub fn seal_after(db_path: &Path) {
    use rusqlite::OpenFlags;
    let Ok(conn) = rusqlite::Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return;
    };
    let _ = conn.busy_timeout(Duration::from_secs(2));
    if !has_log(&conn) || !matches!(custody::head(&conn), Ok(Some(_))) {
        return;
    }
    let r = signer().and_then(|s| {
        if custody::sealed(&conn, &s)? {
            return Ok(None);
        }
        custody::seal(&conn, &s)
    });
    if let Err(e) = r {
        let busy = e.chain().any(|c| {
            matches!(
                c.downcast_ref::<rusqlite::Error>(),
                Some(rusqlite::Error::SqliteFailure(f, _)) if f.code == rusqlite::ErrorCode::DatabaseBusy
            )
        });
        if !busy {
            eprintln!("note: couldn't seal the chain of custody: {e:#}");
        }
    }
}

/// GET /api/custody: an export to download, from the index read-only (the seal it carries, if
/// the head wasn't sealed yet, isn't kept). The file name and the bytes.
pub fn download(
    db_path: &Path,
    since: Option<&str>,
    job: Option<&str>,
    path: Option<&str>,
) -> Result<(String, Vec<u8>)> {
    let filter = filter(since, job, path.map(Path::new))?;
    let db = Db::open_read_only(db_path)?;
    if !has_log(&db.conn) {
        bail!("this index has no chain of custody yet: it starts when BackDown 1.0 first opens it");
    }
    let signer = signer()?;
    let mut out = Vec::new();
    custody::export(&db.conn, &signer, &filter, &mut out)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    Ok((
        format!("backdown-custody-{}.json", &crate::utc(now)[..10]),
        out,
    ))
}
