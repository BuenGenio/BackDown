//! Reaching another machine: the system's own `ssh`, run as the person (their keys, agent,
//! known_hosts and ~/.ssh/config), with `backdown serve-stdio` at the other end.
//!
//! No shell runs here: ssh gets its arguments as a vector. The host is validated
//! (`remote::parse_url`: nothing that could be read as an option) and comes after `--` anyway.
//! ssh itself joins the remote command into one string for the other machine's shell, so every
//! part of it is single-quoted for a POSIX shell here. `BatchMode=yes` means ssh never prompts
//! (no passwords, no "trust this host key?": an unknown or changed host key is an error, as host
//! key checking stays on); forwarding of agents, X11 and ports is turned off whatever the config
//! says.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use backdown_core::remote::{self, Machine, Peer, Target};
use serde_json::{Value, json};

/// The largest answer line accepted from the other machine.
const MAX_ANSWER: usize = 64 << 20;

/// `s` as one word for a POSIX shell: single-quoted, with each `'` written as `'\''`.
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The command the other machine's shell runs: `'backdown' 'serve-stdio' ['--db' '<path>']`.
pub fn remote_command(m: &Machine) -> Result<String> {
    remote_command_for(m, "serve-stdio")
}

/// The command for one of BackDown's ends there: `serve-stdio` (it reads), or `swarm-stdio` (it
/// writes into its own pool folders).
pub fn remote_command_for(m: &Machine, end: &str) -> Result<String> {
    if !matches!(end, "serve-stdio" | "swarm-stdio") {
        bail!("there's no end called {end}");
    }
    let cmd = m.command.as_deref().unwrap_or("backdown");
    remote::check_remote_path(cmd, "the backdown command there")?;
    let mut words = vec![quote(cmd), quote(end)];
    if let Some(db) = &m.db {
        remote::check_remote_path(db, "the index path there")?;
        words.push(quote("--db"));
        words.push(quote(db));
    }
    Ok(words.join(" "))
}

/// ssh's arguments, as a vector: options, `--`, the host, the (quoted) remote command.
pub fn ssh_args(t: &Target, remote_cmd: &str) -> Result<Vec<OsString>> {
    // parse_url refused these already; a host that got here by another way is refused again
    if t.host.starts_with('-') || t.user.as_deref().is_some_and(|u| u.starts_with('-')) {
        bail!("refusing a host or user name that starts with -");
    }
    let mut a: Vec<OsString> = [
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=10",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=3",
        "-o",
        "ForwardAgent=no",
        "-o",
        "ForwardX11=no",
        "-o",
        "ClearAllForwardings=yes",
        "-o",
        "RequestTTY=no",
        "-o",
        "PermitLocalCommand=no",
    ]
    .iter()
    .map(OsString::from)
    .collect();
    if let Some(p) = t.port {
        a.push("-p".into());
        a.push(p.to_string().into());
    }
    if let Some(u) = &t.user {
        a.push("-l".into());
        a.push(u.into());
    }
    a.push("--".into());
    a.push(t.host.clone().into());
    a.push(remote_cmd.into());
    Ok(a)
}

/// The command line, for people (`backdown remote test` prints it).
pub fn describe(m: &Machine) -> Result<String> {
    let args = ssh_args(&m.target()?, &remote_command(m)?)?;
    Ok(std::iter::once("ssh".to_string())
        .chain(args.iter().map(|a| {
            let s = a.to_string_lossy();
            if s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_=./:@".contains(c))
            {
                s.into_owned()
            } else {
                quote(&s)
            }
        }))
        .collect::<Vec<_>>()
        .join(" "))
}

/// A conversation with `backdown serve-stdio` on another machine, over one ssh connection.
pub struct SshPeer {
    name: String,
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<Result<String, String>>,
    stderr: Arc<Mutex<String>>,
    next: u64,
    /// set before a `get` is sent: after the next answer line that says ok, the reader passes
    /// exactly this many raw bytes down the channel instead of reading lines
    raw: RawMode,
}

/// How many raw bytes follow the next ok answer, and where they go.
type RawMode = Arc<Mutex<Option<(u64, std::sync::mpsc::SyncSender<Vec<u8>>)>>>;

/// A chunk of a fetched file's bytes, as the reader hands them over.
pub const CHUNK: usize = 256 << 10;

impl SshPeer {
    pub fn open(m: &Machine) -> Result<SshPeer> {
        Self::open_for(m, "serve-stdio")
    }

    /// A conversation with one of BackDown's ends there (`remote_command_for`).
    pub fn open_for(m: &Machine, end: &str) -> Result<SshPeer> {
        let args = ssh_args(&m.target()?, &remote_command_for(m, end)?)?;
        let mut child = Command::new("ssh")
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("can't run ssh: is OpenSSH installed?")?;
        let stdout = child.stdout.take().context("ssh's output")?;
        let (tx, rx) = channel();
        let raw: RawMode = Arc::new(Mutex::new(None));
        let raw_r = Arc::clone(&raw);
        std::thread::spawn(move || {
            let mut r = BufReader::new(stdout);
            loop {
                let mut buf = Vec::new();
                match (&mut r)
                    .take(MAX_ANSWER as u64 + 1)
                    .read_until(b'\n', &mut buf)
                {
                    Ok(0) => break,
                    Ok(_) if buf.len() > MAX_ANSWER => {
                        let _ = tx.send(Err("the answer was too long".into()));
                        break;
                    }
                    Ok(_) => {
                        let line = String::from_utf8_lossy(&buf).trim_end().to_string();
                        // a `get` was asked: its ok answer is followed by the bytes, raw
                        let pending = raw_r.lock().unwrap().take();
                        let ok = line.contains("\"ok\":true");
                        if tx.send(Ok(line)).is_err() {
                            break;
                        }
                        if let Some((mut left, bytes)) = pending
                            && ok
                        {
                            let mut chunk = vec![0u8; CHUNK];
                            while left > 0 {
                                let want = chunk.len().min(left as usize);
                                match r.read(&mut chunk[..want]) {
                                    Ok(0) | Err(_) => break, // short: the receiver notices
                                    Ok(n) => {
                                        left -= n as u64;
                                        if bytes.send(chunk[..n].to_vec()).is_err() {
                                            break; // nobody wants the rest: the connection goes
                                        }
                                    }
                                }
                            }
                            if left > 0 {
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e.to_string()));
                        break;
                    }
                }
            }
        });
        // ssh's own messages (why it couldn't connect), kept short
        let stderr = Arc::new(Mutex::new(String::new()));
        if let Some(mut e) = child.stderr.take() {
            let keep = Arc::clone(&stderr);
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while let Ok(n) = e.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    let mut s = keep.lock().unwrap();
                    if s.len() < 8192 {
                        s.push_str(&String::from_utf8_lossy(&buf[..n]));
                    }
                }
            });
        }
        let stdin = child.stdin.take();
        Ok(SshPeer {
            name: m.name.clone(),
            child,
            stdin,
            lines: rx,
            stderr,
            next: 0,
            raw,
        })
    }

    /// Ask for a file's bytes (`get`): the plan the other end answered with (size, mtime, name,
    /// sha256, from, len), and the channel its bytes arrive on, in order, `len` of them in all.
    /// Fewer means the conversation died (the file changed, or the link); the caller counts.
    pub fn fetch(
        &mut self,
        file: i64,
        from: Option<u64>,
        len: Option<u64>,
        timeout: Duration,
    ) -> Result<(Value, Receiver<Vec<u8>>)> {
        // how many bytes to expect: asked for, or the whole file, which `file` tells
        let expect = match len {
            Some(l) => l,
            None => {
                let f = self.call("file", json!({ "file": file }), timeout)?;
                let size = f["size"].as_u64().context("no size for that file")?;
                size.saturating_sub(from.unwrap_or(0))
            }
        };
        let (btx, brx) = std::sync::mpsc::sync_channel::<Vec<u8>>(32);
        *self.raw.lock().unwrap() = Some((expect, btx));
        let v = self.exchange(
            "get",
            json!({ "file": file, "from": from, "len": Some(expect) }),
            None,
            timeout,
        );
        let v = match v {
            Ok(v) => v,
            Err(e) => {
                *self.raw.lock().unwrap() = None;
                return Err(e);
            }
        };
        if v["ok"] != true {
            *self.raw.lock().unwrap() = None; // the reader took it already, but be sure
            bail!(
                "{}: {}",
                self.name,
                v["error"].as_str().unwrap_or("it refused the request")
            );
        }
        let plan = v["data"].clone();
        if plan["len"].as_u64() != Some(expect) {
            let _ = self.child.kill();
            bail!(
                "{} promised a different number of bytes than asked",
                self.name
            );
        }
        Ok((plan, brx))
    }

    /// End this conversation now (after a short read, for one).
    pub fn abandon(mut self) {
        let _ = self.child.kill();
    }

    /// Why the conversation ended, in words: from what ssh (or the other end) said.
    fn why(&mut self, what: &str) -> anyhow::Error {
        // give ssh a moment to finish saying why
        for _ in 0..20 {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        std::thread::sleep(Duration::from_millis(50));
        let err = self.stderr.lock().unwrap().trim().to_string();
        anyhow::anyhow!("{}", explain(&self.name, what, &err))
    }
}

/// What ssh's complaint means for someone setting up a machine.
pub fn explain(name: &str, what: &str, stderr: &str) -> String {
    let last = stderr
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let has = |s: &str| stderr.contains(s);
    let hint = if has("Permission denied") {
        format!(
            "ssh couldn't log in to {name} with your keys (BackDown never asks for a password): add your public key there (`ssh-copy-id`), or load it into your ssh agent"
        )
    } else if has("Host key verification failed") || has("REMOTE HOST IDENTIFICATION HAS CHANGED") {
        format!(
            "{name}'s host key isn't known here, or it changed: connect once with `ssh` in a terminal to check it and accept it"
        )
    } else if has("Could not resolve hostname") {
        format!("{name}'s host name can't be found (a typo, or not on this network?)")
    } else if has("Connection refused") {
        format!("{name} refused the connection: is its ssh server on, and is the port right?")
    } else if has("timed out") || has("No route to host") || has("Network is unreachable") {
        format!("{name} didn't answer: is it on, and on this network?")
    } else if has("command not found") || has("No such file or directory") || has("not found") {
        format!(
            "backdown isn't installed on {name}, or isn't on the PATH ssh sessions get there: give its full path with `--backdown` when adding the machine"
        )
    } else if has("unrecognized subcommand") || has("unexpected argument") {
        format!("the BackDown on {name} is too old to be reached this way: update it")
    } else {
        String::new()
    };
    match (hint.is_empty(), last.is_empty()) {
        (true, true) => format!("{what}: {name} closed the connection"),
        (true, false) => format!("{what}: {last}"),
        (false, true) => hint,
        (false, false) => format!("{hint} (ssh said: {last})"),
    }
}

impl SshPeer {
    /// Ask `op` with `args`, and with `body` send exactly that many bytes right after the request
    /// line (a file, raw); the whole answer (`ok`, and `data` or `error`), however it went there.
    pub fn exchange(
        &mut self,
        op: &str,
        args: Value,
        body: Option<(&mut dyn Read, u64)>,
        timeout: Duration,
    ) -> Result<Value> {
        self.next += 1;
        let id = self.next;
        let mut req = if args.is_object() { args } else { json!({}) };
        // the envelope's own keys can't be arguments
        if ["id", "v", "op"].iter().any(|k| req.get(k).is_some()) {
            bail!("a request's arguments can't be called id, v or op");
        }
        req["v"] = json!(1);
        req["id"] = json!(id);
        req["op"] = json!(op);
        let mut short = None;
        let wrote = self
            .stdin
            .as_mut()
            .context("the connection is closed")
            .and_then(|s| {
                writeln!(s, "{req}")?;
                if let Some((body, size)) = body {
                    let sent = std::io::copy(&mut body.take(size), s)?;
                    if sent < size {
                        short = Some(sent);
                    }
                }
                s.flush()?;
                Ok(())
            });
        if let Some(sent) = short {
            // the other end is waiting for bytes that won't come: this conversation is over
            let _ = self.child.kill();
            bail!("the file ended after {sent} bytes, while it was being sent (it changed?)");
        }
        if wrote.is_err() {
            return Err(self.why("couldn't reach it"));
        }
        let line = match self.lines.recv_timeout(timeout) {
            Ok(Ok(l)) => l,
            Ok(Err(e)) => bail!("{}: {e}", self.name),
            Err(RecvTimeoutError::Timeout) => {
                let _ = self.child.kill();
                bail!("{} didn't answer within {} s", self.name, timeout.as_secs());
            }
            Err(RecvTimeoutError::Disconnected) => return Err(self.why("no answer")),
        };
        let v: Value = serde_json::from_str(&line).map_err(|_| {
            anyhow::anyhow!(
                "{} answered something that isn't BackDown's protocol: {}",
                self.name,
                line.chars().take(120).collect::<String>()
            )
        })?;
        if v["id"] != json!(id) {
            bail!("{} answered out of turn", self.name);
        }
        Ok(v)
    }
}

/// The other machine answered, with an error: the conversation itself is fine (it can be kept,
/// and asking again afresh wouldn't change the answer).
#[derive(Debug)]
pub struct Answered(pub String);

impl std::fmt::Display for Answered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Answered {}

impl Peer for SshPeer {
    fn call(&mut self, op: &str, args: Value, timeout: Duration) -> Result<Value> {
        let v = self.exchange(op, args, None, timeout)?;
        if v["ok"] != true {
            return Err(anyhow::Error::new(Answered(format!(
                "{}: {}",
                self.name,
                v["error"].as_str().unwrap_or("it refused the request")
            ))));
        }
        Ok(v["data"].clone())
    }
}

impl Drop for SshPeer {
    fn drop(&mut self) {
        drop(self.stdin.take()); // end of input: the other end exits
        for _ in 0..20 {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Opens ssh conversations (for a wipe's verifier).
pub struct Ssh;

impl remote::Connect for Ssh {
    fn connect(&mut self, m: &Machine) -> Result<Box<dyn Peer>> {
        let mut p = SshPeer::open(m)?;
        p.call("hello", json!({}), remote::ASK)?;
        Ok(Box::new(p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine(url: &str, db: Option<&str>, command: Option<&str>) -> Machine {
        Machine {
            id: 1,
            name: "nas".into(),
            url: url.into(),
            db: db.map(String::from),
            command: command.map(String::from),
            created_at: 0,
            last_try: None,
            last_sync: None,
            last_error: None,
            info: Value::Null,
        }
    }

    #[test]
    fn quoting_survives_any_name() {
        assert_eq!(quote("plain"), "'plain'");
        assert_eq!(quote("it's"), r"'it'\''s'");
        assert_eq!(quote("$(rm -rf ~)"), "'$(rm -rf ~)'");
        // what a POSIX shell makes of it: exactly the original words
        for s in [
            "/volume1/@backdown/index.db",
            "/home/alice/it's \"here\"/index.db",
            "a;b&&c|d`e`$f\\g",
            "  spaces  ",
            "ünïcode ✓",
        ] {
            let out = Command::new("sh")
                .arg("-c")
                .arg(format!("printf '%s' {}", quote(s)))
                .output()
                .unwrap();
            assert_eq!(String::from_utf8(out.stdout).unwrap(), s);
        }
    }

    #[test]
    fn the_command_line_is_a_vector_with_the_host_after_dashes() {
        let m = machine(
            "ssh://alice@nas.example.org:2222",
            Some("/volume1/it's/index.db"),
            Some("/opt/bin/backdown"),
        );
        let cmd = remote_command(&m).unwrap();
        assert_eq!(
            cmd,
            r"'/opt/bin/backdown' 'serve-stdio' '--db' '/volume1/it'\''s/index.db'"
        );
        let a: Vec<String> = ssh_args(&m.target().unwrap(), &cmd)
            .unwrap()
            .into_iter()
            .map(|s| s.into_string().unwrap())
            .collect();
        let dd = a.iter().position(|x| x == "--").unwrap();
        assert_eq!(a[dd + 1], "nas.example.org");
        assert_eq!(a[dd + 2], cmd);
        assert_eq!(a.len(), dd + 3);
        assert!(a.windows(2).any(|w| w == ["-o", "BatchMode=yes"]));
        assert!(a.windows(2).any(|w| w == ["-o", "ConnectTimeout=10"]));
        assert!(a.windows(2).any(|w| w == ["-p", "2222"]));
        assert!(a.windows(2).any(|w| w == ["-l", "alice"]));
        assert!(!a.iter().any(|x| x.contains("StrictHostKeyChecking")));
        // an option-like host can't get this far, and is refused here too
        let t = Target {
            user: None,
            host: "-oProxyCommand=id".into(),
            port: None,
        };
        assert!(ssh_args(&t, &cmd).is_err());
        assert!(remote_command(&machine("ssh://nas", Some("-x"), None)).is_err());
        assert!(describe(&m).unwrap().starts_with("ssh -T -o BatchMode=yes"));
    }

    #[test]
    fn ssh_complaints_are_explained() {
        assert!(
            explain("nas", "x", "alice@nas: Permission denied (publickey).").contains("your keys")
        );
        assert!(explain("nas", "x", "Host key verification failed.").contains("host key"));
        assert!(explain("nas", "x", "sh: 1: backdown: not found").contains("--backdown"));
        assert_eq!(
            explain("nas", "no answer", ""),
            "no answer: nas closed the connection"
        );
    }
}
