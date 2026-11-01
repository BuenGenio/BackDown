//! Updating from a folder, without a network: a release copied to a USB stick or a share, verified
//! the same way before anything is installed.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use backdown_core::clock::now;

use super::*;

// ---------- from a folder, without a network

/// A release's files carried over in a folder, its SHA256SUMS.txt signature checked.
pub(super) struct Bundle {
    /// the folder, as given
    pub(super) folder: PathBuf,
    /// what SHA256SUMS.txt lists: (name, SHA-256)
    pub(super) listed: Vec<(String, String)>,
    /// the names of the files in the folder
    pub(super) present: Vec<String>,
    /// who signed SHA256SUMS.txt; None when it isn't signed (allowed with --allow-unsigned)
    pub(super) signer: Option<&'static str>,
    /// the release, from the names of the archives SHA256SUMS.txt lists
    pub(super) tag: Option<String>,
}

impl Bundle {
    /// Read FOLDER's SHA256SUMS.txt and check its signature. Reads only this folder.
    pub(super) fn open(
        folder: &Path,
        allow_unsigned: bool,
        say: &mut dyn FnMut(&str),
    ) -> Result<Bundle> {
        let meta = std::fs::metadata(folder)
            .with_context(|| format!("can't open {}", folder.display()))?;
        if !meta.is_dir() {
            bail!(
                "{} isn't a folder: give the folder that holds the release's files",
                folder.display()
            );
        }
        let sums = match std::fs::read(folder.join(SUMS)) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!(
                "{} has no {SUMS}: carry it over with the release's archive and {SIG}",
                folder.display()
            ),
            Err(e) => return Err(e).with_context(|| format!("can't read {SUMS}")),
        };
        let text = String::from_utf8_lossy(&sums).into_owned();
        let listed = listed(&text);
        let tag = bundle_tag(&listed)?;
        let signer = match std::fs::read(folder.join(SIG)) {
            Ok(sig) => {
                let who = check_signature(&sums, &sig)
                    .map_err(|e| anyhow::anyhow!("{e:#}. Nothing was installed"))?;
                say(&format!("{SUMS} is signed with {who}"));
                Some(who)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && allow_unsigned => {
                let old = tag
                    .as_deref()
                    .and_then(Version::parse)
                    .is_some_and(|v| !needs_signature(&v));
                say(&format!(
                    "warning: there's no {SIG}{}, so nothing shows these files are BackDown's. With --allow-unsigned they're checked against {SUMS} alone, which shows only that they arrived whole: whoever could change them could change it too",
                    if old {
                        format!(" ({UNSIGNED})")
                    } else {
                        String::new()
                    }
                ));
                None
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!(
                "{} has no {SIG}, so nothing shows these files are BackDown's: carry it over from the same release (every release since v{FIRST_SIGNED} has one). --allow-unsigned goes ahead on {SUMS} alone, which shows only that the files arrived whole",
                folder.display()
            ),
            Err(e) => return Err(e).with_context(|| format!("can't read {SIG}")),
        };
        let mut present: Vec<String> = std::fs::read_dir(folder)
            .with_context(|| format!("can't list {}", folder.display()))?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        present.sort();
        Ok(Bundle {
            folder: folder.to_path_buf(),
            listed,
            present,
            signer,
            tag,
        })
    }

    pub(super) fn has(&self, name: &str) -> bool {
        self.present.iter().any(|n| n == name)
    }

    /// Check `file` (the bundle's `name`, or a copy of it) against SHA256SUMS.txt.
    pub(super) fn verify(&self, file: &Path, name: &str) -> Result<()> {
        let expect = self
            .listed
            .iter()
            .find_map(|(n, h)| (n == name).then_some(h))
            .with_context(|| format!("{SUMS} doesn't list {name}, so it can't be verified"))?;
        let got = sha256_file(file)?;
        if &got != expect {
            bail!(
                "{name} doesn't match {SUMS} (SHA-256 {got}, expected {expect}): it's damaged, or it was changed. Nothing was installed"
            );
        }
        Ok(())
    }

    pub(super) fn checked(&self) -> String {
        match self.signer {
            Some(who) => format!("{SUMS}, which is signed with {who}"),
            None => format!("{SUMS}, which isn't signed"),
        }
    }
}

/// The release SHA256SUMS.txt is for, from the names of the archives it lists.
pub(super) fn bundle_tag(listed: &[(String, String)]) -> Result<Option<String>> {
    let mut tags: Vec<String> = listed
        .iter()
        .filter_map(|(n, _)| {
            let rest = n.strip_prefix("backdown-")?;
            PLATFORMS.iter().find_map(|p| {
                rest.strip_suffix(&format!("-{p}.{}", archive_ext(p)))
                    .filter(|t| t.starts_with('v') && Version::parse(t).is_some())
                    .map(String::from)
            })
        })
        .collect();
    tags.sort();
    tags.dedup();
    if tags.len() > 1 {
        bail!(
            "{SUMS} lists archives of more than one release ({}): carry one release's files",
            tags.join(", ")
        );
    }
    Ok(tags.pop())
}

/// `backdown update --from FOLDER [--check]`: install (or only verify) a release carried over in a
/// folder. No network request is made.
pub(super) fn run_from(a: &Args, folder: &Path, exe: &Path) -> Result<ExitCode> {
    // checking a carried bundle is free, like checking any download; installing from one is the
    // air-gapped feature
    if !a.check {
        crate::tiers::gate_feature("airgap", "backdown update --from")?;
    }
    let mut say = |m: &str| eprintln!("  {m}");
    let b = Bundle::open(folder, a.allow_unsigned, &mut say)?;
    if a.check {
        return check_bundle(&b, exe);
    }
    // a copy that's updated another way: say what to do with the carried files
    if let Some((why, file)) = not_replaceable_offline(exe, folder, &b.present) {
        let note = match file {
            Some(f) if b.listed.iter().any(|(n, _)| *n == f) => {
                b.verify(&folder.join(&f), &f)?;
                format!(" ({f} matches {})", b.checked())
            }
            Some(f) => format!(" ({SUMS} doesn't list {f}, so it isn't verified)"),
            None => String::new(),
        };
        bail!("{why}{note}");
    }
    let plat = platform().context("there are no BackDown builds for this platform")?;
    let tag = b.tag.as_deref().with_context(|| {
        format!("{SUMS} lists no BackDown archive: is it from a BackDown release?")
    })?;
    let name = archive_name(tag, plat);
    if !b.has(&name) {
        bail!(
            "{} has no {name}, this computer's archive of {tag}: carry it over with {SUMS} and {SIG}",
            folder.display()
        );
    }
    let version = tag.trim_start_matches('v');
    let stage = stage_beside(exe)?;
    let result = (|| -> Result<Installed> {
        // copied first, and the copy checked and unpacked, so what's installed is what was checked
        let copy = stage.join(&name);
        std::fs::copy(folder.join(&name), &copy)
            .with_context(|| format!("can't copy {name} from {}", folder.display()))?;
        b.verify(&copy, &name)?;
        say(&format!("SHA-256 of {name} matches {SUMS}"));
        let v = Version::parse(version).context("the release's version isn't semver")?;
        let cur = current();
        if v <= cur && !a.allow_older {
            bail!(
                "BackDown {version} isn't newer than this one ({}): nothing was changed. --allow-older installs it anyway",
                env!("CARGO_PKG_VERSION")
            );
        }
        install_verified(&copy, &stage, version, exe, &mut say)
    })();
    let _ = std::fs::remove_dir_all(&stage);
    let done = result?;
    println!(
        "Updated BackDown {} → {} (from {}).",
        done.from,
        done.to,
        folder.display()
    );
    if let Some(r) = restart_daemon() {
        println!("{r}");
    }
    Ok(ExitCode::SUCCESS)
}

/// `--from FOLDER --check`: verify every file in the folder that SHA256SUMS.txt lists, say what it
/// would do here, and change nothing.
pub(super) fn check_bundle(b: &Bundle, exe: &Path) -> Result<ExitCode> {
    let mut ok = 0;
    for (name, _) in &b.listed {
        if b.has(name) {
            b.verify(&b.folder.join(name), name)?;
            println!("  ok  {name}");
            ok += 1;
        }
    }
    if ok == 0 {
        bail!(
            "none of the files {SUMS} lists are in {}",
            b.folder.display()
        );
    }
    let unlisted: Vec<&str> = b
        .present
        .iter()
        .filter(|n| *n != SUMS && *n != SIG && !b.listed.iter().any(|(l, _)| l == *n))
        .map(String::as_str)
        .collect();
    if !unlisted.is_empty() {
        println!("  not in {SUMS}, so not checked: {}", unlisted.join(", "));
    }
    println!(
        "{ok} file{} in {} {} {}.",
        if ok == 1 { "" } else { "s" },
        b.folder.display(),
        if ok == 1 { "matches" } else { "match" },
        b.checked()
    );
    let tag = b.tag.as_deref().unwrap_or_default();
    let here = match platform() {
        None => "There are no BackDown builds for this system, so it can't be updated from here."
            .to_string(),
        Some(_) if tag.is_empty() => format!("{SUMS} lists no BackDown archive."),
        Some(plat) => {
            let name = archive_name(tag, plat);
            let version = tag.trim_start_matches('v');
            let newer = Version::parse(version).is_some_and(|v| v > current());
            if let Some((why, _)) = not_replaceable_offline(exe, &b.folder, &b.present) {
                format!("Here: {why}.")
            } else if !b.has(&name) {
                format!(
                    "Here ({plat}): there's no {name} in the folder, so it can't update this computer (it can still update others)."
                )
            } else if newer {
                format!(
                    "Here: BackDown {version} is newer than this one ({}); `backdown update --from {}{}` installs it.",
                    env!("CARGO_PKG_VERSION"),
                    b.folder.display(),
                    if b.signer.is_none() {
                        " --allow-unsigned"
                    } else {
                        ""
                    }
                )
            } else {
                format!(
                    "Here: BackDown {version} isn't newer than this one ({}); --allow-older would install it anyway.",
                    env!("CARGO_PKG_VERSION")
                )
            }
        }
    };
    println!("{here}");
    println!("Nothing was changed.");
    Ok(ExitCode::SUCCESS)
}

pub struct Args {
    pub check: bool,
    pub refresh: bool,
    pub rollback: bool,
    pub channel: Option<String>,
    pub json: bool,
    /// install (or with --check, verify) from this folder instead of the network
    pub from: Option<PathBuf>,
    pub allow_unsigned: bool,
    pub allow_older: bool,
}

/// `backdown update [--check] [--rollback] [--from FOLDER]`
pub fn run(a: Args) -> Result<ExitCode> {
    tidy();
    let exe = dunce::canonicalize(std::env::current_exe()?)?;
    if let Some(folder) = &a.from {
        return run_from(&a, folder, &exe);
    }
    if a.rollback {
        let v = rollback(&exe)?;
        println!("put back {v} in {}", exe.display());
        if let Some(r) = restart_daemon() {
            println!("{r}");
        }
        return Ok(ExitCode::SUCCESS);
    }
    let channel = match &a.channel {
        Some(c) => Channel::parse(c)?,
        None => Channel::configured(),
    };
    // installing asks afresh unless the list is under an hour old
    let fresh_enough = load_cache()
        .checked_at
        .is_some_and(|t| now() - t < 3600 && load_cache().source == api_root());
    let st = check(a.refresh || (!a.check && !fresh_enough), channel);
    if a.json {
        println!("{}", serde_json::to_string_pretty(&st)?);
        return Ok(if st.newer {
            ExitCode::from(10)
        } else {
            ExitCode::SUCCESS
        });
    }
    if let Some(e) = &st.error {
        eprintln!("note: {e}");
    }
    let Some(latest) = st.latest.as_ref().filter(|_| st.newer) else {
        if st.checked_at.is_none() {
            bail!("couldn't find out whether there's a newer BackDown");
        }
        println!(
            "BackDown {} is the newest {} (checked {}).",
            st.current,
            match channel {
                Channel::Stable => "release",
                Channel::Preview => "version, previews included",
            },
            crate::utc(st.checked_at.unwrap_or_default())
        );
        return Ok(ExitCode::SUCCESS);
    };
    println!(
        "BackDown {} is available (this is {}). Release notes: {}",
        latest.version, st.current, latest.notes_url
    );
    if a.check {
        println!("Install it with `backdown update`.");
        return Ok(ExitCode::from(10));
    }
    let done = install(latest, &exe, &mut |m| eprintln!("  {m}"))?;
    println!("Updated BackDown {} → {}.", done.from, done.to);
    if let Some(r) = restart_daemon() {
        println!("{r}");
    }
    Ok(ExitCode::SUCCESS)
}
