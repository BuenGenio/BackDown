//! `backdown tag|flag|edge`: the model layer — tags (facts about content), flags (decisions about
//! items) and relationship edges — over the index (`backdown_core::tags`). Short, direct writes to
//! the index, like `manifest`. See docs/MODEL.md and docs/TAGGING.md.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use backdown_core::db::{Lock, Tx};
use backdown_core::{Db, fmt, geo, paths, places, tags};
use clap::Subcommand;
use rusqlite::OptionalExtension;

use crate::{client, locations};

#[derive(Subcommand)]
pub enum TagCmd {
    /// Create a tag (or find the matching one), and optionally attach it to a target
    Add {
        /// What it is about: owner, kind, location, country, city, place, airport, event, date, memory
        #[arg(long)]
        kind: String,
        /// Its value, e.g. "Hong Kong" or "HKG"
        #[arg(long)]
        value: String,
        /// A parent tag id, for a hierarchy (a city under its country)
        #[arg(long)]
        parent: Option<i64>,
        /// Extra data as JSON (coordinates, codes)
        #[arg(long)]
        data: Option<String>,
        /// Attach it to this target: content:<sha256hex>, subject:<key>, entity:<key> or path:<key>
        #[arg(long, value_name = "TARGET")]
        to: Option<String>,
        /// Where the tag came from
        #[arg(long, default_value = "user")]
        source: String,
        /// How sure, 0..1
        #[arg(long, default_value_t = 1.0)]
        confidence: f64,
    },
    /// Remove a tag from a target
    Rm {
        /// The tag id (`backdown tag list`)
        #[arg(long)]
        tag: i64,
        /// The target to remove it from
        #[arg(long, value_name = "TARGET")]
        from: String,
    },
    /// List tags, optionally of one kind
    List {
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show the tags a target carries
    Of {
        /// content:<sha256hex>, subject:<key>, entity:<key> or path:<key>
        target: String,
        #[arg(long)]
        json: bool,
    },
    /// Read a photo or video's own metadata (EXIF / QuickTime) and attach date and location tags
    Exif {
        /// The files to read, or folders: every indexed photo and video under them (each must be
        /// indexed; one `backdown hash` hasn't read is hashed now)
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Also tag the country, region, city and nearest airport of each position, as `tag geo`
        /// does
        #[arg(long)]
        geo: bool,
        /// In folders, read again what was read before (named files always are)
        #[arg(long)]
        again: bool,
        /// Where the derived tags are recorded as coming from
        #[arg(long, default_value = "exif")]
        source: String,
    },
    /// Find content by where it was taken and what it's tagged with: everything within a distance
    /// of a place, carrying tags, taken between dates
    Find {
        /// Around a place: an airport's code (HKG), a town or city (Paris, or "Paris, Texas"), or a
        /// position (22.3089,113.9146)
        #[arg(long, value_name = "PLACE")]
        near: Option<String>,
        /// How far from it, in km
        #[arg(long, default_value_t = places::DEFAULT_KM)]
        km: f64,
        /// A tag it must carry: KIND=VALUE (airport=HKG, country=Hong Kong, event=Boat show 2019)
        /// or a tag id from `tag list`. Repeat it for more; all must be there
        #[arg(long = "tag", value_name = "KIND=VALUE")]
        tags: Vec<String>,
        /// Taken on or after: YYYY, YYYY-MM or YYYY-MM-DD
        #[arg(long, value_name = "DATE")]
        from: Option<String>,
        /// Taken on or before: YYYY, YYYY-MM or YYYY-MM-DD
        #[arg(long, value_name = "DATE")]
        to: Option<String>,
        /// nearest (the default around a place), newest (otherwise) or oldest
        #[arg(long)]
        sort: Option<String>,
        /// Show at most this many (0: all of them)
        #[arg(long, default_value_t = 0)]
        limit: usize,
        /// Print only the path of every copy, one per line, for scripts (`| xargs …`)
        #[arg(long, conflicts_with = "json")]
        paths: bool,
        /// Print it as JSON, for scripts
        #[arg(long)]
        json: bool,
    },
    /// Derive country, region, city and nearest-airport tags from the `location` tags already on
    /// content (attach those first with `tag exif`). Offline, from embedded reference data.
    Geo {
        /// Limit to these content items (content:<sha256hex>); with none, every located item
        targets: Vec<String>,
        /// Where the derived tags are recorded as coming from
        #[arg(long, default_value = "geo")]
        source: String,
        /// Print the embedded dataset sizes and data attribution, then exit
        #[arg(long)]
        about: bool,
    },
}

#[derive(Subcommand)]
pub enum FlagCmd {
    /// Set a flag on an item
    Set {
        /// content:<sha256hex>, subject:<key>, entity:<key> or path:<key>
        target: String,
        /// canonical | duplicate | keep | drop | reviewed | owner-confirmed | mine | not-mine
        state: String,
        /// For a duplicate: the canonical copy's target
        #[arg(long, value_name = "TARGET")]
        canonical: Option<String>,
        /// Who decided
        #[arg(long, default_value = "user")]
        by: String,
        #[arg(long)]
        note: Option<String>,
    },
    /// Clear a flag from an item
    Clear { target: String, state: String },
    /// Show an item's flags
    Of {
        target: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum EdgeCmd {
    /// Add a relationship edge: SRC REL DST, each node as type:key
    Add {
        src: String,
        rel: String,
        dst: String,
        #[arg(long)]
        data: Option<String>,
    },
    /// Edges out of a node
    From {
        src: String,
        #[arg(long)]
        json: bool,
    },
    /// Edges into a node
    To {
        dst: String,
        #[arg(long)]
        json: bool,
    },
}

const TARGET_TYPES: &[&str] = &[
    tags::TARGET_CONTENT,
    tags::TARGET_SUBJECT,
    tags::TARGET_ENTITY,
    tags::TARGET_PATH,
];

/// Parse and validate a tag/flag target `TYPE:KEY`.
fn target(s: &str) -> Result<(String, String)> {
    let Some((ty, key)) = s.split_once(':') else {
        bail!(
            "a target is TYPE:KEY, e.g. content:<sha256hex> (types: {})",
            TARGET_TYPES.join(", ")
        );
    };
    if !TARGET_TYPES.contains(&ty) {
        bail!(
            "unknown target type {ty:?}; use one of {}",
            TARGET_TYPES.join(", ")
        );
    }
    if key.is_empty() {
        bail!("a target needs a key after the colon");
    }
    Ok((ty.to_string(), key.to_string()))
}

/// Parse an edge endpoint `TYPE:KEY` (any type: a node can be content, a subject, an entity, or a
/// tag such as place:fcc).
fn node(s: &str) -> Result<(String, String)> {
    let Some((ty, key)) = s.split_once(':') else {
        bail!("a node is TYPE:KEY, e.g. content:<sha256hex> or place:fcc");
    };
    if ty.is_empty() || key.is_empty() {
        bail!("a node is TYPE:KEY with both parts non-empty");
    }
    Ok((ty.to_string(), key.to_string()))
}

fn index_path(db: Option<PathBuf>) -> PathBuf {
    db.or_else(|| client::Client::connect(None).map(|c| PathBuf::from(c.db)))
        .unwrap_or_else(locations::default_db)
}

fn open(db: Option<PathBuf>) -> Result<Db> {
    Db::open(&index_path(db))
}

/// A path as typed, as the index spells it (absolute, links resolved, names in the index's
/// Unicode form).
fn as_indexed(db: &Db, path: &Path) -> PathBuf {
    let canon = paths::canonical(path)
        .unwrap_or_else(|_| std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()));
    backdown_core::names::as_indexed_or_same(db, &canon)
}

/// An indexed file as the index has it now, or why it isn't there.
fn indexed_file(db: &Db, path: &Path) -> Result<Result<places::MediaFile, String>> {
    let path = as_indexed(db, path);
    let row: Option<(i64, i64, i64, Option<Vec<u8>>)> = db
        .conn
        .query_row(
            "SELECT id, size, mtime, sha256 FROM files WHERE path = ?1 AND current = 1",
            rusqlite::params![paths::to_bytes(&path)],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    Ok(match row {
        None => Err("not in the index (run `backdown index` on its drive first)".into()),
        Some((id, size, mtime, sha256)) => Ok(places::MediaFile {
            id,
            path,
            size: size.max(0) as u64,
            mtime,
            sha256,
        }),
    })
}

/// Read the files and folders `tag exif` was given: each named file, and under each folder every
/// indexed photo and video not read before (`again`: those too). Written a batch at a time.
fn read_exif(db: &Db, given: &[PathBuf], geo: bool, again: bool, source: &str) -> Result<()> {
    let geo = if geo {
        Some(geo::GeoRef::embedded()?)
    } else {
        None
    };
    let mut n = ReadCounts::default();
    let mut tx = Tx::begin(&db.conn, Lock::Immediate)?;
    let mut since = std::time::Instant::now();
    for p in given {
        let (files, named) = if p.is_dir() {
            let dir = as_indexed(db, p);
            let files = places::media_under(&db.conn, &dir)?;
            if files.is_empty() {
                eprintln!(
                    "{}: no indexed photos or videos under it (run `backdown index` on its drive first)",
                    p.display()
                );
            }
            (files, false)
        } else {
            match indexed_file(db, p)? {
                Ok(f) => (vec![f], true),
                Err(why) => {
                    eprintln!("{}: {why} — skipped", p.display());
                    n.unreadable += 1;
                    continue;
                }
            }
        };
        for f in &files {
            let shown = if named {
                p.display().to_string()
            } else {
                paths::show(&f.path)
            };
            let read = places::read_file(&db.conn, f, geo, source, "geo", named || again)?;
            n.add(&read);
            match read {
                places::Read::Already { .. } => {}
                places::Read::Unreadable { why } => eprintln!("{shown}: {why} — skipped"),
                places::Read::Nothing { .. } => {
                    println!("{shown}: no date or location in its metadata")
                }
                places::Read::Tagged { applied, .. } => println!("{shown}: {}", applied.join(", ")),
            }
            // a batch at a time, so a daemon writing too waits seconds, not the whole folder
            if since.elapsed() >= std::time::Duration::from_secs(2) {
                tx.commit()?;
                tx = Tx::begin(&db.conn, Lock::Immediate)?;
                since = std::time::Instant::now();
            }
        }
    }
    tx.commit()?;
    if n.files() > 1 || given.iter().any(|p| p.is_dir()) {
        println!("{}", n.summary());
    }
    Ok(())
}

/// What reading a batch of photos and videos came to.
#[derive(Default)]
pub struct ReadCounts {
    pub tagged: u64,
    pub located: u64,
    pub nothing: u64,
    pub already: u64,
    pub unreadable: u64,
}

impl ReadCounts {
    pub fn add(&mut self, r: &places::Read) {
        match r {
            places::Read::Already { .. } => self.already += 1,
            places::Read::Unreadable { .. } => self.unreadable += 1,
            places::Read::Nothing { .. } => self.nothing += 1,
            places::Read::Tagged { located, .. } => {
                self.tagged += 1;
                self.located += u64::from(*located);
            }
        }
    }

    pub fn files(&self) -> u64 {
        self.tagged + self.nothing + self.already + self.unreadable
    }

    pub fn summary(&self) -> String {
        let mut s = format!(
            "{} photos and videos: {} with a date or position ({} with a position), {} with neither",
            fmt::count(self.files()),
            fmt::count(self.tagged),
            fmt::count(self.located),
            fmt::count(self.nothing)
        );
        if self.already > 0 {
            s += &format!(", {} read before", fmt::count(self.already));
        }
        if self.unreadable > 0 {
            s += &format!(
                ", {} changed since their scan or unreadable (scan the drive again)",
                fmt::count(self.unreadable)
            );
        }
        s
    }
}

/// `tag find`: the search, printed.
#[allow(clippy::too_many_arguments)]
fn find(
    db: &Db,
    near: Option<String>,
    km: f64,
    tags_wanted: Vec<String>,
    from: Option<String>,
    to: Option<String>,
    sort: Option<String>,
    limit: usize,
    only_paths: bool,
    json: bool,
) -> Result<()> {
    if !(km > 0.0 && km.is_finite()) {
        bail!("--km is a distance above 0");
    }
    let centre = match &near {
        None => None,
        Some(q) => Some(geo::GeoRef::embedded()?.resolve(q).with_context(|| {
            format!("don't know where {q:?} is: give an airport's code (HKG), a town (Paris, \"Paris, Texas\") or a position (22.3089,113.9146)")
        })?),
    };
    let query = places::Query {
        near: centre.as_ref().map(|c| places::Near {
            lat: c.lat,
            lon: c.lon,
            km,
        }),
        tags: tags_wanted
            .iter()
            .map(|t| places::TagFilter::parse(t))
            .collect::<Result<_>>()?,
        from,
        to,
        sort: places::Sort::parse(sort.as_deref().unwrap_or(""))?,
        offset: 0,
        limit: if only_paths { 0 } else { limit },
    };
    let found = places::find(&db.conn, &query)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "centre": centre,
                "km": centre.as_ref().map(|_| km),
                "found": found,
            }))?
        );
        return Ok(());
    }
    if only_paths {
        for h in &found.hits {
            for c in &h.copies {
                println!("{}", c.path);
            }
        }
        return Ok(());
    }
    // what was asked, in words
    let mut asked = Vec::new();
    if let Some(c) = &centre {
        let what = if c.detail.is_empty() {
            c.label.clone()
        } else {
            format!("{} ({})", c.label, c.detail)
        };
        asked.push(format!("within {} km of {what}", trim_km(km)));
    }
    for t in &tags_wanted {
        asked.push(format!("tagged {t}"));
    }
    match (&query.from, &query.to) {
        (Some(f), Some(t)) => asked.push(format!("taken {f} to {t}")),
        (Some(f), None) => asked.push(format!("taken from {f}")),
        (None, Some(t)) => asked.push(format!("taken up to {t}")),
        (None, None) => {}
    }
    let asked = if asked.is_empty() {
        "Everything tagged".to_string()
    } else {
        capitalise(&asked.join(", "))
    };
    if found.total == 0 {
        println!("{asked}: nothing found");
        let (tagged, located) = places::counts(&db.conn)?;
        if tagged == 0 {
            println!(
                "Nothing is tagged yet: read your photos' dates and places first, with `backdown tag exif FOLDER --geo`."
            );
        } else if centre.is_some() && located == 0 {
            println!("No photo has a position yet: `backdown tag exif FOLDER --geo` reads them.");
        }
        return Ok(());
    }
    println!(
        "{asked}: {} found{}",
        fmt::count(found.total as u64),
        if found.located < found.total && centre.is_none() {
            format!(", {} with a position", fmt::count(found.located as u64))
        } else {
            String::new()
        }
    );
    for h in &found.hits {
        let km =
            h.km.map_or(String::new(), |d| format!("{:>7} km  ", trim_km(d)));
        let date = h.date.as_deref().unwrap_or("—");
        let value = |kind: &str| {
            h.tags
                .iter()
                .find(|t| t.kind == kind)
                .map(|t| t.value.as_str())
        };
        let mut place: Vec<&str> = [value("city"), value("country")]
            .into_iter()
            .flatten()
            .collect();
        place.dedup();
        let mut place = place.join(", ");
        if let Some(a) = value("airport") {
            place += &format!("{}{a}", if place.is_empty() { "" } else { " · " });
        }
        let at = match h.copies.split_first() {
            None => format!("(no copy on an indexed drive now: content:{})", h.key),
            Some((c, [])) => c.path.clone(),
            Some((c, more)) => format!(
                "{} (+{} {})",
                c.path,
                more.len(),
                if more.len() == 1 { "copy" } else { "copies" }
            ),
        };
        println!("{km}{date:<10}  {place:<32}  {at}");
    }
    if found.hits.len() < found.total {
        println!(
            "… and {} more (--limit 0 shows them all)",
            fmt::count((found.total - found.hits.len()) as u64)
        );
    }
    Ok(())
}

/// A distance without trailing zeros: 120, 2.5, 0.25.
fn trim_km(km: f64) -> String {
    let s = if km >= 100.0 {
        format!("{km:.0}")
    } else if km >= 10.0 {
        format!("{km:.1}")
    } else {
        format!("{km:.2}")
    };
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

pub fn tag(db: Option<PathBuf>, action: TagCmd) -> Result<ExitCode> {
    // `geo --about` only reads embedded data, so answer it without touching the index.
    if let TagCmd::Geo { about: true, .. } = &action {
        let g = geo::GeoRef::embedded()?;
        let (places, airports) = g.counts();
        println!("{places} places, {airports} airports embedded");
        println!("{}", geo::CREDIT);
        return Ok(ExitCode::SUCCESS);
    }
    let db = open(db)?;
    match action {
        TagCmd::Add {
            kind,
            value,
            parent,
            data,
            to,
            source,
            confidence,
        } => {
            let id = tags::upsert_tag(&db.conn, &kind, &value, parent, data.as_deref())?;
            if let Some(t) = to {
                let (ty, key) = target(&t)?;
                tags::attach(&db.conn, &ty, &key, id, &source, confidence)?;
                println!("tag {id} ({kind}={value}) attached to {t}");
            } else {
                println!("tag {id} ({kind}={value})");
            }
        }
        TagCmd::Rm { tag, from } => {
            let (ty, key) = target(&from)?;
            tags::detach(&db.conn, &ty, &key, tag)?;
            println!("tag {tag} removed from {from}");
        }
        TagCmd::List { kind, json } => {
            let v = tags::list_tags(&db.conn, kind.as_deref())?;
            if json {
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else if v.is_empty() {
                println!("no tags yet");
            } else {
                for t in &v {
                    let parent = t
                        .parent_id
                        .map(|p| format!("  (parent {p})"))
                        .unwrap_or_default();
                    println!("{:>6}  {:<9} {}{parent}", t.id, t.kind, t.value);
                }
            }
        }
        TagCmd::Of { target: tgt, json } => {
            let (ty, key) = target(&tgt)?;
            let v = tags::tags_of(&db.conn, &ty, &key)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else if v.is_empty() {
                println!("no tags on {tgt}");
            } else {
                for g in &v {
                    let conf = if (g.confidence - 1.0).abs() > f64::EPSILON {
                        format!(" {:.2}", g.confidence)
                    } else {
                        String::new()
                    };
                    println!("{:<9} {}  [{}{conf}]", g.tag.kind, g.tag.value, g.source);
                }
            }
        }
        TagCmd::Exif {
            paths,
            geo,
            again,
            source,
        } => read_exif(&db, &paths, geo, again, &source)?,
        TagCmd::Find {
            near,
            km,
            tags,
            from,
            to,
            sort,
            limit,
            paths,
            json,
        } => find(&db, near, km, tags, from, to, sort, limit, paths, json)?,
        TagCmd::Geo {
            targets,
            source,
            about: _, // handled before the index is opened
        } => {
            let geo = geo::GeoRef::embedded()?;
            let mut located = geo::content_locations(&db.conn)?;
            if !targets.is_empty() {
                let mut want = HashSet::new();
                for t in &targets {
                    let (ty, key) = target(t)?;
                    if ty != tags::TARGET_CONTENT {
                        bail!("tag geo works on content: targets; got {t}");
                    }
                    want.insert(key);
                }
                located.retain(|(k, _, _)| want.contains(k));
            }
            if located.is_empty() {
                println!("no content carries a location tag — run `backdown tag exif` first");
            }
            for (key, lat, lon) in located {
                let applied = geo::derive_into(&db.conn, geo, &key, lat, lon, &source)?;
                let what = if applied.is_empty() {
                    "no match".to_string()
                } else {
                    applied.join(", ")
                };
                println!("content:{key}: {what}");
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

pub fn flag(db: Option<PathBuf>, action: FlagCmd) -> Result<ExitCode> {
    let db = open(db)?;
    match action {
        FlagCmd::Set {
            target: tgt,
            state,
            canonical,
            by,
            note,
        } => {
            let (ty, key) = target(&tgt)?;
            if let Some(c) = &canonical {
                target(c)?; // validate
            }
            tags::set_flag(
                &db.conn,
                &ty,
                &key,
                &state,
                canonical.as_deref(),
                &by,
                note.as_deref(),
            )?;
            let via = canonical
                .map(|c| format!(" (canonical {c})"))
                .unwrap_or_default();
            println!("{tgt} flagged {state}{via}");
        }
        FlagCmd::Clear { target: tgt, state } => {
            let (ty, key) = target(&tgt)?;
            tags::clear_flag(&db.conn, &ty, &key, &state)?;
            println!("{tgt} flag {state} cleared");
        }
        FlagCmd::Of { target: tgt, json } => {
            let (ty, key) = target(&tgt)?;
            let v = tags::flags_of(&db.conn, &ty, &key)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else if v.is_empty() {
                println!("no flags on {tgt}");
            } else {
                for f in &v {
                    let canon = f
                        .canonical_key
                        .as_ref()
                        .map(|c| format!("  → {c}"))
                        .unwrap_or_default();
                    println!("{:<16} by {}{canon}", f.state, f.decided_by);
                }
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

pub fn edge(db: Option<PathBuf>, action: EdgeCmd) -> Result<ExitCode> {
    let db = open(db)?;
    match action {
        EdgeCmd::Add {
            src,
            rel,
            dst,
            data,
        } => {
            let (st, sk) = node(&src)?;
            let (dt, dk) = node(&dst)?;
            tags::add_edge(&db.conn, &st, &sk, &rel, &dt, &dk, data.as_deref())?;
            println!("{src} -{rel}-> {dst}");
        }
        EdgeCmd::From { src, json } => {
            let (st, sk) = node(&src)?;
            print_edges(&tags::edges_from(&db.conn, &st, &sk)?, json)?;
        }
        EdgeCmd::To { dst, json } => {
            let (dt, dk) = node(&dst)?;
            print_edges(&tags::edges_to(&db.conn, &dt, &dk)?, json)?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn print_edges(v: &[tags::Edge], json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(v)?);
    } else if v.is_empty() {
        println!("no edges");
    } else {
        for e in v {
            println!(
                "{}:{} -{}-> {}:{}",
                e.src_type, e.src_key, e.rel, e.dst_type, e.dst_key
            );
        }
    }
    Ok(())
}
