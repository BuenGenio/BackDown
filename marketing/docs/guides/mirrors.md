---
title: Keep a mirror
description: Keep a folder, usually part of your archive, copied on another drive, with changed files' old copies kept aside and a sample re-read now and then.
order: 4
---

An archive on one drive is one failure away from gone. A mirror keeps a second copy of a folder on another drive and checks it: new and changed files are copied and verified, nothing on the mirror is ever deleted, and a different sample of the mirror is read back regularly to catch copies that went bad.

## 1. Add a mirror

```sh
backdown mirror add /media/archive/Archive/Mail /media/backup/Mail
```

```text
Mirror 1: /media/archive/Archive/Mail → /media/backup/Mail
Copy it now with `backdown mirror sync 1`; the BackDown service syncs daily and re-reads a sample every 30 days.
```

The first folder must exist; the mirror folder is created on the first sync. Neither may be inside the other. `--verify-every 7` re-reads a sample every 7 days instead of 30.

## 2. Sync

```sh
backdown mirror sync
```

```text
mirror 1: 1 copied (3 MB), 1 updated (old copies kept in .backdown-versions), 30 unchanged, 1 only on the mirror (kept)
```

Without a number, every mirror is synced. A file with the same size and modification time on both sides is unchanged. New files are copied and read back to check them, as clones or by the kernel where the drives allow it, like [copies into an archive](/docs/guides/file-into-an-archive/#4-apply). A name the mirror's drive would take for another one, or can't hold, is copied under another name, and extended attributes (Mac) and alternate data streams (Windows) go along; `mirror sync` lists what it renamed or couldn't keep. When a file changed, the mirror's old copy is first moved to `.backdown-versions/<time>/` inside the mirror folder, so an accidental change on the original never destroys the only good copy.

A sync never deletes. Files that exist only on the mirror (because they were deleted from the original, say) are kept and counted. Symbolic links aren't copied. Check the mirror drive has room: sync doesn't check free space first.

## 3. Verify

```sh
backdown mirror verify
backdown mirror verify 1 --all
```

```text
mirror 1: re-read 31 files (99 MB): all identical
```

By default a 5% sample is read back (`--percent 10` for more), and it's a different 5% each day, so repeated checks cover the whole mirror over time. `--all` reads every file. Each is compared with the original's hash in the index when that's current, or with the original itself. Missing files, different sizes or contents, and unreadable files are listed, and the command exits with 2.

## Mirrors and the background service

With the background service running, you don't need to run these by hand. Whenever both drives are connected, it syncs each mirror once a day and re-reads a 5% sample when a verify is due. The browser's Activity view lists the mirrors, whether both drives are there, and when each was last synced and verified. A verify that finds differences becomes a notification, and when a mirror's drive reports a warning or that it's failing ([drive health](/docs/guides/drive-health/)), Activity says "This mirror drive is dying" and suggests a second mirror on a healthy drive.

```sh
backdown mirror list
```

```text
    1  /media/archive/Archive/Mail → /media/backup/Mail
       synced 2026-09-28 23:30 UTC: 31 files: 1 copied (3 MB), 1 updated (old copies kept), 30 unchanged, 1 only on the mirror
       verified 2026-09-28 23:30 UTC: re-read 31 files (99 MB, 100%): all identical
```

To stop keeping a mirror, `backdown mirror remove 1`. The copies stay where they are.

<figure class="demo" data-view="activity" data-caption="Activity lists each mirror with its last sync and the last verification."></figure>
