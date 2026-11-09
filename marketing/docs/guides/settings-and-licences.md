---
title: Settings, updates and licences
nav: Settings and licences
description: The Settings view, how often drives are rescanned, updates, notifications, running BackDown for the whole machine, and what's free and what's Pro.
order: 10
---

Most of what you can change is in the **Settings** view, at the bottom of the sidebar. The background service reads it as it goes: a change applies from the next scan, hash, plan or check, without a restart. Some of it can also be given as options on the command line, for one run.

## The Settings view

| Section | What's there |
|---|---|
| Licence | Your edition, a licence key, and which features are free and which are Pro ([below](#licences)) |
| Updates | Whether and how BackDown looks for a newer version ([below](#updates)) |
| Scanning | How often each drive is rescanned, and [fast rescans](/docs/concepts/#fast-rescans) |
| Hashing | Readers per drive, and whether to retry files that couldn't be read |
| Attestations | Whether the service signs a manifest of each drive after each scan, daily or weekly, and the key it signs with ([manifests and attestations](/docs/guides/manifests-and-attestations/#have-the-service-sign-them)) |
| Junk rules | The folder names counted as caches and the like, and your own |
| Filing schema | Where a [filing plan](/docs/guides/file-into-an-archive/#your-own-schema) puts each kind of thing, checked as you type |
| Notifications | What pops up on your desktop ([below](#notifications)) |
| Other machines | How long a copy on [another machine](/docs/guides/several-machines/) counts as proof, and how often machines are synced |
| Index and data | Where the index, the data folder, the settings, the licence and the service's log are, with Copy buttons |
| About | The version, the edition, the system, and whether the background service is running |

The settings are saved in `settings.json` in your data folder, and your own filing schema in `schema.toml` beside it. Fast-rescan and machine settings are kept in the index itself; notification settings in `notifications.json`. The browser remembers your view, sort order, details pane and hidden-files choice in your web browser.

<figure class="demo" data-view="settings" data-caption="Settings: rescans, hashing and junk rules, attestations, notifications, updates, the licence, and where the index and data live."></figure>

## The index

The index is one SQLite database, `index.db`, in your BackDown data folder:

| System | Data folder |
|---|---|
| Linux | `~/.local/share/backdown` (or `$XDG_DATA_HOME/backdown`) |
| macOS | `~/Library/Application Support/BackDown` |
| Windows | `%LOCALAPPDATA%\BackDown` |
| Linux, machine-wide service | `/var/lib/backdown` |

The folder is readable only by you: the index lists every file path on your drives. `backdown status` and Settings > Index and data show which index is in use. There's no setting that moves it; to use another one, pass `--db` to any command, or set `BACKDOWN_DB`:

```sh
backdown --db /media/archive/backdown/index.db status
export BACKDOWN_DB=/media/archive/backdown/index.db
```

To move it for good: stop the background service, move `index.db` (with `index.db-wal` and `index.db-shm` if they're there), then set `BACKDOWN_DB` where the service starts, or pass `--db`.

Previews before 0.2.0-alpha.3 kept `backdown.db` in the current folder. BackDown mentions it if it finds one; keep using it with `--db backdown.db`, or index again.

## How often drives are rescanned

The service rescans each drive every 24 hours, and as soon as a drive that was away comes back, whatever the setting says. In Settings > Scanning, "Rescan each drive" can be every 6 or 12 hours, every day, every 3 days, every week, or "Only when I ask, or a drive comes back". It applies straight away.

`backdown daemon --rescan-every 12h` (or `30m`, `7d`, `never`) sets it when the service starts; a change in Settings then applies until the service starts again with that option. `--port` moves the browser from port 8787.

Under "Fast rescans" in the same section:

- **Use the drive's change log and file table when possible** (on): off, every scan walks every folder.
- **Walk every folder anyway, every 10 scans**, **or when the last walk is 30 days old**: 0 turns either reason off.

[Fast rescans](/docs/concepts/#fast-rescans) explains what these do, and `backdown index --method walk` walks whatever they say.

## Hashing and junk rules

"Readers per drive" (1 to 16) is how many files are read at once from each drive for the hashing after each scan: 1 suits spinning disks, 2 to 4 suit SSDs. It's `--jobs` for `backdown hash`. "After each scan, try again files that couldn't be read before" is `backdown hash --retry-errors`, every time.

Junk rules decide which folders count as [regenerable](/docs/concepts/#recognised-things-and-provenance): caches, thumbnails, `node_modules`, trash and the like. Settings lists the built-in ones; under "Also count these folder names" you can add your own, one name per line (a name, not a path; case doesn't matter). They count from the next time drives are recognised: after a scan, or What's here > Recognise again. They apply to the command line too.

## Notifications {#notifications}

The service keeps a list of what's worth knowing: a drive whose [health](/docs/guides/drive-health/) got worse, a mirror that no longer matches what it copies, a job it scheduled itself that failed (a scan, hashing, a sync). It's under Notifications in the Activity view, with read and unread, and a count on the Activity button.

In Settings > Notifications:

- **Show notifications on the desktop** turns the pop-ups on or off; everything is listed in Activity either way. The same switch is "Show on the desktop" in Activity.
- **When a drive's health gets worse**, **When a mirror no longer matches what it copies**, **When a scheduled job fails** choose which kinds pop up. Every kind is listed in Activity either way, so a failing drive is never missed.
- **Send a test notification** checks that pop-ups work.

At most 3 pop-ups show in any 10 minutes and 12 a day; the rest are listed only ("listed only" beside them). A drive that's failing always gets through, if pop-ups are on. The same notice isn't repeated for a while: 7 days for a drive's verdict, 20 hours for the rest.

Pop-ups use `notify-send` on Linux (or the desktop's notification service), Notification Center on macOS, and a PowerShell notification on Windows, where Windows may name PowerShell as the sender. The machine-wide service doesn't pop anything up: it isn't on anyone's desktop.

## Updates {#updates}

BackDown makes no network requests unless you ask, so it doesn't look for updates until you tell it to:

- **Check now** in Settings > Updates, or `backdown update --check`, asks once. `--check` exits with 10 when there's a newer version.
- **Look for updates once a day** makes the background service ask once a day.
- **Updates to offer**: "Automatic" offers previews while you're running a preview (like this beta) and releases otherwise; "Releases only" or "Previews too" fix it (`--channel stable|preview`).

What it asks is GitHub's public list of BackDown releases; nothing about you, your drives or your files is sent. The answer is kept for a day (`update.json` in your data folder); `--refresh` asks again. When there's a newer version, the browser shows a banner with the release notes and **Update now**.

```sh
backdown update --check
backdown update
backdown update --rollback
```

Installing downloads this computer's archive, the release's `SHA256SUMS.txt` and its signature, `SHA256SUMS.txt.sig`, over HTTPS. The signature is made with BackDown's release key, whose public half is built into BackDown, so it shows the sums are the ones BackDown published and weren't changed since; the sums then show the archive is the one they list. A signature that doesn't match stops the update, and so does an archive whose SHA-256 doesn't match. Releases before v1.0.0-beta.4 weren't signed: those are installed on their checksums alone, and BackDown says "this release isn't signed (releases before v1.0.0-beta.4 weren't)". A later release without its signature isn't installed.

Then it unpacks the new program beside the one you have, checks that it runs and says the expected version, keeps a copy of the old one in your data folder (`previous/`), swaps them, and restarts the background service. If the new one doesn't start, the old one is put back. `--rollback` puts back the version the last update replaced.

On a computer without a network, `backdown update --from FOLDER` does the same from files you carry over: the archive, `SHA256SUMS.txt` and `SHA256SUMS.txt.sig`. `--from FOLDER --check` only checks them. [Air-gapped machines](/docs/guides/air-gapped/) has the details.

Some copies of BackDown are updated another way, and `backdown update` (or **How to update** in the browser) says so instead of installing:

- installed from the `.deb` (it lives in `/usr/bin`) or another package: install the new package;
- an AppImage: download the new AppImage;
- BackDown.app on a Mac (from the `.dmg`): download the new `.dmg`, since changing the program inside the app would break its signature;
- installed from the `.msi` on Windows: install the new `.msi`, which upgrades the one you have (the setup `.zip` install updates itself);
- Homebrew: `brew upgrade backdown`;
- in a folder you can't write to: `sudo backdown update`, or move it somewhere you own;
- the machine-wide service: `sudo backdown update`, then `sudo systemctl restart backdown`.

## A machine-wide service

On a NAS or a Linux server, one daemon can index the whole machine for everyone:

```sh
sudo mv backdown /usr/local/bin/
sudo backdown service install --system
sudo usermod -aG backdown alice
```

It runs as a `backdown` user that can read every file and write none, with the whole file system read-only to it except its own folder, `/var/lib/backdown`, and no network beyond this computer, all enforced by systemd. Members of the `backdown` group can use it with the usual commands and `backdown open` (after logging in again). It only reads: it doesn't wipe, rescue, tidy, mount, reach other machines or check for updates. Do those from your own account with `--local`. It's Linux-only; on macOS and Windows the service runs while you're logged in.

`backdown service status` (with `--system` for this one) says whether it's installed and running and what it's doing; `backdown service uninstall` removes it and keeps the index.

## The service on each system

| System | What `backdown service install` sets up | Its messages |
|---|---|---|
| Linux | a systemd user service, `~/.config/systemd/user/backdown.service` | `journalctl --user -u backdown` |
| macOS | a LaunchAgent, `~/Library/LaunchAgents/io.backdown.daemon.plist`, started at login and restarted if it stops unexpectedly | `~/Library/Logs/BackDown/daemon.log` |
| Windows | a scheduled task "BackDown" that starts it, without a window, when you log on (not elevated), restarted if it fails | none kept yet |

On Linux, to start it with other options, override its command with a systemd drop-in:

```sh
systemctl --user edit backdown
```

```ini
[Service]
ExecStart=
ExecStart=/usr/local/bin/backdown daemon --rescan-every 12h
```

Then `systemctl --user restart backdown`. `backdown service install --print` shows the unit it installs. To keep the user service running while you're logged out, `loginctl enable-linger $USER`. Most options are in Settings anyway, which is simpler.

## Other options worth knowing

| Option | On | What it does |
|---|---|---|
| `--jobs N` | `hash`, `dupes`, `check` | Readers per drive: 1 suits spinning disks, raise it for SSDs |
| `--cross-filesystems` | `index` | Also index file systems mounted inside the path |
| `--method walk` | `index` | Walk every folder, whatever the drive's change log says |
| `--local` | any command | Use the index directly even while the daemon runs |
| `--schema FILE` | `plan` | Your own filing schema ([how](/docs/guides/file-into-an-archive/#your-own-schema)) |
| `--verify-every DAYS` | `mirror add` | How often the daemon re-reads a sample of a mirror |
| `--browser PATH` | `app` | Which Chromium-family browser shows BackDown's window (or `BACKDOWN_BROWSER`) |
| `--host ADDRESS` | `serve`, `daemon`, `service install` | Also listen on this address, to [look from your other devices](/docs/guides/browse-without-restoring/#look-from-your-other-devices) |

The [CLI reference](/docs/cli/) has every option.

## Licences {#licences}

A licence (Pro, Team or Forensic) is a key that starts with `BD1.`. It's checked on your computer, against a signature only backdown.io can make, so it needs no account and no network. Add it in Settings > Licence, or:

```sh
backdown license add BD1.eyJ2Ijox…
backdown license show
backdown license features
```

```text
BackDown Pro licence 7KQ2M-…
  for you@example.org · 1 machine · issued 2026-10-01
```

`add` checks the key and keeps it in your data folder (`license.txt`); `-` reads it from standard input. `show` exits with 1 when there's no licence. A key bought as a pre-order says so: Pro features switch on in the release that ships them, and your first year starts then.

### Free and Pro {#free-and-pro}

BackDown has one table of which features need which licence, and the command line, the browser and the background service all follow it. `backdown license features` and Settings > Licence show it. In a pre-release version, such as this beta, **everything is unlocked**: "Pro features are free during the preview". A key you add now carries over to the release.

The table marks these as **Pro**:

| Feature | What it covers |
|---|---|
| Full recognition and provenance | Where each thing came from: the host, device or person, from backup scripts, plists, git remotes and dump headers |
| Dedup across every source | `backdown overlaps`: how much of each drive, folder and thing exists elsewhere |
| Reclaim simulator | Freeing up drives: `backdown wipe` and the Free up view |
| Filing suggestions | The filing plan: `plan`, `plans`, `apply` and the Plan view, including merging mailboxes in a plan and the Tidy card there |
| Data viewers | Databases, spreadsheets, presentations, mailboxes and SQL dumps in the browser |
| Several machines | [Other machines](/docs/guides/several-machines/) over ssh: adding, syncing and browsing them (listing and forgetting them stay free) |

And as **Team** and **Forensic**:

| Feature | Edition | What it covers |
|---|---|---|
| Drive health alerts | Team | [Other machines' drives](/docs/guides/drive-health/), gathered with each sync: `backdown health --machines` and the browser's "Other machines' drives" |
| Scheduled attestations | Team | [Signed manifests made on a schedule](/docs/guides/manifests-and-attestations/) (Settings > Attestations), `manifest list` and the manifests in a drive's details |
| Signed, tamper-evident manifests | Forensic | [Signing a manifest](/docs/guides/manifests-and-attestations/) of what a drive held: `manifest create` and `manifest export` |
| Chain-of-custody export | Forensic | [Exporting, verifying and checking](/docs/guides/chain-of-custody/) the signed log of every copy, move, deletion, link and merge: `backdown custody` and the browser's Chain of custody download. The log itself is kept in every edition |
| Air-gapped deployment | Forensic | [Installing updates from a carried folder](/docs/guides/air-gapped/): `backdown update --from` |

Everything else is free: indexing, hashing, duplicates, `check`, browsing with the time slider and the other viewers, recognising what things are, the report, mirrors, Rescue, drive health and notifications, mounting drives read-only, and the `dedup` and `mail` commands. Updating over the network, with its signature checked, is free too. So is checking: a manifest (`backdown manifest verify`, and showing your key with `backdown manifest key`) and a carried update folder (`backdown update --from … --check`) can be checked by anyone. The table also lists what's planned for Pro, Team and Forensic and isn't built yet, marked "coming".

In a release without a licence, a Pro command says what it needs (`backdown wipe` … "is part of BackDown Pro"), and the browser marks Pro views with a badge and offers **Add a licence key** and **See pricing**. [Pricing](/pricing/) has the planned prices, which may change before release.
