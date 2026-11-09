---
title: Recover deleted files
description: Open a drive's or card's file system directly, see deleted files with their names and how likely each is intact, and copy them out to another drive, checked. FAT, exFAT and NTFS; ext4 by content instead.
order: 8
seo_title: Recover deleted files from FAT, exFAT and NTFS drives
---

When a file is deleted, most file systems only mark its entry and its space as free; the bytes stay until something else is written there. BackDown can read a drive's file system itself, without mounting it, list what's deleted with its original name and folder, say how likely each one is to come back whole, and copy what you choose to another drive, checking every copy.

It works on FAT (memory cards, USB sticks), exFAT and NTFS. It only reads the drive.

> [!WARNING]
> Stop using the drive or card first: every file written to it can land on the space a deleted file still occupies. Don't install anything onto it, and don't save what you recover onto it. If it's failing, [copy it into an image](/docs/guides/rescue/#copy-a-failing-drive-into-an-image) first and recover from the image.

This is done in the app (Rescue needs the background service); there's no terminal command for it yet.

## 1. Find the drive's file systems

1. Open **Rescue** and choose the drive or card (or an image file of one). If BackDown can't read it directly, it shows how to allow that ([when BackDown can't read the drive](/docs/guides/rescue/#when-backdown-cant-read-the-drive)).
2. Under "What happened?", choose **It looks empty, or a partition vanished**, and start it. This lists every file system on the drive, whether the partition table still lists it or not. It only reads, and is quick.
3. Each FAT, exFAT, ext or NTFS file system found gets **Browse files**.

A disk image on an indexed drive (a raw image, or a fixed-size VHD or VDI) works the same way: open it in the browser, and each partition has **Browse files**.

<figure class="demo" data-view="rescue" data-caption="Rescue, step 1: every disk, partition and image file, and whether BackDown can read it."></figure>

## 2. Show what was deleted

**Browse files** opens the file system as it is now, read-only, with its folders, names, sizes and dates. Tick **Show deleted files** to add what was deleted, greyed, with a label:

| Label | What it means |
|---|---|
| **deleted, recoverable** | Everything it used is still free. It should come back whole, most likely if it was small or stored in one piece |
| **partly overwritten** | Some of its space has been used by other files since, and those parts come back empty: "about 40% of it is still there" |
| **overwritten** | Other files have been saved where it was. It can't be copied |
| **can't be read** | An NTFS file encrypted with Windows' EFS: it needs the Windows account's keys |

Hover over a deleted file to see why, and how sure BackDown is, as a percentage. For example:

- "it fitted in one cluster, and no other file has used it since" (95%);
- "its MFT record still says where its data is, and no other file has used that space since" (95%);
- "its clusters are all still free; they're taken to follow one another, as most files are written (if it was stored in pieces, parts may come back wrong)" (70%).

That last one is the limit of recovering from FAT: once a file is deleted there, the record of where its pieces were is gone, so BackDown assumes they followed one another, which is true of most files. exFAT often keeps that record (then it's followed), and NTFS keeps it in the file's MFT record. On FAT without long names, the first letter of a deleted file's name is lost and shows as `_`.

A deleted folder opens too; everything in it is deleted. A deleted file whose folder was deleted as well shows at the top level: "Its folder was deleted too, so it's shown here". Recoverable files preview like any other; parts other files have taken show as empty.

## 3. Copy them out

Tick the files and folders you want and choose **Copy these out…**; with nothing ticked, the button copies the folder you're in (**Copy this folder out…**), or at the top, **Copy everything out…**:

1. "Save in" suggests `BackDown Rescue/<name> <date>` on one of your indexed drives; type a full path to put them elsewhere. It must be on another drive: BackDown refuses a folder on the drive being read, because saving there could overwrite the very files being recovered.
2. **With the deleted files in chosen folders** copies what's deleted inside the folders you picked, too.
3. **Start copying.**

Each file is written, flushed to disk, read back and compared before it counts; a copy that doesn't read back the same is removed. Nothing at the destination is overwritten: a name that's taken becomes `name (2).ext`. Characters the destination can't hold become `_`. Files keep their modification time. Links aren't copied, and neither are files marked overwritten.

The copy shows in Rescue and in Activity, can be stopped and carried on, and ends with what was copied and what couldn't be, with **Open the copies**. The copies are then indexed, so they open in the browser.

## ext3 and ext4

Linux's ext3 and ext4 erase where a deleted file's data was, so deleted files can't be brought back by name. Rescue can still find photos, videos and documents on such a drive by their contents: choose **Files were deleted, or it was formatted** instead ([find deleted files](/docs/guides/rescue/#find-deleted-files)). Those come back without their names.

## When this doesn't help

- The file system itself is damaged beyond reading, or it's one BackDown doesn't read (XFS, btrfs, APFS, HFS+ and others): use [find by content](/docs/guides/rescue/#find-deleted-files).
- The drive was formatted: formatting usually replaces the file system's records, so the old names are gone. Find by content.
- An SSD usually discards deleted space soon after (TRIM): what was there can't be recovered by any tool, and BackDown can't tell from the file system that it's gone.
- A file in thousands of scattered pieces on FAT comes back wrong, even when it says "recoverable".
