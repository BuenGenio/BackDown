---
title: Getting started
description: Install BackDown, index two drives, see what's duplicated, find out whether a drive is safe to wipe, and browse it all.
order: 1
group: start
---

A first session with the command-line preview: install it, index two drives, see what's duplicated, ask whether one of them is safe to wipe, and look around in the browser. The examples use an old drive mounted at `/media/old-drive` and an archive drive at `/media/archive`. Use your own paths; on Windows, drive letters such as `E:\`.

BackDown only reads the drives you index. The one file it writes is its index, in your own data folder ([where the index lives](/docs/troubleshooting/#where-the-index-lives)).

## Install

Download the archive for your computer from the [download page](/download/), and `SHA256SUMS.txt` from the same release. Checking the file against it tells you it's the one we published: preview builds aren't signed yet.

```sh
# Linux
sha256sum -c --ignore-missing SHA256SUMS.txt
# macOS
shasum -a 256 -c --ignore-missing SHA256SUMS.txt
```

Each archive holds the `backdown` program, a `README.txt` and the third-party licences.

### Linux

One static binary each for x86-64 and ARM64. Unpack it and put it on your `PATH`:

```sh
mkdir backdown && tar xzf backdown-*-linux-*.tar.gz -C backdown
sudo mv backdown/backdown /usr/local/bin/
backdown --version
```

Without sudo, `~/.local/bin` works as well, if it's on your `PATH`.

### macOS

One binary for Apple silicon and Intel. Because preview builds aren't signed yet, remove the quarantine flag once (or allow it in System Settings > Privacy & Security after the first try):

```sh
mkdir backdown && tar xzf backdown-*-macos-universal.tar.gz -C backdown
xattr -d com.apple.quarantine backdown/backdown
sudo mv backdown/backdown /usr/local/bin/
```

### Windows

Unzip `backdown-…-windows-x86_64.zip` and run `backdown.exe` from a terminal (PowerShell or Command Prompt) in that folder, or add the folder to your `PATH`. SmartScreen may ask you to confirm the first time: "More info", then "Run anyway". To check the download, compare this with its line in `SHA256SUMS.txt`:

```powershell
Get-FileHash .\backdown-*.zip
```

## Index two drives

```sh
backdown index /media/old-drive /media/archive
```

```text
/media/old-drive: 44 files, 182 MB (new 44, changed 0, gone 0) · scan 1
/media/archive: 39 files, 222 MB (new 39, changed 0, gone 0) · scan 2
```

Each path you index becomes a *root*: a drive, or a top-level folder, that BackDown keeps track of. Indexing records every file's path, size and modification time. It doesn't read the files' contents yet, so it's quick. The numbers here come from a small demo; a real drive has hundreds of thousands of files.

Every run is a *scan*. Run the same command again later and BackDown records what's new, changed or gone, keeps what it already knows about unchanged files, and keeps the earlier versions for the [time slider](/docs/concepts/#scans-and-the-time-slider).

On Windows:

```powershell
backdown index E:\ F:\Archive
```

## See what's duplicated

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

## Is the old drive safe to wipe?

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

## Browse it

```sh
backdown serve
```

```text
BackDown browser for /home/you/.local/share/backdown/index.db at
  http://127.0.0.1:8787/?t=4f0c…
(only this link, only on this computer; Ctrl-C to stop)
```

This opens the file browser in your web browser: every indexed drive, every folder as it was at any scan, every version and copy of a file, and read-only viewers for photos, PDFs, documents, databases, code and more. It listens on this computer only, and the link carries a private token that changes each time. [Browse without restoring](/docs/guides/browse-without-restoring/) shows what it can do.

### Keep it running

The daemon runs in the background: it rescans each drive daily and whenever one is plugged back in, hashes after every scan, and keeps the browser available.

```sh
backdown service install
backdown open
```

On Linux, `service install` sets up a systemd user service; it needs no root. With the next preview it also works on macOS (a LaunchAgent) and Windows (a task that starts when you log on); until then, start `backdown daemon` yourself there. `backdown open` opens the browser any time. [The daemon](/docs/concepts/#the-daemon) explains what it does.

## Produce a report {preview} {#produce-a-report}

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
- [CLI reference](/docs/cli/): every command and option.
