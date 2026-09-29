//! The report: every drive, what's on it and what those things are, how much of it exists
//! elsewhere, and whether it's safe to wipe — the REPORT.md that was once written by hand after a
//! night of scripts, generated from the index (APP-PLAN §4 `report/`). Markdown for reading and
//! pasting, HTML for a browser; both from the same `Summary`.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::overlap::{Container, Overlap};
use crate::recognise::{Entity, Junk, RootEntities};
use crate::report::{self, Dupes, UniqueDir};
use crate::{Db, fmt, paths};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    pub generated_at: i64,
    pub drives: Vec<DriveSummary>,
    pub files: u64,
    pub bytes: u64,
    pub hashed_files: u64,
    pub dupes: Dupes,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriveSummary {
    pub name: String,
    pub path: String,
    pub key: String,
    pub files: u64,
    pub bytes: u64,
    pub scanned_at: Option<i64>,
    pub connected: bool,
    pub hashed_files: u64,
    pub elsewhere: u64,
    pub containers: Vec<Container>,
    pub verdict: Verdict,
    pub entities: Vec<Entity>,
    /// recognition is older than the latest scan (or missing)
    pub entities_stale: bool,
    pub junk: Vec<Junk>,
    pub folders: Vec<Overlap>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    pub safe: bool,
    pub unique_files: u64,
    pub unique_bytes: u64,
    pub unreadable_files: u64,
    pub top_unique: Vec<UniqueDir>,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Put the report together from the index, the stored recognition (per root, and whether it's
/// fresh) and the overlaps.
pub fn build(
    db: &Db,
    recognised: &[(RootEntities, bool)],
    overlaps: &[Overlap],
    progress: &mut dyn FnMut(&str),
) -> Result<Summary> {
    let mut drives = Vec::new();
    let mut stmt = db.conn.prepare(
        "SELECT r.id, r.path, s.files, s.bytes, s.finished_at,
                (SELECT COUNT(*) FROM files f WHERE f.root_id = r.id AND f.current = 1 AND f.sha256 IS NOT NULL)
         FROM roots r JOIN scans s ON s.id = r.last_scan ORDER BY r.path",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, Vec<u8>>(1)?,
            r.get::<_, i64>(2)? as u64,
            r.get::<_, i64>(3)? as u64,
            r.get::<_, Option<i64>>(4)?,
            r.get::<_, i64>(5)? as u64,
        ))
    })?;
    let rows: Vec<_> = rows.collect::<rusqlite::Result<_>>()?;
    for (id, path, files, bytes, scanned_at, hashed) in rows {
        let p = paths::from_bytes(&path);
        let shown = p.display().to_string();
        progress(&format!("checking {shown}"));
        let c = report::check_indexed(db, &p)?;
        let drive = overlaps
            .iter()
            .find(|o| o.unit.level == "drive" && o.unit.path == shown);
        let mut folders: Vec<Overlap> = overlaps
            .iter()
            .filter(|o| {
                o.unit.level == "folder" && Path::new(&o.unit.path).parent() == Some(p.as_path())
            })
            .cloned()
            .collect();
        folders.sort_by_key(|f| std::cmp::Reverse(f.unit.bytes));
        let (entities, junk, stale) = match recognised.iter().find(|(r, _)| r.root_id == id) {
            Some((r, fresh)) => (r.entities.clone(), r.junk.clone(), !fresh),
            None => (Vec::new(), Vec::new(), true),
        };
        drives.push(DriveSummary {
            name: p
                .file_name()
                .map_or_else(|| shown.clone(), |n| n.to_string_lossy().into_owned()),
            key: crate::browse::key(&p),
            path: shown,
            files,
            bytes,
            scanned_at,
            connected: p.is_dir(),
            hashed_files: hashed,
            elsewhere: drive.map_or(0, |o| o.elsewhere),
            containers: drive.map(|o| o.containers.clone()).unwrap_or_default(),
            verdict: Verdict {
                safe: c.safe,
                unique_files: c.unique.files,
                unique_bytes: c.unique.bytes,
                unreadable_files: c.unreadable.files,
                top_unique: c.unique_by_dir.into_iter().take(6).collect(),
            },
            entities,
            entities_stale: stale,
            junk,
            folders,
        });
    }
    let files = drives.iter().map(|d| d.files).sum();
    let bytes = drives.iter().map(|d| d.bytes).sum();
    let hashed_files = drives.iter().map(|d| d.hashed_files).sum();
    progress("finding duplicates");
    let dupes = report::dupes(db, 1 << 20, 15)?;
    Ok(Summary {
        generated_at: now(),
        drives,
        files,
        bytes,
        hashed_files,
        dupes,
    })
}

// ---------- shared wording

fn day(t: i64) -> String {
    // days since 1970 -> y-m-d (civil from days, Howard Hinnant's algorithm)
    let z = t.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn years(e: &Entity) -> String {
    match (e.oldest, e.newest) {
        (Some(a), Some(b)) => {
            let (a, b) = (&day(a)[..4], &day(b)[..4]);
            if a == b {
                a.to_string()
            } else {
                format!("{a}–{b}")
            }
        }
        _ => String::new(),
    }
}

fn pct(part: u64, whole: u64) -> String {
    if whole == 0 {
        "—".into()
    } else {
        let p = 100.0 * part as f64 / whole as f64;
        if p > 0.0 && p < 1.0 {
            "<1%".into()
        } else {
            format!("{:.0}%", p.floor())
        }
    }
}

fn files(n: u64) -> String {
    format!("{} file{}", fmt::count(n), if n == 1 { "" } else { "s" })
}

fn verdict_short(d: &DriveSummary) -> String {
    if d.verdict.safe {
        "Safe to wipe".into()
    } else if d.verdict.unique_files > 0 {
        format!("Copy {} first", fmt::bytes(d.verdict.unique_bytes))
    } else {
        format!("{} unreadable", files(d.verdict.unreadable_files))
    }
}

fn verdict_long(d: &DriveSummary) -> String {
    let v = &d.verdict;
    if v.safe {
        return "Safe to wipe: every file on it has a SHA-256-identical copy on another drive."
            .into();
    }
    let mut s = String::new();
    if v.unique_files > 0 {
        s += &format!(
            "Copy {} ({}) somewhere else before wiping: nothing identical exists on any other indexed drive.",
            fmt::bytes(v.unique_bytes),
            files(v.unique_files)
        );
        if !v.top_unique.is_empty() {
            s += " Mostly in ";
            s += &v
                .top_unique
                .iter()
                .take(4)
                .map(|u| {
                    let dir = if u.dir == "." {
                        "the top level"
                    } else {
                        u.dir.as_str()
                    };
                    format!("{dir} ({})", fmt::bytes(u.bytes))
                })
                .collect::<Vec<_>>()
                .join(", ");
            s += ".";
        }
    }
    if v.unreadable_files > 0 {
        s += &format!(
            " {} couldn't be read, so {} be verified.",
            files(v.unreadable_files),
            if v.unreadable_files == 1 {
                "it can't"
            } else {
                "they can't"
            }
        );
    }
    s.trim().to_string()
}

fn rel(drive: &str, path: &str) -> String {
    match Path::new(path).strip_prefix(drive) {
        Ok(r) if !r.as_os_str().is_empty() => r.display().to_string(),
        _ => ".".into(),
    }
}

fn containers_line(d: &DriveSummary) -> String {
    if d.containers.is_empty() {
        return "None of it exists on another indexed drive.".into();
    }
    format!(
        "{} of this drive exists elsewhere: {}.",
        pct(d.elsewhere, d.bytes),
        d.containers
            .iter()
            .map(|c| format!("{} on {}", pct(c.bytes, d.bytes), c.label))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

// ---------- Markdown

fn md_cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

pub fn markdown(s: &Summary) -> String {
    let mut o = String::new();
    o += "# BackDown report\n\n";
    o += &format!(
        "Generated {} · {} drive{} · {} files · {} · {} hashed\n\n",
        day(s.generated_at),
        s.drives.len(),
        if s.drives.len() == 1 { "" } else { "s" },
        fmt::count(s.files),
        fmt::bytes(s.bytes),
        pct(s.hashed_files, s.files)
    );
    o += "## Drives\n\n| Drive | Files | Size | Last scan | Elsewhere | Verdict |\n|---|---:|---:|---|---:|---|\n";
    for d in &s.drives {
        o += &format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            md_cell(&format!("{} ({})", d.name, d.path)),
            fmt::count(d.files),
            fmt::bytes(d.bytes),
            d.scanned_at.map(day).unwrap_or_default(),
            pct(d.elsewhere, d.bytes),
            verdict_short(d)
        );
    }
    for d in &s.drives {
        o += &format!("\n## {}\n\n", d.name);
        o += &format!(
            "`{}` · {} files · {} · scanned {}{}\n\n",
            d.path,
            fmt::count(d.files),
            fmt::bytes(d.bytes),
            d.scanned_at.map(day).unwrap_or_else(|| "never".into()),
            if d.connected { "" } else { " · not connected" }
        );
        o += &format!(
            "**Verdict.** {}\n\n**Copies elsewhere.** {}\n\n",
            verdict_long(d),
            containers_line(d)
        );
        if !d.entities.is_empty() {
            o += "### What's on it\n\n| What | Where | Size | Years | Notes |\n|---|---|---:|---|---|\n";
            for e in d
                .entities
                .iter()
                .filter(|e| e.inside.is_none() || e.kind == "git" || e.kind.starts_with("home"))
            {
                let mut notes: Vec<String> = e.provenance.clone();
                if e.low_value {
                    notes.push("probably not worth keeping as a unit".into());
                }
                let junk: u64 = e.junk.iter().map(|j| j.bytes).sum();
                if junk > 0 {
                    notes.push(format!(
                        "{} of it is regenerable ({})",
                        fmt::bytes(junk),
                        e.junk
                            .iter()
                            .take(2)
                            .map(|j| j.why.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                o += &format!(
                    "| {} | {} | {} | {} | {} |\n",
                    md_cell(&format!("**{}**: {}", e.label, e.title)),
                    md_cell(&format!("`{}`", rel(&d.path, &e.path))),
                    fmt::bytes(e.bytes),
                    years(e),
                    md_cell(&notes.join(" "))
                );
            }
            if d.entities_stale {
                o += "\n_Recognised from an earlier scan: run `backdown entities` to refresh._\n";
            }
            o += "\n";
        } else if d.entities_stale {
            o += "_Not recognised yet: run `backdown entities`._\n\n";
        }
        if !d.folders.is_empty() {
            o += "### Folders\n\n| Folder | Size | Elsewhere | Mostly also in |\n|---|---:|---:|---|\n";
            for f in d.folders.iter().take(15) {
                o += &format!(
                    "| {} | {} | {} | {} |\n",
                    md_cell(&format!("`{}`", f.unit.label)),
                    fmt::bytes(f.unit.bytes),
                    pct(f.elsewhere, f.unit.bytes),
                    md_cell(
                        &f.containers
                            .first()
                            .map(|c| format!("`{}`", c.path))
                            .unwrap_or_default()
                    )
                );
            }
            o += "\n";
        }
        let junk: u64 = d.junk.iter().map(|j| j.bytes).sum::<u64>()
            + d.entities
                .iter()
                .flat_map(|e| &e.junk)
                .map(|j| j.bytes)
                .sum::<u64>();
        if junk > 0 {
            o += &format!(
                "**Regenerable data.** {} of caches, previews, build output and trash.\n",
                fmt::bytes(junk)
            );
        }
    }
    o += "\n## Duplicates\n\n";
    if s.dupes.groups == 0 {
        o += "No files of 1 MB or more exist more than once (among hashed files).\n";
    } else {
        o += &format!(
            "{} groups of identical files (1 MB and up); {} could be freed.\n\n| Copies | Each | Reclaimable | Where |\n|---:|---:|---:|---|\n",
            fmt::count(s.dupes.groups),
            fmt::bytes(s.dupes.reclaimable)
        );
        for g in &s.dupes.largest {
            o += &format!(
                "| {} | {} | {} | {} |\n",
                g.copies,
                fmt::bytes(g.size),
                fmt::bytes(g.reclaimable),
                md_cell(
                    &g.paths
                        .iter()
                        .take(3)
                        .map(|p| format!("`{p}`"))
                        .collect::<Vec<_>>()
                        .join("<br>")
                )
            );
        }
    }
    o += "\n---\nMade by BackDown · every \"elsewhere\" and verdict is backed by SHA-256; files without a hash count as unique: usually no other file has their size, or hashing hasn't reached them yet.\n";
    o
}

// ---------- HTML

fn h(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn bar(part: u64, whole: u64) -> String {
    let w = if whole == 0 {
        0.0
    } else {
        (100.0 * part as f64 / whole as f64).min(100.0)
    };
    format!("<span class=\"bar\"><i style=\"width:{w:.1}%\"></i></span>")
}

pub fn html(s: &Summary) -> String {
    let mut o = String::from(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>BackDown report</title><style>
:root{color-scheme:light dark;--bg:#f7f6f2;--card:#fff;--ink:#17161b;--ink2:#4e4b57;--ink3:#85818f;--line:#e2dfd6;--accent:#d8432b;--good:#2e8b57;--warn:#b8800e}
@media(prefers-color-scheme:dark){:root{--bg:#1b1a1f;--card:#232228;--ink:#f1efe9;--ink2:#bbb7c4;--ink3:#85818f;--line:#2e2c34;--accent:#f0603f;--good:#4fb37a;--warn:#e0a93a}}
body{margin:0;background:var(--bg);color:var(--ink);font:15px/1.5 system-ui,-apple-system,"Segoe UI",sans-serif}
main{max-width:1100px;margin:0 auto;padding:32px 20px 60px}
h1{font-size:2rem;margin:0 0 4px}h2{font-size:1.35rem;margin:36px 0 8px}h3{font-size:.8rem;text-transform:uppercase;letter-spacing:.05em;color:var(--ink3);margin:22px 0 8px}
.sub{color:var(--ink2)}code{font:12.5px ui-monospace,Menlo,Consolas,monospace;background:rgb(127 127 127/.12);padding:1px 5px;border-radius:4px}
table{width:100%;border-collapse:collapse;background:var(--card);border:1px solid var(--line);border-radius:10px;overflow:hidden;font-size:.9rem}
th,td{text-align:left;padding:7px 10px;border-bottom:1px solid var(--line);vertical-align:top}th{font-weight:600;color:var(--ink2);font-size:.8rem}
td.n,th.n{text-align:right;font-variant-numeric:tabular-nums;white-space:nowrap}
.card{background:var(--card);border:1px solid var(--line);border-radius:12px;padding:14px 16px;margin:10px 0}
.ok{color:var(--good);font-weight:600}.warn{color:var(--warn);font-weight:600}
.bar{display:inline-block;width:70px;height:6px;border-radius:9px;background:var(--line);vertical-align:middle;margin-right:6px;overflow:hidden}.bar i{display:block;height:100%;background:var(--accent)}
.note{color:var(--ink3);font-size:.85rem}.kind{font-weight:600}.prov{color:var(--ink2);font-size:.85rem}
</style></head><body><main>
"#,
    );
    o += &format!(
        "<h1>BackDown report</h1><p class=\"sub\">Generated {} · {} drive{} · {} files · {} · {} hashed</p>\n",
        day(s.generated_at),
        s.drives.len(),
        if s.drives.len() == 1 { "" } else { "s" },
        fmt::count(s.files),
        fmt::bytes(s.bytes),
        pct(s.hashed_files, s.files)
    );
    o += "<h2>Drives</h2><table><tr><th>Drive</th><th class=n>Files</th><th class=n>Size</th><th>Last scan</th><th>Elsewhere</th><th>Verdict</th></tr>\n";
    for d in &s.drives {
        o += &format!(
            "<tr><td><b>{}</b><br><span class=note>{}</span></td><td class=n>{}</td><td class=n>{}</td><td>{}</td><td>{}{}</td><td class=\"{}\">{}</td></tr>\n",
            h(&d.name),
            h(&d.path),
            fmt::count(d.files),
            fmt::bytes(d.bytes),
            d.scanned_at.map(day).unwrap_or_default(),
            bar(d.elsewhere, d.bytes),
            pct(d.elsewhere, d.bytes),
            if d.verdict.safe { "ok" } else { "warn" },
            h(&verdict_short(d))
        );
    }
    o += "</table>\n";
    for d in &s.drives {
        o += &format!(
            "<h2>{}</h2><p class=sub><code>{}</code> · {} files · {} · scanned {}{}</p>\n",
            h(&d.name),
            h(&d.path),
            fmt::count(d.files),
            fmt::bytes(d.bytes),
            d.scanned_at.map(day).unwrap_or_else(|| "never".into()),
            if d.connected { "" } else { " · not connected" }
        );
        o += &format!(
            "<div class=card><p><span class=\"{}\">Verdict.</span> {}</p><p><b>Copies elsewhere.</b> {}</p></div>\n",
            if d.verdict.safe { "ok" } else { "warn" },
            h(&verdict_long(d)),
            h(&containers_line(d))
        );
        if !d.entities.is_empty() {
            o += "<h3>What's on it</h3><table><tr><th>What</th><th>Where</th><th class=n>Size</th><th>Years</th></tr>\n";
            for e in d
                .entities
                .iter()
                .filter(|e| e.inside.is_none() || e.kind == "git" || e.kind.starts_with("home"))
            {
                let junk: u64 = e.junk.iter().map(|j| j.bytes).sum();
                let mut notes = e.provenance.iter().map(|p| h(p)).collect::<Vec<_>>();
                if e.low_value {
                    notes.push("Probably not worth keeping as a unit.".into());
                }
                if junk > 0 {
                    notes.push(format!("{} of it is regenerable.", fmt::bytes(junk)));
                }
                o += &format!(
                    "<tr><td><span class=kind>{}</span>: {}{}</td><td><code>{}</code></td><td class=n>{}</td><td>{}</td></tr>\n",
                    h(&e.label),
                    h(&e.title),
                    if notes.is_empty() {
                        String::new()
                    } else {
                        format!("<br><span class=prov>{}</span>", notes.join(" "))
                    },
                    h(&rel(&d.path, &e.path)),
                    fmt::bytes(e.bytes),
                    years(e)
                );
            }
            o += "</table>\n";
            if d.entities_stale {
                o += "<p class=note>Recognised from an earlier scan: run <code>backdown entities</code> to refresh.</p>\n";
            }
        }
        if !d.folders.is_empty() {
            o += "<h3>Folders</h3><table><tr><th>Folder</th><th class=n>Size</th><th>Elsewhere</th><th>Mostly also in</th></tr>\n";
            for f in d.folders.iter().take(15) {
                o += &format!(
                    "<tr><td><code>{}</code></td><td class=n>{}</td><td>{}{}</td><td>{}</td></tr>\n",
                    h(&f.unit.label),
                    fmt::bytes(f.unit.bytes),
                    bar(f.elsewhere, f.unit.bytes),
                    pct(f.elsewhere, f.unit.bytes),
                    f.containers
                        .first()
                        .map(|c| format!("<code>{}</code>", h(&c.path)))
                        .unwrap_or_default()
                );
            }
            o += "</table>\n";
        }
    }
    o += "<h2>Duplicates</h2>";
    if s.dupes.groups == 0 {
        o += "<p class=sub>No files of 1 MB or more exist more than once (among hashed files).</p>";
    } else {
        o += &format!(
            "<p class=sub>{} groups of identical files (1 MB and up); {} could be freed.</p><table><tr><th class=n>Copies</th><th class=n>Each</th><th class=n>Reclaimable</th><th>Where</th></tr>",
            fmt::count(s.dupes.groups),
            fmt::bytes(s.dupes.reclaimable)
        );
        for g in &s.dupes.largest {
            o += &format!(
                "<tr><td class=n>{}</td><td class=n>{}</td><td class=n>{}</td><td>{}</td></tr>",
                g.copies,
                fmt::bytes(g.size),
                fmt::bytes(g.reclaimable),
                g.paths
                    .iter()
                    .take(3)
                    .map(|p| format!("<code>{}</code>", h(p)))
                    .collect::<Vec<_>>()
                    .join("<br>")
            );
        }
        o += "</table>";
    }
    o += "<p class=note style=\"margin-top:30px\">Made by BackDown. Every “elsewhere” and verdict is backed by SHA-256; files without a hash count as unique: usually no other file has their size, or hashing hasn’t reached them yet.</p></main></body></html>\n";
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days() {
        assert_eq!(day(0), "1970-01-01");
        assert_eq!(day(1_600_000_000), "2020-09-13");
        assert_eq!(day(951_782_400), "2000-02-29");
    }
}
