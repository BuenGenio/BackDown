---
title: Settings and licences
description: Where BackDown keeps its index, how often the daemon rescans, notifications, running it for the whole machine, and adding a Pro licence key.
order: 6
---

BackDown has no settings file: the few things you can change are options on the commands, and the daemon's are set where it's started. There's no settings screen in the browser either; it remembers your view, sort order, details pane and hidden-files choice in your web browser.

## The index

The index is one SQLite database, `index.db`, in your BackDown data folder:

| System | Data folder |
|---|---|
| Linux | `~/.local/share/backdown` (or `$XDG_DATA_HOME/backdown`) |
| macOS | `~/Library/Application Support/BackDown` |
| Windows | `%LOCALAPPDATA%\BackDown` |
| Linux, machine-wide service | `/var/lib/backdown` |

The folder is readable only by you: the index lists every file path on your drives. `backdown status` shows which index is in use. To use another one, pass `--db` to any command, or set `BACKDOWN_DB`:

```sh
backdown --db /media/archive/backdown/index.db status
export BACKDOWN_DB=/media/archive/backdown/index.db
```

Previews before 0.2.0-alpha.3 kept `backdown.db` in the current folder. BackDown mentions it if it finds one; keep using it with `--db backdown.db`, or index again.

## How often the daemon rescans

The daemon rescans each drive every 24 hours, and as soon as a drive that was away comes back. `--rescan-every` changes the interval (`30m`, `12h`, `7d`), and `never` rescans only when a drive comes back or you ask. `--port` moves the browser from port 8787.

The service installed by `backdown service install` starts `backdown daemon` without options. On Linux, to change them, override its command with a systemd drop-in:

```sh
systemctl --user edit backdown
```

```ini
[Service]
ExecStart=
ExecStart=/usr/local/bin/backdown daemon --rescan-every 12h
```

Then `systemctl --user restart backdown`. `backdown service install --print` shows the unit it installs.

To keep the user service running while you're logged out:

```sh
loginctl enable-linger $USER
```

On macOS and Windows {preview}, the service runs while you're logged in: a LaunchAgent (`~/Library/LaunchAgents`, with its log in `~/Library/Logs/BackDown`) or a scheduled task named "BackDown" that starts it, without a window, when you log on. It uses the daemon's defaults; for other options, run `backdown daemon --rescan-every 12h` yourself instead.

## Notifications {preview}

The daemon keeps a list of what's worth knowing: a drive whose [health](/docs/concepts/#drive-health) got worse, a mirror that no longer matches its original, a scheduled job that failed. It's under Notifications in the browser's Activity view, with read and unread. "Show on the desktop" there turns desktop notifications on or off; the list is kept either way. The choice is saved in `notifications.json` in your data folder. The machine-wide service doesn't pop anything up: it isn't on anyone's desktop.

## A machine-wide service

On a NAS or a server, one daemon can index the whole machine for everyone:

```sh
sudo mv backdown /usr/local/bin/
sudo backdown service install --system
sudo usermod -aG backdown alice
```

It runs as a `backdown` user that can read every file and write none, with the whole file system read-only to it except its own folder, `/var/lib/backdown`, and no network beyond this computer, all enforced by systemd. Members of the `backdown` group can use it with the usual commands and `backdown open` (after logging in again). It doesn't wipe or rescue: do those from your own account with `--local`.

`backdown service status` (with `--system` for this one) says whether it's installed and running and what it's doing; `backdown service uninstall` removes it and keeps the index.

## Other options worth knowing

| Option | On | What it does |
|---|---|---|
| `--jobs N` | `hash`, `dupes`, `check` | Readers per file system: 1 suits spinning disks, raise it for SSDs |
| `--cross-filesystems` | `index` | Also index file systems mounted inside the path |
| `--local` | any command | Use the index directly even while the daemon runs |
| `--schema FILE` | `plan` {preview} | Your own filing schema ([how](/docs/guides/file-into-an-archive/#your-own-schema)) |
| `--verify-every DAYS` | `mirror add` {preview} | How often the daemon re-reads a sample of a mirror |

The [CLI reference](/docs/cli/) has every option.

## Licences {preview} {#licences}

The preview has no paid features: everything in it works without a licence. BackDown Pro is [planned](/pricing/), and a Pro licence is a key that starts with `BD1.`. It's checked on your computer, against a signature only backdown.io can make, so it needs no account and no network:

```sh
backdown license add BD1.eyJ2Ijox…
backdown license show
```

```text
BackDown Pro licence 7KQ2M-…
  for you@example.org · 1 machine · issued 2026-10-01
```

`add` checks the key and keeps it in your data folder (`license.txt`); `-` reads it from standard input. `show` exits with 1 when there's no licence. A key bought as a pre-order says so: Pro features switch on in the release that ships them, and your first year starts then.
