---
title: Browse without restoring
description: Open any file on any indexed drive as it was at any scan, read mailboxes as conversations, see SQL dumps as schemas, look inside disk images, and search every drive at once.
order: 5
---

The file browser shows everything in the index the way a file manager would, with history: every drive, every folder as it was at any scan, and a viewer for most files, without copying anything back first. It only reads.

The browser, the time slider, versions and copies, and the viewers for photos, PDFs, Word documents, SQLite databases, code, audio, video, git history and diffs are in today's download. Sections marked **next preview** come with the next release.

## Open it

```sh
backdown serve
```

`serve` prints a private link and opens it: `http://127.0.0.1:8787/?t=…`. The browser listens on this computer only, answers only that link (the token becomes a cookie for the session), and makes no requests beyond this computer: the viewers are built in. With the daemon running, `backdown open` opens its browser instead, whose link stays the same across restarts. `backdown serve --port 8788` runs a second one next to the daemon's.

## Find your way

Your indexed drives are in the sidebar, with their size and number of files. Folders open with a double click or <kbd>Enter</kbd>; <kbd>Alt+↑</kbd> goes to the parent, <kbd>Alt+←</kbd> back. Grid (<kbd>Ctrl+1</kbd>) and list (<kbd>Ctrl+2</kbd>) views show the folder; the details pane (<kbd>Ctrl+I</kbd>) shows a file's versions, its copies on other drives, and a preview.

With the next preview there's more: a photo gallery (<kbd>Ctrl+3</kbd>) and a size map (<kbd>Ctrl+4</kbd>), filters by type, size and date, sorting, hidden files (<kbd>Ctrl+H</kbd>), tabs, typing a location (<kbd>Ctrl+L</kbd>, where a drive's name stands for its path), right-click menus, and a dot beside each drive for its [health](/docs/concepts/#drive-health). Press <kbd>?</kbd> for every shortcut.

From the right-click menu {preview}, "Open with system app" opens a file in the program your computer uses for it, and "Show in file manager" shows where it is. BackDown never opens programs or scripts that way. "Download a copy" saves a copy through your web browser, from any drive or any scan whose bytes still exist.

## Go back in time

The bar at the bottom of a drive with two scans or more is the *time slider*: drag it to see the drive as it was at that scan. Files deleted since are struck through, and those that changed later are marked. **Back to now** returns.

A file's old version opens if its bytes still exist: the file itself, unchanged, or an identical copy on any indexed drive ("Read from … (identical copy)"). Otherwise the index still knows its size, dates and hash, and says the bytes are gone. The details pane lists every version of a file, with **Diff** against the current one for text.

**Compare…** {preview} shows what changed in a folder between any two scans: what was added, removed and changed, with a diff for text files. Files count as the same when their SHA-256 hashes match (or, before they're hashed, their size and modification time).

## Search every drive {preview} {#search}

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
| Anything else | The first 4 KB, as hex |

With the next preview {preview}:

| Files | What you see |
|---|---|
| Spreadsheets (`.xlsx`, `.xls`, `.ods` and more) | Sheets as tables, 500 rows a page |
| Slides (`.pptx`, `.odp`) | Titles, text, images and notes |
| Archives (`.zip`, `.tar.gz` and more) | What's inside, and each file in it opens too |
| Mailboxes | Conversations: see below |
| SQL dumps | The schema: see below |
| Disk images | The partitions, and the files inside: see below |

## Mailboxes {preview} {#mailboxes}

Mail opens as conversations, threaded by the headers that link replies together. BackDown reads `.mbox` files, single `.eml` messages, mailbox files without an extension (Thunderbird's `Inbox`, say), and Maildir folders: open a Maildir with "Open as mailbox". You get threads or a plain message list, a filter by sender and subject, unread, flagged, replied and forwarded marks, and a folder list for a mailbox with sub-mailboxes. Attachments are listed and download with a click; images among them show inline.

A message that only has an HTML version is shown as plain text: nothing in it is loaded or run. Very large mailboxes thread their first 250,000 messages.

## SQL dumps {preview} {#sql-dumps}

A database dump (`.sql`, `.mysql`, `.pgsql`, `.dump`, also compressed with gzip, bzip2, xz or zstd) opens as its schema: which program made it, from which server and database, and when, then every table with its row count, data size, columns, keys, indexes and foreign keys, plus views, triggers and routines. It reads dumps from `mysqldump` and MariaDB, phpMyAdmin, Adminer and HeidiSQL, PostgreSQL's `pg_dump` (plain format), SQLite's `.dump`, and plain SQL scripts. It shows the structure and counts, not the rows; the SQL tab shows the text itself. A dump that looks cut off says so.

## Disk images {preview} {#disk-images}

Disk images open read-only, without mounting anything: raw images (`.img`, `.dd`, `.raw`, `.bin`), ISO discs, VHD, VHDX, VMDK, VDI, qcow and Apple disk images (`.dmg`). BackDown shows the partition table and names each partition's file system (NTFS, exFAT, FAT, ext2/3/4, XFS, btrfs, APFS, HFS+ and more, with BitLocker and LUKS shown as encrypted).

The files inside can be browsed on FAT, exFAT, ext2/3/4 and NTFS partitions, and on ISO discs, in raw images, fixed-size VHD and fixed-size VDI. Files inside open in the same viewers and can be downloaded. For the other formats (VMDK, VHDX, qcow, and growing VHD and VDI images), convert to a raw image first, for example with `qemu-img convert -O raw`. The files in `.dmg` images can't be browsed yet.

## Nothing is changed

Browsing never writes to your drives. Everything that changes something (copying into an archive, freeing up a drive, rescuing, rescanning) happens in the daemon and is described in its own guide. The browser opened with `backdown serve` can't do those; it says which command to run instead.
