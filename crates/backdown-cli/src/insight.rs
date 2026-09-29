//! `backdown entities`, `backdown overlaps`, `backdown report`: what things are, how much of each
//! place exists elsewhere, and the whole picture as a document.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use backdown_core::recognise::{self, RootEntities};
use backdown_core::{Db, fmt, overlap, paths, summary};

use crate::{clear, live};

/// Roots to look at: all of them, or the ones containing (or inside) the given paths.
fn roots(db: &Db, only: &[PathBuf]) -> Result<Vec<i64>> {
    let all = recognise::root_ids(db)?;
    if only.is_empty() {
        return Ok(all);
    }
    let mut out = Vec::new();
    for id in all {
        let path: Vec<u8> =
            db.conn
                .query_row("SELECT path FROM roots WHERE id = ?1", [id], |r| r.get(0))?;
        let root = paths::from_bytes(&path);
        if only.iter().any(|p| {
            let p = paths::canonical(p).unwrap_or_else(|_| p.clone());
            backdown_core::names::within(&p, &root) || backdown_core::names::within(&root, &p)
        }) {
            out.push(id);
        }
    }
    if out.is_empty() {
        bail!("none of those paths is an indexed drive or inside one");
    }
    Ok(out)
}

/// Recognition for each root: the stored result when it describes the latest scan (or always,
/// with `stale_ok`), otherwise computed now and stored.
pub fn recognised(
    db: &Db,
    ids: &[i64],
    refresh: bool,
    stale_ok: bool,
) -> Result<Vec<(RootEntities, bool)>> {
    let mut out = Vec::new();
    for &id in ids {
        match recognise::load(db, id)? {
            Some((r, fresh)) if !refresh && (fresh || stale_ok) => out.push((r, fresh)),
            _ => {
                let r = recognise::recognise(db, id, true, &mut |s| {
                    live(&format!("recognising: {s}"))
                })?;
                clear();
                recognise::store(db, &r).context("saving what was recognised")?;
                out.push((r, true));
            }
        }
    }
    // where things came from is a Pro feature (tiers.rs); stored results keep it either way
    crate::tiers::strip_provenance(out.iter_mut().map(|(r, _)| r));
    Ok(out)
}

/// "236 MB: npm packages 200 MB (12 folders), thumbnail cache 36 MB"
fn junk_line(junk: &[recognise::Junk]) -> String {
    let mut by: Vec<(&str, u64, usize)> = Vec::new();
    for j in junk {
        match by.iter_mut().find(|(w, _, _)| *w == j.why) {
            Some(x) => {
                x.1 += j.bytes;
                x.2 += 1;
            }
            None => by.push((&j.why, j.bytes, 1)),
        }
    }
    by.sort_by_key(|x| std::cmp::Reverse(x.1));
    format!(
        "{}: {}",
        fmt::bytes(junk.iter().map(|j| j.bytes).sum()),
        by.iter()
            .take(4)
            .map(|(w, b, n)| format!(
                "{w} {}{}",
                fmt::bytes(*b),
                if *n > 1 {
                    format!(" ({n} folders)")
                } else {
                    String::new()
                }
            ))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn years(e: &recognise::Entity) -> String {
    let y = |t: i64| crate::utc(t)[..4].to_string();
    match (e.oldest, e.newest) {
        (Some(a), Some(b)) if y(a) == y(b) => y(a),
        (Some(a), Some(b)) => format!("{}–{}", y(a), y(b)),
        _ => String::new(),
    }
}

pub fn entities(
    db: &Db,
    only: &[PathBuf],
    refresh: bool,
    kind: Option<&str>,
    json: bool,
) -> Result<ExitCode> {
    let ids = roots(db, only)?;
    let rec = recognised(db, &ids, refresh, false)?;
    if json {
        let list: Vec<&RootEntities> = rec.iter().map(|(r, _)| r).collect();
        println!("{}", serde_json::to_string_pretty(&list)?);
        return Ok(ExitCode::SUCCESS);
    }
    for (r, _) in &rec {
        let shown: Vec<&recognise::Entity> = r
            .entities
            .iter()
            .filter(|e| kind.is_none_or(|k| e.kind == k))
            .collect();
        println!(
            "{}  ({} thing{} recognised{})",
            r.root,
            shown.len(),
            if shown.len() == 1 { "" } else { "s" },
            if r.read_drive {
                ""
            } else {
                "; drive not connected, so no provenance"
            }
        );
        let mut last_kind = "";
        for e in shown {
            if e.kind != last_kind {
                println!("\n  {}", e.label);
                last_kind = &e.kind;
            }
            let rel = Path::new(&e.path)
                .strip_prefix(&r.root)
                .map_or(e.path.clone(), |p| p.display().to_string());
            println!(
                "    {:<44} {:>9}  {:<9}  {}",
                e.title,
                fmt::bytes(e.bytes),
                years(e),
                if rel.is_empty() { ".".into() } else { rel }
            );
            for p in &e.provenance {
                println!("      ↳ {p}");
            }
            if !e.junk.is_empty() {
                println!("      ↳ regenerable: {}", junk_line(&e.junk));
            }
            if e.low_value {
                println!("      ↳ probably not worth keeping as a unit");
            }
        }
        if !r.junk.is_empty() {
            println!(
                "\n  Regenerable elsewhere on the drive: {}",
                junk_line(&r.junk)
            );
        }
        println!();
    }
    Ok(ExitCode::SUCCESS)
}

pub fn overlaps(db: &Db, min_share: f64, json: bool) -> Result<ExitCode> {
    let ids = recognise::root_ids(db)?;
    let rec = recognised(db, &ids, false, true)?;
    let list: Vec<RootEntities> = rec.into_iter().map(|(r, _)| r).collect();
    let ov = overlap::overlaps(db, &list, &mut |s| live(s))?;
    clear();
    if json {
        println!("{}", serde_json::to_string_pretty(&ov)?);
        return Ok(ExitCode::SUCCESS);
    }
    let pct = |o: &overlap::Overlap| format!("{:>3.0}%", (o.share() * 100.0).floor());
    println!("Drives: how much of each exists on the others\n");
    for o in ov.iter().filter(|o| o.unit.level == "drive") {
        println!(
            "  {:<28} {:>9}  {} elsewhere{}",
            o.unit.label,
            fmt::bytes(o.unit.bytes),
            pct(o),
            if o.containers.is_empty() {
                String::new()
            } else {
                format!(
                    ": {}",
                    o.containers
                        .iter()
                        .map(|c| format!(
                            "{:.0}% on {}",
                            100.0 * c.bytes as f64 / o.unit.bytes.max(1) as f64,
                            c.label
                        ))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        );
    }
    for (level, title) in [
        ("folder", "Top-level folders"),
        ("entity", "Recognised things"),
    ] {
        let mut rows: Vec<&overlap::Overlap> = ov
            .iter()
            .filter(|o| o.unit.level == level && o.unit.bytes > 0 && o.share() >= min_share)
            .collect();
        if rows.is_empty() {
            continue;
        }
        rows.sort_by_key(|o| std::cmp::Reverse(o.unit.bytes));
        println!(
            "\n{title}{}\n",
            if min_share > 0.0 {
                format!(" (at least {:.0}% elsewhere)", min_share * 100.0)
            } else {
                String::new()
            }
        );
        for o in rows.iter().take(40) {
            println!(
                "  {:>9}  {} elsewhere  {}{}",
                fmt::bytes(o.unit.bytes),
                pct(o),
                o.unit.path,
                o.containers
                    .first()
                    .map(|c| format!("\n{:>24}mostly in {}", "", c.path))
                    .unwrap_or_default()
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

pub fn report(db: &Db, html: bool, out: Option<&Path>, refresh: bool) -> Result<ExitCode> {
    let ids = recognise::root_ids(db)?;
    if ids.is_empty() {
        bail!("nothing is indexed yet: backdown index <drive>");
    }
    let rec = recognised(db, &ids, refresh, false)?;
    let list: Vec<RootEntities> = rec.iter().map(|(r, _)| r.clone()).collect();
    let ov = overlap::overlaps(db, &list, &mut |s| live(s))?;
    let s = summary::build(db, &rec, &ov, &mut |s| live(s))?;
    clear();
    let text = if html {
        summary::html(&s)
    } else {
        summary::markdown(&s)
    };
    match out {
        Some(p) => {
            std::fs::write(p, text).with_context(|| format!("can't write {}", p.display()))?;
            eprintln!("wrote {}", p.display());
        }
        None => print!("{text}"),
    }
    Ok(ExitCode::SUCCESS)
}
