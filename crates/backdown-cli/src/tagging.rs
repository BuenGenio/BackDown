//! `backdown tag|flag|edge`: the model layer — tags (facts about content), flags (decisions about
//! items) and relationship edges — over the index (`backdown_core::tags`). Short, direct writes to
//! the index, like `manifest`. See docs/MODEL.md and docs/TAGGING.md.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Result, bail};
use backdown_core::{Db, exif, fmt, geo, paths, tags};
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
        /// The files to read (each must be indexed; one `backdown hash` hasn't read is hashed now)
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Where the derived tags are recorded as coming from
        #[arg(long, default_value = "exif")]
        source: String,
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

/// The content hash (hex) of an indexed file, or why there isn't one. `backdown hash` never reads a
/// file whose size no other file shares, which is most photos, so when the index has no hash this
/// reads the file now (it must still be as the index scanned it) and records the hash as `hash`
/// would, so the tags have content to attach to.
fn content_of(conn: &rusqlite::Connection, path: &Path) -> Result<Result<String, String>> {
    let canon = paths::canonical(path)
        .unwrap_or_else(|_| std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()));
    let key = paths::to_bytes(&canon);
    let row: Option<(i64, i64, i64, Option<Vec<u8>>)> = conn
        .query_row(
            "SELECT id, size, mtime, sha256 FROM files WHERE path = ?1 AND current = 1",
            rusqlite::params![key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((id, size, mtime, sha)) = row else {
        return Ok(Err(
            "not in the index (run `backdown index` on its drive first)".into(),
        ));
    };
    if let Some(sha) = sha {
        return Ok(Ok(fmt::hex(&sha)));
    }
    match backdown_core::hash::full_of(path, size as u64, mtime) {
        Ok(sha) => {
            conn.execute(
                "UPDATE files SET sha256 = ?1 WHERE id = ?2",
                rusqlite::params![sha, id],
            )?;
            Ok(Ok(fmt::hex(&sha)))
        }
        Err(why) => Ok(Err(why)),
    }
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
        TagCmd::Exif { paths, source } => {
            for p in &paths {
                match content_of(&db.conn, p)? {
                    Err(why) => eprintln!("{}: {why} — skipped", p.display()),
                    Ok(hex) => {
                        let meta = exif::read_meta(p);
                        if meta.is_empty() {
                            println!("{}: no date or location in its metadata", p.display());
                        } else {
                            let applied = exif::derive_into(&db.conn, &hex, &meta, &source)?;
                            println!("{}: {}", p.display(), applied.join(", "));
                        }
                    }
                }
            }
        }
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
