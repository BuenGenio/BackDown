//! `backdown plan`, `backdown plans`, `backdown apply`: file the old drives into one archive.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use backdown_core::{Db, apply, fmt, overlap, paths, plan, recognise};

use crate::{clear, done, live};

pub struct PlanArgs<'a> {
    pub to: &'a Path,
    pub from: &'a [PathBuf],
    pub schema: Option<&'a Path>,
    /// merge overlapping mailboxes by Message-ID instead of copying the twin's extra files
    pub merge_mail: bool,
    pub json: bool,
}

pub fn make_plan(db: &Db, a: PlanArgs) -> Result<ExitCode> {
    let (schema, custom) = match a.schema {
        Some(p) => (
            plan::parse_schema(
                &std::fs::read_to_string(p)
                    .with_context(|| format!("can't read {}", p.display()))?,
            )?,
            false,
        ),
        // yours from Settings, if you've edited it there; else the built-in one
        None => crate::settings::schema()?,
    };
    let target = if a.to.is_absolute() {
        a.to.to_path_buf()
    } else {
        std::env::current_dir()?.join(a.to)
    };
    let target = dunce::canonicalize(&target).unwrap_or(target);
    let ids = recognise::root_ids(db)?;
    let only: Option<Vec<i64>> = if a.from.is_empty() {
        None
    } else {
        let mut v = Vec::new();
        for id in &ids {
            let p: Vec<u8> =
                db.conn
                    .query_row("SELECT path FROM roots WHERE id = ?1", [id], |r| r.get(0))?;
            let root = paths::from_bytes(&p);
            if a.from
                .iter()
                .any(|f| dunce::canonicalize(f).unwrap_or_else(|_| f.clone()) == root)
            {
                v.push(*id);
            }
        }
        if v.is_empty() {
            bail!("none of --from is an indexed drive (see `backdown scans`)");
        }
        Some(v)
    };
    let rec = crate::insight::recognised(db, &ids, false, false)?;
    let list: Vec<_> = rec.into_iter().map(|(r, _)| r).collect();
    let ov = overlap::overlaps(db, &list, &mut |s| live(s))?;
    clear();
    let mut p = plan::make(db, &target, only.as_deref(), &schema, &list, &ov)?;
    p.schema = a
        .schema
        .map(|s| s.display().to_string())
        .unwrap_or_else(|| {
            if custom {
                "yours (Settings)".into()
            } else {
                "built-in".into()
            }
        });
    let merged = if a.merge_mail {
        plan::merge_all_mail(&mut p)
    } else {
        0
    };
    let id = plan::save(db, &p)?;
    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "id": id, "plan": p }))?
        );
        return Ok(ExitCode::SUCCESS);
    }
    println!(
        "Plan {id}: file {} drive{} into {}\n",
        p.drives.len(),
        if p.drives.len() == 1 { "" } else { "s" },
        p.target
    );
    for action in ["copy", "copy-unique", "merge", "skip"] {
        let items: Vec<&plan::Item> = p.items.iter().filter(|i| i.action == action).collect();
        if items.is_empty() {
            continue;
        }
        println!(
            "{}",
            match action {
                "copy" => "Copy",
                "copy-unique" => "Copy only what's unique",
                "merge" => "Merge snapshots of a mailbox (by Message-ID)",
                _ => "Not copied",
            }
        );
        for it in items {
            println!("  {:>9}  {}  ({})", fmt::bytes(it.bytes), it.what, it.drive);
            println!("             {}", it.source);
            for m in &it.merge {
                println!("           + {m}");
            }
            if action != "skip" {
                println!(
                    "           → {}",
                    Path::new(&p.target).join(&it.dest).display()
                );
            }
            println!("             {}", it.why);
        }
        println!();
    }
    println!("Drives");
    for d in &p.drives {
        println!(
            "  {:<24} {:>9}: copy {}, already archived {}{}",
            d.drive,
            fmt::bytes(d.bytes),
            fmt::bytes(d.copy),
            fmt::bytes(d.already),
            if d.covered {
                "; every file will have a copy".to_string()
            } else {
                format!("; {} stays only here", fmt::bytes(d.not_copied))
            }
        );
    }
    println!(
        "\nIn all: {} in {} files.",
        fmt::bytes(p.copy_bytes),
        fmt::count(p.copy_files)
    );
    if merged > 0 {
        println!(
            "{merged} mailbox{} merged with {} twin by Message-ID instead of copied beside it.",
            if merged == 1 { "" } else { "es" },
            if merged == 1 { "its" } else { "their" }
        );
    } else if (0..p.items.len()).any(|i| plan::merge_target(&p, i).is_some()) {
        println!(
            "Some mailboxes are copies of others: `backdown plan … --merge-mail` merges them by Message-ID instead."
        );
    }
    println!(
        "Next: `backdown apply {id}` shows exactly what would happen; `backdown apply {id} --go` does it."
    );
    Ok(ExitCode::SUCCESS)
}

pub fn list_plans(db: &Db) -> Result<ExitCode> {
    let plans = apply::plans(db)?;
    if plans.is_empty() {
        println!("No plans yet: backdown plan --to <archive folder>");
    }
    for (id, at, target, state) in plans {
        println!("  {id:>4}  {}  {state:<8}  {target}", crate::utc(at));
    }
    Ok(ExitCode::SUCCESS)
}

pub fn run_apply(db: &mut Db, id: i64, go: bool) -> Result<ExitCode> {
    let (p, state) = plan::load(db, id)?;
    if go {
        eprintln!(
            "applying plan {id} (state: {state}) into {}: copies are verified by reading them back; nothing is deleted",
            p.target
        );
    }
    let st = apply::apply(db, id, !go, &mut |pr| {
        live(&format!(
            "{}/{} {} · {} files, {} of {}",
            pr.item,
            pr.items,
            pr.what,
            fmt::count(pr.files),
            fmt::bytes(pr.bytes),
            fmt::bytes(pr.total_bytes)
        ))
    })?;
    clear();
    if !go {
        println!(
            "Dry run of plan {id}: {} files ({}) to copy into {}.",
            fmt::count(st.would_copy),
            fmt::bytes(st.would_copy_bytes),
            p.target
        );
        if st.resumed > 0 {
            println!(
                "{} files are already done (from an earlier run).",
                fmt::count(st.resumed)
            );
        }
        if st.duplicates > 0 {
            println!(
                "{} files are already in the archive and won't be copied again.",
                fmt::count(st.duplicates)
            );
        }
        if st.would_merge > 0 {
            println!(
                "{} mailbox{} will be merged from its snapshots (`backdown mail merge … --dry-run` shows the messages).",
                fmt::count(st.would_merge),
                if st.would_merge == 1 { "" } else { "es" }
            );
        }
        if st.conflicts > 0 {
            println!(
                "{} destinations already exist: they'll be compared, and left alone if they differ.",
                fmt::count(st.conflicts)
            );
        }
        println!("Nothing was written. Run `backdown apply {id} --go` to copy.");
        return Ok(ExitCode::SUCCESS);
    }
    done(&format!(
        "copied and verified {} files ({}); {} already there, {} duplicates, {} resumed",
        fmt::count(st.copied),
        fmt::bytes(st.copied_bytes),
        fmt::count(st.already_there),
        fmt::count(st.duplicates),
        fmt::count(st.resumed)
    ));
    if st.cloned + st.kernel_copied > 0 {
        eprintln!(
            "  {} of them cloned (sharing the source's blocks), {} copied by the kernel",
            fmt::count(st.cloned),
            fmt::count(st.kernel_copied)
        );
    }
    if st.merged > 0 {
        eprintln!(
            "  {} messages in merged mailboxes, each written once and read back",
            fmt::count(st.merged)
        );
    }
    if st.skipped_links > 0 {
        eprintln!(
            "  {} symbolic links were not copied",
            fmt::count(st.skipped_links)
        );
    }
    for pr in &st.first_problems {
        eprintln!("  {pr}");
    }
    let problems = st.conflicts + st.failed;
    if problems > 0 {
        eprintln!(
            "{} files need a look ({} conflicts, {} failed); run the same command again after fixing them.",
            problems, st.conflicts, st.failed
        );
    }
    eprintln!(
        "Next: index the archive so its copies count as proof (`backdown index <the archive's drive>`), then `backdown check <old drive>`."
    );
    Ok(if problems > 0 {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    })
}

pub fn mirror_add(db: &Db, source: &Path, target: &Path, verify_every: i64) -> Result<ExitCode> {
    let target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        std::env::current_dir()?.join(target)
    };
    let id = backdown_core::mirror::add(db, source, &target, verify_every)?;
    println!("Mirror {id}: {} → {}", source.display(), target.display());
    println!(
        "Copy it now with `backdown mirror sync {id}`; the BackDown service syncs daily and re-reads a sample every {verify_every} days."
    );
    Ok(ExitCode::SUCCESS)
}

pub fn mirror_list(db: &Db) -> Result<ExitCode> {
    let ms = backdown_core::mirror::list(db)?;
    if ms.is_empty() {
        println!("No mirrors: backdown mirror add <folder> <mirror folder>");
    }
    for m in ms {
        println!("  {:>3}  {} → {}", m.id, m.source, m.target);
        println!(
            "       synced {}{}",
            m.last_sync
                .map(crate::utc)
                .unwrap_or_else(|| "never".into()),
            m.last_sync_summary
                .map(|s| format!(": {s}"))
                .unwrap_or_default()
        );
        println!(
            "       verified {}{}{}",
            m.last_verify
                .map(crate::utc)
                .unwrap_or_else(|| "never".into()),
            m.last_verify_summary
                .map(|s| format!(": {s}"))
                .unwrap_or_default(),
            match m.last_verify_ok {
                Some(false) => "  ← needs attention",
                _ => "",
            }
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn mirror_ids(db: &Db, id: Option<i64>) -> Result<Vec<i64>> {
    Ok(match id {
        Some(i) => vec![i],
        None => backdown_core::mirror::list(db)?
            .into_iter()
            .map(|m| m.id)
            .collect(),
    })
}

pub fn mirror_sync(db: &Db, id: Option<i64>) -> Result<ExitCode> {
    let mut code = ExitCode::SUCCESS;
    for id in mirror_ids(db, id)? {
        let st = backdown_core::mirror::sync(db, id, &mut |n, b| {
            live(&format!(
                "mirror {id}: {} files, {}",
                fmt::count(n),
                fmt::bytes(b)
            ))
        })?;
        clear();
        done(&format!(
            "mirror {id}: {} copied ({}), {} updated (old copies kept in {}), {} unchanged{}",
            fmt::count(st.copied),
            fmt::bytes(st.copied_bytes),
            fmt::count(st.updated),
            backdown_core::mirror::VERSIONS,
            fmt::count(st.unchanged),
            if st.extra > 0 {
                format!(", {} only on the mirror (kept)", fmt::count(st.extra))
            } else {
                String::new()
            }
        ));
        for p in &st.problems {
            eprintln!("  {p}");
        }
        if st.failed > 0 {
            code = ExitCode::from(2);
        }
    }
    Ok(code)
}

pub fn mirror_verify(db: &Db, id: Option<i64>, percent: u8) -> Result<ExitCode> {
    let mut code = ExitCode::SUCCESS;
    for id in mirror_ids(db, id)? {
        let st = backdown_core::mirror::verify(db, id, percent, &mut |n, b| {
            live(&format!(
                "mirror {id}: re-read {} files, {}",
                fmt::count(n),
                fmt::bytes(b)
            ))
        })?;
        clear();
        let bad = st.missing + st.differs + st.unreadable;
        done(&format!(
            "mirror {id}: re-read {} files ({}): {}",
            fmt::count(st.checked),
            fmt::bytes(st.checked_bytes),
            if bad == 0 {
                "all identical".into()
            } else {
                format!(
                    "{} missing, {} different, {} unreadable",
                    st.missing, st.differs, st.unreadable
                )
            }
        ));
        for p in &st.problems {
            eprintln!("  {p}");
        }
        if bad > 0 {
            code = ExitCode::from(2);
        }
    }
    Ok(code)
}

/// `backdown wipe`: what wiping would do, or (with --go and the drive names typed) do it.
#[allow(clippy::too_many_arguments)]
pub fn wipe(
    db: &mut Db,
    db_path: &Path,
    targets: &[PathBuf],
    move_to: Option<&Path>,
    trust_index: bool,
    go: bool,
    confirm: Option<&str>,
    use_daemon: bool,
) -> Result<ExitCode> {
    use backdown_core::wipe;
    let targets: Vec<PathBuf> = targets
        .iter()
        .map(|p| dunce::canonicalize(p).unwrap_or_else(|_| p.clone()))
        .collect();
    let move_to = move_to.map(|m| {
        if m.is_absolute() {
            m.to_path_buf()
        } else {
            std::env::current_dir().unwrap_or_default().join(m)
        }
    });
    let a = wipe::assess(db, &targets, Some(db_path))?;
    for it in &a.items {
        println!("{}", it.path);
        if let Some(why) = &it.refused {
            println!("  can't be wiped: {why}");
            continue;
        }
        println!("  {} files, {}", fmt::count(it.files), fmt::bytes(it.bytes));
        println!(
            "  {} has an identical copy elsewhere{}",
            fmt::bytes(it.kept_bytes),
            if it.copies.is_empty() {
                String::new()
            } else {
                format!(
                    ": {}",
                    it.copies
                        .iter()
                        .map(|c| format!("{} in {}", fmt::bytes(c.bytes), c.path))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        );
        if it.only_files > 0 {
            println!(
                "  {} ({} files) exists only here: {}",
                fmt::bytes(it.only_bytes),
                fmt::count(it.only_files),
                match &move_to {
                    Some(m) => format!("moved to {} first", m.display()),
                    None => "left in place (--move-to <folder> moves it first)".into(),
                }
            );
        }
        if it.unreadable_files > 0 {
            println!(
                "  {} files couldn't be read at the last scan: left in place",
                fmt::count(it.unreadable_files)
            );
        }
    }
    if a.items.iter().any(|i| i.refused.is_some()) {
        bail!("some of that can't be wiped (see above)");
    }
    if !go {
        println!(
            "\nNothing was changed. To wipe: backdown wipe {}{} --go --confirm \"{}\"",
            targets
                .iter()
                .map(|t| format!("\"{}\"", t.display()))
                .collect::<Vec<_>>()
                .join(" "),
            move_to
                .as_ref()
                .map(|m| format!(" --move-to \"{}\"", m.display()))
                .unwrap_or_default(),
            a.confirm
        );
        println!(
            "Every file is re-read before it's deleted, and so is its copy (--trust-index skips that)."
        );
        return Ok(ExitCode::SUCCESS);
    }
    if confirm != Some(a.confirm.as_str()) {
        bail!(
            "type the drive name{} to confirm: --confirm \"{}\"",
            if a.confirm.contains(',') { "s" } else { "" },
            a.confirm
        );
    }
    let id = wipe::record(db, &targets, move_to.as_deref())?;
    // a running daemon owns the index: it does the wiping, and rescans afterwards
    // --local: here, even when a daemon is running
    if let Some(mut c) = use_daemon
        .then(|| crate::client::Client::connect(Some(db_path)))
        .flatten()
    {
        let r = c.request(&serde_json::json!({ "cmd": "enqueue", "kind": "wipe", "wipe": id, "trust_index": trust_index }))?;
        let j = c.follow(r["job"].as_u64().unwrap_or(0), |j| {
            live(&format!("wiping: {}", j["progress"].as_str().unwrap_or("")))
        })?;
        clear();
        done(j["summary"].as_str().unwrap_or(""));
        return Ok(ExitCode::SUCCESS);
    }
    let st = wipe::wipe(db, id, trust_index, Some(db_path), &mut |n, b| {
        live(&format!(
            "wiping: {} files, {}",
            fmt::count(n),
            fmt::bytes(b)
        ))
    })?;
    clear();
    done(&format!(
        "deleted {} files ({}), {} moved first; left {} changed, {} with no copy, {} unreadable",
        fmt::count(st.deleted),
        fmt::bytes(st.deleted_bytes),
        fmt::count(st.moved),
        fmt::count(st.left_changed),
        fmt::count(st.left_no_copy),
        fmt::count(st.left_unreadable)
    ));
    for p in &st.problems {
        eprintln!("  {p}");
    }
    eprintln!("Index the drive again so BackDown sees what's gone: backdown index <drive>");
    Ok(if st.failed > 0 {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    })
}
