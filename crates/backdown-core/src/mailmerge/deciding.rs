//! Deciding what goes where: every source read and its copies matched, each message placed once
//! in the merged Maildir, and the report of what a merge found and did.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::*;

// ---------- deciding

/// One message's place in the result.
#[derive(Debug, Clone)]
pub(super) struct Placement {
    pub(super) key: String,
    pub(super) folder: u32,
    pub(super) occ: u32,
    pub(super) flags: u8,
}

/// What a merge found and did.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SourceReport {
    pub path: String,
    /// "Maildir", "mbox", "mbox folder", "mail store"
    pub kind: String,
    /// its newest message: snapshots are ordered by this
    pub as_of: Option<i64>,
    pub messages: u64,
    /// messages no other snapshot has
    pub only_here: u64,
    pub folders: u64,
    /// folders no other snapshot has
    pub folders_only_here: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Report {
    pub dest: String,
    /// oldest snapshot first
    pub sources: Vec<SourceReport>,
    /// mailboxes merged (1 unless the sources are mail stores)
    pub mailboxes: u64,
    /// distinct messages (a message in two folders counts once)
    pub distinct: u64,
    /// messages in the result (a message kept in two folders counts twice)
    pub messages: u64,
    pub bytes: u64,
    /// copies left out because another copy of the same message is kept
    pub duplicates: u64,
    /// matched by their headers and body, having no Message-ID
    pub without_id: u64,
    /// the same message with different bytes in different snapshots (headers a server or program
    /// added): the newest copy is kept
    pub differing_copies: u64,
    /// in a different folder in an older snapshot: placed where the newest one has it
    pub moved: u64,
    /// whose flags were combined from more than one copy
    pub flags_merged: u64,
    /// in the result, per folder (mailbox key and folder name)
    pub folders: Vec<(String, u64)>,
    pub written: u64,
    pub written_bytes: u64,
    /// found already written (an earlier, interrupted run)
    pub resumed: u64,
    pub failed: u64,
    pub unreadable: u64,
    pub problems: Vec<String>,
    pub dry_run: bool,
}

/// Every copy read, what goes where decided.
pub(super) struct Merge {
    pub(super) sources: Vec<SourceAt>,
    pub(super) cat: Catalog,
    pub(super) placements: Vec<Placement>,
    pub(super) report: Report,
}

/// `dest` made absolute through whatever of it exists (so it compares with the sources).
pub(super) fn resolve_dest(dest: &Path) -> Result<PathBuf> {
    if !dest.is_absolute() {
        bail!("the merged mailbox must be a full path");
    }
    let mut existing = dest;
    let mut rest = Vec::new();
    while !existing.exists() {
        rest.push(existing.file_name().context("no such folder")?);
        existing = existing.parent().context("no such folder")?;
    }
    let mut p = dunce::canonicalize(existing)?;
    for c in rest.iter().rev() {
        p.push(c);
    }
    Ok(p)
}

pub(super) fn load_sources(sources: &[PathBuf], dest: &Path) -> Result<Vec<SourceAt>> {
    if sources.is_empty() {
        bail!("nothing to merge");
    }
    let mut out = Vec::new();
    for s in sources {
        let s = dunce::canonicalize(s).with_context(|| format!("can't open {}", s.display()))?;
        if dest.starts_with(&s) || s.starts_with(dest) {
            bail!(
                "the merged mailbox can't be inside a source, or the other way round ({})",
                s.display()
            );
        }
        if out.iter().any(|x: &SourceAt| x.path == s) {
            bail!("{} is given twice", s.display());
        }
        out.push(discover(&s)?);
    }
    let stores = out.iter().filter(|s| s.kind == "mail store").count();
    if stores != 0 && stores != out.len() {
        bail!(
            "give mailboxes, or mail stores, not both: {} is a store of {} mailboxes",
            out.iter()
                .find(|s| s.kind == "mail store")
                .map(|s| s.path.display().to_string())
                .unwrap_or_default(),
            out.iter()
                .find(|s| s.kind == "mail store")
                .map_or(0, |s| s.mailboxes.len())
        );
    }
    if out.len() > u16::MAX as usize {
        bail!("too many sources");
    }
    Ok(out)
}

pub(super) fn plan(
    sources: &[PathBuf],
    dest: &Path,
    stop: &AtomicBool,
    progress: &mut dyn FnMut(&str, u64, u64),
) -> Result<Merge> {
    let sources = load_sources(sources, dest)?;
    let mut cat = Catalog {
        as_of: vec![None; sources.len()],
        messages: vec![0; sources.len()],
        has_folder: vec![BTreeSet::new(); sources.len()],
        ..Catalog::default()
    };
    let (mut n, mut b, mut last) = (0u64, 0u64, Instant::now());
    for (si, s) in sources.iter().enumerate() {
        for mb in &s.mailboxes {
            for f in &mb.folders {
                if stop.load(Ordering::Relaxed) {
                    bail!("stopped");
                }
                let folder = cat.folder_id(&f.name);
                cat.has_folder[si].insert((mb.key.clone(), f.name.clone()));
                let what = s.path.display().to_string();
                let mut tick = |dn: u64, db: u64| {
                    n += dn;
                    b += db;
                    if last.elapsed().as_millis() >= 250 {
                        last = Instant::now();
                        progress(&what, n, b);
                    }
                };
                match &f.loc {
                    Loc::Maildir(d) => read_maildir_folder(
                        &mut cat, si as u16, &mb.key, folder, d, stop, &mut tick,
                    )?,
                    Loc::Mbox(p) => {
                        read_mbox_folder(&mut cat, si as u16, &mb.key, folder, p, stop, &mut tick)?
                    }
                }
            }
        }
    }
    // snapshots by their newest message (ties: the order given)
    let mut order: Vec<usize> = (0..sources.len()).collect();
    order.sort_by_key(|&i| (cat.as_of[i].unwrap_or(i64::MIN), i));
    let mut rank = vec![0; sources.len()];
    for (r, &i) in order.iter().enumerate() {
        rank[i] = r;
    }
    let mut report = Report {
        dest: dest.display().to_string(),
        unreadable: cat.unreadable,
        problems: cat.problems.clone(),
        ..Report::default()
    };
    let mut only_here = vec![0u64; sources.len()];
    let mut placements = Vec::new();
    let mut per_folder: BTreeMap<(String, String), u64> = BTreeMap::new();
    let mut mailboxes: BTreeSet<&str> = BTreeSet::new();
    for ((key, id), occs) in &cat.ids {
        mailboxes.insert(key);
        report.distinct += 1;
        if cat.without_id.contains(&(key.clone(), *id)) {
            report.without_id += 1;
        }
        let copies: Vec<&Occ> = occs.iter().map(|&i| &cat.occs[i as usize]).collect();
        let srcs: BTreeSet<u16> = copies.iter().map(|o| o.src).collect();
        if srcs.len() == 1 {
            only_here[*srcs.iter().next().expect("one") as usize] += 1;
        }
        let newest = copies
            .iter()
            .map(|o| rank[o.src as usize])
            .max()
            .expect("at least one copy");
        let folders: BTreeSet<u32> = copies
            .iter()
            .filter(|o| rank[o.src as usize] == newest)
            .map(|o| o.folder)
            .collect();
        if copies.iter().any(|o| !folders.contains(&o.folder)) {
            report.moved += 1;
        }
        let union = copies.iter().fold(0u8, |f, o| f | (o.flags & UNION));
        for &f in &folders {
            // the newest snapshot's copy in this folder (the first if it has two)
            let chosen = occs
                .iter()
                .enumerate()
                .filter(|(_, i)| cat.occs[**i as usize].folder == f)
                .max_by_key(|(n, i)| {
                    (
                        rank[cat.occs[**i as usize].src as usize],
                        std::cmp::Reverse(*n),
                    )
                })
                .map(|(_, i)| *i)
                .expect("a copy in this folder");
            let c = &cat.occs[chosen as usize];
            if copies.iter().any(|o| o.folder == f && o.sha != c.sha) {
                report.differing_copies += 1;
            }
            let flags = union | (c.flags & (DRAFT | TRASHED));
            if flags != c.flags {
                report.flags_merged += 1;
            }
            report.messages += 1;
            report.bytes += c.size;
            *per_folder
                .entry((key.clone(), cat.folders[f as usize].clone()))
                .or_default() += 1;
            placements.push(Placement {
                key: key.clone(),
                folder: f,
                occ: chosen,
                flags,
            });
        }
    }
    report.mailboxes = mailboxes.len() as u64;
    report.duplicates = (cat.occs.len() as u64).saturating_sub(report.messages);
    report.folders = per_folder
        .into_iter()
        .map(|((k, f), n)| (if k.is_empty() { f } else { format!("{k}: {f}") }, n))
        .collect();
    for &i in &order {
        let s = &sources[i];
        let others: BTreeSet<&(String, String)> = cat
            .has_folder
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .flat_map(|(_, f)| f.iter())
            .collect();
        report.sources.push(SourceReport {
            path: s.path.display().to_string(),
            kind: s.kind.to_string(),
            as_of: cat.as_of[i],
            messages: cat.messages[i],
            only_here: only_here[i],
            folders: cat.has_folder[i].len() as u64,
            folders_only_here: cat.has_folder[i]
                .iter()
                .filter(|f| !others.contains(f))
                .map(|(k, f)| {
                    if k.is_empty() {
                        f.clone()
                    } else {
                        format!("{k}: {f}")
                    }
                })
                .collect(),
        });
    }
    // written in a stable order: mailbox, folder, delivery time
    placements.sort_by(|a, b| {
        (
            &a.key,
            &cat.folders[a.folder as usize],
            cat.occs[a.occ as usize].time,
        )
            .cmp(&(
                &b.key,
                &cat.folders[b.folder as usize],
                cat.occs[b.occ as usize].time,
            ))
    });
    Ok(Merge {
        sources,
        cat,
        placements,
        report,
    })
}
