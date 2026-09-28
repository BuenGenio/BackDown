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
    pub json: bool,
}

pub fn make_plan(db: &Db, a: PlanArgs) -> Result<ExitCode> {
    let schema = match a.schema {
        Some(p) => plan::parse_schema(
            &std::fs::read_to_string(p).with_context(|| format!("can't read {}", p.display()))?,
        )?,
        None => plan::Schema::default(),
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
        .unwrap_or_else(|| "built-in".into());
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
    for action in ["copy", "copy-unique", "skip"] {
        let items: Vec<&plan::Item> = p.items.iter().filter(|i| i.action == action).collect();
        if items.is_empty() {
            continue;
        }
        println!(
            "{}",
            match action {
                "copy" => "Copy",
                "copy-unique" => "Copy only what's unique",
                _ => "Not copied",
            }
        );
        for it in items {
            println!("  {:>9}  {}  ({})", fmt::bytes(it.bytes), it.what, it.drive);
            println!("             {}", it.source);
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
