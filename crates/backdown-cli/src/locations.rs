//! Where BackDown keeps its index, its UI token and its control socket.
//!
//! Per user: the platform's usual places (XDG on Linux, ~/Library on macOS, %LOCALAPPDATA% on
//! Windows). The system-wide service (Linux) uses /var/lib/backdown and /run/backdown.

use std::path::PathBuf;

pub const SYSTEM_STATE: &str = "/var/lib/backdown";
pub const SYSTEM_RUNTIME: &str = "/run/backdown";

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
}

fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

/// The index and the UI token (per user).
pub fn state_dir() -> PathBuf {
    if cfg!(target_os = "macos") {
        home().join("Library/Application Support/BackDown")
    } else if cfg!(windows) {
        env_dir("LOCALAPPDATA")
            .unwrap_or_else(|| home().join("AppData").join("Local"))
            .join("BackDown")
    } else {
        env_dir("XDG_DATA_HOME")
            .unwrap_or_else(|| home().join(".local/share"))
            .join("backdown")
    }
}

/// The control socket (per user): somewhere private that disappears at logout where possible.
pub fn runtime_dir() -> PathBuf {
    match env_dir("XDG_RUNTIME_DIR") {
        Some(d) if cfg!(target_os = "linux") => d.join("backdown"),
        _ => state_dir(),
    }
}

pub fn default_db() -> PathBuf {
    state_dir().join("index.db")
}

pub const SOCKET: &str = "daemon.sock";
/// This installation's key for signing manifests (backdown_core::signing), in the state folder
pub const SIGNING_KEY: &str = "signing.key";
pub const UI_FILE: &str = "ui.json";

/// Sockets a client should try, most specific first: this user's daemon, then the system one.
pub fn candidate_sockets() -> Vec<PathBuf> {
    let mut v = vec![runtime_dir().join(SOCKET)];
    if cfg!(target_os = "linux") {
        v.push(PathBuf::from(SYSTEM_RUNTIME).join(SOCKET));
    }
    v
}

/// UI token files, in the same order.
pub fn candidate_ui_files() -> Vec<PathBuf> {
    let mut v = vec![state_dir().join(UI_FILE)];
    if cfg!(target_os = "linux") {
        v.push(PathBuf::from(SYSTEM_STATE).join(UI_FILE));
    }
    v
}

/// Create `dir` if needed and, for a per-user setup, make it owner-only: the index lists every file
/// path on your drives. (The system service's folders are managed by systemd: 0750, group backdown.)
pub fn ensure_private_dir(dir: &std::path::Path, shared: bool) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    if !shared {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    let _ = shared;
    Ok(())
}
