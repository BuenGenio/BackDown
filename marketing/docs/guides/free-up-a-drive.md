---
title: Free up a drive
description: Delete only what has a verified copy elsewhere, move what exists only on the drive first, and see exactly what will happen before anything does.
order: 1
---

Freeing up a drive is the one thing BackDown deletes, and only when you ask. The rule is simple: a file is deleted only when an identical copy exists outside what you're freeing, and both have just been read again and compared. What exists only on the drive can be moved somewhere first, or left where it is.

It works on whole drives or on folders, so it also cleans up duplicates: free up the copy you don't want, and the one you keep is its proof.

Freeing up is marked Pro; in this beta everything is unlocked ([Free and Pro](/docs/guides/settings-and-licences/#free-and-pro)). `backdown check`, which says whether a drive is safe to wipe, is free.

## 1. Index everything that might hold copies

The copies have to be in the index. Index the drive you want to free and the drives that should hold its copies, then check it:

```sh
backdown index /media/old-drive /media/archive
backdown check /media/old-drive
```

`check` hashes what's needed and gives the verdict: **SAFE TO WIPE**, or **COPY FIRST** with the folders holding what exists nowhere else ([what "safe to wipe" means](/docs/concepts/#what-safe-to-wipe-means)). You don't need "safe to wipe" to go on, as long as you decide what happens to the rest.

## 2. See what would happen

Without `--go`, `backdown wipe` only shows what it would do:

```sh
backdown wipe /media/old-drive
```

```text
/media/old-drive
  44 files, 182 MB
  162 MB has an identical copy elsewhere: 99 MB in archive/Media, 60 MB in archive/Backups, 3 MB in archive/Documents
  20 MB (7 files) exists only here: left in place (--move-to <folder> moves it first)

Nothing was changed. To wipe: backdown wipe "/media/old-drive" --go --confirm "old-drive"
Every file is re-read before it's deleted, and so is its copy (--trust-index skips that).
```

You can name several drives or folders at once. Copies inside what you've named don't count: they're going too.

## 3. Decide what happens to the files that exist only there

Either leave them, and they stay on the drive, or move them first with `--move-to`:

```sh
backdown wipe /media/old-drive --move-to /media/archive/Unsorted
```

Each is copied to `<move-to>/<drive name>/<its path on the drive>` (here `/media/archive/Unsorted/old-drive/Photos/2016/…`), read back to check the copy, and only then deleted from the old drive. The place to move to can't be inside what's being freed. Names are kept as they are: if a different file is already there under a name the destination takes for the same one, the file is left in place. On a Mac or Windows, a file whose extended attributes or alternate data streams couldn't be kept on the copy is left in place too, and the message says what couldn't be kept. For a tidier result, [file the drive into your archive](/docs/guides/file-into-an-archive/) first; afterwards everything on it has a copy.

## 4. Wipe

Add `--go`, and type the drive names the dry run showed after `--confirm`:

```sh
backdown wipe /media/old-drive --move-to /media/archive/Unsorted --go --confirm "old-drive"
```

For every file, in turn:

1. It must still be what the index says: same size and modification time. A file that changed, or is new since the last scan, is left alone.
2. It's read again and hashed. If it no longer matches its hash, it's left alone.
3. A copy outside the selection is found in the index and read again too. Only if it's there and identical is the file deleted. On Windows, the copy must also have the same alternate data streams. A copy that's only on [another machine](/docs/guides/several-machines/) is used when there's none here: that machine reads it again, in full, just before the deletion.
4. With no copy: with `--move-to`, it's moved (copied, verified, deleted); without, it's left.
5. The deletion is written to BackDown's journal with its proof: which copy, its hash, and how it was checked. It's also an event in the [chain of custody](/docs/guides/chain-of-custody/), the signed log you can export and hand to someone who needs proof of what was deleted and where its copy is.

Folders the wipe leaves empty are removed; the drive's own top folder stays. Files that couldn't be read at the last scan are left in place, and so are online-only cloud files on Windows, which would have to be downloaded to be read.

`--trust-index` skips the two reads: it checks that the copy is still there with the size and modification time it had when it was scanned, and trusts the hashes in the index (a copy on another machine is checked the same way, there). It's much faster and, right after a fresh scan and hash of both drives, nearly as safe, but a file that quietly went bad since its last hash wouldn't be noticed.

> [!NOTE]
> A drive mounted read-only can't be wiped; the message says so. Remount it read-write first.

## 5. Let BackDown see what's gone

When the background service runs the wipe, it rescans the drives involved afterwards. Without it, index the drive again. If the wipe removed everything, the drive is now empty, and indexing an empty drive needs `--allow-empty`:

```sh
backdown index /media/old-drive --allow-empty
```

## What it refuses

`backdown wipe` stops before doing anything if what you named is:

- part of the running operating system: `/`, `/usr`, `/etc`, `/var`, `/boot`, `/System`, `/Applications` and the like, or anything else on the file system the computer runs from, outside the home folders;
- a whole home folder, such as `/home/you` or `C:\Users\you` (folders inside it are fine);
- on Windows, the system drive itself or `Windows`, `Program Files` or `ProgramData` on it;
- BackDown's own index, or the folder holding it;
- not on an indexed drive.

Other drives are fine wherever they're mounted: `/media`, `/mnt`, `/run/media`, other drive letters. The machine-wide service never wipes; run `backdown --local wipe …` from your own account.

## In the browser

The **Free up** view does the same, with the background service doing the work:

1. Tick drives, folders, files or recognised things under "What to wipe". Each drive shows how much of it exists elsewhere; right-click anything in the browser and choose "Free up" to start from there. Or start from **Duplicates**: tick the copies to let go ("Keep only this" on a copy ticks the others, and **Select** does it for every group, keeping the oldest, the newest or the shortest path), then press **Free up…**. One copy of each always stays.
2. "What would happen" shows, for each, how much has an identical copy and where, how much exists only there, and anything that can't be wiped, and why.
3. Under "What exists only here", choose "Move it first, to" a folder, or "Leave it where it is". The view shows the free space there.
4. Under "Before deleting", "Trust the index" is the same as `--trust-index`.
5. Type the drive names to confirm, and press **Wipe**.

The wipe shows in Activity, and the view lists past wipes with what existed only there and what happened to it. Its **Chain of custody** button downloads the [signed log](/docs/guides/chain-of-custody/) of every deletion and move. When some copies are only on another machine, "What would happen" says how much, and when that machine last confirmed them. The browser opened with `backdown serve` shows what would happen but can't wipe: that needs the background service.
