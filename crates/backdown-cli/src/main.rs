//! `backdown`: find out what's on your drives, what's duplicated, and what's safe to wipe.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use backdown_core::{Db, fmt, hash, index, legacy, report, volumes};
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use serde_json::json;

mod app;
mod client;
mod completions;
mod custody;
mod daemon;
mod desktop;
mod drives;
mod filing;
mod grant;
mod health;
mod insight;
mod license;
mod locations;
mod mailbox;
mod mounting;
mod notifier;
mod peek;
mod remote;
mod rescuing;
mod serve;
mod service;
mod settings;
mod sqldump;
mod tidy;
mod tiers;
mod update;

#[derive(Parser)]
#[command(
    name = "backdown",
    version,
    about = "Backup that thinks: find out what's on your drives, what's duplicated, and what's safe to wipe."
)]
struct Cli {
    /// Index database (default: your BackDown data folder; see `backdown status`)
    #[arg(long, global = true, env = "BACKDOWN_DB")]
    db: Option<PathBuf>,
    /// Work on the index directly even if a BackDown daemon is running
    #[arg(long, global = true)]
    local: bool,
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
        /// Record PATH even if it's now empty although it held files (normally refused: an empty
        /// mount point usually means the drive is unplugged)
        #[arg(long)]
        allow_empty: bool,
        /// How to find what's there: auto (the drive's change log if it's intact, else NTFS's
        /// file table where the device can be read, else a walk; and a walk every so often),
        /// walk (every folder), journal or table (that way or fail, saying why)
        #[arg(long, default_value = "auto", value_parser = ["auto", "walk", "journal", "table"])]
        method: String,
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
        /// List the files that have no copy elsewhere (the largest 20,000)
        #[arg(long)]
        list: bool,
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        hashing: HashArgs,
    },
    /// Import the index the pre-Rust scripts wrote (bd-index.sh, bd-hash.py), so their crawling
    /// and hashing aren't repeated
    #[command(after_long_help = LEGACY_HELP)]
    ImportLegacy {
        /// The scripts' `index` folder
        index_dir: PathBuf,
        /// bd-hash.py's hashes.db: attach its SHA-256s to the files they belong to
        #[arg(long, value_name = "HASHES_DB")]
        hashes: Option<PathBuf>,
        /// A drive mounted somewhere else now: LEGACY_PATH=PATH_NOW (repeatable; a legacy folder
        /// maps everything below it)
        #[arg(long, value_name = "LEGACY=NOW")]
        map: Vec<String>,
        /// Import only this volume, a folder name in INDEX_DIR (repeatable; default: all)
        #[arg(long, value_name = "NAME")]
        volume: Vec<String>,
        /// Replace what the index already holds for a root, or for roots nested with it
        #[arg(long)]
        replace: bool,
        /// Read everything and show what would be imported, writing nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Every disk and partition on this machine: what's on it, and what BackDown can do with it
    /// (read-only; needs no root)
    Volumes {
        #[arg(long)]
        json: bool,
    },
    /// How each drive is doing: SMART verdict, temperature, bad sectors, wear, and what changed
    /// since its first reading (read now, without root; kept in the index). Exits 2 when a
    /// connected drive is failing
    Health {
        #[arg(long)]
        json: bool,
    },
    /// Mount a partition read-only (one `backdown volumes` calls mountable), or attach a disk
    /// image read-only and mount what's on it; never read-write, and as you: udisks on Linux,
    /// diskutil and hdiutil on macOS (Windows attaches ISO, VHD and VHDX images). Without a
    /// target, lists what BackDown has mounted
    Mount {
        /// A partition (e.g. /dev/sdb1) or a disk image file
        target: Option<PathBuf>,
    },
    /// Unmount a partition, or detach a disk image, that BackDown mounted (nothing else)
    Unmount {
        /// The partition or disk image, as `backdown mount` lists it
        target: PathBuf,
    },
    /// Finished scans, oldest first
    Scans {
        #[arg(long)]
        json: bool,
    },
    /// Browse the index in your web browser: folders at any scan, versions, copies, file viewers
    Serve {
        /// Port to listen on
        #[arg(long, default_value_t = 8787)]
        port: u16,
        /// Also listen on this address (e.g. this computer's Tailscale IP) so other devices can look;
        /// from there BackDown only shows, and changes need this computer. 127.0.0.1 always listens
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Don't open a browser, just print the link
        #[arg(long)]
        no_open: bool,
    },
    /// What's in the index (and what the daemon is doing)
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Run in the background: rescan drives, hash, serve the web UI. Usually started by
    /// `backdown service install` rather than by hand
    Daemon {
        /// Port for the web UI (0: any free one)
        #[arg(long, default_value_t = 8787, env = "BACKDOWN_PORT")]
        port: u16,
        /// Also listen on this address (e.g. this computer's Tailscale IP) so other devices can look;
        /// from there BackDown only shows, and changes need this computer. 127.0.0.1 always listens
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Rescan each drive this often (e.g. 12h, 1d; `never` to scan only on request or when a
        /// drive comes back). Default: as set in Settings (24h unless changed there)
        #[arg(long)]
        rescan_every: Option<String>,
        /// Machine-wide service: index in /var/lib/backdown, socket in /run/backdown, shared with
        /// the `backdown` group (set by the system unit)
        #[arg(long)]
        system: bool,
    },
    /// Open the running daemon's web UI in your browser
    Open,
    /// The background service's browser token, for opening BackDown from a browser yourself (in a
    /// terminal, also the links with it in, including the one for other devices with --host)
    Token {
        /// Print only the links, one per line
        #[arg(long)]
        links: bool,
    },
    /// Tab completion for backdown's commands and options: print the script for a shell, or
    /// --install it for yours
    Completions {
        /// bash, zsh, fish, powershell or elvish (default: your shell, from $SHELL)
        shell: Option<clap_complete::Shell>,
        /// Put it where your shell picks it up, for you alone (bash, zsh, fish)
        #[arg(long)]
        install: bool,
    },
    /// Open BackDown in a window of its own, starting its background service first if need be
    /// (what the applications menu, Start menu and BackDown.app run)
    App {
        /// Only make sure the background service runs (for starting at login)
        #[arg(long)]
        no_window: bool,
        /// Print the browser command instead of running it
        #[arg(long)]
        print: bool,
        /// A Chromium-family browser to use (default: Chrome, Chromium, Edge or Brave, whichever
        /// is installed; else your default browser)
        #[arg(long, env = "BACKDOWN_BROWSER")]
        browser: Option<PathBuf>,
    },
    /// Add BackDown to your applications menu, ~/Applications or the Start menu (or remove it)
    Desktop {
        #[command(subcommand)]
        action: desktop::DesktopCmd,
    },
    /// Is there a newer BackDown? Install it (verified against the release's SHA-256 sums)
    Update {
        /// Only say whether there is one (exits 10 when there is)
        #[arg(long)]
        check: bool,
        /// Ask GitHub now instead of using today's answer
        #[arg(long)]
        refresh: bool,
        /// Put back the version the last update replaced
        #[arg(long)]
        rollback: bool,
        /// stable or preview (default: as in Settings; previews while this is a preview)
        #[arg(long)]
        channel: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Run the daemon as a service (systemd on Linux, a login item on macOS, a task at logon on
    /// Windows): install, uninstall or check it
    Service {
        #[command(subcommand)]
        action: ServiceCmd,
    },
    /// What things are: mail stores, backups, phones, systems, repositories, libraries… with where
    /// they came from (reads small files like backup scripts on connected drives)
    Entities {
        /// Only these drives (or the drives these paths are on)
        paths: Vec<PathBuf>,
        /// Recognise again even if nothing was scanned since
        #[arg(long)]
        refresh: bool,
        /// Only this kind (mail-store, timemachine, ios-backup, git, photos, …)
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// How much of each drive, folder and recognised thing already exists elsewhere, and where
    Overlaps {
        /// Only list folders and things with at least this share elsewhere (e.g. 90%)
        #[arg(long, default_value = "0%")]
        min: String,
        #[arg(long)]
        json: bool,
    },
    /// The whole picture as a document: drives, what's on them, overlaps, duplicates, and which
    /// drives are safe to wipe
    Report {
        /// HTML instead of Markdown
        #[arg(long)]
        html: bool,
        /// Write to this file instead of the terminal
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Recognise every drive again first
        #[arg(long)]
        refresh: bool,
    },
    /// Propose where everything on the old drives goes in one archive, and why (saved, so it can be
    /// applied)
    Plan {
        /// The archive folder (on the drive that will keep everything)
        #[arg(long)]
        to: Option<PathBuf>,
        /// Only file these drives (default: every indexed drive but the archive's)
        #[arg(long)]
        from: Vec<PathBuf>,
        /// Your own filing schema (start from `backdown plan --print-schema`)
        #[arg(long)]
        schema: Option<PathBuf>,
        /// Print the built-in schema and exit
        #[arg(long)]
        print_schema: bool,
        /// Merge copies of the same mailbox by Message-ID, instead of filing one and copying the
        /// other's extra files beside it
        #[arg(long)]
        merge_mail: bool,
        #[arg(long)]
        json: bool,
    },
    /// Plans made so far
    Plans,
    /// Apply a plan: show what it would do, or with --go copy, verify and journal every file
    /// (never deletes anything)
    Apply {
        plan: i64,
        /// Actually copy (without it: a dry run)
        #[arg(long)]
        go: bool,
    },
    /// Get files back from a failing, formatted or emptied drive or card (only reads it)
    Rescue {
        #[command(subcommand)]
        action: rescuing::RescueCmd,
    },
    /// Keep a folder (usually part of the archive) copied on another drive, and check the copy now
    /// and then
    Mirror {
        #[command(subcommand)]
        action: MirrorCmd,
    },
    /// Free drives (or folders on them): delete only what has a verified copy elsewhere, moving
    /// what exists only here to --move-to first. Without --go, only shows what would happen.
    Wipe {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Move what exists only in these paths here first (verified), instead of leaving it
        #[arg(long)]
        move_to: Option<PathBuf>,
        /// Don't re-read each file and its copy; trust the index's hashes (faster, a little less
        /// safe). Copies must still be there, as last scanned
        #[arg(long)]
        trust_index: bool,
        /// Actually wipe
        #[arg(long)]
        go: bool,
        /// The drive name(s), as the dry run shows them
        #[arg(long)]
        confirm: Option<String>,
    },
    /// Your BackDown Pro licence: add a key, or show the one you have
    License {
        #[command(subcommand)]
        action: LicenseCmd,
    },
    /// Other machines (a NAS, another computer) whose BackDown index is read over ssh, so copies
    /// there count for duplicates and wiping (Pro)
    Remote {
        #[command(subcommand)]
        action: remote::RemoteCmd,
    },
    /// The other end of `backdown remote`, run there by ssh: answers read-only requests, one JSON
    /// line each, on stdin and stdout
    #[command(hide = true)]
    ServeStdio,
    /// Tidy an archive: identical files in FOLDER share their storage, as hardlinks or clones.
    /// Both files are re-read first; every link is journaled. Without --go, only shows what
    /// would happen
    Dedup {
        folder: PathBuf,
        /// hard: one file with several names (any filesystem; editing one edits them all).
        /// clone: separate files sharing blocks until one changes (btrfs, XFS, APFS)
        #[arg(long, default_value = "hard")]
        link: String,
        /// Leave smaller files alone (e.g. 4K, 1M)
        #[arg(long, default_value = "4K")]
        min_size: String,
        /// Only show what would happen (the default)
        #[arg(long, conflicts_with = "go")]
        dry_run: bool,
        /// Link them
        #[arg(long)]
        go: bool,
        #[arg(long)]
        json: bool,
    },
    /// Mailboxes: merge copies of one into a single Maildir, or export a Maildir folder as mbox
    Mail {
        #[command(subcommand)]
        action: MailCmd,
    },
    /// The chain of custody: every copy, move, deletion, link and merge BackDown made, in a
    /// hash-chained log sealed with this installation's key. Export it as a signed bundle, verify
    /// one, or check the log in the index
    Custody {
        #[command(subcommand)]
        action: custody::CustodyCmd,
    },
}

#[derive(Subcommand)]
enum MailCmd {
    /// Merge copies of the same mailbox taken at different times (Maildir, mbox files, a
    /// Thunderbird folder, or whole mail stores, mailbox by mailbox) into one Maildir: every
    /// message once by Message-ID, in the folder the newest copy has it in, flags combined.
    /// The sources are only read
    Merge {
        #[arg(required = true)]
        sources: Vec<PathBuf>,
        /// The merged Maildir: a new folder (running again carries on where it stopped)
        #[arg(long)]
        to: PathBuf,
        /// Read everything and show what the merge would be, writing nothing
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
    /// Export one Maildir folder (its own messages, not its subfolders) as an mbox file (mboxrd)
    Export {
        maildir: PathBuf,
        /// The mbox file to write (never overwritten)
        #[arg(long)]
        mbox: PathBuf,
    },
}

const LEGACY_HELP: &str = "\
The legacy index is the `index` folder the pre-Rust scripts wrote:

  <vol>/chunks.list      bd-plan.py's crawl plan, one `R<TAB>dir` or `F<TAB>dir` per line;
                         its first line, `F<TAB>/mount/point`, names the drive's root
  <vol>/chunks/<id>.tsv  one chunk, from `find -printf '%s\\t%T@\\t%p\\n'`: size, mtime
                         (seconds.fraction) and the raw path; <id>.done marks it finished
  <vol>.tsv              every finished chunk, merged (read when present and up to date)
  hashes.db              bd-hash.py's table h(path, size, mtime, sha256)

Each volume becomes a root with one finished scan, dated from its chunks. Chunks that never
finished are left out and counted; the next `backdown index` adds their files. A hash attaches
where the path, size and mtime (to the second) still match. Safe to run again: an interrupted
import resumes, a finished one is left alone. Stop the daemon while importing.

Afterwards `dupes` and `check` work straight away, and `backdown index <root>` with the drive
mounted finds its files unchanged and keeps their hashes.

Examples:
  backdown import-legacy ~/scripts/index --hashes ~/scripts/index/hashes.db --dry-run
  backdown import-legacy ~/scripts/index --hashes ~/scripts/index/hashes.db \\
      --map /media/olduser/Photos=/mnt/photos";

#[derive(Subcommand)]
enum MirrorCmd {
    /// Start mirroring FOLDER into MIRROR (a folder on another drive)
    Add {
        folder: PathBuf,
        mirror: PathBuf,
        /// Re-read a sample of the mirror every this many days
        #[arg(long, default_value_t = 30)]
        verify_every: i64,
    },
    /// Mirrors, and how they were when last synced and verified
    List,
    /// Copy what's new or changed (old copies are kept aside; nothing is deleted)
    Sync { id: Option<i64> },
    /// Re-read files on the mirror and compare them with the source
    Verify {
        id: Option<i64>,
        /// Share of the files to re-read (a different share each day)
        #[arg(long, default_value_t = 5)]
        percent: u8,
        /// Re-read every file
        #[arg(long)]
        all: bool,
    },
    /// Stop keeping a mirror (the copies stay where they are)
    Remove { id: i64 },
}

#[derive(Subcommand)]
enum LicenseCmd {
    /// Check a licence key (offline) and keep it in your BackDown data folder
    Add {
        /// The key from your order confirmation (BD1.…), or - to read it from stdin
        key: String,
    },
    /// Show the licence you have (exits 1 without one)
    Show,
    /// What's free, what's Pro, and what this copy may use
    Features,
}

#[derive(Subcommand)]
enum ServiceCmd {
    /// Install and start the daemon as a service for you (on Linux, machine-wide with --system)
    Install {
        /// Machine-wide service running as the `backdown` user (needs sudo)
        #[arg(long)]
        system: bool,
        /// Also listen on this address (e.g. this computer's Tailscale IP) so other devices can look;
        /// from there BackDown only shows, and changes need this computer. 127.0.0.1 always listens
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Port the service binds
        #[arg(long, default_value_t = 8787)]
        port: u16,
        /// Install but don't start it
        #[arg(long)]
        no_start: bool,
        /// Print the unit file instead of installing it
        #[arg(long)]
        print: bool,
    },
    /// Stop and remove the service (the index is kept)
    Uninstall {
        #[arg(long)]
        system: bool,
    },
    /// Is the service installed and running, and what is it doing?
    Status {
        #[arg(long)]
        system: bool,
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
    // run elevated on Windows: read (never write) files the user's own account can't, like other
    // users' folders on an old drive; does nothing otherwise
    backdown_core::paths::enable_backup_privilege();
    // BackDown.app, or backdown.exe double-clicked in Explorer: the app
    if app::double_clicked() {
        return match app::run(app::Args {
            no_window: false,
            print: false,
            browser: std::env::var_os("BACKDOWN_BROWSER").map(PathBuf::from),
        }) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("backdown: {e:#}");
                ExitCode::FAILURE
            }
        };
    }
    let matches = Cli::command().get_matches();
    let cli = match Cli::from_arg_matches(&matches) {
        Ok(c) => c,
        Err(e) => e.exit(),
    };
    // Pro features, as tiers.rs's table says (everything is unlocked in previews)
    if let Some(sub) = matches.subcommand_name()
        && let Err(e) = tiers::gate_command(sub)
    {
        eprintln!("backdown: {e:#}");
        return ExitCode::FAILURE;
    }
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("backdown: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    match cli.cmd {
        Cmd::Daemon {
            port,
            host,
            rescan_every,
            system,
        } => {
            let (state_dir, runtime_dir) = if system {
                (
                    PathBuf::from(locations::SYSTEM_STATE),
                    PathBuf::from(locations::SYSTEM_RUNTIME),
                )
            } else {
                (locations::state_dir(), locations::runtime_dir())
            };
            let db = cli.db.unwrap_or_else(|| state_dir.join("index.db"));
            // the option wins; else Settings (settings.json in the state folder), 24h by default
            settings::init(&state_dir, system, rescan_every.is_some());
            let every = rescan_every.unwrap_or_else(|| settings::load().rescan_every);
            daemon::run(daemon::Options {
                db,
                state_dir,
                runtime_dir,
                port,
                host,
                rescan_every: daemon::parse_every(&every)?,
                shared: system,
                rescan_flag: false,
            })?;
            return Ok(ExitCode::SUCCESS);
        }
        Cmd::Open => return app::open_in_browser(),
        Cmd::Token { links } => return app::print_token(links),
        Cmd::Completions { shell, install } => return completions::run(shell, install),
        Cmd::App {
            no_window,
            print,
            browser,
        } => {
            return app::run(app::Args {
                no_window,
                print,
                browser,
            });
        }
        Cmd::Desktop { action } => return desktop::run(action),
        Cmd::Update {
            check,
            refresh,
            rollback,
            channel,
            json,
        } => {
            return update::run(update::Args {
                check,
                refresh,
                rollback,
                channel,
                json,
            });
        }
        Cmd::Service { action } => {
            return match action {
                ServiceCmd::Install {
                    system,
                    host,
                    port,
                    no_start,
                    print,
                } => service::install(system, &host, port, !no_start, print),
                ServiceCmd::Uninstall { system } => service::uninstall(system),
                ServiceCmd::Status { system } => service::status(system),
            };
        }
        Cmd::License { action } => {
            return match action {
                LicenseCmd::Add { key } => license::add(&key),
                LicenseCmd::Show => license::show(),
                LicenseCmd::Features => {
                    tiers::print_features();
                    Ok(ExitCode::SUCCESS)
                }
            };
        }
        Cmd::Volumes { json } => return run_volumes(cli.db, json),
        // through the daemon when it runs (it keeps the readings), else here
        Cmd::Health { json } => return health::command(cli.db, cli.local, json),
        // as the person at the screen, through the system's own mounter; never opens the index
        Cmd::Mount { target } => return run_mount(target),
        Cmd::Unmount { target } => {
            let t = dunce::canonicalize(&target).unwrap_or(target);
            let v = mounting::unmount(&locations::state_dir(), &t.display().to_string())?;
            done(&format!(
                "Unmounted {}",
                v["unmounted"].as_str().unwrap_or("")
            ));
            return Ok(ExitCode::SUCCESS);
        }
        // reads a drive and writes elsewhere; never opens the index (it may run under sudo)
        Cmd::Rescue { action } => return rescuing::run(action),
        // the other end of `backdown remote`, run there by ssh: reads only, never through a daemon
        Cmd::ServeStdio => return remote::stdio::run(cli.db),
        Cmd::Remote { action } => return remote::command(cli.db, cli.local, action),
        // reads the index (an export also seals it), or only the file given
        Cmd::Custody { action } => return custody::command(cli.db, action),
        _ => {}
    }
    // a running daemon owns the index: hand the work to it (unless told not to)
    if !cli.local
        && !matches!(
            cli.cmd,
            Cmd::Serve { .. }
                | Cmd::ImportLegacy { .. }
                | Cmd::Entities { .. }
                | Cmd::Overlaps { .. }
                | Cmd::Report { .. }
                | Cmd::Plan { .. }
                | Cmd::Plans
                | Cmd::Mirror { .. }
                | Cmd::Wipe { .. }
                // like wipe: they check here, then hand the real work to the daemon if it runs
                | Cmd::Dedup { .. }
                | Cmd::Mail { .. }
                // a dry run only reads; a real one goes to the daemon, the index's one writer
                | Cmd::Apply { go: false, .. }
        )
        && let Some(c) = client::Client::connect(cli.db.as_deref())
    {
        return via_daemon(c, cli.cmd);
    }
    if let Cmd::ImportLegacy { dry_run: false, .. } = cli.cmd
        && !cli.local
        && client::Client::connect(cli.db.as_deref()).is_some()
    {
        anyhow::bail!(
            "a BackDown daemon is running and owns the index; stop it for the import \
             (`systemctl --user stop backdown`, or `sudo systemctl stop backdown` for the system service), \
             then start it again"
        );
    }
    let db_path = match cli.db {
        Some(p) => p,
        None => {
            let p = locations::default_db();
            if let Some(dir) = p.parent() {
                locations::ensure_private_dir(dir, false)
                    .with_context(|| format!("can't create {}", dir.display()))?;
            }
            if Path::new("backdown.db").is_file() && !p.exists() {
                eprintln!(
                    "note: the index now lives in {}; to keep using ./backdown.db, pass --db backdown.db",
                    p.display()
                );
            }
            p
        }
    };
    let r = local(&db_path, cli.cmd, !cli.local);
    // what a command here added to the chain of custody is sealed now (the daemon seals its own)
    custody::seal_after(&db_path);
    r
}

fn local(db_path: &Path, cmd: Cmd, use_daemon: bool) -> Result<ExitCode> {
    // junk rules added in Settings count here too
    settings::apply_to_core(&settings::load());
    if let Cmd::ImportLegacy { .. } = cmd {
        return import_legacy(db_path, cmd);
    }
    let mut db = Db::open(db_path)?;
    match cmd {
        Cmd::Index {
            paths,
            cross_filesystems,
            allow_empty,
            method,
        } => {
            let method: index::Method = method.parse()?;
            for path in paths {
                let opts = index::Options {
                    one_file_system: !cross_filesystems,
                    allow_empty,
                };
                let st = index::scan(&mut db, &path, &opts, method, &mut |s| {
                    live(&format!(
                        "indexing {}: {} files, {}",
                        s.root.display(),
                        fmt::count(s.files),
                        fmt::bytes(s.bytes)
                    ))
                })?;
                done(&format!(
                    "{}: {} files, {} (new {}, changed {}, gone {}) · scan {}\n  {}",
                    st.root.display(),
                    fmt::count(st.files),
                    fmt::bytes(st.bytes),
                    st.new,
                    st.changed,
                    st.removed,
                    st.scan,
                    st.how()
                ));
                if st.errors > 0 {
                    eprintln!("  {} entries couldn't be read, e.g.:", st.errors);
                    for e in st.first_errors.iter().take(5) {
                        eprintln!("    {e}");
                    }
                }
            }
        }
        Cmd::Hash { hashing } => run_hash(&mut db, &hashing)?,
        Cmd::Dupes {
            min_size,
            limit,
            json,
            hashing,
        } => {
            run_hash(&mut db, &hashing)?;
            if !db.has_dupes_index()? {
                eprintln!("preparing the duplicates index, once (a few minutes on a big index)…");
                db.ensure_dupes_index()?;
            }
            print_dupes(
                &report::dupes(&db, fmt::parse_size(&min_size)?, limit)?,
                &min_size,
                json,
            )?;
        }
        Cmd::Check {
            path,
            list,
            json,
            hashing,
        } => {
            run_hash(&mut db, &hashing)?;
            return print_check(&report::check(&db, &path)?, list, json);
        }
        Cmd::Scans { json } => print_scans(&report::scans(&db)?, json)?,
        Cmd::Entities {
            paths,
            refresh,
            kind,
            json,
        } => return insight::entities(&db, &paths, refresh, kind.as_deref(), json),
        Cmd::Overlaps { min, json } => {
            let share = min
                .trim_end_matches('%')
                .parse::<f64>()
                .context("--min is a percentage, e.g. 90%")?
                / 100.0;
            return insight::overlaps(&db, share, json);
        }
        Cmd::Report {
            html,
            output,
            refresh,
        } => return insight::report(&db, html, output.as_deref(), refresh),
        Cmd::Plan {
            to,
            from,
            schema,
            print_schema,
            merge_mail,
            json,
        } => {
            if print_schema {
                print!("{}", backdown_core::plan::DEFAULT_SCHEMA);
                return Ok(ExitCode::SUCCESS);
            }
            let to =
                to.context("where should the archive go? --to <folder on the archive drive>")?;
            return filing::make_plan(
                &db,
                filing::PlanArgs {
                    to: &to,
                    from: &from,
                    schema: schema.as_deref(),
                    merge_mail,
                    json,
                },
            );
        }
        Cmd::Dedup {
            folder,
            link,
            min_size,
            dry_run: _,
            go,
            json,
        } => {
            return tidy::dedup(
                &db,
                db_path,
                tidy::DedupArgs {
                    folder: &folder,
                    link: &link,
                    min_size: fmt::parse_size(&min_size)?,
                    go,
                    json,
                    use_daemon,
                },
            );
        }
        Cmd::Mail { action } => {
            return match action {
                MailCmd::Merge {
                    sources,
                    to,
                    dry_run,
                    json,
                } => tidy::mail_merge(&db, db_path, &sources, &to, dry_run, json, use_daemon),
                MailCmd::Export { maildir, mbox } => tidy::mail_export(&db, &maildir, &mbox),
            };
        }
        Cmd::Plans => return filing::list_plans(&db),
        Cmd::Wipe {
            paths,
            move_to,
            trust_index,
            go,
            confirm,
        } => {
            return filing::wipe(
                &mut db,
                db_path,
                &paths,
                move_to.as_deref(),
                trust_index,
                go,
                confirm.as_deref(),
                use_daemon,
            );
        }
        Cmd::Mirror { action } => {
            return match action {
                MirrorCmd::Add {
                    folder,
                    mirror,
                    verify_every,
                } => filing::mirror_add(&db, &folder, &mirror, verify_every),
                MirrorCmd::List => filing::mirror_list(&db),
                MirrorCmd::Sync { id } => filing::mirror_sync(&db, id),
                MirrorCmd::Verify { id, percent, all } => {
                    filing::mirror_verify(&db, id, if all { 100 } else { percent })
                }
                MirrorCmd::Remove { id } => {
                    backdown_core::mirror::remove(&db, id)?;
                    println!("Mirror {id} removed; its copies stay where they are.");
                    Ok(ExitCode::SUCCESS)
                }
            };
        }
        Cmd::Apply { plan, go } => return filing::run_apply(&mut db, plan, go),
        Cmd::Serve {
            port,
            host,
            no_open,
        } => {
            drop(db);
            serve::serve(db_path, &host, port, !no_open)?;
        }
        Cmd::Status { json } => {
            let s = db.status()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&s)?);
            } else {
                println!(
                    "index: {}",
                    dunce::canonicalize(db_path)
                        .unwrap_or(db_path.to_path_buf())
                        .display()
                );
                print_status(&s);
                println!("daemon: not running (backdown service install, or backdown daemon)");
            }
        }
        Cmd::Daemon { .. }
        | Cmd::Open
        | Cmd::Token { .. }
        | Cmd::Completions { .. }
        | Cmd::App { .. }
        | Cmd::Desktop { .. }
        | Cmd::Update { .. }
        | Cmd::Service { .. }
        | Cmd::License { .. }
        | Cmd::ImportLegacy { .. }
        | Cmd::Rescue { .. }
        | Cmd::Volumes { .. }
        | Cmd::Health { .. }
        | Cmd::Remote { .. }
        | Cmd::ServeStdio
        | Cmd::Mount { .. }
        | Cmd::Unmount { .. }
        | Cmd::Custody { .. } => {
            unreachable!("handled in run")
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// The same commands, carried out by the running daemon.
fn via_daemon(mut c: client::Client, cmd: Cmd) -> Result<ExitCode> {
    let mut follow = |c: &mut client::Client, job: u64, what: &str| -> Result<String> {
        let what = what.to_string();
        let j = c.follow(job, |j| {
            let state = j["state"].as_str().unwrap_or("");
            let p = j["progress"].as_str().unwrap_or("");
            live(&if state == "queued" {
                format!("{what}: waiting for the daemon's current job")
            } else {
                format!("{what}: {p}")
            });
        })?;
        clear();
        Ok(j["summary"].as_str().unwrap_or("").to_string())
    };
    let hash_first = |c: &mut client::Client,
                      h: &HashArgs,
                      follow: &mut dyn FnMut(&mut client::Client, u64, &str) -> Result<String>|
     -> Result<()> {
        let r = c.request(&json!({ "cmd": "enqueue", "kind": "hash", "jobs": h.jobs, "retry_errors": h.retry_errors }))?;
        let summary = follow(c, r["job"].as_u64().unwrap_or(0), "hashing")?;
        if !summary.starts_with("hashed: 0 sampled, 0 full") {
            eprintln!("{summary}");
        }
        Ok(())
    };
    match cmd {
        Cmd::Index {
            paths,
            cross_filesystems,
            allow_empty,
            method,
        } => {
            for path in paths {
                let abs = backdown_core::paths::canonical(&path)
                    .with_context(|| format!("can't open {}", path.display()))?;
                let r = c.request(&json!({ "cmd": "enqueue", "kind": "index", "path": abs.display().to_string(), "cross_filesystems": cross_filesystems, "allow_empty": allow_empty, "method": method }))?;
                let summary = follow(
                    &mut c,
                    r["job"].as_u64().unwrap_or(0),
                    &format!("indexing {}", abs.display()),
                )?;
                eprintln!("{summary}");
            }
            eprintln!("(the daemon hashes new files next; `backdown status` shows progress)");
        }
        Cmd::Hash { hashing } => hash_first(&mut c, &hashing, &mut follow)?,
        Cmd::Apply { plan, .. } => {
            let r = c.request(&json!({ "cmd": "enqueue", "kind": "apply", "plan": plan }))?;
            let summary = follow(
                &mut c,
                r["job"].as_u64().unwrap_or(0),
                &format!("applying plan {plan}"),
            )?;
            eprintln!("{summary}");
            eprintln!(
                "(the daemon indexes the archive next, so its copies count as proof; then `backdown check <old drive>`)"
            );
        }
        Cmd::Dupes {
            min_size,
            limit,
            json,
            hashing,
        } => {
            hash_first(&mut c, &hashing, &mut follow)?;
            let r = c.request(
                &json!({ "cmd": "dupes", "min": fmt::parse_size(&min_size)?, "limit": limit }),
            )?;
            print_dupes(
                &serde_json::from_value(r["report"].clone())?,
                &min_size,
                json,
            )?;
        }
        Cmd::Check {
            path,
            list,
            json,
            hashing,
        } => {
            let abs = backdown_core::paths::canonical(&path)
                .with_context(|| format!("can't open {}", path.display()))?;
            hash_first(&mut c, &hashing, &mut follow)?;
            let r = c.request(&json!({ "cmd": "check", "path": abs.display().to_string() }))?;
            return print_check(&serde_json::from_value(r["report"].clone())?, list, json);
        }
        Cmd::Scans { json } => {
            let r = c.request(&json!({ "cmd": "scans" }))?;
            let scans: Vec<report::Scan> = serde_json::from_value(r["scans"].clone())?;
            print_scans(&scans, json)?;
        }
        Cmd::Status { json } => {
            let r = c.request(&json!({ "cmd": "status" }))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                println!("index: {}", c.db);
                print_status(&serde_json::from_value(r["index"].clone())?);
                let d = &r["daemon"];
                let now = d["current"].as_object().map(|j| {
                    format!(
                        "{} {} {}",
                        j["kind"].as_str().unwrap_or(""),
                        j["path"].as_str().unwrap_or(""),
                        j["progress"].as_str().unwrap_or("")
                    )
                });
                println!(
                    "daemon: running (pid {}, {} queued) · {}",
                    d["pid"],
                    d["queued"].as_array().map_or(0, Vec::len),
                    now.unwrap_or_else(|| "idle".into())
                );
            }
        }
        Cmd::Serve { .. }
        | Cmd::Daemon { .. }
        | Cmd::Open
        | Cmd::Token { .. }
        | Cmd::Completions { .. }
        | Cmd::App { .. }
        | Cmd::Desktop { .. }
        | Cmd::Update { .. }
        | Cmd::Service { .. }
        | Cmd::License { .. }
        | Cmd::ImportLegacy { .. }
        | Cmd::Entities { .. }
        | Cmd::Overlaps { .. }
        | Cmd::Report { .. }
        | Cmd::Plan { .. }
        | Cmd::Plans
        | Cmd::Mirror { .. }
        | Cmd::Wipe { .. }
        | Cmd::Dedup { .. }
        | Cmd::Mail { .. }
        | Cmd::Rescue { .. }
        | Cmd::Volumes { .. }
        | Cmd::Health { .. }
        | Cmd::Remote { .. }
        | Cmd::ServeStdio
        | Cmd::Mount { .. }
        | Cmd::Unmount { .. }
        | Cmd::Custody { .. } => {
            unreachable!("handled in run")
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn import_legacy(db_path: &Path, cmd: Cmd) -> Result<ExitCode> {
    let Cmd::ImportLegacy {
        index_dir,
        hashes,
        map,
        volume,
        replace,
        dry_run,
    } = cmd
    else {
        unreachable!("called for import-legacy only")
    };
    let maps = map
        .iter()
        .map(|m| {
            let (from, to) = m
                .split_once('=')
                .filter(|(f, t)| !f.is_empty() && !t.is_empty())
                .with_context(|| format!("--map {m:?}: expected LEGACY_PATH=PATH_NOW"))?;
            Ok((PathBuf::from(from), std::path::absolute(to)?))
        })
        .collect::<Result<Vec<_>>>()?;
    // a dry run reads the index, never creates or migrates it
    let mut db = if !dry_run {
        Db::open(db_path)?
    } else if db_path.exists() {
        Db::open_read_only(db_path)?
    } else {
        Db::open(Path::new(":memory:"))?
    };
    let opts = legacy::Options {
        hashes,
        maps,
        volumes: volume,
        replace,
        dry_run,
    };
    let r = legacy::import(&mut db, &index_dir, &opts, &mut |p| {
        let pct = if p.total > 0 {
            100.0 * p.done as f64 / p.total as f64
        } else {
            100.0
        };
        live(&match p.stage {
            "hashes" => format!(
                "hashes.db: {} of {} rows ({pct:.0}%), {} attached",
                fmt::count(p.done),
                fmt::count(p.total),
                fmt::count(p.files)
            ),
            "folders" => format!(
                "{}: folder totals for {} files",
                p.volume,
                fmt::count(p.files)
            ),
            _ => format!(
                "{}: {} files, {pct:.0}% of {} read",
                p.volume,
                fmt::count(p.files),
                fmt::bytes(p.total)
            ),
        });
    })?;
    clear();
    print_legacy(&r);
    Ok(ExitCode::SUCCESS)
}

fn print_legacy(r: &legacy::Report) {
    let did = |done: &str, would: &str| if r.dry_run { would } else { done }.to_string();
    for v in &r.volumes {
        println!(
            "{}: {} → {}{}",
            v.name,
            v.legacy_root.display(),
            v.root.display(),
            if v.mounted {
                ""
            } else {
                " (not mounted here now)"
            }
        );
        let t = &v.tally;
        match v.outcome {
            legacy::Outcome::Empty => {
                println!(
                    "  nothing to import: no finished chunk and no {}.tsv",
                    v.name
                );
                continue;
            }
            legacy::Outcome::AlreadyImported => {
                println!(
                    "  imported already (scan {}): {} files, {}; left as it is",
                    v.scan.unwrap_or(0),
                    fmt::count(t.files),
                    fmt::bytes(t.bytes)
                );
                continue;
            }
            _ => {}
        }
        if v.merge_outdated {
            println!(
                "  read {} ({}.tsv is older than its chunks)",
                v.source, v.name
            );
        } else {
            println!("  read {}", v.source);
        }
        if v.chunks_unfinished > 0 {
            println!(
                "  {} of {} planned chunks never finished: their files aren't imported (the next \
                 `backdown index` adds them), e.g.",
                v.chunks_unfinished, v.chunks_planned
            );
            for e in &v.unfinished_examples {
                println!("      {e}");
            }
        }
        if v.chunks_from_earlier_plans > 0 {
            println!(
                "  {} finished chunks from an earlier plan were read too, as `bd-index.sh merge` does",
                v.chunks_from_earlier_plans
            );
        }
        for p in &v.replaced {
            println!(
                "  {} what the index held for {}",
                did("replaced", "would replace"),
                p.display()
            );
        }
        let resumed = match v.outcome {
            legacy::Outcome::Resumed { from_files } => {
                format!("resumed after {} files: ", fmt::count(from_files))
            }
            _ => String::new(),
        };
        println!(
            "  {resumed}{} {} files, {}{} · crawled {}",
            did("imported", "would import"),
            fmt::count(t.files),
            fmt::bytes(t.bytes),
            v.scan.map_or(String::new(), |s| format!(" as scan {s}")),
            utc(v.finished_at)
        );
        let mut skipped = Vec::new();
        for (n, what) in [
            (t.malformed, "malformed lines"),
            (t.truncated, "cut-off last lines"),
            (t.outside_root, "paths outside the root"),
            (t.duplicates, "paths listed twice (the first was kept)"),
        ] {
            if n > 0 {
                skipped.push(format!("{n} {what}"));
            }
        }
        if !skipped.is_empty() {
            println!("  skipped {}", skipped.join(", "));
            for e in &t.examples {
                println!("      {e:?}");
            }
        }
        if t.rejoined > 0 {
            println!("  rejoined {} names holding a newline", t.rejoined);
        }
    }
    if !r.not_selected.is_empty() {
        println!("not selected: {}", r.not_selected.join(", "));
    }
    if !r.ignored.is_empty() {
        println!(
            "ignored, not volumes (no <name>/chunks.list): {}",
            r.ignored.join(", ")
        );
    }
    match (&r.hashes, &r.hashes_unused) {
        (Some(h), _) if r.dry_run => println!(
            "hashes.db: {} rows, {} of them for files on these volumes",
            fmt::count(h.rows),
            fmt::count(h.for_volumes)
        ),
        (Some(h), _) => {
            println!(
                "hashes.db: {} rows: {} attached, {} there already, {} stale (size or mtime changed \
                 since), {} for files not in the index",
                fmt::count(h.rows),
                fmt::count(h.attached),
                fmt::count(h.already),
                fmt::count(h.stale),
                fmt::count(h.unmatched)
            );
            if h.conflicting + h.unusable + h.empty > 0 {
                println!(
                    "  kept {} files' own different hash; skipped {} unusable rows and {} empty files",
                    h.conflicting, h.unusable, h.empty
                );
            }
        }
        (None, Some(p)) => println!(
            "note: {} wasn't used; pass --hashes {} to attach its hashes",
            p.display(),
            p.display()
        ),
        _ => {}
    }
    if r.dry_run {
        println!("dry run: nothing was written");
    } else if r.volumes.iter().any(|v| {
        !matches!(
            v.outcome,
            legacy::Outcome::AlreadyImported | legacy::Outcome::Empty
        )
    }) {
        println!(
            "next: with each drive mounted, `backdown index <root>` (as a user that can read all of it) \
             finds its files unchanged and keeps their hashes"
        );
    }
}

/// `backdown mount [TARGET]`: a partition read-only, or a disk image attached read-only; without a
/// target, what BackDown has mounted.
fn run_mount(target: Option<PathBuf>) -> Result<ExitCode> {
    let state = locations::state_dir();
    let Some(t) = target else {
        let all = mounting::current(&state);
        if all.is_empty() {
            println!("BackDown hasn't mounted anything.");
        }
        for m in all {
            println!(
                "{}{}",
                m.source,
                m.attached
                    .as_deref()
                    .map(|a| format!(" (attached as {a})"))
                    .unwrap_or_default()
            );
            for (dev, at) in &m.mounts {
                println!("  {dev} at {at} (read-only)");
            }
        }
        return Ok(ExitCode::SUCCESS);
    };
    let v = if t.is_file() {
        mounting::attach(&state, &t)?
    } else {
        let dev = t.display().to_string();
        drives::check_mountable(&dev)?;
        mounting::mount(&state, &dev)?
    };
    let points: Vec<String> = match v["mounts"].as_array() {
        Some(list) => list
            .iter()
            .filter_map(|m| m[1].as_str().map(String::from))
            .collect(),
        None => v["mounted"]
            .as_str()
            .map(String::from)
            .into_iter()
            .collect(),
    };
    done(&format!(
        "Mounted read-only at {}",
        if points.is_empty() {
            "(nowhere new)".to_string()
        } else {
            points.join(", ")
        }
    ));
    if let Some(p) = points.first() {
        eprintln!(
            "To index it: backdown index {p}\nWhen you're done: backdown unmount {}",
            t.display()
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// `backdown volumes`: probe the disks, then mark what the index covers, if there is an index.
/// Never creates one, and doesn't need the daemon.
fn run_volumes(db: Option<PathBuf>, json: bool) -> Result<ExitCode> {
    let mut r = volumes::probe()?;
    let explicit = db.is_some();
    let db = db
        .or_else(|| client::Client::connect(None).map(|c| PathBuf::from(c.db)))
        .unwrap_or_else(locations::default_db);
    if db.is_file() {
        match Db::open_read_only(&db).and_then(|d| volumes::mark_indexed(&mut r, &d)) {
            Ok(()) => r.index = Some(db.display().to_string()),
            Err(e) => r
                .notes
                .push(format!("couldn't read the index {}: {e:#}", db.display())),
        }
    } else if explicit {
        r.notes.push(format!("no index at {}", db.display()));
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&r)?);
    } else {
        print_volumes(&r);
    }
    Ok(ExitCode::SUCCESS)
}

/// One block per disk, one row per volume (what's stacked on it indented below), fitting in 100
/// columns: long mount points and notes are shortened in the middle, extra ones go on more lines.
fn print_volumes(r: &volumes::Report) {
    if r.disks.is_empty() {
        println!("no disks found");
    }
    for (i, d) in r.disks.iter().enumerate() {
        if i > 0 {
            println!();
        }
        let mut head = vec![format!("{}  {}", d.name, fmt::bytes(d.size))];
        head.extend(d.model.clone());
        head.extend(d.transport.clone());
        head.extend(
            d.rotational
                .map(|r| if r { "rotational" } else { "solid state" }.to_string()),
        );
        for (on, tag) in [
            (d.removable, "removable"),
            (d.read_only, "read-only"),
            (d.system, "system disk"),
        ] {
            if on {
                head.push(tag.to_string());
            }
        }
        head.extend(d.backing_file.as_ref().map(|f| format!("file {f}")));
        head.extend(d.serial.as_ref().map(|s| format!("S/N {s}")));
        println!("{}", clip(&head.join(" · "), 100));
        for v in &d.volumes {
            print_volume(v, 0);
        }
    }
    if !r.notes.is_empty() {
        println!();
        for n in &r.notes {
            println!("note: {n}");
        }
    }
}

fn print_volume(v: &volumes::Volume, depth: usize) {
    use volumes::State;
    let name = match depth {
        0 => v.name.clone(),
        n => format!("{}└ {}", "  ".repeat(n - 1), v.name),
    };
    let state = match (v.state, v.read_only) {
        (State::Mounted, Some(true)) => "mounted ro",
        (State::Mounted, _) => "mounted rw",
        (s, _) => s.as_str(),
    };
    let fs = v.fs_name.as_deref().unwrap_or(match v.state {
        State::Unknown => "?",
        _ => "-",
    });
    let mut details = Vec::new();
    if !v.mounts.is_empty() {
        let paths: Vec<&str> = v.mounts.iter().map(|m| m.path.as_str()).collect();
        details.push(paths.join(", "));
    }
    if let Some(e) = &v.encryption {
        let lock = if e.unlocked { "unlocked" } else { "locked" };
        details.push(format!("{}, {lock}", e.scheme));
    }
    details.extend(v.note.clone());
    for i in &v.indexed {
        let when = i
            .scanned_at
            .map_or("no finished scan".to_string(), |t| utc(t)[..10].to_string());
        details.push(match i.relation {
            "same" => format!("indexed, scanned {when}"),
            "inside" => format!("inside indexed {} ({when})", i.root),
            _ => format!("indexed: {} ({when})", i.root),
        });
    }
    let row = format!(
        "  {:<12} {:>8}  {:<10} {:<14} {:<10}  ",
        clip(&name, 12),
        fmt::bytes(v.size),
        clip(fs, 10),
        clip(v.label.as_deref().unwrap_or(""), 14),
        state
    );
    let indent = row.chars().count();
    let room = 100 - indent;
    let mut details = details.iter();
    match details.next() {
        Some(d) => println!("{row}{}", clip_middle(d, room)),
        None => println!("{}", row.trim_end()),
    }
    for d in details {
        println!("{:indent$}{}", "", clip_middle(d, room));
    }
    for h in &v.holders {
        print_volume(h, depth + 1);
    }
}

/// At most `width` characters, cut at the end.
fn clip(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    let mut out: String = s.chars().take(width - 1).collect();
    out.push('…');
    out
}

/// At most `width` characters, cut in the middle (the end of a path says the most).
fn clip_middle(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n <= width {
        return s.to_string();
    }
    let head = (width - 1) / 3;
    let tail = width - 1 - head;
    let mut out: String = s.chars().take(head).collect();
    out.push('…');
    out.extend(s.chars().skip(n - tail));
    out
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
        done(
            &(format!(
                "hashed: {} sampled, {} full, {} read{}",
                fmt::count(st.sampled),
                fmt::count(st.hashed),
                fmt::bytes(st.bytes_read),
                if st.errors > 0 {
                    format!(", {} unreadable", st.errors)
                } else {
                    String::new()
                }
            ) + &hash_absent(st.absent)),
        );
    } else {
        clear();
    }
    if st.absent > 0 && st.sampled + st.hashed + st.errors == 0 {
        eprintln!("{}", hash_absent(st.absent).trim_start_matches("; "));
    }
    Ok(())
}

fn print_dupes(d: &report::Dupes, min_size: &str, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(d)?);
        return Ok(());
    }
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
            println!("    {p}");
        }
    }
    Ok(())
}

fn print_scans(scans: &[report::Scan], json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(scans)?);
        return Ok(());
    }
    for s in scans {
        println!(
            "{:>5}  {}  {:>10} files  {:>10}  {}",
            s.id,
            utc(s.finished_at),
            fmt::count(s.files),
            fmt::bytes(s.bytes),
            s.root
        );
    }
    Ok(())
}

fn print_status(s: &backdown_core::db::Status) {
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

/// Prints the verdict; exit 0 when safe to wipe, 2 when files must be copied first.
fn print_check(c: &report::Check, list: bool, json: bool) -> Result<ExitCode> {
    let code = if c.safe {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    };
    if json {
        println!("{}", serde_json::to_string_pretty(c)?);
        return Ok(code);
    }
    println!("{}", c.path);
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
    if c.remote.files > 0 {
        println!(
            "  of the verified, {} in {} files are only on another machine ({}): hashed there and \
             confirmed there within {} days; `backdown wipe` has that machine re-read each copy first",
            fmt::bytes(c.remote.bytes),
            fmt::count(c.remote.files),
            c.remote_machines.join(", "),
            c.proof_days
        );
    }
    println!();
    if c.safe {
        println!(
            "SAFE TO WIPE: every file has a SHA-256-identical copy outside {}{}.",
            c.path,
            if c.remote.files > 0 {
                ", some of them on another machine"
            } else {
                ""
            }
        );
        return Ok(code);
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
            println!("{:>10}  {p}", fmt::bytes(*size));
        }
        if c.unique.files > c.unique_files.len() as u64 {
            println!(
                "  … and {} smaller ones (the largest {} are listed)",
                fmt::count(c.unique.files - c.unique_files.len() as u64),
                fmt::count(report::UNIQUE_LISTED)
            );
        }
        for (p, e) in &c.unreadable_files {
            println!("{:>10}  {p}  ({e})", "unreadable");
        }
    } else if c.unique.files > 0 {
        println!("  (--list shows them, the largest 20,000 first)");
    }
    Ok(code)
}

/// A progress line that rewrites itself; only when stderr is a terminal.
/// "; 1,234 files on drives that aren't connected wait for them"
pub(crate) fn hash_absent(n: u64) -> String {
    if n == 0 {
        String::new()
    } else {
        format!(
            "; {} files on drives that aren't connected wait for them",
            fmt::count(n)
        )
    }
}

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
    fn clipping() {
        assert_eq!(super::clip("photos", 10), "photos");
        assert_eq!(super::clip("a long label", 6), "a lon…");
        assert_eq!(
            super::clip_middle("/media/someone/A Long Drive Name", 16),
            "/medi…Drive Name"
        );
    }

    #[test]
    fn utc_dates() {
        assert_eq!(super::utc(0), "1970-01-01 00:00 UTC");
        assert_eq!(super::utc(1_790_000_000), "2026-09-21 14:13 UTC");
    }
}
