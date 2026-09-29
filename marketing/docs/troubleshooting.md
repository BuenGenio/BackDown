---
title: Troubleshooting and FAQ
description: Files BackDown can't read, drives that don't show up, rescans that walk, the app and the background service, updates, where the index lives, uninstalling, and common questions.
order: 1
group: help
---

> [!NOTE]
> The macOS and Windows builds of this beta haven't been run on a real Mac or Windows PC yet. If something there doesn't work as these pages say, please [open an issue](https://github.com/BuenGenio/backdown-releases/issues) with what you did and what BackDown said.

## Files that can't be read

`backdown index` lists how many entries it couldn't read, with a few examples:

```text
/media/old-drive: 212.4 K files, 931.5 GB (new 212406, changed 0, gone 0) · scan 1
  full walk in 3 min 4 s: Linux keeps no change log for drives; not an NTFS drive
  18 entries couldn't be read, e.g.:
    IO error for operation on /media/old-drive/home/alice/.ssh: Permission denied (os error 13)
```

BackDown reads as you. Folders that belonged to another user on an old computer's disk are often closed to your account. Either:

- on Linux, run the [machine-wide service](/docs/guides/settings-and-licences/#a-machine-wide-service), which can read every file (and write none);
- on Windows, run BackDown as administrator: it then reads (never writes) files your account can't, such as other users' folders;
- or make the drive readable to you (on an old disk you own, `sudo chown -R` on the folders is common), then index again.

Running `sudo backdown index` isn't a good fix on Linux or macOS: it would build a separate index in root's own data folder.

Files that fail while hashing are marked unreadable and skipped from then on. `backdown hash --retry-errors` tries them again, for example after reconnecting a flaky drive (Settings > Hashing can do that after every scan). A file that changed since the last scan asks you to index again.

On Windows, online-only files from OneDrive and similar services are never read, since reading one would download it onto the drive; they show as unreadable ("an online-only file (a cloud placeholder)"). Make them available offline first if they should count.

On macOS, some folders (Mail, Photos, Messages and others in your Library) are protected by the system. Give Full Disk Access (System Settings > Privacy & Security) to your terminal for commands you run there, and to BackDown for the app and its background service, or they show up as unreadable.

## Reading whole drives, for Rescue

Rescue reads the drive itself (`/dev/sdc`), not its files, which needs more than an ordinary account has. BackDown says so and gives the command that lets your account read just that drive until it's unplugged: `sudo setfacl -m u:$USER:r /dev/sdc` on Linux, `sudo chmod o+r /dev/disk4` on macOS. On Linux, the Rescue view also has **Allow BackDown to read this drive**, which asks the system (udisks) with its own password dialog. `sudo backdown rescue …` works too. See [Rescue](/docs/guides/rescue/#when-backdown-cant-read-the-drive).

## A drive doesn't show up

The sidebar lists drives you've indexed; Activity's "Connected drives" lists every disk and partition on the computer. Some things to check:

- **It isn't mounted.** Mount it: **Mount read-only** in "Connected drives" (Linux and macOS), `backdown mount /dev/sdc1`, or your file manager. Then **Index this**, or index the folder it's mounted on. An encrypted volume has to be unlocked first, in your file manager. `backdown volumes` lists every disk and partition, whether it's mounted, and where. On Windows, drives have drive letters and are read where they are.
- **"Your system asks for an administrator's password to mount this drive."** Some systems only let an administrator mount internal drives. Run the command BackDown shows in a terminal: it asks for the password itself.
- **"Linux reads APFS only with apfs-fuse"**, or no driver for a file system: this computer can't read it at all. Plug the drive into a computer that can, or install the driver.
- **It's mounted somewhere else.** BackDown knows a drive by its path. Mounted at a different path, it's a new drive to BackDown; mount it where it was, or index the new path.
- **It's empty now.** If a drive that held files looks empty, BackDown won't record it: "… is empty, but held 212406 files at its last scan. Is the drive unplugged?" Usually the drive isn't plugged in and you're looking at the empty folder it's normally mounted on. If it really was emptied, `backdown index <path> --allow-empty`.
- **It's inside another indexed drive.** Roots can't be nested: "… overlaps the already indexed …". Index each drive or top-level folder once.
- **It's shown as not connected.** Its files wait for it: hashing skips them and the background service rescans it as soon as it's back.

## When a rescan walks {#when-a-rescan-walks}

`backdown index` and Activity say how each drive's last scan was made: "rescanned from the change log in 2 s: 14 changes", "read the file table: 412.8 K files in 41 s", or "full walk in 3 min 4 s", with the reason for a walk. A walk is never wrong, only slower: BackDown walks whenever a quicker way can't vouch for every file ([fast rescans](/docs/concepts/#fast-rescans)). The reasons you may see:

| Reason | What it means |
|---|---|
| the drive's first scan | There's nothing to compare with yet; the next rescan can use the change log |
| a full walk every 10 scans, to be sure (or every 30 days) | The regular walk that catches changes made where nothing was recording them. Settings > Scanning changes how often |
| Linux keeps no change log for drives | On Linux, rescans walk, or read an NTFS drive's file table |
| this drive keeps no change log BackDown can read | FAT and exFAT drives, and others without one (on Windows only NTFS, on macOS only APFS and HFS+) |
| the change log was reset, or trimmed, since the previous scan | Its record doesn't reach back to the last scan (the drive was used a lot, or the log was recreated) |
| a different volume than at the previous scan; the change history was reset (or it's another drive) | The drive at that path isn't the one scanned before |
| the change log left out a name; the change log didn't say that … changed | The log and the drive disagree, so neither is trusted |
| scans that go into other filesystems are always walked | You passed `--cross-filesystems` |
| the change log and file table are turned off in settings | Settings > Scanning |
| not an NTFS drive | The file table is NTFS's |
| reading the file table needs administrator rights | Windows: run BackDown as administrator to read it. The background service runs with your normal rights, so it uses the change log instead |
| the volume is marked as needing a check (chkdsk) | Windows won't have its table trusted until the drive is checked |
| the file table is read for ntfs-3g mounts; this one is the kernel's | Linux's own NTFS driver (`ntfs3`) is fast to walk anyway |
| the file table needs read access to /dev/sdc | Linux, ntfs-3g: let your account read the device (`sudo setfacl -m u:$USER:r /dev/sdc`, until it's unplugged) |

`backdown index --method walk` always walks. `--method journal` and `--method table` use that way or stop with the reason (`can't rescan … from its change log: …`), and skip the regular walk; they're for trying it out.

## The app and the background service

`backdown app` (or BackDown in your applications) starts the background service if it isn't running and opens its window.

- **No window of its own, just a browser tab.** The window needs Chrome, Chromium, Edge or Brave. Without one, BackDown opens in your usual browser; `backdown app --browser /path/to/browser` picks another Chromium-family one.
- **"the BackDown daemon didn't answer within 30 s"** or **"the BackDown daemon stopped"**: the message names its log. If BackDown started the service itself, the log is `daemon.log` in your data folder.
- **"This BackDown window has expired"**: open BackDown again from your applications, or `backdown open`.
- **Something else uses port 8787** ("can't listen on 127.0.0.1:8787"): `backdown serve --port 8788`, or start the service with `--port`.

`backdown open` says "no BackDown daemon is running" when there's none: `backdown app` or `backdown service install` starts one. To see what the service is doing:

| System | Status | Its messages |
|---|---|---|
| Linux | `backdown service status`, `systemctl --user status backdown` | `journalctl --user -u backdown` |
| macOS | `backdown service status` | `~/Library/Logs/BackDown/daemon.log` |
| Windows | `backdown service status`, or Task Scheduler, task "BackDown" | not kept yet: Activity in the app shows what it's doing |

On Linux, after a reboot a user service starts when you log in; `loginctl enable-linger $USER` keeps it running without a login. On Windows, commands in a terminal work on the index directly rather than through the service, so use the app to follow what the service is doing.

The service does one job at a time, so a command may wait ("waiting for the daemon's current job"). Queued jobs can be taken off the list in Activity. A running scan, hash, copy or wipe can't be stopped from there, but rescues, tidying, mail merges and syncs can; stopping the service (on Linux, `systemctl --user stop backdown`; on Windows, End the task in Task Scheduler) is safe too, and it carries on where it stopped when it starts again.

`backdown import-legacy` refuses to write while a daemon is running: stop it for the import, then start it again.

**The browser says "Open BackDown with `backdown open`".** The link carries a private token. Open it with `backdown open` or `backdown app` (or the link `backdown serve` printed) rather than a bookmark without the token.

## Notifications

Nothing pops up? Check Settings > Notifications ("Show notifications on the desktop", and the kind), and press **Send a test notification**. On Linux, pop-ups need `notify-send` (or a desktop notification service). At most 3 pop-ups show in 10 minutes and 12 a day; the rest are listed in Activity, marked "listed only". The machine-wide service only lists them.

## Updates

`backdown update` and **Update now** install only a copy of BackDown that can replace itself. Otherwise they say how to update: a `.deb` through the package manager (install the new package), an AppImage by downloading the new one, Homebrew with `brew upgrade backdown`, a program in a folder you can't write to with `sudo backdown update`. "GitHub's limit for update checks from this network is reached" means it asks again after the time it names. An update that doesn't start is rolled back by itself; `backdown update --rollback` puts back the previous version later. [Updates](/docs/guides/settings-and-licences/#updates) has the rest.

## Other machines

[Several machines](/docs/guides/several-machines/#when-it-cant-connect) lists what BackDown says when it can't reach one, and what to do. Most often: log in with `ssh` once in a terminal, so that your key works without a password and the host key is accepted.

## Where the index lives

| System | Data folder |
|---|---|
| Linux | `~/.local/share/backdown` (or `$XDG_DATA_HOME/backdown`) |
| macOS | `~/Library/Application Support/BackDown` |
| Windows | `%LOCALAPPDATA%\BackDown` |
| Linux, machine-wide service | `/var/lib/backdown` |

It holds:

- `index.db`, with `index.db-wal` and `index.db-shm` beside it while it's in use: the index;
- `settings.json` and `schema.toml`: Settings, and your filing schema if you changed it;
- `notifications.json`: which notifications show;
- `license.txt`: your licence key, if you added one;
- `ui.json`: the service's browser link and token;
- `mounts.json`: what BackDown has mounted, so it only unmounts that;
- `update.json`, and `previous/` with `previous.json` after an update: the last update check, and the version an update replaced;
- `app-window/` and `app.json`: the app window's browser profile, and which window is open;
- `daemon.log`, if the app started the service itself;
- `daemon.sock` on macOS: how commands talk to the service (on Linux it's in `$XDG_RUNTIME_DIR/backdown`, or `/run/backdown` for the machine-wide service).

`backdown status` and Settings > Index and data show the index in use. Its size grows with the number of files and scans kept. To start over, stop the service and delete `index.db` with its `-wal` and `-shm` files.

## Uninstalling

1. Stop and remove the background service, if you installed one: `backdown service uninstall` (or, on Linux, `sudo backdown service uninstall --system`). Removing the app doesn't do this for you. The index is kept.
2. Remove BackDown from your applications, if `backdown desktop install` added it: `backdown desktop uninstall`.
3. Remove the program:
   - `.deb`: `sudo apt remove backdown`;
   - AppImage, or a plain archive: delete the file (`backdown` from wherever you put it, such as `/usr/local/bin`);
   - macOS: move BackDown from Applications to the Trash;
   - Windows (`.msi` or setup): Settings > Apps > BackDown > Uninstall.
4. Delete the data folder above, to remove the index, settings and your licence; on macOS also `~/Library/Logs/BackDown`.
5. For the machine-wide service, also delete `/var/lib/backdown` and the `backdown` user and group (`sudo userdel backdown`).

BackDown leaves nothing else behind, except what you asked it to make: mirrors keep changed files' old copies in `.backdown-versions/`, and a copy interrupted mid-file leaves a `.<name>.backdown-partial` file, which the next run replaces.

## Questions

**Does BackDown send anything anywhere?** No. It needs no account and sends no telemetry. It makes a network request only when you ask: checking for updates (off until you turn it on, or run `backdown update`), and reaching your own machines over ssh. The browser listens on 127.0.0.1 only, unless you add an address with `--host`, and from there it only shows what's there. [Principles](/docs/principles/) has the rest.

**Does it change my drives?** Indexing, hashing, checking, browsing, recognising and reading drive health only read, and drives it mounts are mounted read-only. BackDown writes when you ask it to: copies into your archive (`apply`), to a mirror (`mirror sync`), merged mailboxes, what Rescue saves, always to the destination you chose, and links between identical files in a folder you tidy. It deletes only when you free up a drive, and only files with a verified copy.

**Can it index a NAS or a network share?** Yes, anything mounted as a folder. Indexing reads names and sizes over the network, and hashing reads the contents of files that could be duplicates, so it's slower than a local disk. Index the mount point itself. If the NAS can run BackDown itself, [several machines](/docs/guides/several-machines/) is faster: it indexes and hashes there, and this computer reads its index over ssh.

**Does it make backups?** It copies and verifies: into one archive, and to mirrors. Scheduled, encrypted, versioned backups to other disks or the cloud are [planned](/features/), not built yet.

**Does it work on Windows and macOS?** Yes: the app, the background service and every command, with a few differences. On Windows, BackDown mounts disk images but not partitions, terminal commands don't go through the service, tidying an archive isn't available yet, and hardlinks count as separate files. This beta's macOS and Windows builds haven't been tried on real machines yet, so please report what doesn't work.

**Is it free?** The beta is, with everything unlocked. The core is planned to stay free, with paid Pro, Team and Forensic tiers: see [Free and Pro](/docs/guides/settings-and-licences/#free-and-pro) and [pricing](/pricing/).
