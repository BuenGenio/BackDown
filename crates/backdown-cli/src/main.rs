//! `backdown`: find out what's on your drives, what's duplicated, and what's safe to wipe.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use backdown_core::{Db, fmt, hash, index, report};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "backdown",
    version,
    about = "Backup that thinks: find out what's on your drives, what's duplicated, and what's safe to wipe."
)]
struct Cli {
    /// Index database (created if missing)
    #[arg(
        long,
        global = true,
        env = "BACKDOWN_DB",
        default_value = "backdown.db"
    )]
    db: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Index every file under each PATH as a new scan (read-only; re-run to update)
    Index {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Also descend into other filesystems mounted below PATH
        #[arg(long)]
        cross_filesystems: bool,
    },
    /// Hash only what could be a duplicate: size, then a sample, then full SHA-256
    Hash {
        #[command(flatten)]
        hashing: HashArgs,
    },
    /// List duplicated content, largest reclaimable space first (runs `hash` first)
    Dupes {
        /// Ignore files smaller than this (e.g. 100K, 1M, 2G)
        #[arg(long, default_value = "1M")]
        min_size: String,
        /// How many groups to list
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        hashing: HashArgs,
    },
    /// Is PATH safe to wipe? Every file needs a SHA-256-identical copy outside PATH (runs `hash` first).
    /// Exits 0 when safe, 2 when files must be copied first.
    Check {
        path: PathBuf,
        /// List every file that has no copy elsewhere
        #[arg(long)]
        list: bool,
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        hashing: HashArgs,
    },
    /// Finished scans, oldest first
    Scans {
        #[arg(long)]
        json: bool,
    },
    /// What's in the index
    Status {
        #[arg(long)]
        json: bool,
    },
}

#[derive(clap::Args)]
struct HashArgs {
    /// Readers per device; 1 suits spinning disks, raise it for SSDs
    #[arg(long, default_value_t = 1)]
    jobs: usize,
    /// Retry files that failed to read last time
    #[arg(long)]
    retry_errors: bool,
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("backdown: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    let mut db = Db::open(&cli.db)?;
    match cli.cmd {
        Cmd::Index {
            paths,
            cross_filesystems,
        } => {
            for path in paths {
                let opts = index::Options {
                    one_file_system: !cross_filesystems,
                };
                let st = index::index(&mut db, &path, &opts, &mut |s| {
                    live(&format!(
                        "indexing {}: {} files, {}",
                        s.root.display(),
                        fmt::count(s.files),
                        fmt::bytes(s.bytes)
                    ))
                })?;
                done(&format!(
                    "{}: {} files, {} (new {}, changed {}, gone {}) · scan {}",
                    st.root.display(),
                    fmt::count(st.files),
                    fmt::bytes(st.bytes),
                    st.new,
                    st.changed,
                    st.removed,
                    st.scan
                ));
                if st.errors > 0 {
                    eprintln!("  {} entries couldn't be read, e.g.:", st.errors);
                    for e in st.first_errors.iter().take(5) {
                        eprintln!("    {e}");
                    }
                }
            }
        }
        Cmd::Hash { hashing } => {
            run_hash(&mut db, &hashing)?;
        }
        Cmd::Dupes {
            min_size,
            limit,
            json,
            hashing,
        } => {
            run_hash(&mut db, &hashing)?;
            let d = report::dupes(&db, fmt::parse_size(&min_size)?, limit)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&d)?);
            } else {
                println!(
                    "{} reclaimable in {} groups of duplicates (files ≥ {min_size})\n",
                    fmt::bytes(d.reclaimable),
                    d.groups
                );
                for g in &d.largest {
                    println!(
                        "{}  {} × {}  sha256 {}…",
                        fmt::bytes(g.reclaimable),
                        g.copies,
                        fmt::bytes(g.size),
                        &g.sha256[..12]
                    );
                    for p in &g.paths {
                        println!("    {}", p.display());
                    }
                }
            }
        }
        Cmd::Check {
            path,
            list,
            json,
            hashing,
        } => {
            run_hash(&mut db, &hashing)?;
            let c = report::check(&db, &path)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&c)?);
            } else {
                print_check(&c, list);
            }
            return Ok(if c.safe {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            });
        }
        Cmd::Scans { json } => {
            let scans = report::scans(&db)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&scans)?);
            } else {
                for s in scans {
                    println!(
                        "{:>5}  {}  {:>10} files  {:>10}  {}",
                        s.id,
                        utc(s.finished_at),
                        fmt::count(s.files),
                        fmt::bytes(s.bytes),
                        s.root.display()
                    );
                }
            }
        }
        Cmd::Status { json } => {
            let s = db.status()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&s)?);
            } else {
                println!(
                    "{} files, {} in {} roots",
                    fmt::count(s.files),
                    fmt::bytes(s.bytes),
                    s.roots.len()
                );
                for r in &s.roots {
                    println!("  {r}");
                }
                println!(
                    "{} sampled, {} fully hashed, {} unreadable",
                    fmt::count(s.sampled),
                    fmt::count(s.hashed),
                    fmt::count(s.unreadable)
                );
                println!(
                    "{} finished scans, {} earlier versions kept",
                    s.scans,
                    fmt::count(s.versions)
                );
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn run_hash(db: &mut Db, a: &HashArgs) -> Result<()> {
    let opts = hash::Options {
        jobs_per_device: a.jobs,
        retry_errors: a.retry_errors,
    };
    let st = hash::hash(db, &opts, &mut |p| {
        let pct = if p.total_bytes > 0 {
            100.0 * p.bytes as f64 / p.total_bytes as f64
        } else {
            100.0
        };
        live(&format!(
            "{}: {}/{} files, {} of {} ({pct:.0}%)",
            p.stage,
            fmt::count(p.files),
            fmt::count(p.total_files),
            fmt::bytes(p.bytes),
            fmt::bytes(p.total_bytes)
        ));
    })?;
    if st.sampled + st.hashed + st.errors > 0 {
        done(&format!(
            "hashed: {} sampled, {} full, {} read{}",
            fmt::count(st.sampled),
            fmt::count(st.hashed),
            fmt::bytes(st.bytes_read),
            if st.errors > 0 {
                format!(", {} unreadable", st.errors)
            } else {
                String::new()
            }
        ));
    } else {
        clear();
    }
    Ok(())
}

fn print_check(c: &report::Check, list: bool) {
    println!("{}", c.path.display());
    println!(
        "  {} files, {}",
        fmt::count(c.total.files),
        fmt::bytes(c.total.bytes)
    );
    println!(
        "  verified   {:>10} in {} files (identical copy elsewhere)",
        fmt::bytes(c.verified.bytes),
        fmt::count(c.verified.files)
    );
    println!(
        "  unique     {:>10} in {} files (no copy anywhere else)",
        fmt::bytes(c.unique.bytes),
        fmt::count(c.unique.files)
    );
    if c.unreadable.files > 0 {
        println!(
            "  unreadable {:>10} in {} files (can't be verified)",
            fmt::bytes(c.unreadable.bytes),
            fmt::count(c.unreadable.files)
        );
    }
    if c.empty > 0 {
        println!("  empty      {:>10} files (no data)", fmt::count(c.empty));
    }
    println!();
    if c.safe {
        println!(
            "SAFE TO WIPE: every file has a SHA-256-identical copy outside {}.",
            c.path.display()
        );
        return;
    }
    let mut msg = format!(
        "COPY FIRST: {} in {} files exists nowhere else",
        fmt::bytes(c.unique.bytes),
        fmt::count(c.unique.files)
    );
    if c.unreadable.files > 0 {
        msg += &format!(", and {} files couldn't be read", c.unreadable.files);
    }
    println!("{msg}.");
    for d in c.unique_by_dir.iter().take(10) {
        println!(
            "  {:>10}  {:>8} files  {}",
            fmt::bytes(d.bytes),
            fmt::count(d.files),
            d.dir
        );
    }
    if list {
        println!();
        for (p, size) in &c.unique_files {
            println!("{:>10}  {}", fmt::bytes(*size), p.display());
        }
        for (p, e) in &c.unreadable_files {
            println!("{:>10}  {}  ({e})", "unreadable", p.display());
        }
    } else if c.unique.files > 0 {
        println!("  (--list shows every file)");
    }
}

/// A progress line that rewrites itself; only when stderr is a terminal.
fn live(msg: &str) {
    let mut err = std::io::stderr();
    if err.is_terminal() {
        let _ = write!(err, "\r\x1b[2K{msg}");
        let _ = err.flush();
    }
}

fn clear() {
    live("");
}

fn done(msg: &str) {
    clear();
    eprintln!("{msg}");
}

/// Unix seconds -> "YYYY-MM-DD HH:MM UTC" without a date library.
fn utc(t: i64) -> String {
    let (days, secs) = (t.div_euclid(86_400), t.rem_euclid(86_400));
    // civil-from-days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02} UTC",
        secs / 3600,
        secs % 3600 / 60
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn utc_dates() {
        assert_eq!(super::utc(0), "1970-01-01 00:00 UTC");
        assert_eq!(super::utc(1_790_000_000), "2026-09-21 14:13 UTC");
    }
}
