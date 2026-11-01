//! The daemon's side: a daily look for a newer release, and installs started from the browser.

use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Value, json};

use crate::settings;

use super::*;

// ---------- the daemon's side: a daily look, and installs from the browser

/// Look once a day (per Settings) whether a newer BackDown is out, so the browser can say so.
/// Not for the machine-wide service, which has no network.
pub fn spawn_checker(shared: bool) {
    if shared || std::env::var_os("BACKDOWN_NO_UPDATE_CHECK").is_some() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("updates".into())
        .spawn(|| {
            std::thread::sleep(Duration::from_secs(90));
            loop {
                if settings::load().update_check {
                    let _ = refresh(false);
                }
                std::thread::sleep(Duration::from_secs(3600));
            }
        });
}

#[derive(Clone, Default, Serialize)]
pub(super) struct Install {
    pub(super) state: &'static str, // "" | "running" | "done" | "failed"
    pub(super) message: String,
    pub(super) version: String,
}

pub(super) static INSTALL: Mutex<Install> = Mutex::new(Install {
    state: "",
    message: String::new(),
    version: String::new(),
});

/// GET /api/update (what's known), POST /api/update-check (ask GitHub now).
pub fn status_json(ask: bool, env: &settings::Env) -> Value {
    let channel = Channel::configured();
    let st = if ask {
        check(true, channel)
    } else {
        status_of(&load_cache(), channel)
    };
    let exe = settings::started_exe().or_else(|| std::env::current_exe().ok());
    let why_not = if env.daemon.is_none() {
        Some("installing runs in the BackDown service; in a terminal: backdown update".to_string())
    } else if !env.desktop {
        Some(
            "the machine-wide service is updated by whoever installed it: sudo backdown update"
                .to_string(),
        )
    } else {
        exe.as_deref().and_then(not_replaceable)
    };
    let install = INSTALL.lock().unwrap_or_else(|e| e.into_inner()).clone();
    json!({
        "status": st,
        "enabled": settings::load().update_check,
        "can_install": why_not.is_none(),
        "why_not": why_not,
        "install": install,
    })
}

/// POST /api/update-install: install the newer version in the background, then restart.
pub fn start_install(env: &settings::Env) -> Result<Value> {
    if env.daemon.is_none() || !env.desktop {
        bail!(
            "installing updates runs in your own BackDown service; in a terminal: backdown update"
        );
    }
    let exe = settings::started_exe()
        .or_else(|| std::env::current_exe().ok())
        .context("where is backdown?")?;
    if let Some(why) = not_replaceable(&exe) {
        bail!(why);
    }
    let st = status_of(&load_cache(), Channel::configured());
    let found = st
        .latest
        .filter(|_| st.newer)
        .context("there's no newer version to install: check again")?;
    {
        let mut i = INSTALL.lock().unwrap_or_else(|e| e.into_inner());
        if i.state == "running" {
            bail!("an update is being installed already");
        }
        *i = Install {
            state: "running",
            message: "starting".into(),
            version: found.version.clone(),
        };
    }
    std::thread::spawn(move || {
        let r = install(&found, &exe, &mut |m| {
            eprintln!("update: {m}");
            INSTALL.lock().unwrap_or_else(|e| e.into_inner()).message = m.to_string();
        });
        let mut i = INSTALL.lock().unwrap_or_else(|e| e.into_inner());
        match r {
            Ok(done) => {
                i.state = "done";
                i.message = format!("installed {}; restarting", done.to);
                drop(i);
                settings::restart_soon();
            }
            Err(e) => {
                eprintln!("update failed: {e:#}");
                i.state = "failed";
                i.message = format!("{e:#}");
            }
        }
    });
    Ok(json!({ "ok": true, "version": st.current }))
}
