---
title: Troubleshooting and FAQ
description: Files BackDown can't read, drives that don't show up, the daemon, where the index lives, uninstalling, and common questions.
order: 1
group: help
---

## Files that can't be read

`backdown index` lists how many entries it couldn't read, with a few examples:

```text
/media/old-drive: 212.4 K files, 931.5 GB (new 212406, changed 0, gone 0) · scan 1
  18 entries couldn't be read, e.g.:
    IO error for operation on /media/old-drive/home/alice/.ssh: Permission denied (os error 13)
```

BackDown reads as you. Folders that belonged to another user on an old computer's disk are often closed to your account. Either:

- run the [machine-wide service](/docs/guides/settings-and-licences/#a-machine-wide-service) on Linux, which can read every file (and write none); or
- make the drive readable to you (on an old disk you own, `sudo chown -R` on the folders is common), then index again.

Running `sudo backdown index` isn't a good fix: it would build a separate index in root's own data folder.

Files that fail while hashing are marked unreadable and skipped from then on. `backdown hash --retry-errors` tries them again, for example after reconnecting a flaky drive. A file that changed since the last scan asks you to index again.

On macOS, some folders (Mail, Photos, Messages and others in your Library) are protected by the system. Give your terminal Full Disk Access in System Settings > Privacy & Security, or they show up as unreadable.

## Reading whole drives, for Rescue

Rescue reads the drive itself (`/dev/sdc`), not its files, which needs more than an ordinary account has. BackDown says so and gives the command that lets your account read just that drive until it's unplugged: `sudo setfacl -m u:$USER:r /dev/sdc` on Linux, `sudo chmod o+r /dev/disk4` on macOS. `sudo backdown rescue …` works too. See [Rescue](/docs/guides/rescue/#when-backdown-cant-read-the-drive).

## A drive doesn't show up

The browser's sidebar lists drives you've indexed. Some things to check:

- **It isn't mounted.** BackDown doesn't mount drives: mount it (your file manager does, or `udisksctl mount -b /dev/sdc1`), then index the folder it's mounted on. An encrypted volume has to be unlocked first. `backdown volumes` {preview} lists every disk and partition, whether it's mounted, and where.
- **It's mounted somewhere else.** BackDown knows a drive by its path. Mounted at a different path, it's a new drive to BackDown; mount it where it was, or index the new path.
- **It's empty now.** If a drive that held files looks empty, BackDown won't record it: "… is empty, but held 212406 files at its last scan. Is the drive unplugged?" Usually the drive isn't plugged in and you're looking at the empty folder it's normally mounted on. If it really was emptied, `backdown index <path> --allow-empty`.
- **It's inside another indexed drive.** Roots can't be nested: "… overlaps the already indexed …". Index each drive or top-level folder once.
- **It's shown as not connected.** Its files wait for it: hashing skips them and the daemon rescans it as soon as it's back.

## The daemon

`backdown open` says "no BackDown daemon is running" when there's none. On Linux:

```sh
backdown service status
systemctl --user status backdown
journalctl --user -u backdown
```

The last one shows the daemon's messages. After a reboot, a user service starts when you log in; `loginctl enable-linger $USER` keeps it running without a login. With the next preview, `backdown service status` works on macOS and Windows too; on macOS the daemon's messages are in `~/Library/Logs/BackDown/daemon.log`, and on Windows the service is the task "BackDown" in Task Scheduler.

The daemon does one job at a time, so a command may wait ("waiting for the daemon's current job"). Queued jobs can be taken off the list in the browser's Activity view. A running scan, hash, copy or wipe can't be stopped from there; stopping the daemon (`systemctl --user stop backdown`) is safe, and it carries on where it stopped when it starts again.

`backdown import-legacy` refuses to write while a daemon is running: stop it for the import, then start it again.

**The browser says "Open BackDown with `backdown open`".** The link carries a private token. Open it with `backdown open` (or the link `backdown serve` printed) rather than a bookmark without the token.

**"can't listen on 127.0.0.1:8787".** Something else, often the daemon, uses that port. `backdown serve --port 8788`.

## Where the index lives

| System | Data folder |
|---|---|
| Linux | `~/.local/share/backdown` (or `$XDG_DATA_HOME/backdown`) |
| macOS | `~/Library/Application Support/BackDown` |
| Windows | `%LOCALAPPDATA%\BackDown` |
| Linux, machine-wide service | `/var/lib/backdown` |

It holds:

- `index.db`, with `index.db-wal` and `index.db-shm` beside it while it's in use: the index;
- `ui.json`: the daemon's browser link and token;
- `license.txt`: your licence key, if you added one;
- `notifications.json`: whether notifications show on the desktop;
- `daemon.sock` on macOS: how commands talk to the daemon (on Linux it's in `$XDG_RUNTIME_DIR/backdown`, or `/run/backdown` for the machine-wide service).

`backdown status` shows the index in use. Its size grows with the number of files and scans kept. To start over, stop the daemon and delete `index.db` with its `-wal` and `-shm` files.

## Uninstalling

1. Stop and remove the service, if you installed one: `backdown service uninstall` (or, on Linux, `sudo backdown service uninstall --system`). The index is kept.
2. Delete the program: `backdown` from wherever you put it, such as `/usr/local/bin`.
3. Delete the data folder above, to remove the index and your licence.
4. For the machine-wide service, also delete `/var/lib/backdown` and the `backdown` user and group (`sudo userdel backdown`).

BackDown leaves nothing else behind, except what you asked it to make: mirrors keep changed files' old copies in `.backdown-versions/`, and a copy interrupted mid-file leaves a `.<name>.backdown-partial` file, which the next run replaces.

## Questions

**Does BackDown send anything anywhere?** No. It needs no account, sends no telemetry, and makes no network requests. The browser listens on 127.0.0.1 only. [Principles](/docs/principles/) has the rest.

**Does it change my drives?** Indexing, hashing, checking, browsing and recognising only read. BackDown writes when you ask it to: copies into your archive (`apply`), to a mirror (`mirror sync`), and what Rescue saves, always to the destination you chose. It deletes only when you free up a drive, and only files with a verified copy.

**Can it index a NAS or a network share?** Yes, anything mounted as a folder. Indexing reads names and sizes over the network, and hashing reads the contents of files that could be duplicates, so it's slower than a local disk. Index the mount point itself.

**Does it make backups?** It copies and verifies: into one archive, and to mirrors. Scheduled, encrypted, versioned backups to other disks or the cloud are [planned](/features/), not built yet.

**Does it work on Windows and macOS?** The command-line preview does: indexing, hashing, duplicates, `check`, the browser and the daemon. In today's download `backdown service` is Linux-only; the next preview adds macOS and Windows. On Windows, hardlinks count as separate files.

**Is it free?** The preview is. The core is planned to stay free, with paid Pro, Team and Forensic tiers: see [pricing](/pricing/).
