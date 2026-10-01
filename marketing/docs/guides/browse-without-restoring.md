---
title: Browse without restoring
description: Open any file on any indexed drive as it was at any scan, read mailboxes as conversations, see SQL dumps as schemas, look inside disk images, and search every drive at once.
order: 9
---

The file browser shows everything in the index the way a file manager would, with history: every drive, every folder as it was at any scan, and a viewer for most files, without copying anything back first. It only reads.

## Open it

```sh
backdown serve
```

`serve` prints a private link and opens it: `http://127.0.0.1:8787/?t=…`. The browser listens on this computer only, answers only that link (the token becomes a cookie for the session), and makes no requests beyond this computer: the viewers are built in. The app's window (`backdown app`) and `backdown open` show the background service's browser instead, which can also start scans, copies and rescues; they open it with a one-time code, so the private token never appears on a command line. `backdown serve --port 8788` runs a second one next to the service's.

## Find your way

Your indexed drives are in the sidebar, with their size and number of files. Folders open with a double click or <kbd>Enter</kbd>; <kbd>Alt+↑</kbd> goes to the parent, <kbd>Alt+←</kbd> back. Grid (<kbd>Ctrl+1</kbd>) and list (<kbd>Ctrl+2</kbd>) views show the folder; the details pane (<kbd>Ctrl+I</kbd>) shows a file's versions, its copies on other drives, and a preview.

There's more: a photo gallery (<kbd>Ctrl+3</kbd>) and a size map (<kbd>Ctrl+4</kbd>), filters by type, size and date, sorting, hidden files (<kbd>Ctrl+H</kbd>), tabs, typing a location (<kbd>Ctrl+L</kbd>, where a drive's name stands for its path), right-click menus, and a dot beside each drive for its [health](/docs/concepts/#drive-health). Press <kbd>?</kbd> for every shortcut.

From the right-click menu, "Open with system app" opens a file in the program your computer uses for it, and "Show in file manager" shows where it is. BackDown never opens programs or scripts that way. "Download a copy" saves a copy through your web browser, from any drive or any scan whose bytes still exist.

## Go back in time

The bar at the bottom of a drive with two scans or more is the *time slider*: drag it to see the drive as it was at that scan. Files deleted since are struck through, and those that changed later are marked. **Back to now** returns.

A file's old version opens if its bytes still exist: the file itself, unchanged, or an identical copy on any indexed drive ("Read from … (identical copy)"). Otherwise the index still knows its size, dates and hash, and says the bytes are gone. The details pane lists every version of a file, with **Diff** against the current one for text.

**Compare…** shows what changed in a folder between any two scans: what was added, removed and changed, with a diff for text files. Files count as the same when their SHA-256 hashes match (or, before they're hashed, their size and modification time).

## Search every drive {#search}

<kbd>Ctrl+F</kbd> searches the folder you're in and everything below it; <kbd>Ctrl+Shift+F</kbd> searches every indexed drive. It matches file and folder names, not their contents, ignoring case and accents; with several words, a name must contain all of them. Exact names come first, then names that start with what you typed, then the rest, newest first.

Tick "Earlier versions" to find files that were there at earlier scans but aren't now; opening one shows its folder as it was when it was last seen. The first 200 matches show, and "Show more" goes up to 2,000.

## What opens in the browser

| Files | What you see |
|---|---|
| Photos and images | The image, with actual size and fit |
| Audio and video | Your browser's player |
| PDF | Pages, with zoom |
| Word (`.docx`) | The document |
| SQLite databases | Tables and views with row counts, 200 rows a page, and each table's SQL; opened read-only |
| Text and code | With syntax highlighting, up to 8 MB |
| CSV and TSV | A table |
| Git repositories | The history, last 300 commits with their changes (needs `git` installed) |
| Two text files | A diff, with how similar they are |
| Spreadsheets (`.xlsx`, `.xls`, `.ods` and more) | Sheets as tables, 500 rows a page |
| Slides (`.pptx`, `.odp`) | Titles, text, images and notes |
| Archives (`.zip`, `.tar.gz` and more) | What's inside, and each file in it opens too |
| Mailboxes | Conversations: see below |
| SQL dumps | The schema: see below |
| Disk images | The partitions, and the files inside: see below |
| Anything else | The first 4 KB, as hex |

## Mailboxes {#mailboxes}

Mail opens as conversations, threaded by the headers that link replies together. BackDown reads `.mbox` files, single `.eml` messages, mailbox files without an extension (Thunderbird's `Inbox`, say), and Maildir folders: open a Maildir with "Open as mailbox". You get threads or a plain message list, a filter by sender and subject, unread, flagged, replied and forwarded marks, and a folder list for a mailbox with sub-mailboxes. Attachments are listed and download with a click; images among them show inline.

A message that only has an HTML version is shown as plain text: nothing in it is loaded or run. Very large mailboxes thread their first 250,000 messages.

## SQL dumps {#sql-dumps}

A database dump (`.sql`, `.mysql`, `.pgsql`, `.dump`, also compressed with gzip, bzip2, xz or zstd) opens as its schema: which program made it, from which server and database, and when, then every table with its row count, data size, columns, keys, indexes and foreign keys, plus views, triggers and routines. It reads dumps from `mysqldump` and MariaDB, phpMyAdmin, Adminer and HeidiSQL, PostgreSQL's `pg_dump` (plain format), SQLite's `.dump`, and plain SQL scripts. It shows the structure and counts, not the rows; the SQL tab shows the text itself. A dump that looks cut off says so.

## Disk images {#disk-images}

Disk images open read-only, without mounting anything: raw images (`.img`, `.dd`, `.raw`, `.bin`), ISO discs, VHD, VHDX, VMDK, VDI, qcow and Apple disk images (`.dmg`). BackDown shows the partition table and names each partition's file system (NTFS, exFAT, FAT, ext2/3/4, XFS, btrfs, APFS, HFS+ and more, with BitLocker and LUKS shown as encrypted).

The files inside can be browsed on FAT, exFAT, ext2/3/4 and NTFS partitions, and on ISO discs, in raw images, fixed-size VHD and fixed-size VDI. Files inside open in the same viewers and can be downloaded, and deleted files on FAT, exFAT and NTFS can be listed and copied out ([Recover deleted files](/docs/guides/recover-deleted-files/)). For the other formats (VMDK, VHDX, qcow, and growing VHD and VDI images), convert to a raw image first, for example with `qemu-img convert -O raw`. BackDown can't browse the files in `.dmg` images itself.

When an image holds file systems BackDown doesn't read itself (XFS, btrfs, HFS+, APFS and others), the viewer offers **Mount read-only**: the system attaches the image read-only and mounts what it can read (udisks on Linux, `hdiutil` on macOS; on Windows, only ISO, VHD and VHDX images). Each mounted file system then has **Index this**, and **Detach** unmounts it and lets the image go. `backdown mount` does the same in a terminal ([mounting drives](/docs/concepts/#mounting-drives-read-only)).

## Other machines

Drives on [another machine](/docs/guides/several-machines/) are listed under Machines in the sidebar and browse the same way, read-only, with each file's copies here and there. Their files don't open or preview here.

## Look from your other devices {#look-from-your-other-devices}

The browser listens on this computer only, unless you ask. `--host ADDRESS` makes it listen on one more address as well, such as this computer's Tailscale address, so you can look at your drives from your phone or another computer:

```sh
backdown serve --host 100.x.y.z
```

```text
BackDown browser for /home/you/.local/share/backdown/index.db at
  http://127.0.0.1:8787/?t=4f0c…
and from other devices, to look but not change anything, at
  http://100.x.y.z:8787/?t=4f0c…
(the link carries a one-time token; Ctrl-C to stop)
```

From anywhere but this computer, BackDown only shows what's there. Browsing, the time slider, search, the viewers and downloading a copy of a file work, and so does asking for a rescan. Nothing can be wiped, copied, rescued, mounted, opened with a program or otherwise changed, and settings stay as they are: "Over the network, BackDown only shows what's there: freeing up drives, copying, rescuing, mounting, opening files and changing settings need someone at the computer BackDown runs on."

- The link's token is still needed, as on this computer. 127.0.0.1 keeps listening, so `backdown app` and `backdown open` work as before.
- The link works with the address you gave, with this computer's full Tailscale name (`nextcloud.tailnet-1234.ts.net`, when Tailscale runs here) and, on Linux, with its short name. Other names it's reached by (your own DNS, a reverse proxy) can be allowed with `BACKDOWN_HOST_NAMES=name1,name2`.
- The connection is plain HTTP. Over Tailscale the network encrypts it; on any other network, someone who can watch it could read the token and what you look at. Prefer an address only your own devices can reach: a wildcard such as `0.0.0.0` listens on every network the computer is on.
- For the background service: `backdown daemon --host 100.x.y.z`, or, on Linux, `backdown service install --host 100.x.y.z`. `backdown token --links` prints its links with the token in them: one for this computer and one for your other devices, named by Tailscale when it can be. `backdown token` prints the token alone. On macOS and Windows, `service install` doesn't pass `--host` on to the service yet, and the machine-wide service on Linux stays cut off from the network by systemd.

## Nothing is changed

Browsing never writes to your drives. Everything that changes something (copying into an archive, freeing up a drive, rescuing, rescanning) happens in the background service and is described in its own guide. The browser opened with `backdown serve` can't do those; it says which command to run instead.

The viewers for databases, spreadsheets, presentations, mailboxes and SQL dumps are marked Pro; in this beta everything is unlocked ([Free and Pro](/docs/guides/settings-and-licences/#free-and-pro)).
