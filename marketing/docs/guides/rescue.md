---
title: Rescue a failing drive
description: Copy a failing drive or memory card safely, find deleted photos and documents by their contents, and look for lost partitions and the files on them. BackDown only reads the drive.
order: 7
---

Three things can go wrong with a drive or a memory card, and Rescue has an answer for each:

| What happened | What to do | Command |
|---|---|---|
| It's failing: clicks, freezes, read errors, very slow | Copy it into an image first, the good areas before the bad | `backdown rescue image` |
| Files were deleted, or it was formatted | Find photos, videos and documents by what's inside them | `backdown rescue find` |
| It looks empty, or a partition vanished | Look for file systems the partition table no longer lists, and browse them | `backdown rescue partitions` |

Deleted files can often come back with their names too, from FAT, exFAT and NTFS: [Recover deleted files](/docs/guides/recover-deleted-files/).

Rescue only ever reads the drive. Whatever it saves goes to another drive: it refuses to write the image or the files it finds onto the drive being rescued.

> [!WARNING]
> If a drive is failing, every read counts. Copy it into an image first, then look for files and partitions in the image, not on the drive.

BackDown can tell you a drive is failing before it stops: `backdown health` and the browser show what each drive reports about itself ([drive health](/docs/guides/drive-health/)), and a failing drive gets a banner with **Copy it now**, which opens Rescue set up to copy it.

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

### Allow BackDown to read this drive

On Linux, the Rescue view has a third way: **Allow BackDown to read this drive**. BackDown asks udisks, the system's disk service, to open the drive for it read-only, and the system shows its own password dialog. Nothing of BackDown's runs as root, and the drive can't be changed through what it's given. It lasts until the background service stops; if the dialog is closed or the system refuses, the terminal command above still works. If the drive is in use (mounted, say), udisks may refuse to open it this way.

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

It never changes the partition table. To get files out of a lost partition:

- In the browser, a FAT, exFAT, ext or NTFS file system that was found gets **Browse files**: its folders and files as they are, read-only, with its deleted files if you ask, and copying out to another drive ([Recover deleted files](/docs/guides/recover-deleted-files/)).
- For any file system, "Find files in it" in the browser, or `rescue find` on just that range (its start, and its start plus its size), finds files by their contents.

## In the browser

The **Rescue** view asks three questions:

1. **Which drive or card?** Every disk and partition, or an image file. It checks BackDown can read it and, if it can't, shows the permission command with a Copy button and "Check again" (and, on Linux, **Allow BackDown to read this drive**).
2. **What happened?** "It's failing", "Files were deleted, or it was formatted", or "It looks empty, or a partition vanished".
3. **Where should it go?** A folder on another drive (it suggests `BackDown Rescue/<drive> <date>` on one of your indexed drives), and what to look for.

Rescues run in the background service beside scans and hashing, show their progress (a copy shows a map of what's been read), can be stopped and resumed, and stay on the list afterwards. From a finished copy you can go straight to "Find files in the copy" or "Look for partitions in the copy", and from a partition that was found to "Browse files" or "Find files in it". Rescues need the background service; the machine-wide service doesn't do them, so run `backdown rescue` from your own account.

A copy that's an image file on an indexed drive also opens in the browser, where the files on its FAT, exFAT, ext and NTFS partitions can be browsed; for other file systems (XFS, btrfs, HFS+, APFS), the image can be mounted read-only by the system instead ([disk images](/docs/guides/browse-without-restoring/#disk-images)).

## Not yet

Finding files by content can't put a file stored in scattered pieces back together. Partitions Rescue finds with file systems BackDown doesn't read itself (XFS, btrfs, HFS+, APFS and others) can only be searched by content, not browsed or mounted. Browsing, undeleting and copying out are in the browser only, not in `backdown rescue`.
