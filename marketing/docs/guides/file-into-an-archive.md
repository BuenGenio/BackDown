---
title: File your old drives into one archive
description: Let BackDown propose where everything on your old drives goes in one archive, and why, then copy it there with every file verified. Nothing is deleted.
order: 2
seo_title: File old drives into one archive, with a plan and a dry run
---

You have several old drives and want everything on them in one archive, sorted into something sensible, with nothing lost and nothing copied twice. BackDown does it in two steps: a **plan** that says where each thing goes and why, which you can read and change, then **apply**, which copies, reads every copy back to check it, and keeps a journal. Neither step deletes anything. Once the archive is indexed, the old drives can be [freed up](/docs/guides/free-up-a-drive/). Afterwards, identical files inside the archive can share their storage ([tidy the archive](#tidy-the-archive)).

The filing plan is marked Pro; in this beta everything is unlocked ([Free and Pro](/docs/guides/settings-and-licences/#free-and-pro)).

## 1. Index everything, archive included

Index the old drives and the drive the archive will live on, so BackDown knows what's already there:

```sh
backdown index /media/old-drive /media/laptop-2012 /media/archive
backdown hash
```

The plan is built from what BackDown [recognises](/docs/concepts/#recognised-things-and-provenance): mail stores, phone backups, Time Machine sets, home folders, photo libraries and so on, with where they came from. `plan` recognises any drive that needs it, with the drives connected so it can read their backup scripts and plists; `backdown entities` shows you what it found beforehand, and the daemon keeps it up to date after every scan.

## 2. Make a plan

Name the archive folder, on the drive that will keep everything:

```sh
backdown plan --to /media/archive/Archive
```

```text
Plan 1: file 1 drive into /media/archive/Archive

Copy
      60 MB  Disk image: VDI image  (old-drive)
             /media/old-drive/VM/win10.vdi
           → /media/archive/Archive/DiskImages/old-drive/win10.vdi
             Disk image → DiskImages; newest file 2020
     119 MB  Folder  (old-drive)
             /media/old-drive/Photos
           → /media/archive/Archive/Unsorted/old-drive/Photos
             not recognised as anything in particular: kept as it is, under its drive's name

Drives
  old-drive                   182 MB: copy 182 MB, already archived 0 B; every file will have a copy

In all: 182 MB in 44 files.
Next: `backdown apply 1` shows exactly what would happen; `backdown apply 1 --go` does it.
```

Every indexed drive except the archive's own is filed, unless you name some with `--from` (repeat it for several). Each item says what it is, where it goes, and why. The plan is saved; `backdown plans` lists the ones you've made.

### How things are placed

Each kind of thing has a place in the archive, by type, then origin, then time. A mail store from `mail.example.org` goes to `Mail/mail.example.org`, an iPhone backup to `Phones/<device>/<year>`, a git repository to `Code/<origin>/<name>`. The *origin* comes from what BackDown found out about it (the server a backup script copied from, the phone's name, the computer's host name, a repository's remote); without one, it's the drive's name. Folders that aren't recognised as anything in particular keep their shape under `Unsorted/<drive>/`, and loose files at a drive's top level go to `Unsorted/<drive>/(top level)`.

A few rules keep the archive free of repeats:

- **Already archived.** Something whose bytes are already at least 99.9% inside the archive folder isn't copied: "already in the archive at …".
- **Near twins.** When two things of the same kind and origin are at least 90% identical (the same mailbox on two old drives, say), only what's unique to the second one is copied, into `Also from <drive>/<name>` inside the first one's folder. If it's all in the first, it isn't copied at all. Two copies of a mailbox (Maildir or a mail store) can instead be [merged by Message-ID](/docs/guides/merge-mailboxes/#in-a-filing-plan) into one: `--merge-mail`, or **Merge these mailboxes** in the Plan view.
- **Low-value piles.** Downloads folders and recovered-file dumps (`found.000`, `recup_dir`, `lost+found`) are listed, not copied.
- **Caches** (caches, thumbnails, build output, trash) are copied by default, because a drive can only be proven safe to wipe when every file has a copy. The browser's "Leave out caches, previews and build output" box, or `skip_junk = true` in a schema, leaves them out; they then stay behind when you free up the drive.

### Your own schema

The places are a small TOML file you can change:

```sh
backdown plan --print-schema > schema.toml
backdown plan --to /media/archive/Archive --schema schema.toml
```

```toml
[place]
mail-store = "Mail/{origin}"
ios-backup = "Phones/{origin}/{year}"
photos = "Media/Photos/{name} ({drive})"
unsorted = "Unsorted/{drive}/{name}"
# … one line for every kind

[options]
# downloads piles and recovered fragments: list them, but don't copy
skip_low_value = true
# regenerable caches, previews, build output and trash: copy them too
skip_junk = false
# a thing at least this identical to another of its kind and origin copies only its own files
merge_overlap = 0.9
```

Places are relative paths inside the archive, and can use `{origin}`, `{name}`, `{title}`, `{drive}`, `{year}` and `{years}`. Characters that don't belong in file names become `-`.

## 3. Dry run

```sh
backdown apply 1
```

```text
Dry run of plan 1: 44 files (182 MB) to copy into /media/archive/Archive.
Nothing was written. Run `backdown apply 1 --go` to copy.
```

The dry run reads the drives and writes nothing. It needs every source drive connected, says what's already done from an earlier run, what's already in the archive, and which destinations already exist, and checks the archive's drive has room.

## 4. Apply

```sh
backdown apply 1 --go
```

Each file is copied under a temporary name beside its destination (`.<name>.backdown-partial`), hashed as it's written, flushed to disk, given the original's modification time, renamed into place, then read back and compared. A copy that reads back differently is removed and reported. Where the archive's drive allows it, copies are made faster: as clones that share the original's blocks (btrfs, XFS, APFS, when the old drive is the same file system) or by the kernel (Linux); they're read back all the same. Along the way:

- an existing file at the destination is never overwritten: if it's identical it counts as already there, and if it differs it's left alone and reported as a conflict;
- a name the archive's drive would take for another one (`Photo.jpg` beside `photo.jpg` on a Mac or Windows drive), or can't hold at all, is copied under another name (`photo (2).jpg`), and the journal says so ([copies across file systems](/docs/concepts/#copies-across-file-systems));
- on a Mac, extended attributes are copied too, and on Windows alternate data streams; what couldn't be kept is listed;
- nothing is ever deleted from the old drives;
- symbolic links aren't copied (the summary counts them);
- every file is written to a journal, so an interrupted run carries on where it stopped when you run the same command again;
- every copy is also an event in the [chain of custody](/docs/guides/chain-of-custody/): a hash-chained log, signed by this installation, that you can export for an auditor or a court.

It ends with a summary (how many were cloned or copied by the kernel, renamed, or kept without some attributes), and exits with 2 when there were conflicts or failures to look at. When the background service is running, `--go` is handed to it (you follow the progress in the terminal), and it indexes the archive afterwards.

## 5. Check, then free up

Copies only count as proof once the archive is indexed. Without the daemon:

```sh
backdown index /media/archive
backdown check /media/old-drive
```

When `check` says **SAFE TO WIPE**, the old drive can be [freed up](/docs/guides/free-up-a-drive/).

## 6. Tidy the archive {#tidy-the-archive}

An archive filled from several old drives still holds identical files in different places: the same photo filed under two albums, a backup inside a backup. `backdown dedup` makes identical files inside one folder share their storage, without taking any of them away:

```sh
backdown dedup /media/archive/Archive
```

```text
/media/archive/Archive (ext4): hardlinks
  412 sets of identical files: 530 files would be linked, saving 38.2 GB
  17 identical files kept apart: their permissions, owner or extended attributes, or modification time, differ from every other copy's
Biggest savings:
     4.7 GB  Backups/laptop-2019/VM/win10.vdi ← DiskImages/old-drive/win10.vdi
Hardlinked files are one file with several names: editing any of them changes them all. To make one a separate file again, copy it and replace it with the copy (cp -p photo.jpg photo.tmp && mv photo.tmp photo.jpg).

Nothing was changed. To link them: backdown dedup "/media/archive/Archive" --link hard --go
```

There are two ways to share:

- **Hardlinks** (`--link hard`, the default) work on any drive that has them: the files become one file with several names, so editing one edits them all. For an archive you only add to, that's usually what you want.
- **Clones** (`--link clone`) work on btrfs, XFS and APFS: they stay separate files that share their blocks until one of them changes.

It only links files the index says are identical (index and hash the folder first), on the same drive, inside the folder you name; files smaller than 4 KB are left alone (`--min-size`). Files whose permissions, owner or extended attributes differ (and, for hardlinks, their modification time) are kept apart, and so are files that also have names outside the folder. Without `--go` it only shows what it would do. With `--go`, each pair is read again first and must still match the index's hash; the link is made under a temporary name and swapped in only if nothing changed meanwhile; and every link is journaled in the index. An interrupted run carries on when you run it again. Index the drive afterwards so BackDown sees the links (the background service does it for you). It exits with 2 if any file failed.

It refuses the running system, whole home folders and BackDown's own index, like freeing up does. It isn't available on Windows yet, where linking could lose files' alternate data streams.

## In the browser

The **Plan** view does the same:

1. With no plan yet, "New plan" asks for the archive folder (each drive's `/Archive` is suggested), which drives to file, and whether to leave out caches. Press **Make plan**.
2. The plan shows each drive (to copy, already archived, and what would stay only there), then the items under "Copy", "Copy only what's unique" and "Not copied", each with its source, destination and why.
3. **Merge these mailboxes**, on an item that has a twin, merges the two by Message-ID instead of copying one's extra files beside the other.
4. **Dry run** reports what applying would do. **Apply** asks you to confirm, then copies in the background service; follow it in Activity.

Making a plan works in the browser opened with `backdown serve`; applying needs the background service.

At the bottom of the Plan view, **Tidy the archive** does what `backdown dedup` does: choose the folder (the plan's archive is suggested), **Hardlinks** or **Clones**, and the smallest size, then **See what it would save**, and **Link** to start it in the background service. Earlier runs are listed below it.

<figure class="demo" data-view="plan" data-caption="A plan in the browser: what goes where in the archive and why, what isn’t copied because it’s already there, and the drives’ totals."></figure>
