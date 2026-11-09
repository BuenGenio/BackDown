---
title: BackDown and recovery tools
nav: Recovery tools
seo_title: Recover deleted files: BackDown vs PhotoRec and TestDisk
description: PhotoRec carves files out of raw disk space, TestDisk repairs partition tables, ddrescue images a failing drive. BackDown's Rescue covers the common cases in one place, images first, reads only, and checks what it finds against everything you already have, so you keep only what you don't.
order: 3
---

The classic recovery tools are old, free and good, and BackDown doesn't try to replace the deep end of what they do. What it adds is the part they leave to you: the order of operations, the refusal to write where it shouldn't, and knowing which of the recovered files you already have.

## What the tools do

- **ddrescue** copies a failing drive into an image file, good areas first, retrying the bad ones later, keeping a log so it can resume. The right first step for a dying disk.
- **PhotoRec** reads raw disk space and carves out files by their signatures — photos, videos, documents, hundreds of types — with no need for a working file system. Names and folders are lost; the files come back as `f0012345.jpg`.
- **TestDisk** finds lost partitions, repairs partition tables and boot sectors, and can undelete from FAT, exFAT, NTFS and ext2.
- **Recuva, Disk Drill and similar** wrap the same ideas in a desktop app, for Windows and macOS.

They are powerful and unforgiving: nothing stops you running them on the drive you're trying to save, or saving the recovered files onto it.

## What BackDown's Rescue does

[Rescue](/docs/guides/rescue/) is one view in the app (and `backdown rescue` in the terminal) for the three things that usually go wrong:

- **It's failing**: copy it into an image, good areas before bad, resumable — what ddrescue does, started from a banner that appears the moment the drive's own health check says it's failing.
- **Files were deleted, or it was formatted**: find photos, videos and documents by their contents, as PhotoRec does. On FAT, exFAT and NTFS, [deleted files](/docs/guides/recover-deleted-files/) can also come back with their names and folders.
- **It looks empty, or a partition vanished**: look for file systems the partition table no longer lists, and browse them.

Three things are different from doing the same by hand.

**It only ever reads the drive.** Rescue refuses to write the image, or the files it finds, onto the drive being rescued; it insists on another drive. Images are opened read-only.

**It knows what you already have.** What Rescue copies out is indexed like any other drive, so the duplicates view and the overlaps show which of the recovered files also sit on your laptop, on last year's backup or on the NAS, and you can keep just the ones that exist nowhere else. Carving a 1 TB drive with PhotoRec yields a heap; with BackDown you can see which part of the heap matters.

**It's recorded.** What was rescued, from where, to where, with which hashes, goes into the [chain of custody](/docs/guides/chain-of-custody/) like every other copy BackDown makes.

<figure class="demo" data-view="rescue" data-caption="Rescue in the app: which drive or card, what happened, and the rescues so far."></figure>

## Where the classic tools still win

- **File systems BackDown doesn't read** (XFS, btrfs, HFS+, APFS and others): BackDown can search them by content, but TestDisk and the file systems' own tools can do more.
- **Repairing in place**: TestDisk writes a fixed partition table or boot sector back to the drive. BackDown never writes to a source drive; it rescues what it can read into a copy.
- **Fragmented files**: no carving tool reassembles a file stored in scattered pieces reliably, BackDown included. A deleted file on FAT in thousands of pieces comes back wrong whatever you use.
- **SSDs**: deleted space is usually discarded within minutes (TRIM). Nothing recovers it; no tool can tell you from the file system that it's gone.

## A sensible order

1. If the drive is failing, image it first — with BackDown or ddrescue — and work on the image.
2. Try the file system's own records first: BackDown's deleted-files view (FAT, exFAT, NTFS) or TestDisk's undelete, which keep names and folders.
3. Carve by content last, and let BackDown tell you which of the carved files you already have elsewhere.
