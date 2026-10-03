//! `backdown manifest`: signed, tamper-evident manifests of drives (backdown_core::manifest), the
//! daemon's scheduled attestations (the `attest` setting), and the browser's list of them.
//!
//! The commands work on the index directly, like `entities`: making a manifest reads the index and
//! adds one row, which waits its turn behind a running daemon's writes, and checking one needs no
//! index at all. They sign with this installation's key (`signing.key` in your data folder); the
//! daemon signs with the one in its own data folder, which for a per-user service is the same
//! file, and for the machine-wide service a key of its own.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use backdown_core::manifest::{self, Checked, Comparison};
use backdown_core::signing::Signer;
use backdown_core::{Db, fmt, paths};
use clap::Subcommand;
use serde_json::{Value, json};

use crate::daemon::{Counts, Daemon, Job, JobOpts};
use crate::{clear, client, live, locations, tiers, utc};

#[derive(Subcommand)]
pub enum ManifestCmd {
    /// Sign a manifest of DRIVE as of its latest finished scan: every file's path, size and hash
    /// in one Merkle root, signed with this installation's key and chained to the drive's previous
    /// manifest, and kept in the index. The document goes to the terminal unless --out names a file
    Create {
        /// The drive (an indexed folder), or a folder on it
        drive: PathBuf,
        /// Attest this finished scan of the drive instead (`backdown scans` lists them)
        #[arg(long)]
        scan: Option<i64>,
        /// Write the signed document to this file (never overwritten)
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
        /// Also write its listing, every file as a line of JSON, to this file: checking which
        /// files were signed needs it (never overwritten)
        #[arg(long, value_name = "FILE")]
        list: Option<PathBuf>,
    },
    /// Check manifests: the signature; the chain, among several files or with the manifests this
    /// index keeps; with --list, that the listing is exactly what was signed. Exits 0 when it all
    /// checks, 2 when something doesn't
    Verify {
        #[arg(required = true, value_name = "FILE")]
        files: Vec<PathBuf>,
        /// The manifest's listing (with one manifest)
        #[arg(long, value_name = "FILE")]
        list: Option<PathBuf>,
        /// Also compare it with the index now: what was added, removed or changed on the drive
        /// since (with one manifest)
        #[arg(long)]
        against_index: bool,
        #[arg(long)]
        json: bool,
    },
    /// The manifests this index keeps, oldest first (one drive's, or every drive's), and whether
    /// each drive's chain is whole
    List {
        /// Only this drive (or the drive this folder is on)
        drive: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Write out a manifest this index keeps (by its number in `manifest list`), and its listing,
    /// made again from the index's record of its scan
    Export {
        id: i64,
        /// Write the document to this file instead of the terminal (never overwritten)
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
        /// Write its listing to this file (never overwritten)
        #[arg(long, value_name = "FILE")]
        list: Option<PathBuf>,
    },
    /// This installation's signing key: its fingerprint and public key, for whoever checks your
    /// manifests (made now if there's none yet)
    Key {
        #[arg(long)]
        json: bool,
    },
}

/// The index to use: --db, else the running daemon's, else yours.
fn index_path(db: Option<PathBuf>, local: bool) -> PathBuf {
    db.or_else(|| {
        (!local)
            .then(|| client::Client::connect(None).map(|c| PathBuf::from(c.db)))
            .flatten()
    })
    .unwrap_or_else(locations::default_db)
}

fn open(db: &Path) -> Result<Db> {
    if let Some(dir) = db.parent() {
        locations::ensure_private_dir(dir, false)
            .with_context(|| format!("can't create {}", dir.display()))?;
    }
    Db::open(db)
}

/// A new file, refusing to replace one: a manifest or listing written earlier is worth keeping.
fn new_file(p: &Path) -> Result<File> {
    File::options()
        .write(true)
        .create_new(true)
        .open(p)
        .with_context(|| {
            if p.exists() {
                format!(
                    "{} exists already: choose another name (manifests and listings aren't overwritten)",
                    p.display()
                )
            } else {
                format!("can't create {}", p.display())
            }
        })
}

fn refuse_existing(files: &[&Option<PathBuf>]) -> Result<()> {
    for p in files.iter().copied().flatten() {
        if p.exists() {
            bail!(
                "{} exists already: choose another name (manifests and listings aren't overwritten)",
                p.display()
            );
        }
    }
    Ok(())
}

/// The indexed drive holding `drive` (the drive itself, or a folder on it).
fn root_of(db: &Db, drive: &Path) -> Result<(i64, PathBuf)> {
    let p = paths::canonical(drive)
        .or_else(|_| std::path::absolute(drive))
        .unwrap_or_else(|_| drive.to_path_buf());
    let want = paths::dir_prefix(&p);
    let mut stmt = db.conn.prepare("SELECT id, path FROM roots")?;
    let roots: Vec<(i64, PathBuf)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get::<_, Vec<u8>>(1)?)))?
        .map(|r| r.map(|(id, b)| (id, paths::from_bytes(&b))))
        .collect::<rusqlite::Result<_>>()?;
    roots
        .into_iter()
        .filter(|(_, r)| want.starts_with(&paths::dir_prefix(r)))
        .max_by_key(|(_, r)| r.as_os_str().len())
        .with_context(|| {
            format!(
                "{} isn't an indexed drive or on one (`backdown status` lists the drives)",
                drive.display()
            )
        })
}

fn key_path() -> PathBuf {
    locations::state_dir().join(locations::SIGNING_KEY)
}

fn signer() -> Result<Signer> {
    let p = key_path();
    Signer::load_or_create(&p)
        .with_context(|| format!("can't use the signing key at {}", p.display()))
}

pub fn run(db: Option<PathBuf>, local: bool, cmd: ManifestCmd) -> Result<ExitCode> {
    match cmd {
        ManifestCmd::Create {
            drive,
            scan,
            out,
            list,
        } => create(&index_path(db, local), &drive, scan, out, list),
        ManifestCmd::Verify {
            files,
            list,
            against_index,
            json,
        } => verify(
            &index_path(db, local),
            &files,
            list.as_deref(),
            against_index,
            json,
        ),
        ManifestCmd::List { drive, json } => list_cmd(&index_path(db, local), drive, json),
        ManifestCmd::Export { id, out, list } => export(&index_path(db, local), id, out, list),
        ManifestCmd::Key { json } => {
            let s = signer()?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "fingerprint": s.fingerprint(), "public_key": s.public_hex(),
                        "file": key_path().display().to_string(),
                    }))?
                );
            } else {
                println!(
                    "This installation's signing key, kept in {} (readable by you alone):",
                    key_path().display()
                );
                println!("  fingerprint  {}", s.fingerprint());
                println!("  public key   {}", s.public_hex());
                println!(
                    "Every manifest signed here names this public key. Give it, or at least its fingerprint, to whoever checks your manifests, so they can tell a manifest is yours."
                );
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn create(
    db_path: &Path,
    drive: &Path,
    scan: Option<i64>,
    out: Option<PathBuf>,
    list: Option<PathBuf>,
) -> Result<ExitCode> {
    refuse_existing(&[&out, &list])?;
    let db = open(db_path).with_context(|| {
        format!(
            "manifests are kept in the index, and {} can't be written from here",
            db_path.display()
        )
    })?;
    let (root_id, root) = root_of(&db, drive)?;
    let show = paths::show(&root);
    let signer = signer()?;
    let mut listing = list
        .as_deref()
        .map(|p| new_file(p).map(BufWriter::new))
        .transpose()?;
    let made = manifest::create(
        &db,
        &signer,
        root_id,
        scan,
        listing.as_mut().map(|w| w as &mut dyn Write),
        &mut |n| live(&format!("signing {show}: {} files", fmt::count(n))),
    );
    clear();
    let made = match made {
        Ok(m) => m,
        Err(e) => {
            drop(listing);
            if let Some(p) = &list {
                let _ = std::fs::remove_file(p);
            }
            return Err(e);
        }
    };
    let text = serde_json::to_string_pretty(&made.doc)? + "\n";
    match &out {
        Some(p) => new_file(p)?
            .write_all(text.as_bytes())
            .with_context(|| format!("can't write {}", p.display()))?,
        None => print!("{text}"),
    }
    eprintln!(
        "Signed manifest {} of {show} at scan {}: {} files, {}; {} hashed, {} by path and size only",
        made.id,
        made.scan,
        fmt::count(made.files),
        fmt::bytes(made.bytes),
        fmt::count(made.hashed),
        fmt::count(made.files - made.hashed)
    );
    eprintln!(
        "  Merkle root {}",
        made.doc["merkle"].as_str().unwrap_or("")
    );
    eprintln!(
        "  {} · key {}",
        match made.prev_id {
            Some(p) => format!("follows manifest {p} of this drive"),
            None => "the first of this drive's chain".into(),
        },
        signer.fingerprint()
    );
    if let Some(p) = &out {
        eprintln!("  document: {}", p.display());
    }
    match &list {
        Some(p) => eprintln!("  listing:  {}", p.display()),
        None => eprintln!(
            "  (no listing kept: `backdown manifest export {} --list FILE` makes it again from the index)",
            made.id
        ),
    }
    Ok(ExitCode::SUCCESS)
}

fn read_doc(p: &Path) -> Result<Value> {
    let f = File::open(p).with_context(|| format!("can't open {}", p.display()))?;
    serde_json::from_reader(BufReader::new(f))
        .with_context(|| format!("{} isn't a manifest: not JSON", p.display()))
}

/// How a manifest stands in the chain this index keeps for its drive: whether that's all right,
/// and a line saying so.
fn chain_here(db: &Db, doc: &Value, c: &Checked) -> Result<(bool, String)> {
    let kept: Vec<manifest::Stored> = manifest::list(db, Some(c.root_id))?
        .into_iter()
        .filter(|k| k.doc["key"] == doc["key"] && k.drive == c.drive)
        .collect();
    if kept.is_empty() {
        return Ok((
            true,
            "not checked here: this index keeps no manifests of that drive signed with that key"
                .into(),
        ));
    }
    let me = manifest::chain_hash(doc);
    let docs: Vec<Value> = kept.iter().map(|k| k.doc.clone()).collect();
    if let Some(i) = kept.iter().position(|k| manifest::chain_hash(&k.doc) == me) {
        let wrong: Vec<String> = manifest::check_chain(&docs)
            .into_iter()
            .filter(|p| p.at == i || p.at == i + 1)
            .map(|p| {
                format!(
                    "manifest {}: {}",
                    kept[p.at].id,
                    p.what.replace('#', "position ")
                )
            })
            .collect();
        if !wrong.is_empty() {
            return Ok((false, format!("BROKEN here: {}", wrong.join("; "))));
        }
        return Ok((
            true,
            match i {
                0 => format!(
                    "kept here as manifest {}, the first of this drive's chain",
                    kept[i].id
                ),
                _ => format!(
                    "kept here as manifest {}, following manifest {}",
                    kept[i].id,
                    kept[i - 1].id
                ),
            },
        ));
    }
    Ok(match &c.prev {
        None => (true, "the first of a chain, not kept in this index".into()),
        Some(p) => match kept.iter().find(|k| manifest::chain_hash(&k.doc) == *p) {
            Some(k) => (
                true,
                format!(
                    "follows manifest {} kept here (it isn't kept here itself)",
                    k.id
                ),
            ),
            None => (
                false,
                "a gap: the manifest it follows isn't among this drive's manifests kept here"
                    .into(),
            ),
        },
    })
}

fn side(s: &Option<manifest::Side>) -> String {
    match s {
        None => String::new(),
        Some(s) => format!(
            "{}{}",
            fmt::bytes(s.size),
            s.sha256
                .as_deref()
                .map(|h| format!(" {}…", &h[..12.min(h.len())]))
                .unwrap_or_default()
        ),
    }
}

fn print_comparison(cmp: &Comparison, scan: i64, show: &str) {
    println!(
        "  Compared with the index: {show} at scan {} ({})",
        cmp.now_scan,
        utc(cmp.now_finished_at)
    );
    match (cmp.from, cmp.reproduced, cmp.exact) {
        ("index", true, _) => println!(
            "    The index's record of scan {scan} still gives the signed Merkle root: it holds exactly what was signed."
        ),
        ("index", false, true) => println!(
            "    The index's record of scan {scan} NO LONGER gives the signed Merkle root: the index changed since it was signed."
        ),
        ("index", false, false) => println!(
            "    The index's record of scan {scan} doesn't give the signed Merkle root, most likely because files were hashed since; give its listing (--list) to compare with exactly what was signed."
        ),
        _ => {}
    }
    println!(
        "    since then: {} added ({}), {} removed ({}), {} changed; {} unchanged{}",
        fmt::count(cmp.added.files),
        fmt::bytes(cmp.added.bytes),
        fmt::count(cmp.removed.files),
        fmt::bytes(cmp.removed.bytes),
        fmt::count(cmp.changed.files),
        fmt::count(cmp.unchanged),
        if cmp.unverified > 0 {
            format!(
                ", {} of them the same size but not hashed on both sides",
                fmt::count(cmp.unverified)
            )
        } else {
            String::new()
        }
    );
    for (what, t) in [
        ("added", &cmp.added),
        ("removed", &cmp.removed),
        ("changed", &cmp.changed),
    ] {
        for d in &t.examples {
            let detail = match what {
                "changed" => format!("  ({} → {})", side(&d.was), side(&d.now)),
                _ => String::new(),
            };
            println!("    {what:<8} {}{detail}", d.path);
        }
        if t.files > t.examples.len() as u64 {
            println!(
                "    {what:<8} … and {} more",
                fmt::count(t.files - t.examples.len() as u64)
            );
        }
    }
}

fn verify(
    db_path: &Path,
    files: &[PathBuf],
    list: Option<&Path>,
    against_index: bool,
    json: bool,
) -> Result<ExitCode> {
    if files.len() > 1 && (list.is_some() || against_index) {
        bail!("--list and --against-index go with one manifest at a time");
    }
    let docs: Vec<Value> = files.iter().map(|f| read_doc(f)).collect::<Result<_>>()?;
    let index = Db::open_read_only(db_path).ok();
    if against_index && index.is_none() {
        bail!(
            "--against-index needs the index, and there's none at {}",
            db_path.display()
        );
    }
    let mut ok = true;
    let mut report = Vec::new();
    let mut good: Vec<(usize, Checked)> = Vec::new();
    for (i, (file, doc)) in files.iter().zip(&docs).enumerate() {
        let mut r = json!({ "file": file.display().to_string() });
        if !json {
            println!("{}:", file.display());
        }
        let c = match manifest::check(doc) {
            Ok(c) => c,
            Err(e) => {
                ok = false;
                r["ok"] = json!(false);
                r["error"] = json!(format!("{e:#}"));
                if !json {
                    println!("  Signature: FAILS: {e:#}");
                }
                report.push(r);
                continue;
            }
        };
        // signed here? (checking never makes a key)
        let mine = key_info(&locations::state_dir())["public_key"] == c.key.as_str();
        let mut this_ok = true;
        if !json {
            println!(
                "  Signature: good, by key {}{}",
                c.fingerprint,
                if mine {
                    " (this installation's own)"
                } else {
                    ""
                }
            );
            println!(
                "  A manifest of {} (drive {}) at scan {}, which finished {}; signed {}",
                c.drive, c.root_id, c.scan_id, c.scan_finished, c.created
            );
            println!(
                "  {} files, {}: {} hashed, {} attested by path and size only",
                fmt::count(c.files),
                fmt::bytes(c.bytes),
                fmt::count(c.hashed),
                fmt::count(c.unhashed)
            );
        }
        r["manifest"] = json!(c);
        r["own_key"] = json!(mine);
        if let Some(db) = &index {
            let (fine, line) = chain_here(db, doc, &c)?;
            this_ok &= fine;
            if !json {
                println!("  Chain: {line}");
            }
            r["chain"] = json!({ "ok": fine, "says": line });
        }
        if let Some(l) = list {
            let f = File::open(l).with_context(|| format!("can't open {}", l.display()))?;
            let lc = manifest::verify_listing(&c, &mut BufReader::new(f))?;
            this_ok &= lc.ok;
            if !json {
                if lc.ok {
                    println!(
                        "  Listing: good: its {} files give the signed Merkle root",
                        fmt::count(lc.files)
                    );
                } else {
                    println!("  Listing: DOESN'T MATCH:");
                    for p in &lc.problems {
                        println!("    {p}");
                    }
                }
            }
            r["listing"] = json!(lc);
        }
        // a listing that isn't what was signed says nothing about what changed since
        let listing_bad = r["listing"]["ok"] == false;
        if against_index && listing_bad && !json {
            println!(
                "  Compared with the index: not done, since the listing isn't what was signed"
            );
        }
        if against_index
            && !listing_bad
            && let Some(db) = &index
        {
            let mut reader = match list {
                Some(l) => Some(BufReader::new(
                    File::open(l).with_context(|| format!("can't open {}", l.display()))?,
                )),
                None => None,
            };
            let cmp = manifest::compare(
                db,
                doc,
                &c,
                reader.as_mut().map(|r| r as &mut dyn std::io::BufRead),
                if json { 100 } else { 10 },
            )?;
            // a manifest kept here whose scan the index no longer gives as it was signed
            if cmp.from == "index" && cmp.exact && !cmp.reproduced {
                this_ok = false;
            }
            if !json {
                print_comparison(&cmp, c.scan_id, &c.drive);
            }
            r["comparison"] = json!(cmp);
        }
        ok &= this_ok;
        r["ok"] = json!(this_ok);
        report.push(r);
        good.push((i, c));
    }

    // several files: each drive's manifests as one chain
    let mut chains = Vec::new();
    if docs.len() > 1 {
        let mut groups: HashMap<(String, String), Vec<usize>> = HashMap::new();
        for (i, c) in &good {
            groups
                .entry((c.key.clone(), c.drive.clone()))
                .or_default()
                .push(*i);
        }
        let mut groups: Vec<_> = groups.into_iter().collect();
        groups.sort_by(|a, b| a.0.cmp(&b.0));
        for ((_, drive), idx) in groups {
            let set: Vec<Value> = idx.iter().map(|&i| docs[i].clone()).collect();
            let order: Vec<usize> = manifest::chain_order(&set);
            let ordered: Vec<Value> = order.iter().map(|&k| set[k].clone()).collect();
            let problems = manifest::check_chain(&ordered);
            let name = |k: usize| files[idx[order[k]]].display().to_string();
            let said: Vec<String> = problems
                .iter()
                .map(|p| format!("{}: {}", name(p.at), p.what))
                .collect();
            if !json {
                if said.is_empty() {
                    println!(
                        "Chain of {} manifests of {drive}: whole, in this order: {}",
                        ordered.len(),
                        (0..ordered.len()).map(name).collect::<Vec<_>>().join(", ")
                    );
                } else {
                    println!("Chain of {} manifests of {drive}: BROKEN", ordered.len());
                    for s in &said {
                        println!("  {}", s.replace('#', "position "));
                    }
                }
            }
            ok &= said.is_empty();
            chains.push(json!({
                "drive": drive,
                "order": (0..ordered.len()).map(name).collect::<Vec<_>>(),
                "ok": said.is_empty(),
                "problems": said,
            }));
        }
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &json!({ "ok": ok, "manifests": report, "chains": chains })
            )?
        );
    } else if !ok {
        println!("Something doesn't check: see above.");
    }
    Ok(if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
}

fn list_cmd(db_path: &Path, drive: Option<PathBuf>, json: bool) -> Result<ExitCode> {
    let db = Db::open_read_only(db_path).or_else(|_| open(db_path))?;
    let root = drive.as_deref().map(|d| root_of(&db, d)).transpose()?;
    let kept = manifest::list(&db, root.as_ref().map(|r| r.0))?;
    // each drive's chain, as kept
    let mut by_drive: Vec<(i64, Vec<&manifest::Stored>)> = Vec::new();
    for k in &kept {
        match by_drive.iter_mut().find(|(r, _)| *r == k.root_id) {
            Some((_, v)) => v.push(k),
            None => by_drive.push((k.root_id, vec![k])),
        }
    }
    let chains: Vec<Value> = by_drive
        .iter()
        .map(|(root_id, ks)| {
            let docs: Vec<Value> = ks.iter().map(|k| k.doc.clone()).collect();
            let problems: Vec<String> = manifest::check_chain(&docs)
                .into_iter()
                .map(|p| {
                    format!(
                        "manifest {}: {}",
                        ks[p.at].id,
                        p.what.replace('#', "position ")
                    )
                })
                .collect();
            json!({ "root_id": root_id, "drive": ks[0].drive, "manifests": ks.len(),
                    "ok": problems.is_empty(), "problems": problems })
        })
        .collect();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "manifests": kept, "chains": chains }))?
        );
        return Ok(ExitCode::SUCCESS);
    }
    if kept.is_empty() {
        println!(
            "No manifests yet{}: `backdown manifest create {}` signs one (Settings > Attestations has the service sign them on a schedule).",
            if root.is_some() { " of that drive" } else { "" },
            root.as_ref()
                .map_or("/media/old-drive".to_string(), |r| paths::show(&r.1))
        );
        return Ok(ExitCode::SUCCESS);
    }
    println!(
        "{:>4}  {:<20}  {:>6}  {:>9}  {:>9}  {:>6}  {:<19}  DRIVE",
        "ID", "SIGNED", "SCAN", "FILES", "SIZE", "HASHED", "KEY"
    );
    for k in &kept {
        let pct = (k.hashed * 100)
            .checked_div(k.files)
            .map_or_else(|| "-".to_string(), |p| format!("{p}%"));
        println!(
            "{:>4}  {:<20}  {:>6}  {:>9}  {:>9}  {:>6}  {:<19}  {}",
            k.id,
            utc(k.created_at),
            k.scan_id,
            fmt::count(k.files),
            fmt::bytes(k.bytes),
            pct,
            k.fingerprint,
            k.drive
        );
    }
    println!();
    let mut whole = true;
    for c in &chains {
        let n = c["manifests"].as_u64().unwrap_or(0);
        if c["ok"] == true {
            println!(
                "{}: {n} manifest{}, the chain is whole",
                c["drive"].as_str().unwrap_or(""),
                if n == 1 { "" } else { "s" }
            );
        } else {
            whole = false;
            println!(
                "{}: {n} manifests, the chain is BROKEN:",
                c["drive"].as_str().unwrap_or("")
            );
            for p in c["problems"].as_array().into_iter().flatten() {
                println!("  {}", p.as_str().unwrap_or(""));
            }
        }
    }
    Ok(if whole {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
}

fn export(
    db_path: &Path,
    id: i64,
    out: Option<PathBuf>,
    list: Option<PathBuf>,
) -> Result<ExitCode> {
    refuse_existing(&[&out, &list])?;
    let db = Db::open_read_only(db_path)
        .with_context(|| format!("no index at {}", db_path.display()))?;
    let k = manifest::get(&db, id)?;
    if let Some(p) = &list {
        let mut w = BufWriter::new(new_file(p)?);
        let root = manifest::replay(&db, &k, &mut |e| {
            writeln!(w, "{}", e.line()).context("writing the listing")
        })
        .and_then(|root| {
            w.flush()?;
            Ok(root)
        });
        drop(w);
        match root {
            Ok(root) if backdown_core::fmt::hex(&root) == k.merkle => {}
            Ok(_) => {
                let _ = std::fs::remove_file(p);
                bail!(
                    "the index's record of scan {} no longer gives manifest {id}'s Merkle root, so its listing can't be made again: the index changed since it was signed",
                    k.scan_id
                );
            }
            Err(e) => {
                let _ = std::fs::remove_file(p);
                return Err(e);
            }
        }
    }
    let text = serde_json::to_string_pretty(&k.doc)? + "\n";
    match &out {
        Some(p) => new_file(p)?
            .write_all(text.as_bytes())
            .with_context(|| format!("can't write {}", p.display()))?,
        None => print!("{text}"),
    }
    if let Some(p) = &list {
        eprintln!(
            "Listing of manifest {id} ({} files) written to {}: it gives the signed Merkle root",
            fmt::count(k.files),
            p.display()
        );
    }
    Ok(ExitCode::SUCCESS)
}

// ---------- scheduled attestations (the daemon)

/// When the daemon signs a manifest of each drive (the `attest` setting).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attest {
    Off,
    /// once a drive's scan and its hashing finish
    AfterScan,
    Daily,
    Weekly,
}

impl Attest {
    pub fn parse(s: &str) -> Result<Attest> {
        Ok(match s.trim() {
            "" | "off" | "never" => Attest::Off,
            "after-scan" => Attest::AfterScan,
            "daily" => Attest::Daily,
            "weekly" => Attest::Weekly,
            other => bail!("attest is off, after-scan, daily or weekly, not “{other}”"),
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Attest::Off => "off",
            Attest::AfterScan => "after-scan",
            Attest::Daily => "daily",
            Attest::Weekly => "weekly",
        }
    }
}

/// Whether a drive is due a manifest: `latest` is its newest finished scan, `attested` the newest
/// scan a manifest of it covers, `last_at` when its newest manifest was signed. Only a scan that
/// no manifest covers yet is attested; daily and weekly wait that long after the last one.
pub fn due(
    mode: Attest,
    latest: Option<i64>,
    attested: Option<i64>,
    last_at: Option<i64>,
    now: i64,
) -> bool {
    let Some(latest) = latest else {
        return false; // never scanned to the end
    };
    if attested.is_some_and(|a| a >= latest) {
        return false;
    }
    let wait = match mode {
        Attest::Off => return false,
        Attest::AfterScan => 0,
        Attest::Daily => 86_400,
        Attest::Weekly => 7 * 86_400,
    };
    last_at.is_none_or(|t| now - t >= wait)
}

/// Drives whose last attestation failed, and when: tried again after a while, not every check.
static FAILED: Mutex<Option<HashMap<i64, i64>>> = Mutex::new(None);
const RETRY_AFTER: i64 = 6 * 3600;

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Whether scheduled attestations may run here: the setting is on, the licence covers them, and
/// (on the machine-wide service) its key can be kept in its data folder. Why not, if not.
fn may_attest(d: &Daemon) -> Result<Attest, String> {
    let mode = Attest::parse(&crate::settings::load().attest).unwrap_or(Attest::Off);
    if mode == Attest::Off {
        return Err("off in Settings".into());
    }
    if let Err(e) = tiers::require("attestations") {
        return Err(format!("{e:#}"));
    }
    if d.shared
        && let Err(e) = Signer::load_or_create(&d.signing_key)
    {
        return Err(format!(
            "the machine-wide service can't keep its signing key at {}: {e:#}",
            d.signing_key.display()
        ));
    }
    Ok(mode)
}

/// The scheduler's look (with the drives' check): queue a manifest of each drive that's due one.
/// Never while a scan or hashing is waiting or running, so a manifest has the hashes its scan
/// will get.
pub fn schedule(d: &Daemon, db: &Db) {
    static SAID: Mutex<Option<String>> = Mutex::new(None);
    let mode = match may_attest(d) {
        Ok(m) => m,
        Err(why) => {
            // say once in the log why attestations don't run, if they're asked for
            let mut said = SAID.lock().unwrap_or_else(|e| e.into_inner());
            if why != "off in Settings" && said.as_deref() != Some(why.as_str()) {
                eprintln!("attestations: not signing: {why}");
            }
            *said = Some(why);
            return;
        }
    };
    if d.busy_with(&["index", "hash"]) {
        return;
    }
    if let Err(e) = queue_due(d, db, mode) {
        eprintln!("attestations: {e:#}");
    }
}

fn queue_due(d: &Daemon, db: &Db, mode: Attest) -> Result<()> {
    // per drive: the newest scan a manifest covers, and when the newest was signed
    let mut stmt = db
        .conn
        .prepare("SELECT root_id, MAX(scan_id), MAX(created_at) FROM manifests GROUP BY root_id")?;
    let newest: HashMap<i64, (i64, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?
        .collect::<rusqlite::Result<_>>()?;
    let mut stmt = db
        .conn
        .prepare("SELECT id, path, last_scan FROM roots WHERE last_scan IS NOT NULL")?;
    let roots: Vec<(i64, Vec<u8>, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let now = now();
    let failed = FAILED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default();
    for (id, path, last_scan) in roots {
        let n = newest.get(&id);
        if !due(mode, Some(last_scan), n.map(|x| x.0), n.map(|x| x.1), now)
            || failed.get(&id).is_some_and(|t| now - t < RETRY_AFTER)
        {
            continue;
        }
        let key = paths::from_bytes(&path).display().to_string();
        if d.pending("attest", &key) {
            continue;
        }
        let reason = if mode == Attest::AfterScan {
            "after-scan"
        } else {
            "scheduled"
        };
        d.enqueue(
            "attest",
            Some(key),
            reason,
            JobOpts {
                root: Some(id),
                ..JobOpts::default()
            },
        );
    }
    Ok(())
}

/// The worker's `attest` job: sign a manifest of one drive's latest finished scan.
pub fn run_attest(d: &Daemon, db: &Db, job: &Job) -> Result<String> {
    let root_id = job.opts.root.context("a manifest without a drive")?;
    let r = attest(d, db, job, root_id);
    let mut failed = FAILED.lock().unwrap_or_else(|e| e.into_inner());
    let failed = failed.get_or_insert_with(HashMap::new);
    match &r {
        Ok(_) => failed.remove(&root_id),
        Err(_) => failed.insert(root_id, now()),
    };
    r
}

fn attest(d: &Daemon, db: &Db, job: &Job, root_id: i64) -> Result<String> {
    let (last, files): (Option<i64>, i64) = db.conn.query_row(
        "SELECT r.last_scan, COALESCE(s.files, 0) FROM roots r LEFT JOIN scans s ON s.id = r.last_scan WHERE r.id = ?1",
        [root_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let show = job.path.clone().unwrap_or_default();
    // made meanwhile (from the command line, say)
    if let Some(s) = last
        && let Some(k) = manifest::list(db, Some(root_id))?
            .into_iter()
            .find(|k| k.scan_id == s)
    {
        return Ok(format!(
            "{show}: scan {s} has a manifest already (manifest {})",
            k.id
        ));
    }
    let signer = Signer::load_or_create(&d.signing_key).with_context(|| {
        if d.shared {
            format!(
                "the machine-wide service can't keep its signing key at {}",
                d.signing_key.display()
            )
        } else {
            format!("can't use the signing key at {}", d.signing_key.display())
        }
    })?;
    let made = manifest::create(db, &signer, root_id, None, None, &mut |n| {
        d.progress(
            job.id,
            format!("{} of {} files", fmt::count(n), fmt::count(files as u64)),
            Counts {
                stage: "signing",
                files: n,
                total_files: files as u64,
                ..Counts::default()
            },
        );
    })?;
    Ok(format!(
        "manifest {} of {show} at scan {}: {} files, {} ({} hashed){} · key {}",
        made.id,
        made.scan,
        fmt::count(made.files),
        fmt::bytes(made.bytes),
        fmt::count(made.hashed),
        match made.prev_id {
            Some(p) => format!(", follows manifest {p}"),
            None => String::new(),
        },
        signer.fingerprint()
    ))
}

// ---------- the browser: GET /api/manifests, GET /api/manifest (serve.rs), Settings

/// GET /api/manifests[?root=ID]: the manifests kept, newest first.
pub fn list_json(db: &Db, root: Option<&str>) -> Result<Value> {
    let root = root
        .filter(|r| !r.is_empty())
        .map(|r| r.parse::<i64>().context("bad root"))
        .transpose()?;
    let mut kept = manifest::list(db, root)?;
    kept.reverse();
    Ok(json!({ "manifests": kept }))
}

/// GET /api/manifest?id=N: a kept manifest as a file to download (its name, and its bytes).
pub fn document(db: &Db, id: Option<&str>) -> Result<(String, Vec<u8>)> {
    let id: i64 = id.context("missing id")?.parse().context("bad id")?;
    let k = manifest::get(db, id)?;
    let name = Path::new(&k.drive)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "drive".into());
    Ok((
        format!("manifest-{name}-scan{}-{id}.json", k.scan_id),
        (serde_json::to_string_pretty(&k.doc)? + "\n").into_bytes(),
    ))
}

/// The key manifests are signed with here, for Settings (never made by looking).
pub fn key_info(dir: &Path) -> Value {
    let p = dir.join(locations::SIGNING_KEY);
    let key = p
        .exists()
        .then(|| Signer::load_or_create(&p).ok())
        .flatten();
    json!({
        "file": p.display().to_string(),
        "fingerprint": key.as_ref().map(Signer::fingerprint),
        "public_key": key.as_ref().map(Signer::public_hex),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attest_settings() {
        for (s, m) in [
            ("off", Attest::Off),
            ("after-scan", Attest::AfterScan),
            ("daily", Attest::Daily),
            ("weekly", Attest::Weekly),
        ] {
            assert_eq!(Attest::parse(s).unwrap(), m);
            assert_eq!(m.name(), s);
        }
        assert_eq!(Attest::parse("").unwrap(), Attest::Off);
        assert!(Attest::parse("hourly").is_err());
    }

    #[test]
    fn when_a_drive_is_due_a_manifest() {
        let now = 1_800_000_000;
        let (day, week) = (86_400, 7 * 86_400);
        use Attest::*;
        // never when off, or before a drive's first finished scan
        assert!(!due(Off, Some(10), None, None, now));
        for m in [AfterScan, Daily, Weekly] {
            assert!(!due(m, None, None, None, now), "{m:?}: nothing scanned");
            // a drive's first manifest is due straight away
            assert!(due(m, Some(10), None, None, now), "{m:?}: no manifest yet");
            // nothing new since the last one: never due, however long ago it was
            assert!(
                !due(m, Some(10), Some(10), Some(now - 30 * day), now),
                "{m:?}"
            );
        }
        // after each scan: as soon as there's a newer scan
        assert!(due(AfterScan, Some(12), Some(10), Some(now - 60), now));
        // daily and weekly: a newer scan, and the last manifest that old
        assert!(!due(Daily, Some(12), Some(10), Some(now - day + 60), now));
        assert!(due(Daily, Some(12), Some(10), Some(now - day), now));
        assert!(!due(Weekly, Some(12), Some(10), Some(now - 6 * day), now));
        assert!(due(Weekly, Some(12), Some(10), Some(now - week - 1), now));
        // a manifest of an earlier scan made later doesn't hide the newest scan
        assert!(due(AfterScan, Some(12), Some(10), Some(now), now));
    }
}
