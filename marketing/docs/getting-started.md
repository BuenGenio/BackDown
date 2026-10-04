---
title: Getting started
description: Install BackDown, open it, index your drives, see what's duplicated, and find out whether a drive is safe to wipe, in the app or in a terminal.
order: 1
group: start
---

A first session: install BackDown, open it, index two drives, see what's duplicated, and ask whether one of them is safe to wipe. Everything can be done in the app's window or in a terminal; this page shows both. The examples use an old drive mounted at `/media/old-drive` and an archive drive at `/media/archive`. Use your own paths; on Windows, drive letters such as `E:\`.

BackDown only reads the drives you index. The one file it writes is its index, in your own data folder ([where the index lives](/docs/troubleshooting/#where-the-index-lives)).

## Install

Download what suits your computer from the [download page](/download/), and `SHA256SUMS.txt` from the same release. Checking the file against it tells you it arrived whole and is the one in that release. From v1.0.0-beta.4, each release also has `SHA256SUMS.txt.sig`, a signature of `SHA256SUMS.txt` made with BackDown's release key: [checking it](/docs/guides/air-gapped/#check-a-signature-without-backdown) tells you the sums are the ones we published. BackDown's builds aren't signed with an Apple or Microsoft certificate yet.

```sh
# Linux
sha256sum -c --ignore-missing SHA256SUMS.txt
# macOS
shasum -a 256 -c --ignore-missing SHA256SUMS.txt
```

```powershell
# Windows: compare this with the file's line in SHA256SUMS.txt
Get-FileHash .\BackDown-*.msi
```

| System | Installers | Or just the program |
|---|---|---|
| Linux (x86-64 and ARM64) | a `.deb` (Debian, Ubuntu and their relatives), or an AppImage (any distribution) | `backdown-…-linux-x86_64.tar.gz` or `-linux-arm64.tar.gz` |
| macOS 11 or later (Apple silicon and Intel) | `BackDown-….dmg`, or the same app as `BackDown-…-macos.zip` | `backdown-…-macos-universal.tar.gz` |
| Windows (x86-64) | `BackDown-…-x64.msi`, or `BackDown-…-windows-x86_64-setup.zip` | `backdown-…-windows-x86_64.zip` |

The installers add BackDown to your applications menu, Launchpad or Start menu. The plain archives hold the `backdown` program, a `README.txt` and the third-party licences; `backdown desktop install` adds one of them to your applications afterwards. Either way, it's one program: the app and the `backdown` command are the same thing.

> [!NOTE]
> This is a beta. It's the first release with the desktop app, and the first whose macOS and Windows builds were made without our automated build service (they were cross-built on Linux); they haven't been run on a real Mac or Windows PC yet. If something doesn't work, please [open an issue](https://github.com/BuenGenio/backdown-releases/issues) with what you did and what it said.

### Linux

The `.deb` installs the program as `/usr/bin/backdown`, the menu entry and icons, and a background service for each user that nobody's turned on:

```sh
sudo apt install ./backdown-io_*_amd64.deb
```

The package is called `backdown-io` because Debian and Ubuntu already have an unrelated duplicate remover called `backdown`. If a terminal ever suggests `sudo apt install backdown`, that's the other program. Both install `/usr/bin/backdown`, so ours replaces it, along with the `backdown` package of BackDown beta.1 and earlier.

Nothing starts by itself: open **BackDown** from your applications menu. The package manager keeps it up to date (download the new `.deb`); `backdown update` says so rather than changing a packaged file.

The AppImage needs no installing. Make it executable and run it: with no arguments it opens BackDown's window, and with arguments it's the `backdown` command.

```sh
chmod +x BackDown-*-x86_64.AppImage
./BackDown-*-x86_64.AppImage                       # the app
./BackDown-*-x86_64.AppImage index /media/old-drive  # the command
./BackDown-*-x86_64.AppImage desktop install       # add it to the applications menu
```

An AppImage can't replace itself: to update, download the new one.

The plain archive is one static program. Unpack it and put it on your `PATH` (without sudo, `~/.local/bin` works as well, if it's on your `PATH`):

```sh
mkdir backdown && tar xzf backdown-*-linux-*.tar.gz -C backdown
sudo mv backdown/backdown /usr/local/bin/
backdown --version
backdown desktop install    # optional: the applications menu
```

### macOS

Open the `.dmg` and drag BackDown onto Applications (or unzip the `.zip` and move `BackDown.app` there). BackDown isn't notarised by Apple yet, so the first time macOS may say it can't check it:

1. In Applications, right-click (or Control-click) BackDown and choose **Open**, then **Open** again.
2. On macOS 15 and later, if there's no Open button: System Settings > Privacy & Security, then **Open Anyway** beside BackDown's name.

After that it opens like any other app. The `backdown` command is inside it; to use it in a terminal, link it onto your `PATH`:

```sh
sudo ln -s /Applications/BackDown.app/Contents/MacOS/backdown /usr/local/bin/backdown
```

With the plain archive instead, remove the quarantine flag once (or allow it in System Settings > Privacy & Security after the first try):

```sh
mkdir backdown && tar xzf backdown-*-macos-universal.tar.gz -C backdown
xattr -d com.apple.quarantine backdown/backdown
sudo mv backdown/backdown /usr/local/bin/
backdown desktop install    # optional: ~/Applications/BackDown.app
```

### Windows

The `.msi` installs BackDown for you alone, without administrator rights: into `%LOCALAPPDATA%\Programs\BackDown`, with a Start menu entry, and on your `PATH` for terminals opened afterwards. The setup `.zip` does the same without Windows Installer: unzip it and double-click **Install BackDown.cmd**; it opens BackDown when it's done. Either one is removed in Settings > Apps.

Because the files aren't code-signed yet, SmartScreen may ask the first time: **More info**, then **Run anyway**.

With the plain `.zip`, unzip it and run `backdown.exe` from a terminal (PowerShell or Command Prompt) in that folder, or add the folder to your `PATH`; `backdown desktop install` adds a Start menu entry.

## Open BackDown

Open **BackDown** from your applications menu, Launchpad or Start menu, or in a terminal:

```sh
backdown app
```

It starts BackDown's background service if it isn't running (the one you installed, or else one of its own), and opens in a window of its own. The window is Chrome, Chromium, Edge or Brave, whichever you have, running as an app with a profile of its own; without any of them, BackDown opens in a tab of your usual web browser. Opening it again brings the open window forward instead of starting anything twice.

The first time, **Activity** welcomes you with the drives connected to this computer. Choose **Index this** on each drive you want to know about, or type a folder under "Or a folder" and press **Index it**. BackDown only reads them. Each drive appears on the left when its scan is done, and hashing follows by itself. Then:

- **Duplicates** shows what's stored twice, the groups that free the most first: open one to see where each copy is, or group by **Folders** to see which folders hold copies of each other. The filter bar narrows it by type, size, date and hidden files, and the copies you tick go to **Free up**;
- **What's here** shows what things are: mail stores, phone backups, photo libraries and more;
- a drive's **Report** button gives the whole picture.

On Linux and macOS, a drive that isn't mounted can be mounted from Activity, read-only ([mounting drives](/docs/concepts/#mounting-drives-read-only)).

The background service keeps running after you close the window, rescanning drives when they're due or plugged back in. To have it start whenever you log in, `backdown service install` ([keep it running](#keep-it-running)).

## The same in a terminal

### Index two drives

```sh
backdown index /media/old-drive /media/archive
```

```text
/media/old-drive: 44 files, 182 MB (new 44, changed 0, gone 0) · scan 1
  full walk in under a second: Linux keeps no change log for drives; not an NTFS drive
/media/archive: 39 files, 222 MB (new 39, changed 0, gone 0) · scan 2
  full walk in under a second: Linux keeps no change log for drives; not an NTFS drive
```

Each path you index becomes a *root*: a drive, or a top-level folder, that BackDown keeps track of. Indexing records every file's path, size and modification time. It doesn't read the files' contents yet, so it's quick. The numbers here come from a small demo; a real drive has hundreds of thousands of files. The second line says how the drive was scanned, and why: on Windows and macOS a rescan can often read the drive's own change log instead of every folder ([fast rescans](/docs/concepts/#fast-rescans)).

Every run is a *scan*. Run the same command again later and BackDown records what's new, changed or gone, keeps what it already knows about unchanged files, and keeps the earlier versions for the [time slider](/docs/concepts/#scans-and-the-time-slider).

On Windows:

```powershell
backdown index E:\ F:\Archive
```

### See what's duplicated

```sh
backdown dupes
```

`dupes` first hashes whatever could be a duplicate, then lists groups of identical files, the ones that waste the most space first. A file whose size no other file shares is never read; the rest are sampled, and only files whose samples match are read in full ([how hashing works](/docs/concepts/#hashes-and-proof)).

```text
219 MB reclaimable in 32 groups of duplicates (files ≥ 1M)

120 MB  3 × 60 MB  sha256 f5511040fea1…
    /media/archive/Backups/laptop-2019/VM/win10-copy.vdi
    /media/archive/Backups/laptop-2019/VM/win10.vdi
    /media/old-drive/VM/win10.vdi
5 MB  2 × 5 MB  sha256 cccfc2d24435…
    /media/archive/Media/Photos/2014/IMG_2014003.jpg
    /media/old-drive/Photos/2014/IMG_2014003.jpg
```

`--min-size 100M` leaves out smaller files; `--limit 50` lists more groups. Hardlinks count once, so the reclaimable figure is real.

### Is the old drive safe to wipe?

```sh
backdown check /media/old-drive
```

```text
/media/old-drive
  44 files, 182 MB
  verified       162 MB in 37 files (identical copy elsewhere)
  unique          20 MB in 7 files (no copy anywhere else)

COPY FIRST: 20 MB in 7 files exists nowhere else.
       19 MB         5 files  Photos
      767 KB         1 files  Documents
        14 B         1 files  .
  (--list shows every file)
```

A drive is safe to wipe when every file on it has a SHA-256-identical copy somewhere outside it. Here 7 files don't, so the verdict is "copy first", with the folders they're in. `--list` names them. Once they're copied and the archive is indexed again, the same command says:

```text
SAFE TO WIPE: every file has a SHA-256-identical copy outside /media/old-drive.
```

`check` exits with 0 when the drive is safe to wipe and 2 when it isn't, so scripts can use it. [What "safe to wipe" means](/docs/concepts/#what-safe-to-wipe-means) has the details, and [Free up a drive](/docs/guides/free-up-a-drive/) goes on from here.

### Browse it

```sh
backdown serve
```

```text
BackDown browser for /home/you/.local/share/backdown/index.db at
  http://127.0.0.1:8787/?t=4f0c…
(only this link, only on this computer; Ctrl-C to stop)
```

`serve` opens the same browser as the app, in your web browser, without the background service: every indexed drive, every folder as it was at any scan, every version and copy of a file, and read-only viewers for photos, PDFs, documents, databases, code and more. It listens on this computer only (unless you [ask for more](/docs/guides/browse-without-restoring/#look-from-your-other-devices)), and the link carries a private token that changes each time. It can't change anything (copying, freeing up, rescues and rescans need the service), and says which command to run instead. [Browse without restoring](/docs/guides/browse-without-restoring/) shows what it can do.

### Keep it running {#keep-it-running}

The background service (the *daemon*) rescans each drive daily and whenever one is plugged back in, hashes after every scan, reads each drive's [health](/docs/guides/drive-health/), and keeps the browser available.

```sh
backdown service install
backdown open
```

`service install` runs it as you, with no extra rights: a systemd user service on Linux, a LaunchAgent on macOS, and a scheduled task that starts it when you log on on Windows. If you installed the `.deb`, its service is already there: `systemctl --user enable --now backdown` turns it on. `backdown open` opens the browser in your usual web browser, and `backdown app` in its own window. [The daemon](/docs/concepts/#the-daemon) explains what it does.

### Produce a report {#produce-a-report}

The whole picture as one document: every drive with its verdict, what's on it, how much of it exists elsewhere, and the biggest duplicates.

```sh
backdown report > report.md
backdown report --html -o report.html
```

The browser's "What's here" view has the same report, as a page or a Markdown download. The report reads the index as it is, so run `backdown dupes` or `backdown hash` first to count every duplicate.

## Where next

- [Concepts](/docs/concepts/): drives, scans, hashes and what "safe to wipe" means.
- [Free up a drive](/docs/guides/free-up-a-drive/): delete only what has a verified copy.
- [File your old drives into one archive](/docs/guides/file-into-an-archive/): a plan, then verified copies.
- [Settings, updates and licences](/docs/guides/settings-and-licences/): the Settings view, updates, and what's free.
- [CLI reference](/docs/cli/): every command and option.
