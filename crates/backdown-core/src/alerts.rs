//! Notifications: the few things worth interrupting someone for (a drive getting worse, a mirror
//! that no longer matches its source, a scheduled job that failed), kept in the index's
//! `notifications` table with read/unread, and the rules for how often they may reach the desktop.
//!
//! The same thing is never said twice in a row: each note has a key (the drive and what changed,
//! the mirror, the job) and a quiet period, and a note whose key was recorded within its quiet
//! period is dropped. What's left is always recorded; it goes to the desktop only while desktop
//! notifications are on and within the limits (a few in any ten minutes, a dozen a day), so a bad
//! morning with many drives doesn't become a wall of pop-ups: the rest wait, unread, in the list.
//! A drive that's failing is the exception: that one always reaches the desktop.
//!
//! What a drive's new health reading is worth saying is decided here too (`drive_notes`: worse,
//! failing, or only its cable), and which mirrors that drive holds (`mirrors_on`).

use std::path::Path;

use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use crate::health::{Change, Health, Recorded, Verdict};
use crate::{Db, fmt, mirror};

/// Something to tell.
#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    /// `health`, `drift` or `job`
    pub kind: &'static str,
    /// what it's about; the same key within `quiet_for` is the same news
    pub key: String,
    /// `info`, `warning` or `failing`
    pub level: &'static str,
    pub title: String,
    pub body: String,
    /// the drive's health id, when it's about a drive
    pub drive: Option<String>,
    /// seconds during which the same key isn't recorded again
    pub quiet_for: i64,
}

/// A recorded notification.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Stored {
    pub id: i64,
    pub at: i64,
    pub kind: String,
    pub key: String,
    pub level: String,
    pub title: String,
    pub body: String,
    pub drive: Option<String>,
    /// it went to the desktop
    pub shown: bool,
    pub read: bool,
}

/// How many notifications may reach the desktop.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// at most this many…
    pub burst: usize,
    /// …in this many seconds,
    pub window: i64,
    /// and this many in a day
    pub per_day: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            burst: 3,
            window: 600,
            per_day: 12,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// said already, within its quiet period
    Drop,
    /// record it; and show it on the desktop, or not (turned off, or over the limits)
    Record { show: bool },
}

/// What to do with `note`, given when its key was last recorded, when the notifications of the
/// last day reached the desktop, and whether desktop notifications are on.
pub fn decide(
    note: &Note,
    same_key_at: Option<i64>,
    shown_at: &[i64],
    desktop: bool,
    now: i64,
    limits: &Limits,
) -> Decision {
    if same_key_at.is_some_and(|t| now - t < note.quiet_for) {
        return Decision::Drop;
    }
    let in_window = shown_at
        .iter()
        .filter(|t| now - **t < limits.window)
        .count();
    let in_day = shown_at.iter().filter(|t| now - **t < 86_400).count();
    // a failing drive is never held back (its own quiet period still applies)
    let urgent = note.level == "failing";
    Decision::Record {
        show: desktop && (urgent || (in_window < limits.burst && in_day < limits.per_day)),
    }
}

/// Record `note` if it's news; returns its id and whether to show it on the desktop.
pub fn add(
    db: &Db,
    note: &Note,
    desktop: bool,
    now: i64,
    limits: &Limits,
) -> Result<Option<(i64, bool)>> {
    let same: Option<i64> = db
        .conn
        .query_row(
            "SELECT MAX(at) FROM notifications WHERE key = ?1",
            [&note.key],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let shown: Vec<i64> = db
        .conn
        .prepare("SELECT at FROM notifications WHERE shown = 1 AND at > ?1")?
        .query_map([now - 86_400], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    match decide(note, same, &shown, desktop, now, limits) {
        Decision::Drop => Ok(None),
        Decision::Record { show } => {
            db.conn.execute(
                "INSERT INTO notifications (at, kind, key, level, title, body, drive, shown)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    now, note.kind, note.key, note.level, note.title, note.body, note.drive, show
                ],
            )?;
            let id = db.conn.last_insert_rowid();
            // a small list: the newest 500
            db.conn.execute(
                "DELETE FROM notifications WHERE id NOT IN (SELECT id FROM notifications ORDER BY id DESC LIMIT 500)",
                [],
            )?;
            Ok(Some((id, show)))
        }
    }
}

/// The newest `limit` notifications, newest first.
pub fn list(db: &Db, limit: usize) -> Result<Vec<Stored>> {
    let mut stmt = db.conn.prepare(
        "SELECT id, at, kind, key, level, title, body, drive, shown, read FROM notifications ORDER BY id DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit as i64], |r| {
        Ok(Stored {
            id: r.get(0)?,
            at: r.get(1)?,
            kind: r.get(2)?,
            key: r.get(3)?,
            level: r.get(4)?,
            title: r.get(5)?,
            body: r.get(6)?,
            drive: r.get(7)?,
            shown: r.get(8)?,
            read: r.get(9)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn unread(db: &Db) -> Result<u64> {
    Ok(db.conn.query_row(
        "SELECT COUNT(*) FROM notifications WHERE read = 0",
        [],
        |r| r.get::<_, i64>(0),
    )? as u64)
}

/// Mark one notification read, or all of them (`None`). Returns how many changed.
pub fn mark_read(db: &Db, id: Option<i64>) -> Result<usize> {
    Ok(match id {
        Some(id) => db.conn.execute(
            "UPDATE notifications SET read = 1 WHERE id = ?1 AND read = 0",
            [id],
        )?,
        None => db
            .conn
            .execute("UPDATE notifications SET read = 1 WHERE read = 0", [])?,
    })
}

// ---------- what a drive's new reading says

/// The mirrors whose copies are on drive `h`: the deepest mount point (of every drive) holding
/// the mirror's folder is one of `h`'s.
pub fn mirrors_on<'a>(
    h: &Health,
    mirrors: &'a [mirror::Mirror],
    drives: &[Health],
) -> Vec<&'a mirror::Mirror> {
    mirrors
        .iter()
        .filter(|m| {
            let t = Path::new(&m.target);
            drives
                .iter()
                .flat_map(|d| d.mounts.iter().map(move |mp| (d, mp)))
                .filter(|(_, mp)| t.starts_with(mp.as_str()))
                .max_by_key(|(_, mp)| mp.len())
                .is_some_and(|(d, _)| d.device == h.device)
        })
        .collect()
}

/// What to tell about one reading: its verdict getting worse (once a week at most for the same
/// verdict), or, short of that, counters getting worse (once a day at most per drive).
pub fn drive_notes(h: &Health, r: &Recorded, on: &[&mirror::Mirror]) -> Vec<Note> {
    let serious: Vec<&Change> = r
        .changes
        .iter()
        .filter(|c| c.level.rank() > 0 && c.code != "verdict")
        .collect();
    let worsened = r.changes.iter().find(|c| c.code == "verdict");
    if worsened.is_none() && serious.is_empty() {
        return Vec::new();
    }
    let label = h.label();
    let level = r
        .changes
        .iter()
        .map(|c| c.level)
        .max_by_key(|l| l.rank())
        .unwrap_or(Verdict::Warning);
    let cable_only = worsened.is_none() && serious.iter().all(|c| c.code == "crc");
    let what = if serious.is_empty() {
        worsened
            .map(|w| w.text.split_once(": ").map_or(w.text.as_str(), |(_, x)| x))
            .unwrap_or("")
            .to_string()
    } else {
        serious
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>()
            .join("; ")
    };
    let advice = if cable_only {
        ""
    } else if level == Verdict::Failing {
        " Copy what matters off it now: BackDown's Rescue can copy the whole drive."
    } else {
        " Make sure what's on it has another copy."
    };
    // the mirrored folder by its last two names: enough to recognise, short enough for a pop-up
    let short = |p: &str| {
        let parts: Vec<String> = Path::new(p)
            .iter()
            .map(|c| c.to_string_lossy().into_owned())
            .filter(|c| !c.is_empty() && c != "/" && c != "\\")
            .collect();
        if parts.len() <= 2 {
            p.to_string()
        } else {
            format!("…/{}", parts[parts.len() - 2..].join("/"))
        }
    };
    let mirror_line = if !on.is_empty() && !cable_only {
        format!(
            " It holds the mirror of {}: this mirror drive is dying, add another mirror.",
            on.iter()
                .map(|m| short(&m.source))
                .collect::<Vec<_>>()
                .join(", ")
        )
    } else {
        String::new()
    };
    let (title, key, quiet) = match worsened {
        Some(_) if level == Verdict::Failing => (
            format!("{label} is failing"),
            format!("health:{}:verdict:failing", h.id),
            7 * 86_400,
        ),
        Some(_) => (
            format!("{label} needs watching"),
            format!("health:{}:verdict:{}", h.id, level.as_str()),
            7 * 86_400,
        ),
        None if cable_only => (
            format!("Transfer errors on {label}"),
            format!("health:{}:cable", h.id),
            20 * 3600,
        ),
        None => (
            format!("{label} is getting worse"),
            format!("health:{}:worse", h.id),
            20 * 3600,
        ),
    };
    vec![Note {
        kind: "health",
        key,
        level: level.as_str(),
        title,
        body: format!("{}.{advice}{mirror_line}", fmt::capitalised(&what)),
        drive: Some(h.id.clone()),
        quiet_for: quiet,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health;

    fn note(key: &str) -> Note {
        Note {
            kind: "health",
            key: key.into(),
            level: "warning",
            title: "A drive is getting worse".into(),
            body: "2 more sectors have gone bad".into(),
            drive: Some("sn:FAKE0001".into()),
            quiet_for: 86_400,
        }
    }

    const NOW: i64 = 1_790_000_000;

    #[test]
    fn the_same_news_is_told_once() {
        let l = Limits::default();
        let n = note("health:sn:FAKE0001:reallocated:2");
        assert_eq!(
            decide(&n, None, &[], true, NOW, &l),
            Decision::Record { show: true }
        );
        assert_eq!(
            decide(&n, Some(NOW - 3_600), &[], true, NOW, &l),
            Decision::Drop
        );
        // after its quiet period it's news again
        assert_eq!(
            decide(&n, Some(NOW - 90_000), &[], true, NOW, &l),
            Decision::Record { show: true }
        );
    }

    #[test]
    fn the_desktop_gets_only_so_many() {
        let l = Limits::default();
        let n = note("k");
        // three in the last ten minutes: recorded, not shown
        let burst = [NOW - 60, NOW - 120, NOW - 300];
        assert_eq!(
            decide(&n, None, &burst, true, NOW, &l),
            Decision::Record { show: false }
        );
        // those were half an hour ago: fine again
        let earlier = [NOW - 1_800, NOW - 1_900, NOW - 2_000];
        assert_eq!(
            decide(&n, None, &earlier, true, NOW, &l),
            Decision::Record { show: true }
        );
        // a dozen today already
        let day: Vec<i64> = (1..=12).map(|i| NOW - i * 3_600).collect();
        assert_eq!(
            decide(&n, None, &day, true, NOW, &l),
            Decision::Record { show: false }
        );
        // turned off: recorded, never shown
        assert_eq!(
            decide(&n, None, &[], false, NOW, &l),
            Decision::Record { show: false }
        );
        // a drive that's failing gets through the limits (not through "off")
        let failing = Note {
            level: "failing",
            ..note("k2")
        };
        assert_eq!(
            decide(&failing, None, &burst, true, NOW, &l),
            Decision::Record { show: true }
        );
        assert_eq!(
            decide(&failing, None, &[], false, NOW, &l),
            Decision::Record { show: false }
        );
    }

    #[test]
    fn stored_listed_and_read() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("index.db")).unwrap();
        let l = Limits::default();
        let (a, shown) = add(&db, &note("one"), true, NOW, &l).unwrap().unwrap();
        assert!(shown);
        assert_eq!(add(&db, &note("one"), true, NOW + 60, &l).unwrap(), None);
        for (i, k) in ["two", "three", "four"].iter().enumerate() {
            let (_, shown) = add(&db, &note(k), true, NOW + 10 + i as i64, &l)
                .unwrap()
                .unwrap();
            // the fourth within ten minutes waits in the list
            assert_eq!(shown, i < 2, "{k}");
        }
        let all = list(&db, 10).unwrap();
        assert_eq!(all.len(), 4);
        assert_eq!(all[0].key, "four");
        assert!(!all[0].shown);
        assert_eq!(unread(&db).unwrap(), 4);
        assert_eq!(mark_read(&db, Some(a)).unwrap(), 1);
        assert_eq!(unread(&db).unwrap(), 3);
        assert_eq!(mark_read(&db, None).unwrap(), 3);
        assert_eq!(unread(&db).unwrap(), 0);
        assert!(list(&db, 10).unwrap().iter().all(|s| s.read));
    }

    fn reading(realloc: u64, at: i64) -> Health {
        Health {
            id: "sn:FAKE0001".into(),
            device: "/dev/sdb".into(),
            name: "sdb".into(),
            model: Some("FAKEDISK HD4000".into()),
            serial: Some("FAKE0001".into()),
            rotational: Some(true),
            reported: Some(Verdict::Ok),
            reallocated: Some(realloc),
            pending: Some(0),
            mounts: vec!["/media/someone/Mirror".into()],
            read_at: at,
            ..Health::default()
        }
    }

    fn mirror_to(target: &str) -> mirror::Mirror {
        mirror::Mirror {
            id: 1,
            source: "/home/someone/Archive".into(),
            target: target.into(),
            created_at: 0,
            verify_every_days: 30,
            last_sync: None,
            last_sync_summary: None,
            last_verify: None,
            last_verify_summary: None,
            last_verify_ok: None,
        }
    }

    #[test]
    fn notes_say_what_got_worse_and_about_mirrors() {
        let prev = reading(0, 1_000);
        let cur = reading(3, 2_000);
        let r = Recorded {
            stored: true,
            first: false,
            before: Some(Verdict::Ok),
            verdict: Verdict::Warning,
            changes: health::step(Some(&prev), &cur),
        };
        let mirrors = [mirror_to("/media/someone/Mirror/Archive")];
        let root = Health {
            device: "/dev/nvme0n1".into(),
            mounts: vec!["/".into()],
            ..Health::default()
        };
        let all = [root.clone(), cur.clone()];
        let on = mirrors_on(&cur, &mirrors, &all);
        assert_eq!(on.len(), 1);
        // the system drive's "/" holds it too, but less deeply
        assert!(mirrors_on(&root, &mirrors, &all).is_empty());
        let n = drive_notes(&cur, &r, &on);
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].title, "FAKEDISK HD4000 needs watching");
        assert_eq!(n[0].key, "health:sn:FAKE0001:verdict:warning");
        assert_eq!(n[0].level, "warning");
        assert!(n[0].body.starts_with("3 more sectors have gone bad"));
        assert!(n[0].body.contains(
            "It holds the mirror of …/someone/Archive: this mirror drive is dying, add another mirror."
        ));
        // not about a mirror elsewhere
        let elsewhere = [mirror_to("/media/other/Archive")];
        let n = drive_notes(&cur, &r, &mirrors_on(&cur, &elsewhere, &all));
        assert!(!n[0].body.contains("mirror"));

        // already warning, more sectors: "getting worse", at most daily
        let next = reading(7, 3_000);
        let r = Recorded {
            changes: health::step(Some(&cur), &next),
            verdict: Verdict::Warning,
            ..Recorded::default()
        };
        let n = drive_notes(&next, &r, &[]);
        assert_eq!(n[0].title, "FAKEDISK HD4000 is getting worse");
        assert_eq!(n[0].key, "health:sn:FAKE0001:worse");
        // nothing new: nothing to say
        let r = Recorded {
            changes: health::step(Some(&next), &next),
            ..Recorded::default()
        };
        assert!(drive_notes(&next, &r, &[]).is_empty());
    }

    #[test]
    fn cable_errors_are_about_the_cable() {
        let mut a = reading(0, 1_000);
        a.crc_errors = Some(2);
        let mut b = a.clone();
        b.read_at = 2_000;
        b.crc_errors = Some(40);
        let r = Recorded {
            changes: health::step(Some(&a), &b),
            ..Recorded::default()
        };
        let m = [mirror_to("/media/someone/Mirror/x")];
        let n = drive_notes(&b, &r, &m.iter().collect::<Vec<_>>());
        assert_eq!(n[0].title, "Transfer errors on FAKEDISK HD4000");
        assert!(n[0].body.contains("cable"));
        assert!(!n[0].body.contains("Copy what matters"));
        assert!(!n[0].body.contains("dying"));
    }
}
