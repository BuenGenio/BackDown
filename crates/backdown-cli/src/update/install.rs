//! Installing a release: download, verify (SHA256SUMS and its signature), unpack, and put the new
//! binary in place the way this installation was made.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use backdown_core::clock::now;
use backdown_core::fmt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::settings;

use super::*;

// ---------- installing

/// How a copy of BackDown that can't replace itself is updated instead.
#[derive(Debug, PartialEq)]
pub(super) enum Managed {
    AppImage,
    /// in /usr/bin and the like: a .deb, or another of the system's packages
    System(&'static str),
    /// Nix, snap
    Other(&'static str),
    Homebrew,
    /// inside BackDown.app
    App,
    Msi,
}

pub(super) fn managed(exe: &Path) -> Option<Managed> {
    if std::env::var_os("APPIMAGE").is_some() {
        return Some(Managed::AppImage);
    }
    let s = exe.to_string_lossy();
    let system = [
        (
            "/usr/bin/",
            "your package manager (it came in a .deb or another package)",
        ),
        ("/bin/", "your package manager"),
        ("/usr/sbin/", "your package manager"),
    ];
    for (prefix, who) in system {
        if s.starts_with(prefix) {
            return Some(Managed::System(who));
        }
    }
    for (prefix, who) in [("/nix/store/", "Nix"), ("/snap/", "snap")] {
        if s.starts_with(prefix) {
            return Some(Managed::Other(who));
        }
    }
    if s.contains("/Cellar/") {
        return Some(Managed::Homebrew);
    }
    // a new binary inside the bundle would break its signature, and macOS then won't start it
    if s.contains(".app/Contents/MacOS/") {
        return Some(Managed::App);
    }
    // Windows Installer keeps track of the files it installed: the new .msi upgrades this one
    if cfg!(windows) && s.contains(r"\Programs\BackDown\") && installed_by_msi() {
        return Some(Managed::Msi);
    }
    None
}

/// Why this binary can't replace itself, if it can't.
pub fn not_replaceable(exe: &Path) -> Option<String> {
    Some(match managed(exe)? {
        Managed::AppImage => "this BackDown runs from an AppImage, which can't change itself: download the new AppImage from https://backdown.io/download".into(),
        Managed::System(who) | Managed::Other(who) => format!(
            "{} is managed by {who}: update it that way (the new package is at https://backdown.io/download)",
            exe.display()
        ),
        Managed::Homebrew => format!(
            "{} is managed by Homebrew: brew upgrade backdown",
            exe.display()
        ),
        Managed::App => "this BackDown is inside BackDown.app, which can't change itself: download the new .dmg from https://backdown.io/download".into(),
        Managed::Msi => "BackDown was installed from an .msi: install the new .msi from https://backdown.io/download, which upgrades this one".into(),
    })
}

/// The same for `update --from FOLDER`, saying what to do with the carried files instead (`files`:
/// the names of those in the folder), with the one to use if it's there.
pub(super) fn not_replaceable_offline(
    exe: &Path,
    folder: &Path,
    files: &[String],
) -> Option<(String, Option<String>)> {
    let find = |prefix: &str, suffix: &str| {
        files
            .iter()
            .filter(|n| n.starts_with(prefix) && n.ends_with(suffix))
            .max()
            .cloned()
    };
    let at = |name: &str| folder.join(name).display().to_string();
    let arch = std::env::consts::ARCH;
    let m = managed(exe)?;
    let (why, file) = match m {
        Managed::AppImage => {
            let f = find("BackDown-", &format!("-{arch}.AppImage"));
            let how = match &f {
                Some(n) => format!("use {} in its place", at(n)),
                None => format!(
                    "carry over the new AppImage (BackDown-…-{arch}.AppImage) and use it in its place"
                ),
            };
            (
                format!("this BackDown runs from an AppImage, which can't change itself: {how}"),
                f,
            )
        }
        Managed::System(who) => {
            let deb_arch = match arch {
                "x86_64" => "amd64",
                "aarch64" => "arm64",
                a => a,
            };
            let f = find("backdown-io_", &format!("_{deb_arch}.deb"));
            let how = match &f {
                Some(n) => format!(
                    "install the new package from the carried files instead: sudo apt install {}",
                    deb_path(&folder.join(n))
                ),
                None => format!(
                    "install the new package instead (for the .deb, carry it over too, then sudo apt install ./backdown-io_…_{deb_arch}.deb)"
                ),
            };
            (format!("{} is managed by {who}: {how}", exe.display()), f)
        }
        Managed::Other(who) => (
            format!("{} is managed by {who}: update it that way", exe.display()),
            None,
        ),
        Managed::Homebrew => {
            let f = platform().and_then(|p| find("backdown-", &format!("-{p}.tar.gz")));
            let what = f.as_deref().map_or("this system's archive".into(), at);
            (
                format!(
                    "{} is managed by Homebrew, which needs a network: unpack {what} somewhere you own (like ~/.local/bin) and use that BackDown instead",
                    exe.display()
                ),
                f,
            )
        }
        Managed::App => {
            let f = find("BackDown-", ".dmg");
            let what = f.as_deref().map_or("the new .dmg".into(), at);
            (
                format!(
                    "this BackDown is inside BackDown.app, which can't change itself: open {what} and drag BackDown to Applications"
                ),
                f,
            )
        }
        Managed::Msi => {
            let f = find("BackDown-", "-x64.msi");
            let what = f.as_deref().map_or("the new .msi".into(), at);
            (
                format!("BackDown was installed from an .msi: run {what}, which upgrades this one"),
                f,
            )
        }
    };
    Some((why, file))
}

/// A .deb's path as apt takes it: "./" before a relative one (else apt looks for a package of that
/// name), and quoted for the shell if need be.
pub(super) fn deb_path(p: &Path) -> String {
    let s = p.display().to_string();
    let s = if p.is_relative() && !s.starts_with('.') {
        format!("./{s}")
    } else {
        s
    };
    if s.bytes()
        .all(|c| c.is_ascii_alphanumeric() || b"._-+/:=@,%".contains(&c))
    {
        s
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// Whether the .msi installed BackDown for this user (it records `HKCU\Software\BackDown`
/// `program`; the setup .zip doesn't).
pub(super) fn installed_by_msi() -> bool {
    cfg!(windows)
        && Command::new("reg")
            .args(["query", r"HKCU\Software\BackDown", "/v", "program"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|st| st.success())
}

pub(super) fn bin_name() -> &'static str {
    if cfg!(windows) {
        "backdown.exe"
    } else {
        "backdown"
    }
}

/// `path --version`, waiting at most 30 s.
pub(super) fn version_of(path: &Path) -> Result<String> {
    let mut child = Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("can't run {}", path.display()))?;
    let start = Instant::now();
    loop {
        if let Some(st) = child.try_wait()? {
            let mut out = String::new();
            let mut err = String::new();
            if let Some(mut o) = child.stdout.take() {
                let _ = o.read_to_string(&mut out);
            }
            if let Some(mut e) = child.stderr.take() {
                let _ = e.read_to_string(&mut err);
            }
            if !st.success() {
                bail!(
                    "it exited with {st}{}",
                    if err.trim().is_empty() {
                        String::new()
                    } else {
                        format!(": {}", err.trim())
                    }
                );
            }
            return Ok(out.lines().next().unwrap_or_default().trim().to_string());
        }
        if start.elapsed() > Duration::from_secs(30) {
            let _ = child.kill();
            bail!("it didn't answer --version within 30 s");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub(super) fn sha256_file(p: &Path) -> Result<String> {
    let mut f = std::fs::File::open(p).with_context(|| format!("can't read {}", p.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(fmt::hex(&h.finalize()))
}

/// Take `name` out of a release archive (.tar.gz, or .zip for Windows) into `to`.
pub(super) fn unpack(archive: &Path, name: &str, to: &Path) -> Result<()> {
    if archive.to_string_lossy().ends_with(".zip") {
        return unzip_one(archive, name, to);
    }
    let f = std::fs::File::open(archive)?;
    let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(f));
    for e in ar.entries().context("the archive is damaged")? {
        let mut e = e.context("the archive is damaged")?;
        let is_it = e.path()?.file_name() == Some(std::ffi::OsStr::new(name));
        if is_it && e.header().entry_type().is_file() {
            let mut out = std::fs::File::create(to)?;
            std::io::copy(&mut e, &mut out).context("the archive is damaged")?;
            return Ok(());
        }
    }
    bail!("there's no {name} in the archive")
}

/// One file from a zip archive (stored or deflated; what 7z and zip write).
pub(super) fn unzip_one(archive: &Path, name: &str, to: &Path) -> Result<()> {
    let d = std::fs::read(archive)?;
    let u16at = |i: usize| -> Result<usize> {
        Ok(u16::from_le_bytes(d.get(i..i + 2).context("the zip is damaged")?.try_into()?) as usize)
    };
    let u32at = |i: usize| -> Result<usize> {
        Ok(u32::from_le_bytes(d.get(i..i + 4).context("the zip is damaged")?.try_into()?) as usize)
    };
    let lowest = d.len().saturating_sub(22 + 65_535);
    let eocd = (lowest..=d.len().saturating_sub(22))
        .rev()
        .find(|&i| d[i..].starts_with(b"PK\x05\x06"))
        .context("not a zip archive")?;
    let (entries, mut p) = (u16at(eocd + 10)?, u32at(eocd + 16)?);
    for _ in 0..entries {
        if !d.get(p..).is_some_and(|x| x.starts_with(b"PK\x01\x02")) {
            bail!("the zip's directory is damaged");
        }
        let (method, crc, csize, usize_) = (
            u16at(p + 10)?,
            u32at(p + 16)?,
            u32at(p + 20)?,
            u32at(p + 24)?,
        );
        let (nlen, elen, clen, local) = (
            u16at(p + 28)?,
            u16at(p + 30)?,
            u16at(p + 32)?,
            u32at(p + 42)?,
        );
        let fname = d.get(p + 46..p + 46 + nlen).context("the zip is damaged")?;
        p += 46 + nlen + elen + clen;
        let fname = String::from_utf8_lossy(fname).replace('\\', "/");
        if fname.rsplit('/').next() != Some(name) {
            continue;
        }
        if !d.get(local..).is_some_and(|x| x.starts_with(b"PK\x03\x04")) {
            bail!("the zip is damaged");
        }
        let start = local + 30 + u16at(local + 26)? + u16at(local + 28)?;
        let raw = d
            .get(start..start + csize)
            .context("the zip is cut short")?;
        let data = match method {
            0 => raw.to_vec(),
            8 => {
                let mut v = Vec::with_capacity(usize_);
                flate2::read::DeflateDecoder::new(raw).read_to_end(&mut v)?;
                v
            }
            m => bail!("{name} is compressed with method {m}, which this updater doesn't read"),
        };
        let mut c = flate2::Crc::new();
        c.update(&data);
        if data.len() != usize_ || c.sum() as usize != crc {
            bail!("{name} in the zip is damaged (its CRC doesn't match)");
        }
        std::fs::write(to, data)?;
        return Ok(());
    }
    bail!("there's no {name} in the archive")
}

#[cfg(unix)]
pub(super) fn make_executable(p: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[cfg(not(unix))]
pub(super) fn make_executable(_: &Path) -> Result<()> {
    Ok(())
}

/// Put `new` where `exe` is, in one rename. On Windows the running .exe is renamed aside first
/// (`backdown.exe.old`, removed the next time); returns that path so a failure can undo it.
pub(super) fn swap_in(new: &Path, exe: &Path) -> Result<Option<PathBuf>> {
    if cfg!(windows) {
        let old = exe.with_extension("exe.old");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(exe, &old)
            .with_context(|| format!("can't move {} aside", exe.display()))?;
        if let Err(e) = std::fs::rename(new, exe) {
            let _ = std::fs::rename(&old, exe);
            return Err(e)
                .with_context(|| format!("can't put the new version in {}", exe.display()));
        }
        Ok(Some(old))
    } else {
        std::fs::rename(new, exe).with_context(|| format!("can't replace {}", exe.display()))?;
        Ok(None)
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Previous {
    pub(super) version: String,
    pub(super) exe: PathBuf,
    pub(super) copy: PathBuf,
    pub(super) replaced_at: i64,
}

pub(super) fn previous_file() -> PathBuf {
    settings::dir().join("previous.json")
}

/// A copy of the binary about to be replaced, in the data folder, for rolling back.
pub(super) fn keep_copy(exe: &Path) -> Result<PathBuf> {
    let dir = settings::dir().join("previous");
    crate::locations::ensure_private_dir(&dir, settings::shared())?;
    let copy = dir.join(format!(
        "backdown-{}{}",
        env!("CARGO_PKG_VERSION"),
        if cfg!(windows) { ".exe" } else { "" }
    ));
    std::fs::copy(exe, &copy).with_context(|| format!("can't keep a copy of {}", exe.display()))?;
    make_executable(&copy)?;
    let rec = Previous {
        version: env!("CARGO_PKG_VERSION").into(),
        exe: exe.to_path_buf(),
        copy: copy.clone(),
        replaced_at: now(),
    };
    settings::write_private(
        &previous_file(),
        &serde_json::to_vec_pretty(&rec)?,
        settings::shared(),
    )?;
    Ok(copy)
}

/// Put a kept copy back in place of `exe`, through the same folder so the last step is a rename.
pub(super) fn restore(copy: &Path, exe: &Path) -> Result<()> {
    let stage = exe.with_file_name(format!(".{}.restore", bin_name()));
    std::fs::copy(copy, &stage).with_context(|| format!("can't copy {}", copy.display()))?;
    make_executable(&stage)?;
    if let Err(e) = swap_in(&stage, exe) {
        let _ = std::fs::remove_file(&stage);
        return Err(e);
    }
    Ok(())
}

pub struct Installed {
    pub from: String,
    pub to: String,
}

/// An empty folder beside `exe` to unpack a new version in: on the same file system, so the swap
/// is one rename.
pub(super) fn stage_beside(exe: &Path) -> Result<PathBuf> {
    let dir = exe.parent().context("where is backdown?")?;
    let stage = dir.join(".backdown-update");
    let _ = std::fs::remove_dir_all(&stage);
    if let Err(e) = std::fs::create_dir(&stage) {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            bail!(
                "can't write to {} ({e}){}",
                dir.display(),
                if cfg!(unix) {
                    ": run `sudo backdown update`, or move backdown somewhere you own (like ~/.local/bin)"
                } else {
                    ""
                }
            );
        }
        return Err(e).with_context(|| format!("can't create {}", stage.display()));
    }
    Ok(stage)
}

/// Download, verify, check and install `found` over `exe`. Any failure before the swap leaves
/// everything as it was; a new binary that fails `--version` once installed is replaced by the old.
pub fn install(found: &Found, exe: &Path, say: &mut dyn FnMut(&str)) -> Result<Installed> {
    let plat = platform().context("there are no BackDown builds for this platform")?;
    if let Some(why) = not_replaceable(exe) {
        bail!(why);
    }
    // the same release, as last listed
    let cache = load_cache();
    let release = cache
        .releases
        .iter()
        .find(|r| r.tag == found.tag)
        .context("check for updates again first")?;
    let archive = release
        .assets
        .iter()
        .find(|a| a.name == archive_name(&release.tag, plat))
        .context("that release has no build for this platform")?;
    let sums = release
        .assets
        .iter()
        .find(|a| a.name == SUMS)
        .context("that release has no SHA256SUMS.txt, so it can't be verified")?;
    let sig = release.assets.iter().find(|a| a.name == SIG);
    let version = Version::parse(&found.version).context("that release's version isn't semver")?;
    if sig.is_none() && needs_signature(&version) {
        bail!(
            "{} has no {SIG}, but every release since v{FIRST_SIGNED} is signed: it wasn't installed, and nothing was changed",
            release.tag
        );
    }
    let stage = stage_beside(exe)?;
    let result = (|| -> Result<Installed> {
        say(&format!("downloading {SUMS} for {}", release.tag));
        let sums_path = stage.join(SUMS);
        let r = http(&sums.url, &[], Some(&sums_path), 120)?;
        if r.status != 200 {
            bail!("couldn't download {SUMS} (HTTP {})", r.status);
        }
        let sums_bytes = std::fs::read(&sums_path)?;
        match sig {
            Some(sig) => {
                say(&format!("downloading {SIG}"));
                let sig_path = stage.join(SIG);
                let r = http(&sig.url, &[], Some(&sig_path), 120)?;
                if r.status != 200 {
                    bail!("couldn't download {SIG} (HTTP {})", r.status);
                }
                let who =
                    check_signature(&sums_bytes, &std::fs::read(&sig_path)?).map_err(|e| {
                        anyhow::anyhow!(
                            "{e:#}. {} wasn't installed, and nothing was changed",
                            release.tag
                        )
                    })?;
                say(&format!("{SUMS} is signed with {who}"));
            }
            None => say(&format!("{UNSIGNED}: it's checked against {SUMS} alone")),
        }
        let expect = expected_sum(&String::from_utf8_lossy(&sums_bytes), &archive.name)
            .with_context(|| {
                format!(
                    "{SUMS} doesn't list {}, so it can't be verified",
                    archive.name
                )
            })?;
        say(&format!(
            "downloading {} ({})",
            archive.name,
            fmt::bytes(archive.size)
        ));
        let arch_path = stage.join(&archive.name);
        let r = http(
            &archive.url,
            &[("Accept", "application/octet-stream".into())],
            Some(&arch_path),
            1800,
        )?;
        if r.status != 200 {
            bail!("couldn't download {} (HTTP {})", archive.name, r.status);
        }
        let got = sha256_file(&arch_path)?;
        if got != expect {
            bail!(
                "{} doesn't match {SUMS} (SHA-256 {got}, expected {expect}): it wasn't installed, and nothing was changed",
                archive.name
            );
        }
        say("SHA-256 matches SHA256SUMS.txt");
        install_verified(&arch_path, &stage, &found.version, exe, say)
    })();
    let _ = std::fs::remove_dir_all(&stage);
    result
}

/// From an archive whose SHA-256 checked out (in `stage`, beside `exe`): unpack the program, check
/// it runs and says `version`, keep a copy of the old one, swap the new one in, and put the old one
/// back if the new one doesn't run once it's there.
pub(super) fn install_verified(
    archive: &Path,
    stage: &Path,
    version: &str,
    exe: &Path,
    say: &mut dyn FnMut(&str),
) -> Result<Installed> {
    let new = stage.join(bin_name());
    unpack(archive, bin_name(), &new)?;
    make_executable(&new)?;
    let want = format!("backdown {version}");
    match version_of(&new) {
        Ok(v) if v == want => {}
        Ok(v) => bail!("the new program says {v:?}, not {want:?}: nothing was changed"),
        Err(e) => bail!("the new program doesn't run here ({e:#}): nothing was changed"),
    }
    let copy = keep_copy(exe)?;
    let aside = swap_in(&new, exe)?;
    // the installed binary must run; if it doesn't, the previous one goes back
    if let Err(why) = version_of(exe).and_then(|v| {
        if v == want {
            Ok(())
        } else {
            Err(anyhow::anyhow!("it says {v:?}"))
        }
    }) {
        match &aside {
            Some(old) => {
                let _ = std::fs::remove_file(exe);
                std::fs::rename(old, exe)?;
            }
            None => restore(&copy, exe)?,
        }
        bail!(
            "BackDown {version} didn't start once installed ({why:#}), so {} is back in place (rolled back)",
            env!("CARGO_PKG_VERSION")
        );
    }
    say(&format!(
        "installed BackDown {version} in {}",
        exe.display()
    ));
    Ok(Installed {
        from: env!("CARGO_PKG_VERSION").into(),
        to: version.to_string(),
    })
}

/// `backdown update --rollback`: the copy kept by the last update goes back in place.
pub(super) fn rollback(exe: &Path) -> Result<String> {
    let rec: Previous = serde_json::from_slice(
        &std::fs::read(previous_file())
            .context("no earlier version was kept (nothing was updated here yet)")?,
    )?;
    if rec.exe != exe {
        bail!(
            "the kept copy (BackDown {}) was for {}, not {}",
            rec.version,
            rec.exe.display(),
            exe.display()
        );
    }
    if let Some(why) = not_replaceable(exe) {
        bail!(why);
    }
    let v = version_of(&rec.copy)
        .with_context(|| format!("the kept copy {} doesn't run", rec.copy.display()))?;
    restore(&rec.copy, exe)?;
    Ok(v)
}

/// After an install: the per-user daemon runs the new binary. Under systemd, restart the service;
/// otherwise ask the daemon (through its web UI's local port) to start itself again.
pub fn restart_daemon() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let active = Command::new("systemctl")
            .args(["--user", "is-active", "--quiet", "backdown.service"])
            .status()
            .is_ok_and(|s| s.success());
        if active {
            let ok = Command::new("systemctl")
                .args(["--user", "restart", "backdown.service"])
                .status()
                .is_ok_and(|s| s.success());
            return Some(if ok {
                "restarted the BackDown service".into()
            } else {
                "couldn't restart the service: systemctl --user restart backdown".into()
            });
        }
    }
    let found = crate::app::find_daemon()?;
    if found.system {
        return Some(
            "the machine-wide service still runs the old version: sudo systemctl restart backdown"
                .into(),
        );
    }
    match crate::app::local_request(found.port, "POST", "/api/restart", &found.token) {
        Ok((200, _)) => Some("restarted the background daemon".into()),
        Ok((s, b)) => Some(format!(
            "the daemon didn't restart ({s}: {}); stop and start it",
            b.trim()
        )),
        Err(e) => Some(format!("the daemon didn't restart ({e:#})")),
    }
}

/// Windows leaves the replaced .exe renamed beside the new one until it can go.
pub fn tidy() {
    if cfg!(windows)
        && let Ok(exe) = std::env::current_exe()
    {
        let _ = std::fs::remove_file(exe.with_extension("exe.old"));
    }
}
