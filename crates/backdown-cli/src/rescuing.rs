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
    }
}

fn copy(
    src: &Path,
    dest: &Path,
    retries: u32,
    sector: Option<usize>,
    same_drive: bool,
) -> Result<ExitCode> {
    let a = rescue::access(src);
    if !a.readable {
        let hint = if a.device {
            format!(
                "\nRun it as an administrator (sudo backdown rescue image …), or let your account read that drive: {}",
                a.fix.as_deref().unwrap_or("")
            )
        } else {
            String::new()
        };
        bail!(
            "can't read {}: {}{hint}",
            src.display(),
            a.why.unwrap_or_default()
        );
    }
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

fn absolute(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(p)
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
