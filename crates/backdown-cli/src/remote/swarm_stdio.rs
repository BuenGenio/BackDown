//! `backdown swarm-stdio`: the receiving end of copying into the pool (docs/SWARM.md §5), run by
//! ssh on the machine whose pool folder files go into, as the person whose keys reach it; and the
//! sending side's way of reaching it (`Ssh`, a `send::Connect`).
//!
//! One JSON request per line on stdin, one JSON answer per line on stdout, like `serve-stdio`;
//! a `put`'s file follows its request line raw, exactly `size` bytes. It writes only into this
//! machine's own pool folders, as backdown_core::pool::receive does whoever asks:
//!
//! - `hello {from}`: who's sending (a machine's name); answers with this end's version and its
//!   pool folders with their room now
//! - `put {area, path, size, mtime, src, plan}` + the file: written, read back, answered with what
//!   became of it and the SHA-256 read back
//! - `done {area}`: everything for that pool folder has come: it's indexed (by this machine's
//!   BackDown service when one runs, else by a `backdown index` started now)
//!
//! A request it can't read (or one with a size it can't trust) ends the conversation: the stream
//! can't be trusted to be in step after it.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use backdown_core::pool::plan::AreaRef;
use backdown_core::pool::receive::{self, Origin, Received};
use backdown_core::pool::send::{self, Sink};
use backdown_core::remote as core;
use backdown_core::{Db, browse, names, pool};
use serde::Deserialize;
use serde_json::{Value, json};

use super::ssh::SshPeer;

/// The protocol both sides speak.
pub const PROTO: u32 = 1;
const MAX_LINE: usize = 64 * 1024;

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case", deny_unknown_fields)]
enum Request {
    Hello {
        #[serde(default)]
        from: Option<String>,
    },
    Put {
        area: i64,
        path: String,
        size: u64,
        mtime: i64,
        #[serde(default)]
        src: Option<String>,
        #[serde(default)]
        plan: Option<i64>,
    },
    Done {
        area: i64,
    },
}

/// A machine's name as it says it: printable, short.
fn clean_name(s: &str) -> String {
    let s: String = s.chars().filter(|c| !c.is_control()).take(64).collect();
    if s.trim().is_empty() {
        "another machine".into()
    } else {
        s
    }
}

/// The pool folder `area` is indexed: by this machine's BackDown service if one runs, else by a
/// `backdown index` of its own started now (it outlives this conversation). What was done.
fn index_after(db_path: &Path, db: &Db, area: &pool::Area) -> Result<String> {
    let folder = PathBuf::from(&area.path);
    // the indexed drive whose scans cover it (a pool folder is often a folder of a big drive),
    // else the folder itself
    let root = browse::roots(db)?
        .into_iter()
        .filter(|r| names::within(&folder, Path::new(&r.path)))
        .max_by_key(|r| r.path.len())
        .map_or(folder.clone(), |r| PathBuf::from(r.path));
    let shown = root.display().to_string();
    if let Some(mut c) = crate::client::Client::connect(Some(db_path))
        && c.request(&json!({ "cmd": "enqueue", "kind": "index", "path": shown }))
            .is_ok_and(|r| r["ok"] == true)
    {
        return Ok(format!("BackDown's service there is indexing {shown}"));
    }
    let exe = std::env::current_exe().context("where this backdown is")?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--db")
        .arg(db_path)
        .arg("--local")
        .arg("index")
        .arg(&root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0); // not ended with this ssh session
    }
    cmd.spawn().context("starting backdown index there")?;
    Ok(format!("indexing {shown} now"))
}

pub fn run(db: Option<PathBuf>) -> Result<ExitCode> {
    let path = super::stdio::which_db(db);
    let stdin = io::stdin();
    let mut input = BufReader::new(stdin.lock());
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let opened = if path.is_file() {
        Db::open(&path).map_err(|e| format!("{e:#}"))
    } else {
        Err(format!(
            "there's no BackDown index at {} on this machine",
            path.display()
        ))
    };
    let mut from = String::from("another machine");
    loop {
        let mut line = String::new();
        let n = (&mut input)
            .take(MAX_LINE as u64 + 1)
            .read_line(&mut line)?;
        if n == 0 {
            break;
        }
        if line.len() > MAX_LINE {
            writeln!(
                out,
                "{}",
                json!({ "id": null, "ok": false, "error": "request too long" })
            )?;
            break;
        }
        let mut v: Value = match serde_json::from_str(line.trim_end()) {
            Ok(v) => v,
            Err(e) => {
                writeln!(
                    out,
                    "{}",
                    json!({ "id": null, "ok": false, "error": format!("not JSON: {e}") })
                )?;
                break;
            }
        };
        let id = v.get("id").cloned().unwrap_or(Value::Null);
        if let Some(o) = v.as_object_mut() {
            o.remove("id");
            o.remove("v");
        }
        let req: Request = match serde_json::from_value(v) {
            Ok(r) => r,
            Err(e) => {
                // its size can't be trusted, so neither can the rest of the stream
                writeln!(
                    out,
                    "{}",
                    json!({ "id": id, "ok": false, "error": format!("bad request: {e}") })
                )?;
                break;
            }
        };
        let answer: Result<Value> = match (&opened, req) {
            (_, Request::Hello { from: f }) => {
                from = clean_name(f.as_deref().unwrap_or(""));
                match &opened {
                    Ok(db) => Ok(json!({
                        "proto": PROTO, "version": env!("CARGO_PKG_VERSION"),
                        "host": super::stdio::host_name(), "db": path.display().to_string(),
                        "areas": pool::report(db)?,
                    })),
                    Err(e) => Err(anyhow::anyhow!("{e}")),
                }
            }
            (Err(e), Request::Put { size, .. }) => {
                io::copy(&mut (&mut input).take(size), &mut io::sink())?;
                Err(anyhow::anyhow!("{e}"))
            }
            (
                Ok(db),
                Request::Put {
                    area,
                    path: rel,
                    size,
                    mtime,
                    src,
                    plan,
                },
            ) => match pool::get(db, area) {
                Ok(a) => {
                    let shown = src.unwrap_or_default();
                    let origin = Origin {
                        machine: &from,
                        path: &shown,
                        plan: plan.unwrap_or(0),
                    };
                    receive::put(db, &a, &rel, size, mtime, &mut input, &origin).map(|r| json!(r))
                }
                Err(e) => {
                    io::copy(&mut (&mut input).take(size), &mut io::sink())?;
                    Err(e)
                }
            },
            (Err(e), Request::Done { .. }) => Err(anyhow::anyhow!("{e}")),
            (Ok(db), Request::Done { area }) => pool::get(db, area)
                .and_then(|a| index_after(&path, db, &a))
                .map(|said| json!({ "said": said })),
        };
        let line = match answer {
            Ok(data) => json!({ "id": id, "ok": true, "data": data }),
            Err(e) => json!({ "id": id, "ok": false, "error": format!("{e:#}") }),
        };
        writeln!(out, "{line}")?;
        out.flush()?;
    }
    Ok(ExitCode::SUCCESS)
}

// ---------- the sending side

/// One machine's pool folders, reached with ssh (`swarm-stdio` there).
struct SshSink {
    peer: SshPeer,
    name: String,
}

impl Sink for SshSink {
    fn put(
        &mut self,
        area: &AreaRef,
        rel: &str,
        size: u64,
        mtime: i64,
        src: &mut dyn Read,
        origin: &Origin,
    ) -> Result<std::result::Result<Received, String>> {
        // a minute, and as long as the file takes at 1 MB/s (written, flushed and read back there)
        let timeout = Duration::from_secs(60 + size / 1_000_000);
        let v = self.peer.exchange(
            "put",
            json!({ "area": area.area, "path": rel, "size": size, "mtime": mtime,
                    "src": origin.path, "plan": origin.plan }),
            Some((src, size)),
            timeout,
        )?;
        if v["ok"] != true {
            return Ok(Err(v["error"]
                .as_str()
                .unwrap_or("it refused the file")
                .to_string()));
        }
        let d = &v["data"];
        let outcome = match d["outcome"].as_str() {
            Some("copied") => "copied",
            Some("renamed") => "renamed",
            Some("same") => "same",
            _ => bail!("{} answered something unexpected about {rel}", self.name),
        };
        Ok(Ok(Received {
            outcome,
            path: d["path"].as_str().unwrap_or(rel).to_string(),
            sha256: d["sha256"].as_str().unwrap_or("").to_string(),
        }))
    }

    fn done(&mut self, area: &AreaRef) -> Result<String> {
        let v = self.peer.exchange(
            "done",
            json!({ "area": area.area }),
            None,
            Duration::from_secs(60),
        )?;
        if v["ok"] != true {
            bail!("{}", v["error"].as_str().unwrap_or("it refused"));
        }
        Ok(v["data"]["said"].as_str().unwrap_or("").to_string())
    }
}

/// Reaches other machines' `swarm-stdio` with ssh, as `name` (this machine), the machines as this
/// index (`db`) has them.
pub struct Ssh {
    pub db: PathBuf,
    pub name: String,
}

impl send::Connect for Ssh {
    fn connect(&mut self, machine_id: i64) -> Result<Box<dyn Sink>> {
        let db = Db::open_read_only(&self.db)?;
        let m = core::get(&db, machine_id)?;
        let mut peer = SshPeer::open_for(&m, "swarm-stdio")?;
        let v = peer.exchange(
            "hello",
            json!({ "from": self.name }),
            None,
            Duration::from_secs(60),
        )?;
        if v["ok"] != true {
            bail!(
                "{}: {}",
                m.name,
                v["error"].as_str().unwrap_or("it refused")
            );
        }
        if v["data"]["proto"].as_u64() != Some(PROTO as u64) {
            bail!(
                "{} runs BackDown {}, which can't take files into its pool folders this way: update it",
                m.name,
                v["data"]["version"].as_str().unwrap_or("(older)")
            );
        }
        Ok(Box::new(SshSink { peer, name: m.name }))
    }
}
