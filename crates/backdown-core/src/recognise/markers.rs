//! What names say: the folders and files that mark a kind of thing (a mail store, a backup, a
//! repository…) wherever they turn up, and the extensions grouped by what they hold.

use std::path::{MAIN_SEPARATOR_STR, Path};

use crate::{paths, timemachine};

// ---------- names

pub(super) fn sep() -> Vec<u8> {
    paths::to_bytes(Path::new(MAIN_SEPARATOR_STR))
}

/// The last component of `path` below `parent`, as text (lossy for odd bytes).
pub(super) fn child_name(parent: &[u8], path: &[u8], sep: &[u8]) -> Option<String> {
    let rest = path.strip_prefix(parent)?.strip_prefix(sep)?;
    Some(paths::from_bytes(rest).to_string_lossy().into_owned())
}

pub(super) fn ext(name: &str) -> String {
    name.rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default()
}

/// Folder names that mean something on their own or next to their siblings.
pub(super) fn dir_marker(lower: &str) -> bool {
    matches!(
        lower,
        "cur"
            | "new"
            | "tmp"
            | "maildir"
            | "storage"
            | "mailboxes"
            | "dbox-mails"
            | "imapmail"
            | "mail"
            | ".git"
            | "objects"
            | "refs"
            | "backups.backupdb"
            | "dcim"
            | "appdata"
            | "library"
            | "desktop"
            | "documents"
            | "downloads"
            | "movies"
            | "music"
            | "pictures"
            | "public"
            | "videos"
            | "etc"
            | "usr"
            | "var"
            | "bin"
            | "sbin"
            | "lib"
            | "boot"
            | "opt"
            | "srv"
            | "root"
            | "home"
            | "proc"
            | "dev"
            | "windows"
            | "program files"
            | "users"
            | ".config"
            | ".local"
            | ".ssh"
            | ".cache"
            | "node_modules"
            | "__pycache__"
            | ".npm"
            | ".gradle"
            | ".m2"
            | "target"
            | ".venv"
            | "venv"
            | ".thumbnails"
            | "thumbnails"
            | "$recycle.bin"
            | "recycler"
            | "system volume information"
            | ".spotlight-v100"
            | ".fseventsd"
            | ".documentrevisions-v100"
            | ".temporaryitems"
            | ".trashes"
            | "caches"
            | "temp"
            | "preview"
            | "files_trashbin"
            | "files"
            | "wp-content"
            | "wp-admin"
            | "administrator"
            | "apps"
            | "core"
            | "config"
            | "twrp"
            | "titaniumbackup"
            | "android"
            | "takeout"
            | "lost+found"
            | "incoming"
            | ".stfolder"
            | ".sync"
            | ".dropbox.cache"
            | "vhosts"
            | "sites"
    ) || lower.starts_with("appdata_")
        || lower.starts_with("updater-")
        || lower.starts_with(".trash")
        || lower.starts_with("found.")
        || lower.starts_with("recup_dir")
        || lower.ends_with(".photoslibrary")
        || lower.ends_with(".sparsebundle")
        || lower.ends_with(".backupbundle")
        || timemachine::apfs_snapshot(lower)
        || lower.starts_with("takeout-")
        || lower.starts_with("facebook-")
        || lower.starts_with("instagram-")
        || lower.starts_with("twitter-")
        || lower.contains("linkedindataexport")
}

/// File names that mean something next to their siblings.
pub(super) fn file_marker(lower: &str) -> bool {
    matches!(
        lower,
        "dovecot-uidlist"
            | "dovecot.index"
            | "dovecot.index.log"
            | "dovecot.map.index"
            | "dovecot.list.index"
            | "subscriptions"
            | "maildirfolder"
            | "manifest.db"
            | "manifest.mbdb"
            | "info.plist"
            | "status.plist"
            | ".ocdata"
            | "ntuser.dat"
            | "prefs.js"
            | ".bashrc"
            | ".zshrc"
            | ".profile"
            | ".bash_history"
            | ".zsh_history"
            | ".cfusertextencoding"
            | "wp-config.php"
            | "configuration.php"
            | "composer.json"
            | "artisan"
            | "index.php"
            | ".htaccess"
            | "head"
            | "config"
            | "description"
            | "hostname"
            | "os-release"
            | "archive_browser.html"
            | "cargo.toml"
            | "package.json"
            | "config.php"
            | "nextcloud.log"
            | ".nextcloudsync.log"
            | ".owncloudsync.log"
            | ".dropbox"
            | "id"
            | "thumbs.db"
            | ".ds_store"
            | "com.apple.timemachine.machineid.plist"
            | ".com.apple.timemachine.machineid.plist"
            | "com.apple.timemachine.machineid.bckup"
            | "com.apple.timemachine.snapshothistory.plist"
    ) || lower.starts_with("._sync_")
        || lower.ends_with(".lrcat")
        || lower.ends_with(".msf")
}

pub(super) const DISK_IMAGE: &[&str] = &[
    "img",
    "iso",
    "dd",
    "raw",
    "dmg",
    "vdi",
    "vmdk",
    "qcow2",
    "vhd",
    "vhdx",
    "sparseimage",
    "wim",
    "e01",
    "toast",
];
pub(super) const AUDIO: &[&str] = &[
    "mp3", "m4a", "aac", "flac", "wav", "ogg", "oga", "opus", "wma", "aiff", "aif", "alac", "ape",
];
pub(super) const VIDEO: &[&str] = &[
    "mp4", "m4v", "mov", "mkv", "webm", "avi", "wmv", "mpg", "mpeg", "3gp", "mts", "m2ts", "vob",
    "ts",
];
/// photographs: camera and phone formats (icons, screenshots and web graphics are PNG, GIF, SVG…)
pub(super) const IMAGE: &[&str] = &[
    "jpg", "jpeg", "heic", "heif", "tif", "tiff", "raw", "cr2", "cr3", "nef", "arw", "dng", "orf",
    "rw2", "raf", "srw", "pef",
];
/// a library's files are real media, not icons or ringtones: average size at least this (audio, video, photos)
pub(super) const MIN_AVERAGE: [u64; 3] = [1 << 20, 2 << 20, 50 << 10];
pub(super) const INSTALLER: &[&str] = &[
    "exe", "msi", "dmg", "pkg", "deb", "rpm", "zip", "iso", "7z", "rar", "appimage", "apk", "tgz",
    "gz", "xz", "bz2", "tar",
];
pub(super) const SCRIPT: &[&str] = &["sh", "bash", "bat", "cmd", "ps1", "cron", "command"];

pub(super) fn is_sql_dump(lower: &str) -> bool {
    lower.ends_with(".sql")
        || lower.ends_with(".sql.gz")
        || lower.ends_with(".sql.bz2")
        || lower.ends_with(".sql.xz")
        || lower.ends_with(".sql.zst")
        || lower.ends_with(".dump")
        || lower.ends_with(".pgdump")
}
