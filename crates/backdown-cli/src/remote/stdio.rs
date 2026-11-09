//! `backdown serve-stdio`: the other machine's end of `backdown remote`, run there by ssh.
//!
//! One JSON request per line on stdin, one JSON answer per line on stdout, nothing else on stdout
//! (backdown_core::remote::serve has the protocol). Read-only: the index is opened read-only and
//! never created or migrated, and nothing is written anywhere. A line longer than the limit is
//! refused without being kept; each request gets a time limit (an index query running over it is
//! interrupted), and the process ends after 15 minutes without a request, or when stdin closes.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use backdown_core::Db;
use backdown_core::remote::serve::{self, Here};
use serde_json::json;

const IDLE: Duration = Duration::from_secs(15 * 60);
const MAX_ANSWER: usize = 48 << 20;

pub(crate) fn host_name() -> String {
    // for tests and demos: what this machine calls itself to the swarm, instead of its hostname
    if let Some(n) = std::env::var_os("BACKDOWN_HOSTNAME").filter(|n| !n.is_empty()) {
        return n.to_string_lossy().trim().to_string();
    }
    for f in ["/proc/sys/kernel/hostname", "/etc/hostname"] {
        if let Ok(s) = std::fs::read_to_string(f)
            && !s.trim().is_empty()
        {
            return s.trim().to_string();
        }
    }
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_default()
}

/// The index to answer from: `--db`, else the one a running daemon here serves, else the default.
pub(crate) fn which_db(db: Option<PathBuf>) -> PathBuf {
    db.or_else(|| crate::client::Client::connect(None).map(|c| PathBuf::from(c.db)))
        .unwrap_or_else(crate::locations::default_db)
}

pub fn run(db: Option<PathBuf>) -> Result<ExitCode> {
    let path = which_db(db);
    let here = Here {
        db_path: path.display().to_string(),
        host: host_name(),
    };
    // never create an index here: a missing one is an answer, not a new file
    let opened = if path.is_file() {
        Db::open_read_only(&path).map_err(|e| format!("{e:#}"))
    } else {
        Err(format!(
            "there's no BackDown index at {} on {} (index something there first, or say where it is with `backdown remote add … --remote-db PATH`)",
            path.display(),
            if here.host.is_empty() {
                "that machine"
            } else {
                &here.host
            }
        ))
    };

    // a watchdog interrupts an index query that runs past its request's time limit
    let deadline: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
    if let Ok(db) = &opened {
        let handle = db.conn.get_interrupt_handle();
        let d = Arc::clone(&deadline);
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(200));
                if d.lock().unwrap().is_some_and(|t| Instant::now() >= t) {
                    handle.interrupt();
                }
            }
        });
    }

    // stdin, a line at a time, never more than the limit kept
    let (tx, rx) = channel::<Option<String>>();
    std::thread::spawn(move || {
        let mut r = BufReader::new(std::io::stdin().lock());
        loop {
            let mut buf = Vec::new();
            match (&mut r)
                .take(serve::MAX_LINE as u64 + 1)
                .read_until(b'\n', &mut buf)
            {
                Ok(0) | Err(_) => break,
                Ok(_) if buf.len() > serve::MAX_LINE && !buf.ends_with(b"\n") => {
                    // skip the rest of this line, then refuse it
                    let mut rest = Vec::new();
                    loop {
                        rest.clear();
                        match (&mut r).take(1 << 20).read_until(b'\n', &mut rest) {
                            Ok(0) | Err(_) => break,
                            Ok(_) if rest.ends_with(b"\n") => break,
                            Ok(_) => {}
                        }
                    }
                    if tx.send(None).is_err() {
                        break;
                    }
                }
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf).trim().to_string();
                    if tx.send(Some(line)).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let stdout = std::io::stdout();
    loop {
        let line = match rx.recv_timeout(IDLE) {
            Ok(Some(l)) if l.is_empty() => continue,
            Ok(l) => l,
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => break,
        };
        let answer = match (&line, &opened) {
            (None, _) => {
                json!({ "id": null, "ok": false, "error": "request too long" }).to_string()
            }
            (Some(l), Err(why)) => {
                let (id, _) = serve::parse(l);
                json!({ "id": id, "ok": false, "error": why }).to_string()
            }
            (Some(l), Ok(db)) => {
                let budget = match serve::parse(l) {
                    (_, Ok(req)) => req.budget_secs(),
                    _ => 5,
                };
                let until = Instant::now() + Duration::from_secs(budget);
                *deadline.lock().unwrap() = Some(until);
                let a = serve::answer_line(db, &here, l, until);
                *deadline.lock().unwrap() = None;
                if a.len() > MAX_ANSWER {
                    let (id, _) = serve::parse(l);
                    json!({ "id": id, "ok": false, "error": "the answer would be too long: ask for less" }).to_string()
                } else {
                    a
                }
            }
        };
        let mut out = stdout.lock();
        if writeln!(out, "{answer}").and_then(|_| out.flush()).is_err() {
            break; // the other side went away
        }
        // a `get` that was answered with a plan: exactly `len` raw bytes follow the line
        if let (Some(l), Ok(db)) = (&line, &opened)
            && let (_, Ok(serve::Request::Get { file, from, len })) = serve::parse(l)
            && let Ok(v) = serde_json::from_str::<serde_json::Value>(&answer)
            && v["ok"] == true
            && let Ok(plan) = serve::get_plan(db, file, from, len)
            && !stream_bytes(&mut out, &plan)
        {
            break; // the bytes stopped short: the promise can't be kept, so the conversation ends
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Write the planned bytes to `out`; false when the file didn't give them all (it changed, or
/// couldn't be read), in which case the other end sees a short stream and knows.
fn stream_bytes(out: &mut impl Write, plan: &serve::GetPlan) -> bool {
    use std::io::{Seek, SeekFrom};
    let Ok(mut f) = backdown_core::paths::open_read(&plan.path) else {
        return false;
    };
    if f.seek(SeekFrom::Start(plan.from)).is_err() {
        return false;
    }
    let mut left = plan.len;
    let mut buf = vec![0u8; 256 << 10];
    while left > 0 {
        let want = buf.len().min(left as usize);
        let n = match f.read(&mut buf[..want]) {
            Ok(0) | Err(_) => return false,
            Ok(n) => n,
        };
        if out.write_all(&buf[..n]).is_err() {
            return false;
        }
        left -= n as u64;
    }
    out.flush().is_ok()
}
