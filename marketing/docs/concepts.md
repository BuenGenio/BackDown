---
title: Concepts
description: Drives and scans, the time slider, hashes and proof, what "safe to wipe" means, recognised things and where they came from, the daemon, and drive health.
order: 2
group: start
---

The ideas the rest of the docs rely on. Each section says what BackDown does, and what it doesn't.

## Drives and roots

A *root* is a drive, or a top-level folder, that you've indexed: `/media/old-drive`, `/mnt/nas`, `E:\`. BackDown calls it a drive and names it after the last part of its path (`old-drive`). Roots can't be nested inside each other: index each drive or top-level folder once.

BackDown knows a root by its path. Plug a drive back in where it was mounted before and BackDown carries on with it; mounted somewhere else, it's a new root. A root whose folder is missing, or empty although it held files, counts as not connected: its files aren't read, and they wait for it.

Indexing stays on one file system unless you pass `--cross-filesystems`, so a drive's scan doesn't wander into other drives mounted inside it. Symbolic links are neither followed nor recorded.

`backdown volumes` {preview} lists every disk and partition on the computer (model, serial number, file system, label, where it's mounted) and marks the ones the index covers. It only reads, and needs no administrator rights.

## Scans and the time slider

Every `backdown index` run records a *scan* of each root: every file's path, size and modification time, and on Linux and macOS its device and inode, so hardlinks are recognised. The files' contents aren't read while scanning.

When you index a drive again:

- a file with the same path, size and modification time is unchanged, and keeps everything known about it, hashes included;
- a changed file gets a new version, and the old one is kept;
- a file that's gone is marked gone, and its last version is kept.

A renamed or moved file counts as gone at the old path and new at the new one. A scan that's interrupted doesn't count; run the command again.

Because every version is kept, the browser can show any folder as it was at any scan: that's the *time slider* at the bottom of a drive with two scans or more. An old version can be opened while its exact bytes still exist somewhere indexed, either the file itself, unchanged, or an identical copy (same SHA-256) on an indexed drive. Otherwise the index still remembers its size, dates and hash, but the bytes are gone. Hashing after each scan keeps that fingerprint; the daemon does it for you.

If a drive that held files is suddenly empty, `index` refuses to record it: usually the drive isn't plugged in and you're looking at its empty mount point. If it really was emptied, pass `--allow-empty`.

### Fast rescans

A scan doesn't always have to open every folder. Where a drive keeps a record of what changed (the change journal of an NTFS drive on Windows, the change history of an APFS or HFS+ drive on a Mac), a rescan reads only the files and folders that record names, and carries everything else over: an unchanged drive with millions of files is rescanned in seconds. Where a drive's NTFS file table can be read directly (on Windows as administrator; on Linux for drives mounted with ntfs-3g, once BackDown may read the drive), a scan reads that table from start to end instead of going folder by folder.

Whichever way a scan is made, it records exactly what opening every folder would, because proof needs every file. If the change record was reset, lost its oldest entries, belongs to another drive, or says anything that doesn't add up, the scan walks every folder instead, and it does so anyway every tenth scan or after 30 days, which catches changes made where nothing was recording them (the drive plugged into another computer). Activity shows how each drive's last scan was made and why; *Settings → Scanning* turns this off, or changes how often to walk anyway. Linux keeps no change record for drives, so rescans there walk (or read an NTFS drive's table). `backdown index --method walk` always walks.

## Hashes and proof

Every claim BackDown makes about copies rests on SHA-256: two files are the same when their SHA-256 hashes are the same. Hashing everything would mean reading every byte on every drive, so hashing goes in three tiers:

1. **Size.** A file whose size no other file shares can't have a duplicate, and is never read.
2. **Sample.** For the rest, a quick fingerprint: the size, plus 64 KiB from the start, the middle and the end of the file. Files up to 192 KiB are read whole here, which gives their full hash straight away.
3. **Full SHA-256.** Only files whose sizes and samples still match another file's are read completely.

Hardlinks are one file: they're read once, and never count as each other's copy. Hashing runs one reader per file system (`--jobs 1`, right for spinning disks; raise it for SSDs). Files on drives that aren't connected wait for them. A file that can't be read is marked unreadable and skipped until `backdown hash --retry-errors`; one that changed since it was indexed asks you to index again.

`dupes`, `check` and the daemon run `hash` for you. The hash is SHA-256 throughout.

## What "safe to wipe" means {#what-safe-to-wipe-means}

`backdown check PATH` says a drive (or a folder) is **safe to wipe** when every file in it has an identical copy outside it:

- another file with the same full SHA-256,
- outside PATH (a copy inside the drive you want to wipe doesn't count),
- and not just a hardlink to the same file.

Files that couldn't be read block the verdict, because nobody knows what's in them. Empty files don't count against a drive. A file without a full hash counts as unique. `check` hashes everything that could have a copy first, so that only happens when no other file could be identical, or when its look-alikes are on drives that have never been connected while hashing.

The verdict is about the index. A copy on a drive that isn't plugged in still counts, and a copy that went missing since its drive was last scanned would still count too. That's why [freeing up a drive](/docs/guides/free-up-a-drive/) doesn't trust the verdict alone: before deleting a file, it reads the file and its copy again and compares them.

`check` exits with 0 when the path is safe to wipe and 2 when files must be copied first, and lists the folders holding what exists only there (`--list` names the largest 20,000 files).

## Recognised things and provenance {preview} {#recognised-things-and-provenance}

BackDown recognises what things are, from the names and layout of files and folders: mail stores and Maildir mailboxes, Thunderbird profiles, Nextcloud data, Time Machine backups, iPhone and Android backups, operating systems, Mac, Linux and Windows home folders, websites, git repositories, database dumps, disk images, photo, music and video libraries, data exports (Google Takeout, Facebook, Instagram, Twitter, LinkedIn), downloads piles and recovered files. Each comes with its size, the years its files span and, where it's less than sure, how sure it is.

Folders that programs can make again are flagged as *regenerable*: caches, thumbnails, `node_modules`, build output, virtual environments, trash and the like. They still count: a drive is only safe to wipe when they have copies too. A filing plan can leave them out, and then they stay on the old drive when you free it up.

It also works out where things came from, and when, by reading small files on connected drives:

| Source | What it says |
|---|---|
| Backup scripts (`rsync`, `scp`, `rclone`, `cp -a`, `robocopy`, `tar`) | Copied from which machine, with which tool, from which script |
| iPhone backups' `Info.plist` | Backed up from which device, which model, on which day |
| Time Machine folder names | Backed up from which computer, last on which day |
| `/etc/hostname`, `/etc/os-release` | Which machine a system disk was, which system |
| git configuration | A clone of which remote, on which branch |
| GNU ddrescue map files | Rescued from which device |
| Database dump headers | Dumped from which host, on which day |
| Sync markers | Kept in sync by Syncthing, Resilio Sync, Dropbox, Nextcloud or ownCloud |

Only small files are read (scripts up to 256 KiB, plists up to 8 MiB), and only on drives that are connected. `backdown entities` lists what's recognised; the daemon recognises each drive again after it's been scanned and hashed.

## Overlaps {preview}

`backdown overlaps` shows how much of each drive, top-level folder and recognised thing already exists elsewhere, and where most of it is: "89% elsewhere: 89% on archive". It's the same SHA-256 test as `check`, summed up per place. The report and the [filing plan](/docs/guides/file-into-an-archive/) use it to find what's already archived.

## The daemon

`backdown daemon` is BackDown running in the background. It:

- rescans each drive when it's due (every 24 hours by default; `--rescan-every 12h`, or `never`), and as soon as a drive that was away comes back;
- hashes after every scan, then recognises what changed;
- serves the browser on 127.0.0.1 (port 8787), with a private link that `backdown open` opens;
- runs one job at a time, so the index has one writer and each drive one reader; rescues run beside the other jobs;
- with the next preview, reads every drive's [health](#drive-health) every six hours and tells you when one gets worse.

It's killed safely at any point: jobs commit as they go and carry on where they stopped.

`backdown service install` runs it for you, as you, with no extra rights: a systemd user service on Linux, and with the next preview a LaunchAgent on macOS and a task that starts when you log on on Windows. Until then, start `backdown daemon` yourself on macOS and Windows. On Linux, `sudo backdown service install --system` runs it for the whole machine instead (a NAS or a server): as a `backdown` user that can read every file and write none, sandboxed by systemd, with no network beyond this computer. Members of the `backdown` group can use it. The machine-wide service only reads: it never wipes and never rescues.

While a daemon is running, it owns the index. `index`, `hash`, `dupes`, `check`, `scans`, `status` and `apply --go` are handed to it, and your terminal follows the progress. `--local` makes a command use the index directly. On Windows, terminal commands always use the index directly.

## Drive health {preview} {#drive-health}

Drives say how they're doing, through SMART: sectors they've had to move or can't read, cable errors, wear on SSDs, temperature, and their own verdict. BackDown reads that for every physical drive without administrator rights, and without waking a drive that has spun down: through udisks2 on Linux (and `smartctl` where it's installed and allowed), `diskutil` on macOS, and the storage cmdlets on Windows. What it can't read, it says, with the command that would.

Each reading is kept in the index under the drive's serial number, so a drive's history follows it from port to port and machine to machine, and what changed since the first reading shows as a trend: new bad sectors, rising cable errors, wear crossing 50, 80, 90 or 100%. A drive's verdict is **ok**, **warning**, **failing** or **unknown**.

`backdown health` shows every drive, and exits with 2 when a connected drive is failing. The daemon reads them every six hours, when a drive appears, and when you ask ("Check now" in the browser). When a drive gets worse, a mirror stops matching its original, or a scheduled job fails, it's listed under Notifications in the browser's Activity view and, unless you untick "Show on the desktop", shown as a desktop notification: a few at most in any ten minutes and a dozen a day, except for a failing drive, which always gets through. A failing drive also gets a banner with **Copy it now**, which opens [Rescue](/docs/guides/rescue/) on that drive.
