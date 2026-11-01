//! Another machine over ssh, end to end, without a network: a fake `ssh` first on the PATH
//! checks the arguments it's given the way OpenSSH would read them, then runs the remote command
//! through `sh -c` exactly as the other machine's shell would, which starts this very binary as
//! `backdown serve-stdio` against a second index. So the quoting is proved by a real shell, and
//! everything from `remote add` to a wipe proved by a copy "on the NAS" runs for real. The host
//! old.example.org stands for a BackDown from before the `health` request: it refuses that one
//! the way those did, and passes everything else on.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};

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
  old.example.org)
    { while IFS= read -r line; do
        case "$line" in
          *'"op":"health"'*)
            id=${line#*\"id\":}; id=${id%%[!0-9]*}
            printf '{"id":%s,"ok":false,"error":"bad request: unknown variant `health`, expected one of `hello`, `roots`"}\n' "$id" >&3 ;;
          *) printf '%s\n' "$line" ;;
        esac
      done | sh -c "$1"; } 3>&1
    exit ;;
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
        self.command(db, args, release).output().unwrap()
    }

    fn command(&self, db: &Path, args: &[&str], release: bool) -> Command {
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
        c
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

    // free since 1 Nov 2026 (with the swarm): a release without a licence reaches machines too
    let o = e.run_as(&e.local_db, &["remote", "sync", "nas"], true);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = e.run_as(
        &e.local_db,
        &["remote", "add", "pc2", "ssh://pc2.example.org", "--no-test"],
        true,
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
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

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn other_machines_drive_health_over_ssh() {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    fs::create_dir_all(&base).unwrap();
    let tmp = tempfile::tempdir_in(fs::canonicalize(&base).unwrap()).unwrap();
    let e = Env::new(tmp.path());
    // the NAS reads its own drives (fake readings) into its own index, as its BackDown does
    let nas_db = e.root.join("nas.db");
    let readings = e.root.join("readings.json");
    let read_there = |reallocated: u64| {
        let v = json!({ "drives": [
            { "device": "/dev/sdx", "name": "sdx", "model": "FAKEDISK HD4000", "serial": "FAKE-0001",
              "rotational": true, "protocol": "ata", "reported": "ok", "temperature": 36,
              "reallocated": reallocated, "pending": 0 },
            { "device": "/dev/nvme9n1", "name": "nvme9n1", "model": "FAKEDISK NV970",
              "serial": "S0FAKE0NV970123", "rotational": false, "protocol": "nvme",
              "critical_warning": 0, "available_spare": 100, "spare_threshold": 10,
              "percentage_used": 3, "media_errors": 0 }
        ] });
        fs::write(&readings, serde_json::to_vec(&v).unwrap()).unwrap();
        e.command(&nas_db, &["health"], false)
            .env("BACKDOWN_FAKE_HEALTH", &readings)
            .output()
            .unwrap()
    };
    let told = || -> Vec<String> {
        let c = rusqlite::Connection::open(&e.local_db).unwrap();
        let mut s = c
            .prepare("SELECT title FROM notifications WHERE kind = 'health' ORDER BY id")
            .unwrap();
        s.query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert!(read_there(0).status.success());
    let nas = nas_db.to_str().unwrap();
    e.ok(
        &e.local_db,
        &[
            "remote",
            "add",
            "nas",
            "ssh://nas.example.org",
            "--remote-db",
            nas,
        ],
    );

    // a sync fetches them; the first sight of its drives tells nothing
    let out = e.ok(&e.local_db, &["remote", "sync", "nas"]);
    assert!(out.contains("drive health of 2 drives"), "{out}");
    assert!(told().is_empty());
    let out = e.ok(&e.local_db, &["health", "--machines"]);
    assert!(
        out.contains("nas (ssh://nas.example.org) · synced just now"),
        "{out}"
    );
    assert!(out.contains("  ok       FAKEDISK HD4000"), "{out}");
    assert!(out.contains("  ok       FAKEDISK NV970"), "{out}");

    // a drive there fails: told here once, and `health --machines` says so (and exits 2)
    assert_eq!(read_there(150).status.code(), Some(2), "failing there");
    let out = e.ok(&e.local_db, &["remote", "sync", "nas"]);
    assert!(out.contains("1 worse since the last sync"), "{out}");
    assert_eq!(told(), ["A drive on nas is failing"]);
    let o = e.run(&e.local_db, &["health", "--machines"]);
    assert_eq!(o.status.code(), Some(2));
    let out = stdout(&o);
    assert!(out.contains("  failing  FAKEDISK HD4000"), "{out}");
    assert!(
        out.contains("  FAKEDISK HD4000: 150 sectors had gone bad"),
        "{out}"
    );
    // …with the NAS's own notification about it
    assert!(
        out.contains("  notified on nas just now: FAKEDISK HD4000 is failing"),
        "{out}"
    );
    // a repeat tells nothing
    e.ok(&e.local_db, &["remote", "sync", "nas"]);
    assert_eq!(told().len(), 1);
    let o = e.run(&e.local_db, &["health", "--machines", "--json"]);
    let j: Value = serde_json::from_str(&stdout(&o)).unwrap();
    assert_eq!(j["worst"], "failing");
    assert_eq!(j["machines"][0]["drives"][0]["verdict"], "failing");
    assert_eq!(
        j["machines"][0]["drives"][0]["health"]["serial"],
        "FAKE-0001"
    );

    // an older BackDown there: its files sync as ever, and it says what would show its drives
    e.ok(
        &e.local_db,
        &[
            "remote",
            "add",
            "old",
            "ssh://old.example.org",
            "--remote-db",
            nas,
        ],
    );
    let out = e.ok(&e.local_db, &["remote", "sync", "old"]);
    assert!(
        out.contains("update BackDown on old to see its drive health"),
        "{out}"
    );
    let out = stdout(&e.run(&e.local_db, &["health", "--machines"]));
    assert!(
        out.contains(
            "old (ssh://old.example.org) · synced just now\n  update BackDown on old to see its drive health\n"
        ),
        "{out}"
    );

    // forgetting a machine forgets its drives
    e.ok(&e.local_db, &["remote", "remove", "nas"]);
    let c = rusqlite::Connection::open(&e.local_db).unwrap();
    let rows: i64 = c
        .query_row("SELECT COUNT(*) FROM remote_health", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 0);
    let out = e.ok(&e.local_db, &["health", "--machines"]);
    assert!(!out.contains("nas"), "{out}");

    // Team: a release without a licence doesn't gather them, and says why
    let o = e.run_as(&e.local_db, &["health", "--machines"], true);
    assert!(!o.status.success());
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("BackDown Team"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
}

/// The swarm over ssh: `backdown swarm` shows the NAS's drives as it says them, and `swarm where`
/// finds a file there by asking it, or, when it can't be reached, in what was synced of it.
#[test]
fn the_swarm_over_ssh() {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    fs::create_dir_all(&base).unwrap();
    let tmp = tempfile::tempdir_in(fs::canonicalize(&base).unwrap()).unwrap();
    let e = Env::new(tmp.path());
    let laptop = e.root.join("Laptop");
    let vol = e.root.join("volume1");
    write(&laptop.join("docs/notes.txt"), &blob(1, 1000));
    write(
        &vol.join("archive/2021/Annual Report.pdf"),
        &blob(2, 30_000),
    );
    write(&vol.join("archive/report-draft.odt"), &blob(3, 5_000));
    let nas_db = e.root.join("nas.db");
    e.ok(&nas_db, &["index", vol.to_str().unwrap()]);
    e.ok(&e.local_db, &["index", laptop.to_str().unwrap()]);
    let nas_url = "ssh://nas.example.org";
    e.ok(
        &e.local_db,
        &[
            "remote",
            "add",
            "nas",
            nas_url,
            "--remote-db",
            nas_db.to_str().unwrap(),
        ],
    );
    e.ok(&e.local_db, &["remote", "sync", "nas"]);

    // every machine at once: this one probed now, the NAS asked over ssh
    let o: Value =
        serde_json::from_str(&stdout(&e.run(&e.local_db, &["swarm", "--json"]))).unwrap();
    let ms = o["machines"].as_array().unwrap();
    assert_eq!(ms.len(), 2, "{o}");
    assert_eq!(ms[0]["here"], true);
    assert_eq!(ms[1]["name"], "nas");
    assert_eq!(ms[1]["state"], "ok", "{}", ms[1]);
    assert_eq!(o["totals"]["files"], 3, "1 here, 2 on the NAS");
    let text = e.ok(&e.local_db, &["swarm", "--cached"]);
    assert!(text.contains("nas  drives as of"), "{text}");
    assert!(text.contains("3 files indexed"), "{text}");

    // where is it: asked of the NAS itself
    let w: Value = serde_json::from_str(&stdout(
        &e.run(&e.local_db, &["swarm", "where", "report", "--json"]),
    ))
    .unwrap();
    let hits = w["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 2, "{w}");
    assert!(
        hits.iter()
            .all(|h| h["machine"] == "nas" && h["here"] == false)
    );
    let nas = w["machines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == "nas")
        .unwrap()
        .clone();
    assert_eq!(nas["how"], "live", "{nas}");
    let text = e.ok(&e.local_db, &["swarm", "where", "annual", "report"]);
    assert!(
        text.contains("nas  ") && text.contains("Annual Report.pdf"),
        "{text}"
    );
    // this machine's files are found the same way
    let text = e.ok(&e.local_db, &["swarm", "where", "notes"]);
    assert!(text.contains("notes.txt"), "{text}");

    // the NAS can't be reached: its files as synced are searched instead, and that's said
    let db = rusqlite::Connection::open(&e.local_db).unwrap();
    db.execute(
        "UPDATE remotes SET url = 'ssh://unreachable.example.org' WHERE name = 'nas'",
        [],
    )
    .unwrap();
    drop(db);
    let o = e.run(&e.local_db, &["swarm", "where", "report"]);
    let out = String::from_utf8_lossy(&o.stdout);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{out}\n{err}");
    assert!(out.contains("report-draft.odt"), "{out}");
    assert!(err.contains("nas couldn't be asked"), "{err}");
    assert!(err.contains("as synced"), "{err}");
    // and the overview says it couldn't be asked, keeping its drives as last said
    let o: Value =
        serde_json::from_str(&stdout(&e.run(&e.local_db, &["swarm", "--json"]))).unwrap();
    assert_eq!(o["machines"][1]["state"], "error", "{}", o["machines"][1]);
    assert!(o["machines"][1]["ok_at"].is_i64());
}

/// The pool over ssh: a NAS's own pool folder (added there), seen here after a sync; a drive here
/// planned into it, with what's in it already found by content and the rest given that folder.
#[test]
fn the_pool_over_ssh() {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    fs::create_dir_all(&base).unwrap();
    let tmp = tempfile::tempdir_in(fs::canonicalize(&base).unwrap()).unwrap();
    let e = Env::new(tmp.path());
    let old = e.root.join("OLD-DRIVE");
    let pool = e.root.join("nas/pool");
    let photo = blob(7, 300_000);
    write(&old.join("Stuff/beach.jpg"), &photo);
    write(&old.join("Stuff/new.jpg"), &blob(8, 300_001));
    write(&pool.join("Unsorted/elsewhere/beach-copy.jpg"), &photo);
    let nas_db = e.root.join("nas.db");
    // on the NAS: its pool folder, indexed and hashed there
    let out = e.ok(
        &nas_db,
        &["pool", "add", pool.to_str().unwrap(), "--reserve", "1G"],
    );
    assert!(out.contains("Pool folder 1"), "{out}");
    e.ok(&nas_db, &["index", pool.to_str().unwrap()]);
    e.ok(&nas_db, &["hash"]);
    // here: the old drive, and the NAS
    e.ok(&e.local_db, &["index", old.to_str().unwrap()]);
    e.ok(
        &e.local_db,
        &[
            "remote",
            "add",
            "nas",
            "ssh://nas.example.org",
            "--remote-db",
            nas_db.to_str().unwrap(),
        ],
    );
    e.ok(&e.local_db, &["remote", "sync", "nas"]);
    e.ok(&e.local_db, &["hash"]);
    // the NAS hashes there what's the same size as a file here
    e.ok(&e.local_db, &["remote", "sync", "nas"]);

    let list = e.ok(&e.local_db, &["pool", "list"]);
    assert!(list.contains("nas 1"), "{list}");
    assert!(list.contains(pool.to_str().unwrap()), "{list}");
    let p: Value = serde_json::from_str(&stdout(
        &e.run(&e.local_db, &["pool", "plan", "OLD-DRIVE", "--json"]),
    ))
    .unwrap();
    assert_eq!(p["in_pool_bytes"], 300_000, "{p:#}");
    assert_eq!(p["copy_bytes"], 300_001);
    let stuff = &p["things"][0];
    assert_eq!(stuff["to"]["machine"], "nas", "{stuff:#}");
    assert!(
        stuff["in_pool_at"].as_str().unwrap().starts_with("nas: "),
        "{stuff:#}"
    );
    // kept: listed, and shown again
    let plans = e.ok(&e.local_db, &["pool", "plans"]);
    assert!(plans.contains("OLD-DRIVE"), "{plans}");
    let shown = e.ok(&e.local_db, &["pool", "plans", "1"]);
    assert!(shown.contains("→ nas:"), "{shown}");
}

/// Copying into the pool over ssh, end to end: a drive here planned into the NAS's pool folder,
/// sent to `swarm-stdio` there (each file read back there and compared), indexed there by itself
/// with the hashes it verified on arrival, synced here; then the drive here is safe to wipe, proved
/// by its copies on the NAS.
#[test]
fn copying_a_drive_into_a_nas_pool_over_ssh() {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    fs::create_dir_all(&base).unwrap();
    let tmp = tempfile::tempdir_in(fs::canonicalize(&base).unwrap()).unwrap();
    let e = Env::new(tmp.path());
    let old = e.root.join("OLD-DRIVE");
    let pool = e.root.join("nas/pool");
    write(&old.join("Stuff/beach.jpg"), &blob(7, 300_000));
    write(&old.join("Stuff/notes.txt"), &blob(8, 3_000));
    write(&old.join("Stuff/deep/er/report.odt"), &blob(9, 40_000));
    let nas_db = e.root.join("nas.db");
    e.ok(
        &nas_db,
        &["pool", "add", pool.to_str().unwrap(), "--reserve", "1G"],
    );
    e.ok(&nas_db, &["index", pool.to_str().unwrap()]);
    e.ok(&e.local_db, &["index", old.to_str().unwrap()]);
    e.ok(
        &e.local_db,
        &[
            "remote",
            "add",
            "nas",
            "ssh://nas.example.org",
            "--remote-db",
            nas_db.to_str().unwrap(),
        ],
    );
    e.ok(&e.local_db, &["remote", "sync", "nas"]);
    e.ok(&e.local_db, &["pool", "plan", "OLD-DRIVE"]);

    let out = e.ok(&e.local_db, &["pool", "send", "1"]);
    assert!(out.contains("3 copied"), "{out}");
    assert!(out.contains("nas:") && out.contains("indexing"), "{out}");
    let dest = pool.join("Unsorted/OLD-DRIVE/Stuff");
    assert_eq!(
        fs::read(dest.join("deep/er/report.odt")).unwrap(),
        blob(9, 40_000)
    );
    assert_eq!(fs::read(dest.join("beach.jpg")).unwrap(), blob(7, 300_000));
    // the NAS indexes what it received by itself (no service of its own runs here)
    let indexed = || -> i64 {
        rusqlite::Connection::open(&nas_db)
            .and_then(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM files WHERE current = 1 AND sha256 IS NOT NULL AND CAST(path AS TEXT) LIKE '%/Unsorted/OLD-DRIVE/%'",
                    [],
                    |r| r.get(0),
                )
            })
            .unwrap_or(0)
    };
    for _ in 0..150 {
        if indexed() == 3 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    assert_eq!(
        indexed(),
        3,
        "indexed there, with the hashes verified on arrival"
    );
    // sent again: nothing goes twice
    let out = e.ok(&e.local_db, &["pool", "send", "1"]);
    assert!(
        out.contains("0 copied") && out.contains("3 done before"),
        "{out}"
    );

    // here: the NAS's copies, synced, prove the drive is safe to wipe
    e.ok(&e.local_db, &["remote", "sync", "nas"]);
    let o = e.run(&e.local_db, &["check", old.to_str().unwrap()]);
    let text = format!("{}{}", stdout(&o), String::from_utf8_lossy(&o.stderr));
    assert!(
        o.status.success(),
        "safe to wipe, by the copies on the NAS:\n{text}"
    );
}

/// One request line to `backdown swarm-stdio` on index `db`, after a hello: its answer.
fn swarm_ask(e: &Env, db: &Path, line: &str) -> Value {
    use std::io::Write;
    let mut c = e.command(db, &["swarm-stdio"], false);
    let mut child = c
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut i = child.stdin.take().unwrap();
        writeln!(i, r#"{{"id":1,"op":"hello","from":"laptop"}}"#).unwrap();
        writeln!(i, "{line}").unwrap();
    }
    let out = child.wait_with_output().unwrap();
    let lines: Vec<Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines[0]["ok"], true, "{lines:?}");
    lines[1].clone()
}

/// A drive on another machine into the pool, from here: `swarm-stdio` there makes its plan, takes
/// the person's choice and carries it out there, and says how it's going.
#[test]
fn a_drive_there_is_planned_and_copied_there_when_asked_from_here() {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    fs::create_dir_all(&base).unwrap();
    let tmp = tempfile::tempdir_in(fs::canonicalize(&base).unwrap()).unwrap();
    let e = Env::new(tmp.path());
    let drive = e.root.join("nas/BD-1T-A");
    let pool = e.root.join("nas/big/Pool");
    write(&drive.join("Old/a.bin"), &blob(1, 20_000));
    write(&drive.join("Old/b.bin"), &blob(2, 30_000));
    write(&drive.join("Junk/c.bin"), &blob(3, 4_000));
    let nas_db = e.root.join("nas.db");
    e.ok(
        &nas_db,
        &["pool", "add", pool.to_str().unwrap(), "--reserve", "1G"],
    );
    e.ok(&nas_db, &["index", pool.to_str().unwrap()]);
    e.ok(&nas_db, &["index", drive.to_str().unwrap()]);
    let root: i64 = rusqlite::Connection::open(&nas_db)
        .unwrap()
        .query_row(
            "SELECT id FROM roots WHERE CAST(path AS TEXT) = ?1",
            [drive.to_str().unwrap()],
            |r| r.get(0),
        )
        .unwrap();

    let p = swarm_ask(
        &e,
        &nas_db,
        &format!(r#"{{"id":2,"op":"plan","root":{root}}}"#),
    );
    assert_eq!(p["ok"], true, "{p}");
    let plan = p["data"]["id"].as_i64().unwrap();
    assert_eq!(p["data"]["copy_bytes"], 54_000, "{p}");
    let things = p["data"]["things"].as_array().unwrap();
    let junk = things
        .iter()
        .position(|t| t["item"]["source"].as_str().unwrap().ends_with("Junk"))
        .unwrap();
    // leave the junk out
    let c = swarm_ask(
        &e,
        &nas_db,
        &format!(r#"{{"id":2,"op":"choose","plan":{plan},"i":{junk},"skip":true}}"#),
    );
    assert_eq!(c["data"]["copy_bytes"], 50_000, "{c}");
    // carried out there (no service of its own here: a `backdown pool send` of its own)
    let s = swarm_ask(
        &e,
        &nas_db,
        &format!(r#"{{"id":2,"op":"send","plan":{plan}}}"#),
    );
    assert_eq!(s["data"]["how"], "process", "{s}");
    let mut last = Value::Null;
    for _ in 0..150 {
        last = swarm_ask(
            &e,
            &nas_db,
            &format!(r#"{{"id":2,"op":"progress","plan":{plan}}}"#),
        );
        if last["data"]["state"] == "done" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    assert_eq!(last["data"]["state"], "done", "{last}");
    assert!(
        last["data"]["log"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["state"] == "copied" && l["files"] == 2),
        "{last}"
    );
    let dest = pool.join("Unsorted/BD-1T-A/Old");
    assert_eq!(fs::read(dest.join("a.bin")).unwrap(), blob(1, 20_000));
    assert!(!pool.join("Unsorted/BD-1T-A/Junk").exists(), "left out");
    // the plan as it is there
    let g = swarm_ask(
        &e,
        &nas_db,
        &format!(r#"{{"id":2,"op":"plan-get","plan":{plan}}}"#),
    );
    assert_eq!(g["data"]["state"], "done");
}
