//! Another machine over ssh, end to end, without a network: a fake `ssh` first on the PATH
//! checks the arguments it's given the way OpenSSH would read them, then runs the remote command
//! through `sh -c` exactly as the other machine's shell would, which starts this very binary as
//! `backdown serve-stdio` against a second index. So the quoting is proved by a real shell, and
//! everything from `remote add` to a wipe proved by a copy "on the NAS" runs for real.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const FAKE_SSH: &str = r#"#!/bin/sh
# a stand-in for OpenSSH: log the arguments, check them, then be the other machine's shell
log="$FAKE_SSH_LOG"
for a in "$@"; do printf '%s\n' "$a" >> "$log"; done
printf -- '----\n' >> "$log"
batch=no
while [ $# -gt 0 ]; do
  case "$1" in
    -T) shift ;;
    -o) [ "$2" = "BatchMode=yes" ] && batch=yes; shift 2 ;;
    -p|-l) shift 2 ;;
    --) shift; break ;;
    -*) echo "fake ssh: unexpected option $1" >&2; exit 255 ;;
    *) echo "fake ssh: the host must come after --" >&2; exit 255 ;;
  esac
done
[ "$batch" = yes ] || { echo "fake ssh: BatchMode=yes missing" >&2; exit 255; }
host="$1"; shift
[ $# -eq 1 ] || { echo "fake ssh: expected the remote command as one argument, got $#" >&2; exit 255; }
case "$host" in
  -*) echo "fake ssh: an option as a host" >&2; exit 255 ;;
  unreachable.example.org) echo "ssh: connect to host $host port 22: No route to host" >&2; exit 255 ;;
  nokeys.example.org) echo "alice@$host: Permission denied (publickey)." >&2; exit 255 ;;
esac
exec sh -c "$1"
"#;

struct Env {
    root: PathBuf,
    bin: PathBuf,
    log: PathBuf,
    local_db: PathBuf,
}

impl Env {
    fn new(tmp: &Path) -> Env {
        let bin = tmp.join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("ssh"), FAKE_SSH).unwrap();
        fs::set_permissions(bin.join("ssh"), fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_backdown"), bin.join("backdown")).unwrap();
        Env {
            root: tmp.to_path_buf(),
            bin,
            log: tmp.join("ssh.log"),
            local_db: tmp.join("local.db"),
        }
    }

    /// `backdown ARGS` with the fake ssh first on the PATH, and a home of its own (so no daemon
    /// of the person running the tests is ever found)
    fn run(&self, db: &Path, args: &[&str]) -> Output {
        self.run_as(db, args, false)
    }

    /// `release`: behave like a release build without a licence (BACKDOWN_PRETEND_RELEASE)
    fn run_as(&self, db: &Path, args: &[&str], release: bool) -> Output {
        let home = self.root.join("home");
        fs::create_dir_all(&home).unwrap();
        let path = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut c = Command::new(env!("CARGO_BIN_EXE_backdown"));
        c.arg("--db")
            .arg(db)
            .arg("--local")
            .args(args)
            .env("PATH", path)
            .env("HOME", &home)
            .env("XDG_DATA_HOME", home.join("data"))
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("XDG_RUNTIME_DIR", home.join("run"))
            .env("FAKE_SSH_LOG", &self.log)
            .env_remove("BACKDOWN_PRETEND_RELEASE");
        if release {
            c.env("BACKDOWN_PRETEND_RELEASE", "1");
        }
        c.output().unwrap()
    }

    fn ok(&self, db: &Path, args: &[&str]) -> String {
        let o = self.run(db, args);
        let out = String::from_utf8_lossy(&o.stdout).into_owned();
        let err = String::from_utf8_lossy(&o.stderr).into_owned();
        assert!(
            o.status.success(),
            "backdown {args:?} failed:\n{out}\n{err}"
        );
        out + &err
    }
}

fn write(p: &Path, bytes: &[u8]) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, bytes).unwrap();
}

fn blob(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed) ^ ((i >> 8) as u8))
        .collect()
}

#[test]
fn a_nas_over_ssh_end_to_end() {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    fs::create_dir_all(&base).unwrap();
    let tmp = tempfile::tempdir_in(fs::canonicalize(&base).unwrap()).unwrap();
    let e = Env::new(tmp.path());
    let laptop = e.root.join("Laptop");
    let vol = e.root.join("volume1");
    write(&laptop.join("photos/a.jpg"), &blob(1, 300_000));
    write(&vol.join("backup/photos/a.jpg"), &blob(1, 300_000));
    write(&laptop.join("docs/b.txt"), &blob(2, 1000));
    write(&vol.join("backup/docs/b.txt"), &blob(2, 1000));
    write(&laptop.join("only/thesis.odt"), &blob(3, 50_000));
    // the NAS's index, somewhere its shell has to be told about with care
    let nas_dir = e.root.join("nas's index $(touch pwned) `id`");
    fs::create_dir_all(&nas_dir).unwrap();
    let nas_db = nas_dir.join("index.db");
    e.ok(&nas_db, &["index", vol.to_str().unwrap()]);
    e.ok(&nas_db, &["hash"]);
    e.ok(&e.local_db, &["index", laptop.to_str().unwrap()]);
    e.ok(&e.local_db, &["hash"]);

    // option-like hosts never reach ssh
    let o = e.run(
        &e.local_db,
        &[
            "remote",
            "add",
            "evil",
            "ssh://-oProxyCommand=touch%20pwned",
        ],
    );
    assert!(!o.status.success());
    assert!(!e.log.exists(), "ssh must not have run");

    let out = e.ok(
        &e.local_db,
        &[
            "remote",
            "add",
            "nas",
            "ssh://alice@nas.example.org:2222",
            "--remote-db",
            nas_db.to_str().unwrap(),
        ],
    );
    assert!(out.contains("Reached nas"), "{out}");
    assert!(out.contains("1 drive indexed"), "{out}");
    assert!(
        !e.root.join("pwned").exists() && !Path::new("pwned").exists(),
        "the path was run as a command"
    );
    // what ssh was given: options, --, the host, one quoted command
    let log = fs::read_to_string(&e.log).unwrap();
    let args: Vec<&str> = log.split("----\n").next().unwrap().lines().collect();
    let dd = args.iter().position(|a| *a == "--").unwrap();
    assert_eq!(args[dd + 1], "nas.example.org");
    assert_eq!(args.len(), dd + 3);
    assert!(args.windows(2).any(|w| w == ["-l", "alice"]));
    assert!(args.windows(2).any(|w| w == ["-p", "2222"]));
    assert!(args[dd + 2].starts_with("'backdown' 'serve-stdio' '--db' '"));

    let out = e.ok(&e.local_db, &["remote", "sync", "nas"]);
    assert!(out.contains("nas: 1 drive, 2 files"), "{out}");
    // hashing here now takes the NAS's files into account; the next sync hashes there what matches
    e.ok(&e.local_db, &["hash"]);
    e.ok(&e.local_db, &["remote", "sync"]);
    let list = e.ok(&e.local_db, &["remote", "list"]);
    assert!(list.contains("its copies count"), "{list}");

    // the photos and docs are safe to wipe because of the NAS; the thesis isn't
    let o = e.run(
        &e.local_db,
        &["check", laptop.join("photos").to_str().unwrap()],
    );
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{out}");
    assert!(
        out.contains("only on another machine (nas (ssh://alice@nas.example.org:2222))"),
        "{out}"
    );
    let o = e.run(&e.local_db, &["check", laptop.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(2));

    // a wipe re-reads the NAS's copy over ssh before deleting
    let dry = e.ok(
        &e.local_db,
        &["wipe", laptop.join("docs").to_str().unwrap()],
    );
    assert!(dry.contains("only on another machine"), "{dry}");
    e.ok(
        &e.local_db,
        &[
            "wipe",
            laptop.join("docs").to_str().unwrap(),
            "--go",
            "--confirm",
            "Laptop",
        ],
    );
    assert!(!laptop.join("docs/b.txt").exists());

    // machines that can't be reached say why, in words
    e.ok(
        &e.local_db,
        &[
            "remote",
            "add",
            "away",
            "ssh://unreachable.example.org",
            "--no-test",
        ],
    );
    let o = e.run(&e.local_db, &["remote", "test", "away"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("didn't answer"));
    e.ok(
        &e.local_db,
        &[
            "remote",
            "add",
            "locked",
            "ssh://nokeys.example.org",
            "--no-test",
        ],
    );
    let o = e.run(&e.local_db, &["remote", "test", "locked"]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("your keys"));
    e.ok(&e.local_db, &["remote", "remove", "away"]);

    // the other end refuses what isn't the protocol, and never writes its index
    let before = fs::metadata(&nas_db).unwrap().modified().unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_backdown"))
        .args(["serve-stdio", "--db"])
        .arg(&nas_db)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            use std::io::Write;
            let mut i = c.stdin.take().unwrap();
            writeln!(i, r#"{{"id":1,"op":"hello"}}"#)?;
            writeln!(i, r#"{{"id":2,"op":"wipe","path":"/"}}"#)?;
            writeln!(i, "{}", "x".repeat(200_000))?;
            writeln!(i, r#"{{"id":3,"op":"roots"}}"#)?;
            drop(i);
            c.wait_with_output()
        })
        .unwrap();
    let lines: Vec<serde_json::Value> = String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[0]["data"]["read_only"], true);
    assert_eq!(
        (lines[1]["id"].clone(), lines[1]["ok"].clone()),
        (2.into(), false.into())
    );
    assert_eq!(lines[2]["error"], "request too long");
    assert_eq!(lines[3]["ok"], true);
    assert_eq!(fs::metadata(&nas_db).unwrap().modified().unwrap(), before);

    // Pro: a release without a licence can't reach machines, but can still list and forget them
    let o = e.run_as(&e.local_db, &["remote", "sync", "nas"], true);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("BackDown Pro"));
    let o = e.run_as(
        &e.local_db,
        &["remote", "add", "pc2", "ssh://pc2.example.org", "--no-test"],
        true,
    );
    assert!(!o.status.success());
    assert!(
        e.run_as(&e.local_db, &["remote", "list"], true)
            .status
            .success()
    );
    assert!(
        e.run_as(&e.local_db, &["remote", "remove", "locked"], true)
            .status
            .success()
    );
}
