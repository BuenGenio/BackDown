//! The CLI's side of the daemon's control socket: find a running daemon, send it requests, and
//! follow a job's progress. Unix only for now; elsewhere the CLI always works on its own.

use std::path::Path;

use anyhow::Result;
use serde_json::Value;

#[cfg(unix)]
pub struct Client {
    reader: std::io::BufReader<std::os::unix::net::UnixStream>,
    writer: std::os::unix::net::UnixStream,
    pub db: String,
}

#[cfg(not(unix))]
pub struct Client {
    pub db: String,
}

#[cfg(unix)]
impl Client {
    /// The first reachable daemon (this user's, then the system's). With `want_db`, only one
    /// that serves that same index file.
    pub fn connect(want_db: Option<&Path>) -> Option<Client> {
        use std::os::unix::net::UnixStream;
        use std::time::Duration;
        let want = want_db.map(|p| dunce::canonicalize(p).ok());
        for sock in crate::locations::candidate_sockets() {
            let Ok(stream) = UnixStream::connect(&sock) else {
                continue;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
            let Ok(writer) = stream.try_clone() else {
                continue;
            };
            let mut c = Client {
                reader: std::io::BufReader::new(stream),
                writer,
                db: String::new(),
            };
            let Ok(hello) = c.request(&serde_json::json!({ "cmd": "hello" })) else {
                continue;
            };
            let db = hello["db"].as_str().unwrap_or_default().to_string();
            match &want {
                Some(Some(w)) if Path::new(&db) != w.as_path() => continue,
                Some(None) => return None, // an --db that doesn't exist yet can't be a daemon's
                _ => {}
            }
            let _ = c.reader.get_ref().set_read_timeout(None); // jobs may run for hours
            c.db = db;
            return Some(c);
        }
        None
    }

    pub fn request(&mut self, req: &Value) -> Result<Value> {
        use std::io::{BufRead, Write};
        writeln!(self.writer, "{req}")?;
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            anyhow::bail!("the daemon closed the connection");
        }
        let v: Value = serde_json::from_str(line.trim())?;
        if v["ok"] != true {
            anyhow::bail!(
                "{}",
                v["error"]
                    .as_str()
                    .unwrap_or("the daemon refused the request")
            );
        }
        Ok(v)
    }

    /// Follow a job until it ends; `progress` sees each update. Errors if the job failed.
    pub fn follow(&mut self, job: u64, mut progress: impl FnMut(&Value)) -> Result<Value> {
        use std::io::{BufRead, Write};
        writeln!(
            self.writer,
            "{}",
            serde_json::json!({ "cmd": "follow", "job": job })
        )?;
        loop {
            let mut line = String::new();
            if self.reader.read_line(&mut line)? == 0 {
                anyhow::bail!("the daemon stopped while job {job} was running");
            }
            let v: Value = serde_json::from_str(line.trim())?;
            if v["ok"] != true {
                anyhow::bail!("{}", v["error"].as_str().unwrap_or("unknown job"));
            }
            let j = &v["job"];
            match j["state"].as_str() {
                Some("done") => return Ok(j.clone()),
                Some("failed") => anyhow::bail!("{}", j["error"].as_str().unwrap_or("job failed")),
                _ => progress(j),
            }
        }
    }
}

#[cfg(not(unix))]
impl Client {
    pub fn connect(_: Option<&Path>) -> Option<Client> {
        None
    }
    pub fn request(&mut self, _: &Value) -> Result<Value> {
        unreachable!("no daemon client on this platform")
    }
    pub fn follow(&mut self, _: u64, _: impl FnMut(&Value)) -> Result<Value> {
        unreachable!("no daemon client on this platform")
    }
}
