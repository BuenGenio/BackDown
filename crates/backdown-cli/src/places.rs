//! The Places view's requests (/api/places*) and the daemon's `places` job: finding photos and
//! videos by where they were taken and what they're tagged with, tagging them by hand, and
//! reading the dates and places of a drive's photos in the background. The search itself is
//! `backdown_core::places`; `backdown tag find` is the same search in a terminal.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use backdown_core::db::{Lock, Tx};
use backdown_core::{Db, browse, fmt, geo, places, tags};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::daemon::{Counts, Daemon, Job};
use crate::tagging::ReadCounts;

/// What a page of results holds unless asked for more (or fewer).
const PAGE: usize = 120;

/// Kinds BackDown writes from the files themselves, in a form it relies on: not for hands.
const DERIVED_ONLY: &[&str] = &["location", "date"];

/// The flags the view sets.
const FLAGS: &[&str] = &["keep", "drop", "reviewed", "canonical", "owner-confirmed"];

pub fn handles(path: &str) -> bool {
    path == "/api/places" || path.starts_with("/api/places-")
}

/// The basemap's dots (geo.rs `basemap`), worked out once.
pub fn basemap() -> Result<&'static [u8]> {
    static MAP: OnceLock<Vec<u8>> = OnceLock::new();
    if let Some(m) = MAP.get() {
        return Ok(m);
    }
    let m = geo::GeoRef::embedded()?.basemap();
    Ok(MAP.get_or_init(|| m))
}

fn all<'a>(params: &'a [(String, String)], name: &str) -> impl Iterator<Item = &'a str> {
    params
        .iter()
        .filter(move |(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

fn one<'a>(params: &'a [(String, String)], name: &str) -> Option<&'a str> {
    all(params, name).next().filter(|v| !v.trim().is_empty())
}

fn num<T: std::str::FromStr>(params: &[(String, String)], name: &str) -> Result<Option<T>> {
    one(params, name)
        .map(|v| {
            v.trim()
                .parse::<T>()
                .map_err(|_| anyhow::anyhow!("bad {name}: {v:?}"))
        })
        .transpose()
}

/// GET /api/places (the search), /api/places-suggest?q= (what a search box offers),
/// /api/places-item?key= (one photo: its tags, flags and copies), /api/places-labels?s=&w=&n=&e=
/// (names for the map), /api/places-tags?kind= (every tag, with counts).
pub fn get(db_path: &Path, path: &str, params: &[(String, String)]) -> Result<Value> {
    if path == "/api/places-labels" {
        let b =
            |k: &str| -> Result<f64> { num(params, k)?.with_context(|| format!("missing {k}")) };
        let limit = num(params, "limit")?.unwrap_or(40usize).min(200);
        let g = geo::GeoRef::embedded()?;
        return Ok(json!({ "labels": g.labels(b("s")?, b("w")?, b("n")?, b("e")?, limit) }));
    }
    let db = Db::open_read_only(db_path)?;
    Ok(match path {
        "/api/places" => search(&db, params)?,
        "/api/places-suggest" => {
            let q = one(params, "q").unwrap_or("");
            json!({
                "centres": geo::GeoRef::embedded()?.suggest(q, 6),
                "tags": tags::find_tags(&db.conn, q, 8)?,
            })
        }
        "/api/places-item" => item(&db, one(params, "key").context("which photo?")?)?,
        "/api/places-tags" => json!({ "tags": tags::tag_counts(&db.conn, one(params, "kind"))? }),
        _ => bail!("no such endpoint"),
    })
}

/// The search: `near=` (a place as typed) or `lat=&lon=`, with `km=`; `tag=` (an id or
/// KIND=VALUE) as many times as wanted; `from=`, `to=`, `sort=`, `offset=`, `limit=`. With
/// `paths=1`, only the path of every copy of every match.
fn search(db: &Db, params: &[(String, String)]) -> Result<Value> {
    let km = num::<f64>(params, "km")?.unwrap_or(places::DEFAULT_KM);
    if !(km > 0.0 && km.is_finite()) {
        bail!("a distance above 0 km");
    }
    let centre = match (
        num::<f64>(params, "lat")?,
        num::<f64>(params, "lon")?,
        one(params, "near"),
    ) {
        (Some(lat), Some(lon), _) => {
            if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
                bail!("a position is a latitude of -90 to 90 and a longitude of -180 to 180");
            }
            Some(geo::Centre {
                kind: "point",
                label: one(params, "label")
                    .map_or_else(|| format!("{lat:.4}, {lon:.4}"), str::to_string),
                detail: String::new(),
                lat,
                lon,
            })
        }
        (_, _, Some(q)) => Some(
            geo::GeoRef::embedded()?
                .resolve(q)
                .with_context(|| format!("don't know where {q:?} is"))?,
        ),
        _ => None,
    };
    let only_paths = one(params, "paths") == Some("1");
    let query = places::Query {
        near: centre.as_ref().map(|c| places::Near {
            lat: c.lat,
            lon: c.lon,
            km,
        }),
        tags: all(params, "tag")
            .filter(|t| !t.trim().is_empty())
            .map(places::TagFilter::parse)
            .collect::<Result<_>>()?,
        from: one(params, "from").map(str::to_string),
        to: one(params, "to").map(str::to_string),
        sort: places::Sort::parse(one(params, "sort").unwrap_or(""))?,
        offset: if only_paths {
            0
        } else {
            num(params, "offset")?.unwrap_or(0)
        },
        limit: if only_paths {
            0
        } else {
            num(params, "limit")?.unwrap_or(PAGE).clamp(1, 1000)
        },
    };
    let found = places::find(&db.conn, &query)?;
    if only_paths {
        let paths: Vec<&str> = found
            .hits
            .iter()
            .flat_map(|h| h.copies.iter().map(|c| c.path.as_str()))
            .collect();
        return Ok(json!({ "paths": paths, "total": found.total }));
    }
    let (tagged, located) = places::counts(&db.conn)?;
    Ok(json!({
        "centre": centre,
        "km": km,
        "found": found,
        "tagged": tagged,
        "located_all": located,
    }))
}

/// One photo: its tags (with where each came from), flags and copies.
fn item(db: &Db, key: &str) -> Result<Value> {
    let key = content_key(key)?;
    let tagged = tags::tags_of(&db.conn, tags::TARGET_CONTENT, &key)?;
    let mut at = None;
    let mut date = None;
    for t in &tagged {
        match t.tag.kind.as_str() {
            "location" => at = geo::parse_point(&t.tag.value),
            "date" => date = Some(t.tag.value.clone()),
            _ => {}
        }
    }
    Ok(json!({
        "key": key,
        "lat": at.map(|a| a.0),
        "lon": at.map(|a| a.1),
        "date": date,
        "tags": tagged,
        "flags": tags::flags_of(&db.conn, tags::TARGET_CONTENT, &key)?,
        "copies": places::copies(&db.conn, &key)?,
    }))
}

/// A content key as the browser sent it: 64 hex digits.
fn content_key(k: &str) -> Result<String> {
    let k = k.trim().to_ascii_lowercase();
    if k.len() != 64 || !k.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("not a photo's key: {k:?}");
    }
    Ok(k)
}

#[derive(Deserialize)]
struct Change {
    #[serde(default)]
    keys: Vec<String>,
    kind: Option<String>,
    value: Option<String>,
    tag: Option<i64>,
    state: Option<String>,
    #[serde(default)]
    on: bool,
}

/// POST /api/places-read?path= (read the dates and places of the photos under a folder, or of
/// every drive, in the background); and, with a JSON body, /api/places-tag {keys, kind, value},
/// /api/places-untag {keys, tag}, /api/places-flag {keys, state, on}, /api/places-tag-edit {tag,
/// value} and /api/places-tag-delete {tag}.
pub fn post(
    d: Option<&Daemon>,
    db_path: &Path,
    path: &str,
    params: &[(String, String)],
    body: &[u8],
) -> Result<Value> {
    if path == "/api/places-read" {
        let d = d.context(
            "reading runs in BackDown's background service: `backdown tag exif FOLDER --geo` in a terminal does the same",
        )?;
        let folder = one(params, "path").map(|p| p.trim().to_string());
        if let Some(f) = &folder
            && !Path::new(f).is_absolute()
        {
            bail!("which folder? {f:?} isn't a full path");
        }
        return Ok(
            json!({ "ok": true, "job": d.enqueue("places", folder, "requested", Default::default()) }),
        );
    }
    let c: Change =
        serde_json::from_slice(body).context("the request's body isn't what was expected")?;
    let keys = c
        .keys
        .iter()
        .map(|k| content_key(k))
        .collect::<Result<Vec<_>>>()?;
    let db = Db::open(db_path)?;
    let tag_id = || c.tag.context("which tag?");
    Ok(match path {
        "/api/places-tag" => {
            let kind = c.kind.as_deref().unwrap_or("").trim().to_lowercase();
            let value = c.value.as_deref().unwrap_or("").trim().to_string();
            if kind.is_empty() || value.is_empty() {
                bail!("a tag needs a kind and a value");
            }
            if DERIVED_ONLY.contains(&kind.as_str()) {
                bail!("{kind} tags come from the photos themselves");
            }
            if value.chars().count() > 200 || kind.chars().count() > 40 {
                bail!("that's too long for a tag");
            }
            if keys.is_empty() {
                bail!("nothing chosen to tag");
            }
            let tx = Tx::begin(&db.conn, Lock::Immediate)?;
            let id = tags::upsert_tag(&db.conn, &kind, &value, None, None)?;
            for k in &keys {
                tags::attach(&db.conn, tags::TARGET_CONTENT, k, id, "user", 1.0)?;
            }
            tx.commit()?;
            json!({ "ok": true, "tag": id, "count": keys.len() })
        }
        "/api/places-untag" => {
            let id = tag_id()?;
            let tx = Tx::begin(&db.conn, Lock::Immediate)?;
            for k in &keys {
                tags::detach(&db.conn, tags::TARGET_CONTENT, k, id)?;
            }
            tx.commit()?;
            json!({ "ok": true, "count": keys.len() })
        }
        "/api/places-flag" => {
            let state = c.state.as_deref().unwrap_or("");
            if !FLAGS.contains(&state) {
                bail!("flags here are {}", FLAGS.join(", "));
            }
            let tx = Tx::begin(&db.conn, Lock::Immediate)?;
            for k in &keys {
                if c.on {
                    tags::set_flag(&db.conn, tags::TARGET_CONTENT, k, state, None, "user", None)?;
                } else {
                    tags::clear_flag(&db.conn, tags::TARGET_CONTENT, k, state)?;
                }
            }
            tx.commit()?;
            json!({ "ok": true, "count": keys.len() })
        }
        "/api/places-tag-edit" | "/api/places-tag-delete" => {
            let id = tag_id()?;
            let own = tags::tag_counts(&db.conn, None)?
                .into_iter()
                .find(|t| t.tag.id == id)
                .context("there's no such tag (any more)")?;
            if !own.own {
                bail!(
                    "BackDown read this tag from the photos, so reading them again would bring it back: take it off the photos it's wrong for instead"
                );
            }
            if path == "/api/places-tag-edit" {
                let value = c.value.as_deref().unwrap_or("");
                json!({ "ok": true, "tag": tags::rename_tag(&db.conn, id, value)? })
            } else {
                tags::delete_tag(&db.conn, id)?;
                json!({ "ok": true })
            }
        }
        _ => bail!("no such endpoint"),
    })
}

/// The daemon's `places` job: read the dates and places of the photos and videos under the job's
/// folder (every indexed drive without one) that weren't read before, hashing those `hash` left
/// alone. Written a second at a time, and stoppable at the next file.
pub fn run_job(d: &Daemon, db: &Db, job: &Job, stop: &AtomicBool) -> Result<String> {
    let dirs: Vec<PathBuf> = match &job.path {
        Some(p) => vec![PathBuf::from(p)],
        None => browse::roots(db)?
            .iter()
            .map(|r| browse::path_of(&r.key))
            .collect::<Result<_>>()?,
    };
    d.progress(
        job.id,
        "finding the photos and videos".into(),
        Counts {
            stage: "listing",
            ..Counts::default()
        },
    );
    let mut files = Vec::new();
    for dir in &dirs {
        files.extend(places::media_under(&db.conn, dir)?);
    }
    let total = files.len() as u64;
    let total_bytes: u64 = files.iter().map(|f| f.size).sum();
    let geo = geo::GeoRef::embedded()?;
    let mut n = ReadCounts::default();
    let mut bytes = 0u64;
    let mut tx = Tx::begin(&db.conn, Lock::Immediate)?;
    let mut since = Instant::now();
    for (i, f) in files.iter().enumerate() {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let r = places::read_file(&db.conn, f, Some(geo), "exif", "geo", false)?;
        n.add(&r);
        bytes += f.size;
        if since.elapsed() >= Duration::from_secs(1) {
            tx.commit()?;
            d.progress(
                job.id,
                format!(
                    "{} of {} · {} with a place",
                    fmt::count(i as u64 + 1),
                    fmt::count(total),
                    fmt::count(n.located)
                ),
                Counts {
                    stage: "reading",
                    files: i as u64 + 1,
                    total_files: total,
                    bytes,
                    total_bytes,
                },
            );
            tx = Tx::begin(&db.conn, Lock::Immediate)?;
            since = Instant::now();
        }
    }
    tx.commit()?;
    if total == 0 {
        return Ok("no indexed photos or videos there".into());
    }
    Ok(n.summary())
}
