//! `backdown completions`: tab completion for backdown's commands and options, generated from
//! the same definitions as `--help` (so it never falls behind), for bash, zsh, fish, PowerShell
//! and elvish. The .deb installs bash, zsh and fish completions system-wide; `--install` puts the
//! script for your shell where it picks it up, for you alone.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::CommandFactory;
use clap_complete::Shell;

use crate::locations;

pub fn run(shell: Option<Shell>, install: bool) -> Result<ExitCode> {
    let shell = shell.or_else(Shell::from_env).context(
        "which shell? e.g. `backdown completions bash` (or zsh, fish, powershell, elvish)",
    )?;
    let mut script = Vec::new();
    clap_complete::generate(shell, &mut crate::Cli::command(), "backdown", &mut script);
    if !install {
        use std::io::Write;
        std::io::stdout().write_all(&script)?;
        return Ok(ExitCode::SUCCESS);
    }
    let (path, then) = place(shell)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("can't create {}", dir.display()))?;
    }
    std::fs::write(&path, &script).with_context(|| format!("can't write {}", path.display()))?;
    println!("Tab completion for {shell}: {}\n{then}", path.display());
    Ok(ExitCode::SUCCESS)
}

/// Where a shell looks for one user's completions, and what to do for it to take effect.
fn place(shell: Shell) -> Result<(PathBuf, &'static str)> {
    let xdg = |var: &str, fallback: &str| {
        std::env::var_os(var)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| locations::home().join(fallback))
    };
    Ok(match shell {
        // bash-completion's loader looks here on demand (it comes with most Linux desktops)
        Shell::Bash => (
            xdg("XDG_DATA_HOME", ".local/share").join("bash-completion/completions/backdown"),
            "New terminals complete `backdown` with Tab (it needs the bash-completion package).",
        ),
        Shell::Fish => (
            xdg("XDG_CONFIG_HOME", ".config").join("fish/completions/backdown.fish"),
            "New fish sessions complete `backdown` with Tab.",
        ),
        Shell::Zsh => (
            locations::home().join(".zfunc/_backdown"),
            "If ~/.zshrc doesn't have them yet, add `fpath=(~/.zfunc $fpath)` and then \
             `autoload -Uz compinit && compinit`, and open a new terminal.",
        ),
        other => bail!(
            "--install knows bash, zsh and fish; for {other}, add the output of `backdown \
             completions {other}` to your profile"
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shell_gets_the_commands_and_their_options() {
        for shell in [
            Shell::Bash,
            Shell::Zsh,
            Shell::Fish,
            Shell::PowerShell,
            Shell::Elvish,
        ] {
            let mut out = Vec::new();
            clap_complete::generate(shell, &mut crate::Cli::command(), "backdown", &mut out);
            let s = String::from_utf8(out).unwrap();
            for word in [
                "index",
                "dupes",
                "wipe",
                "rescue",
                "token",
                "completions",
                "min-size",
            ] {
                assert!(s.contains(word), "{shell}: no {word}");
            }
        }
    }
}
