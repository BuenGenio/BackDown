---
title: Rescue a failing drive
description: Copy a failing drive or memory card safely, find deleted photos and documents by their contents, and look for lost partitions. BackDown only reads the drive.
order: 4
preview: yes
---

> [!PREVIEW]
> `backdown rescue` and the browser's Rescue view come with the next preview.

Three things can go wrong with a drive or a memory card, and Rescue has an answer for each:

| What happened | What to do | Command |
|---|---|---|
| It's failing: clicks, freezes, read errors, very slow | Copy it into an image first, the good areas before the bad | `backdown rescue image` |
| Files were deleted, or it was formatted | Find photos, videos and documents by what's inside them | `backdown rescue find` |
| It looks empty, or a partition vanished | Look for file systems the partition table no longer lists | `backdown rescue partitions` |

Rescue only ever reads the drive. Whatever it saves goes to another drive: it refuses to write the image or the files it finds onto the drive being rescued.

> [!WARNING]
> If a drive is failing, every read counts. Copy it into an image first, then look for files and partitions in the image, not on the drive.

BackDown can tell you a drive is failing before it stops: `backdown health` and the browser show what each drive reports about itself ([drive health](/docs/concepts/#drive-health)), and a failing drive gets a banner with **Copy it now**, which opens Rescue set up to copy it.

## Which drive is it?

In the browser, the Rescue view lists every disk and partition. In a terminal, `backdown volumes` does, with model, size, serial number and file systems; or use `lsblk` on Linux and `diskutil list` on macOS. Drives are device files such as `/dev/sdc` on Linux or `/dev/disk4` on macOS. Every rescue command also takes an image file instead of a drive.

## When BackDown can't read the drive

Reading a whole drive directly needs a permission your account usually doesn't have. BackDown says so, and gives the one command that lets your account read that one drive, and never change it, until it's unplugged or the computer restarts:

```text
backdown: can't read /dev/sdc: reading a whole drive directly needs permission your user account doesn't have
Run it as an administrator (sudo backdown rescue image …), or let your account read that drive: sudo setfacl -m u:you:r /dev/sdc
```

On macOS the command is `sudo chmod o+r /dev/disk4`. On Windows, run the terminal as administrator.

Running Rescue itself with `sudo` is fine too: it never opens BackDown's index, and what it writes is handed back to you rather than left owned by root.

## Copy a failing drive into an image

```sh
backdown rescue image /dev/sdc /media/archive/Rescue/sdc.img
```

```text
Copying /dev/sdc (500.1 GB) into /media/archive/Rescue/sdc.img
The map: /media/archive/Rescue/sdc.img.map
Ctrl+C stops it; run the same command again to carry on.
```

It works the way GNU ddrescue does. The first passes copy what reads easily, forwards and then backwards, skipping ahead past slow or bad areas instead of hammering at them. Later passes go back to the edges of the bad areas, then read what's left sector by sector, and finally retry unreadable sectors (`--retries 3` for more tries). Unreadable parts stay as zeros in the image.

A *map* beside the image records what's been read, in GNU ddrescue's own format. It's saved every few seconds, so you can stop the copy at any time, or lose power, and run the same command to carry on where it left off. When it's done:

```text
Copied 500.1 GB of 500.1 GB; 12 KB couldn't be read, in 3 places. Running it again with --retries 3 tries those again.
```

It exits with 0 when everything was read, and 2 when some of it couldn't be. It won't start if:

- the image would be on the drive being rescued, or on another partition of the same physical drive (`--same-drive` allows that for a drive that isn't failing);
- the destination doesn't have room for what's left to copy, or is formatted FAT32, which can't hold a file of 4 GB or more (it says so);
- an image of that name exists without its map (it's never overwritten).

## Find deleted files

```sh
backdown rescue find /media/archive/Rescue/sdc.img --to /media/archive/Rescue/sdc-files
```

This looks through every byte for the signatures of files, whatever the file system says: after files were deleted, the card was formatted, or the file system is damaged. What it recognises:

| Kind | Types |
|---|---|
| Photos | JPEG, HEIC, PNG, GIF, WebP, BMP, AVIF, TIFF, camera RAW (CR2, CR3, NEF, ARW, DNG, ORF, RW2, PEF, SRW, RAF) |
| Videos | MP4, MOV, M4V, 3GP, AVI, MKV, WebM, WMV, MTS, TS, MPEG, OGV |
| Audio | MP3, M4A, WAV, FLAC, Ogg, Opus, AIFF, WMA |
| Documents | PDF, Word, Excel and PowerPoint (old and new), OpenDocument, EPUB, Outlook messages |
| Archives | ZIP, RAR, 7z |
| Other | SQLite databases, other Office-format files |

`--only photos,documents` narrows it to groups or types (`--only jpg,pdf`). What's found is saved in a folder per kind, named after where it was found on the drive, with the list in `found.jsonl`:

```text
Found 1.3 K files (6.1 GB): 1.2 K photos, 40 videos, 12 documents
In /media/archive/Rescue/sdc-files (the list: /media/archive/Rescue/sdc-files/found.jsonl)
```

Files found this way have no names, folders or dates of their own (photos and videos keep the dates inside them). A file that was stored in scattered pieces comes back damaged, or not at all: finding by content can't put pieces back together. A file whose end isn't found is kept up to a size limit for its type and counted as cut off.

When the daemon does the finding, it indexes what it found, so the files open in the browser.

## Look for lost partitions

```sh
backdown rescue partitions /dev/sdc
```

It reads the partition table (MBR, including logical partitions, or GPT, checking both of its copies), then looks for file systems where partitions usually start, and at every sector with `--deep` (as slow as copying the drive). File systems are found by their own structures, including their backups when the start is damaged: NTFS, exFAT, FAT, ext2/3/4, XFS, btrfs, APFS, HFS+, F2FS, SquashFS, ReFS, ISO 9660 and UDF; encrypted LUKS and BitLocker volumes, LVM, RAID members and swap are recognised as such.

```text
/dev/sdc (500.1 GB): no partition table

  Starts at byte        Sector        Size  File system            Name               In the table
         1048576          2048    500.1 GB  NTFS                   Photos             NO: lost

1 not in the table. To get files out of one: backdown rescue find /dev/sdc --to <folder> --start <byte> --end <byte>
```

It never changes the partition table. To get files out of a lost partition, run `rescue find` on just that range (its start, and its start plus its size), or use "Find files in it" in the browser.

## In the browser

The **Rescue** view asks three questions:

1. **Which drive or card?** Every disk and partition, or an image file. It checks BackDown can read it and, if it can't, shows the permission command with a Copy button and "Check again".
2. **What happened?** "It's failing", "Files were deleted, or it was formatted", or "It looks empty, or a partition vanished".
3. **Where should it go?** A folder on another drive (it suggests `BackDown Rescue/<drive> <date>` on one of your indexed drives), and what to look for.

Rescues run in the daemon beside scans and hashing, show their progress (a copy shows a map of what's been read), can be stopped and resumed, and stay on the list afterwards. From a finished copy you can go straight to "Find files in the copy" or "Look for partitions in the copy". Rescues need the daemon; the machine-wide service doesn't do them, so run `backdown rescue` from your own account.

## Not yet

Rescue doesn't reassemble fragmented files or open a lost partition's file system directly. A copy that's an image file on an indexed drive can be opened in the browser, where the files on its FAT, exFAT, ext and NTFS partitions can be browsed ([disk images](/docs/guides/browse-without-restoring/#disk-images)).
