//! `backdown rescue`: the rescue engines (backdown_core::rescue) in a terminal, in the foreground.
//!
//! This is also the way in when BackDown can't read a drive directly: `sudo backdown rescue …`
//! runs as root without touching the index (it isn't opened), and what it writes is handed back to
//! the user who ran sudo.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use anyhow::{Result, bail};
use backdown_core::fmt;
use backdown_core::rescue::{self, image};
use clap::Subcommand;

use crate::{clear, done, live};

#[derive(Subcommand)]
pub enum RescueCmd {
    /// Copy a failing drive or card into an image file: the good areas first, the bad ones later.
    /// Stop it any time (Ctrl+C) and run the same command again to carry on. A map beside the
    /// image (IMAGE.map, in GNU ddrescue's format) records what's been read
    Image {
        /// The drive, partition or card (e.g. /dev/sdc), or an image file
        source: PathBuf,
        /// The image file to write, on another drive
        image: PathBuf,
        /// Passes over unreadable sectors at the end
        #[arg(long, default_value_t = 1)]
        retries: u32,
        /// Sector size to read unreadable areas in (512 or 4096; default: 4096 for drives on
        /// Linux, which reads them a page at a time anyway, else 512)
        #[arg(long)]
        sector: Option<usize>,
        /// Allow the image on another partition of the same physical drive (not recommended for
        /// a failing drive; always allowed when the source is an image file)
        #[arg(long)]
        same_drive: bool,
    },
    /// Find photos, videos, documents and more by what's inside them, whatever the file system
    /// says (after files were deleted, or the drive was formatted), and save them sorted by kind
    Find {
        /// The drive, partition or card (e.g. /dev/sdc), or an image of one
        source: PathBuf,
        /// The folder to save what's found in, on another drive
        #[arg(long)]
        to: PathBuf,
        /// Only these kinds: groups (photos, videos, audio, documents, archives, other) or
        /// extensions (jpg, pdf, docx, …), comma-separated
        #[arg(long, value_delimiter = ',')]
        only: Vec<String>,
        /// Where to start, in bytes (e.g. a lost partition's start from `rescue partitions`)
        #[arg(long)]
        start: Option<u64>,
        /// Where to stop, in bytes
        #[arg(long)]
        end: Option<u64>,
    },
    /// Look for file systems the partition table doesn't list (after a drive was re-initialised,
    /// repartitioned, or shows as empty). Only reads
    Partitions {
        /// The drive or card (e.g. /dev/sdc), or an image of one
        source: PathBuf,
        /// Read every sector, not only where partitions usually start (as slow as copying it)
        #[arg(long)]
        deep: bool,
        /// Print what was found as JSON
        #[arg(long)]
        json: bool,
    },
}

pub fn run(cmd: RescueCmd) -> Result<ExitCode> {
    match cmd {
        RescueCmd::Image {
            source,
            image,
            retries,
            sector,
            same_drive,
        } => copy(&source, &image, retries, sector, same_drive),
        RescueCmd::Partitions { source, deep, json } => partitions(&source, deep, json),
        RescueCmd::Find {
            source,
            to,
            only,
            start,
            end,
        } => find(&source, &to, only, start, end),
    }
}

fn copy(
    src: &Path,
    dest: &Path,
    retries: u32,
    sector: Option<usize>,
    same_drive: bool,
) -> Result<ExitCode> {
    readable(src, "image")?;
    let a = rescue::access(src);
    let dest = absolute(dest);
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let map = rescue::map_path(&dest);
    let opts = image::Options {
        retries,
        sector: sector.unwrap_or(if a.device && cfg!(target_os = "linux") {
            4096
        } else {
            512
        }),
        // an image file is usually a copy made earlier, not the failing drive itself
        allow_same_drive: same_drive || !a.device,
        ..image::Options::default()
    };
    image::check_places(src, &dest, &map, opts.allow_same_drive)?;
    eprintln!(
        "Copying {} ({}) into {}\nThe map: {}\nCtrl+C stops it; run the same command again to carry on.",
        src.display(),
        fmt::bytes(a.size.unwrap_or(0)),
        dest.display(),
        map.display()
    );
    let stop = AtomicBool::new(false);
    let r = image::image(
        src,
        &dest,
        &map,
        &opts,
        &mut |p| {
            live(&format!(
                "{}: {} of {} copied ({:.1}%){} · {}/s",
                p.pass,
                fmt::bytes(p.rescued),
                fmt::bytes(p.size),
                if p.size > 0 {
                    100.0 * (p.size - p.todo) as f64 / p.size as f64
                } else {
                    0.0
                },
                if p.bad > 0 {
                    format!(
                        " · {} unreadable in {} places",
                        fmt::bytes(p.bad),
                        fmt::count(p.bad_areas)
                    )
                } else {
                    String::new()
                },
                fmt::bytes(p.read_rate as u64)
            ))
        },
        &stop,
    );
    give_back(&[&dest, &map]);
    let s = r?;
    clear();
    if s.complete {
        done(&format!(
            "Copied all {} into {}",
            fmt::bytes(s.size),
            dest.display()
        ));
    } else {
        done(&format!(
            "Copied {} of {}; {} couldn't be read, in {} place{}. Running it again with --retries 3 tries those again.",
            fmt::bytes(s.rescued),
            fmt::bytes(s.size),
            fmt::bytes(s.bad),
            fmt::count(s.bad_areas),
            if s.bad_areas == 1 { "" } else { "s" }
        ));
    }
    Ok(if s.complete {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
}

fn partitions(src: &Path, deep: bool, json: bool) -> Result<ExitCode> {
    use rescue::parts;
    readable(src, "partitions")?;
    let stop = AtomicBool::new(false);
    let s = parts::search(
        src,
        &parts::Options {
            deep,
            ..parts::Options::default()
        },
        &mut |p| {
            live(&format!(
                "{} of {} read · {} found",
                fmt::bytes(p.pos),
                fmt::bytes(p.size),
                fmt::count(p.found)
            ))
        },
        &stop,
    )?;
    clear();
    if json {
        println!("{}", serde_json::to_string_pretty(&s)?);
        return Ok(ExitCode::SUCCESS);
    }
    let t = &s.table;
    println!(
        "{} ({}): {}",
        src.display(),
        fmt::bytes(s.size),
        match t.scheme {
            "none" => "no partition table".to_string(),
            k => format!(
                "{} partition table, {} entr{}",
                k.to_uppercase(),
                t.entries.len(),
                if t.entries.len() == 1 { "y" } else { "ies" }
            ),
        }
    );
    for p in &t.problems {
        println!("  ! {p}");
    }
    if s.found.is_empty() {
        println!(
            "No file systems found{}.",
            if deep {
                ""
            } else {
                " (--deep reads every sector)"
            }
        );
        return Ok(ExitCode::from(2));
    }
    println!(
        "\n{:>16}  {:>12}  {:>10}  {:<22} {:<18} In the table",
        "Starts at byte", "Sector", "Size", "File system", "Name"
    );
    for f in &s.found {
        println!(
            "{:>16}  {:>12}  {:>10}  {:<22} {:<18} {}{}{}{}",
            f.start,
            f.start / s.sector.max(1),
            f.size.map(fmt::bytes).unwrap_or_else(|| "?".into()),
            f.fs,
            f.label.as_deref().unwrap_or(""),
            if f.in_table { "yes" } else { "NO: lost" },
            if f.via.starts_with("backup") || f.via.starts_with("alternate") {
                format!(" (found by its {})", f.via)
            } else {
                String::new()
            },
            if f.nested {
                " (inside another file system)"
            } else {
                ""
            },
            if f.truncated {
                " (runs past the end)"
            } else {
                ""
            },
        );
    }
    let lost = s.found.iter().filter(|f| !f.in_table && !f.nested).count();
    if lost > 0 {
        println!(
            "\n{lost} not in the table. To get files out of one: backdown rescue find {} --to <folder> --start <byte> --end <byte>",
            src.display()
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn find(
    src: &Path,
    to: &Path,
    only: Vec<String>,
    start: Option<u64>,
    end: Option<u64>,
) -> Result<ExitCode> {
    use rescue::carve;
    readable(src, "find")?;
    let to = absolute(to);
    let opts = carve::Options {
        kinds: (!only.is_empty()).then_some(only),
        start: start.unwrap_or(0),
        end,
        ..carve::Options::default()
    };
    eprintln!(
        "Looking through {} for files; what's found goes to {}, a folder per kind.",
        src.display(),
        to.display()
    );
    let stop = AtomicBool::new(false);
    let r = carve::scan_and_save(
        src,
        &opts,
        &to,
        &mut |p| {
            live(&format!(
                "{} of {} looked through · {} found ({})",
                fmt::bytes(p.done),
                fmt::bytes(p.size),
                fmt::count(p.found),
                fmt::bytes(p.found_bytes)
            ))
        },
        &stop,
    );
    give_back_tree(&to);
    let st = r?;
    clear();
    let g = &st.by_group;
    let parts: Vec<String> = [
        (g.photos, "photos"),
        (g.videos, "videos"),
        (g.audio, "audio"),
        (g.documents, "documents"),
        (g.archives, "archives"),
        (g.other, "other"),
    ]
    .iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, l)| format!("{} {l}", fmt::count(*n)))
    .collect();
    done(&format!(
        "Found {} file{} ({}){}{}",
        fmt::count(st.found),
        if st.found == 1 { "" } else { "s" },
        fmt::bytes(st.found_bytes),
        if parts.is_empty() {
            String::new()
        } else {
            format!(": {}", parts.join(", "))
        },
        if st.incomplete > 0 {
            format!(
                ". {} of them are cut off or damaged, and come back partly",
                fmt::count(st.incomplete)
            )
        } else {
            String::new()
        }
    ));
    if st.found > 0 {
        eprintln!(
            "In {} (the list: {})",
            to.display(),
            to.join(carve::INDEX).display()
        );
    }
    for p in st.problems.iter().take(10) {
        eprintln!("  {p}");
    }
    Ok(if st.found > 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
}

/// Say plainly when the source can't be read, and what would let it be.
fn readable(src: &Path, what: &str) -> Result<()> {
    let a = rescue::access(src);
    if a.readable {
        return Ok(());
    }
    let hint = if a.device {
        format!(
            "\nRun it as an administrator (sudo backdown rescue {what} …), or let your account read that drive: {}",
            a.fix.as_deref().unwrap_or("")
        )
    } else {
        String::new()
    };
    bail!(
        "can't read {}: {}{hint}",
        src.display(),
        a.why.unwrap_or_default()
    )
}

fn absolute(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(p)
    }
}

/// `give_back` for a folder and everything in it.
fn give_back_tree(dir: &Path) {
    if std::env::var_os("SUDO_UID").is_none() {
        return;
    }
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        give_back(&[&d]);
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                match e.file_type() {
                    Ok(t) if t.is_dir() => stack.push(e.path()),
                    Ok(_) => give_back(&[&e.path()]),
                    Err(_) => {}
                }
            }
        }
    }
}

/// Under sudo, what was written belongs to the person who ran it, not to root.
fn give_back(paths: &[&Path]) {
    #[cfg(unix)]
    {
        let id = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<u32>().ok());
        if let (Some(uid), Some(gid)) = (id("SUDO_UID"), id("SUDO_GID")) {
            for p in paths {
                if p.exists() {
                    let _ = std::os::unix::fs::chown(p, Some(uid), Some(gid));
                }
            }
        }
    }
    #[cfg(not(unix))]
    let _ = paths;
}
