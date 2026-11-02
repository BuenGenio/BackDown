---
title: Concepts
description: Drives and scans, fast rescans, the time slider, hashes and proof, what "safe to wipe" means, copies across file systems, recognised things and where they came from, the background service, and drive health.
order: 2
group: start
---

The ideas the rest of the docs rely on. Each section says what BackDown does, and what it doesn't.

## Drives and roots

A *root* is a drive, or a top-level folder, that you've indexed: `/media/old-drive`, `/mnt/nas`, `E:\`. BackDown calls it a drive and names it after the last part of its path (`old-drive`). Roots can't be nested inside each other: index each drive or top-level folder once.

BackDown knows a root by its path. Plug a drive back in where it was mounted before and BackDown carries on with it; mounted somewhere else, it's a new root. A root whose folder is missing, or empty although it held files, counts as not connected: its files aren't read, and they wait for it.

Indexing stays on one file system unless you pass `--cross-filesystems`, so a drive's scan doesn't wander into other drives mounted inside it. Symbolic links are neither followed nor recorded.

`backdown volumes` lists every disk and partition on the computer (model, serial number, file system, label, where it's mounted), says what BackDown can do with each (mountable, mounted, system, encrypted, part of a volume group, swap, empty), and marks the ones the index covers. It only reads, and needs no administrator rights. Activity's "Connected drives" shows the same, with **Index this** on each mounted one.

### Mounting drives read-only {#mounting-drives-read-only}

A drive has to be mounted, as a folder, before it can be indexed. BackDown can mount one for you, always read-only and always as you, through the system's own tools:

```sh
backdown mount /dev/sdb1                           # a partition volumes calls mountable
backdown mount /media/archive/Images/old-laptop.img  # a disk image
backdown mount                                     # what BackDown has mounted
backdown unmount /dev/sdb1
```

| System | Partitions | Disk images |
|---|---|---|
| Linux | udisks (`udisksctl mount -o ro`), for file systems this computer has a driver or helper for | attached read-only as a loop device, then each partition mounted read-only |
| macOS | `diskutil mount readOnly` | `hdiutil attach -readonly` |
| Windows | not offered: Windows can't mount a drive read-only in general, and BackDown reads drives where they are | ISO, VHD and VHDX, with `Mount-DiskImage -Access ReadOnly` |

On Linux, BackDown checks the mount really is read-only and undoes it if not. It doesn't mount the running system's partitions, encrypted volumes that are still locked (unlock them in your file manager first; the unlocked volume can then be mounted), members of LVM or RAID sets, swap, or partitions with nothing recognisable on them. Where your system wants an administrator's password to mount, BackDown says so and shows the command, which asks for it itself. `unmount` only unmounts what BackDown mounted; detaching an image unmounts everything that was mounted from it. In the browser, Activity's "Connected drives" has **Mount read-only** and **Unmount**, and a disk image whose file systems BackDown can't read itself (XFS, btrfs, HFS+, APFS and others) offers **Mount read-only** too.

## Scans and the time slider

Every `backdown index` run records a *scan* of each root: every file's path, size and modification time, and on Linux and macOS its device and inode, so hardlinks are recognised. The files' contents aren't read while scanning.

When you index a drive again:

- a file with the same path, size and modification time is unchanged, and keeps everything known about it, hashes included;
- a changed file gets a new version, and the old one is kept;
- a file that's gone is marked gone, and its last version is kept.

A renamed or moved file counts as gone at the old path and new at the new one. A scan that's interrupted doesn't count; run the command again.

Because every version is kept, the browser can show any folder as it was at any scan: that's the *time slider* at the bottom of a drive with two scans or more. An old version can be opened while its exact bytes still exist somewhere indexed, either the file itself, unchanged, or an identical copy (same SHA-256) on an indexed drive. Otherwise the index still remembers its size, dates and hash, but the bytes are gone. Hashing after each scan keeps that fingerprint; the background service does it for you.

If a drive that held files is suddenly empty, `index` refuses to record it: usually the drive isn't plugged in and you're looking at its empty mount point. If it really was emptied, pass `--allow-empty`.

### Fast rescans

A scan doesn't always have to open every folder. There are three ways to make one:

| Way | What it reads | Where |
|---|---|---|
| **Walk** | every folder | everywhere; always the first scan of a drive |
| **Change log** | only the files and folders the drive's own record of changes names since the last scan | Windows: the USN journal of NTFS drives, without administrator rights. macOS: the change history (FSEvents) of APFS and HFS+ drives |
| **File table** | NTFS's master file table, from start to end, instead of folder by folder | Windows, when BackDown runs as administrator. Linux, for NTFS drives mounted with ntfs-3g, when your account can read the drive's device |

An unchanged drive with millions of files is rescanned from its change log in seconds. Linux keeps no change log for drives, so rescans there walk, or read an NTFS drive's file table.

Whichever way a scan is made, it records exactly what walking every folder would, because proof needs every file. BackDown walks instead whenever there's any doubt: the change log was reset, lost its oldest entries, belongs to another drive, left something out, or says anything that doesn't add up. And it walks anyway every tenth scan, or when the last walk is 30 days old, which catches changes made where nothing was recording them (the drive plugged into another computer). `index` prints how each drive was scanned and why; Activity shows it under each drive's last scan. *Settings > Scanning* turns the change log and file table off, or changes how often to walk anyway. `backdown index --method walk` always walks, and `--method journal` or `--method table` use that way or fail, saying why ([when a rescan walks](/docs/troubleshooting/#when-a-rescan-walks)).

## Hashes and proof

Every claim BackDown makes about copies rests on SHA-256: two files are the same when their SHA-256 hashes are the same. Hashing everything would mean reading every byte on every drive, so hashing goes in three tiers:

1. **Size.** A file whose size no other file shares can't have a duplicate, and is never read.
2. **Sample.** For the rest, a quick fingerprint: the size, plus 64 KiB from the start, the middle and the end of the file. Files up to 192 KiB are read whole here, which gives their full hash straight away.
3. **Full SHA-256.** Only files whose sizes and samples still match another file's are read completely.

Hardlinks are one file: they're read once, and never count as each other's copy (on Linux and macOS; see [below](#copies-across-file-systems) for Windows). Hashing runs one reader per drive (`--jobs 1`, right for spinning disks; raise it for SSDs, or in Settings > Hashing). Files on drives that aren't connected wait for them. A file that can't be read is marked unreadable and skipped until `backdown hash --retry-errors`; one that changed since it was indexed asks you to index again.

`dupes`, `check` and the background service run `hash` for you. The hash is SHA-256 throughout.

### Signed manifests {#signed-manifests}

The same hashes let BackDown sign a statement of exactly what a drive held at a scan: a *manifest*, every file's path, size and hash sealed into one Merkle root and signed with your installation's own key, chained to the drive's previous manifest. Anyone can check one wasn't changed, on any computer, with `backdown manifest verify` or without BackDown at all. Files that weren't hashed are attested by their path and size only. The background service can sign one of each drive after every scan, or daily or weekly (*Settings > Attestations*). [Signed manifests and attestations](/docs/guides/manifests-and-attestations/) explains what they prove and the exact format.

## What "safe to wipe" means {#what-safe-to-wipe-means}

`backdown check PATH` says a drive (or a folder) is **safe to wipe** when every file in it has an identical copy outside it:

- another file with the same full SHA-256,
- outside PATH (a copy inside the drive you want to wipe doesn't count),
- and not just a hardlink to the same file.

Files that couldn't be read block the verdict, because nobody knows what's in them. Empty files don't count against a drive. A file without a full hash counts as unique. `check` hashes everything that could have a copy first, so that only happens when no other file could be identical, or when its look-alikes are on drives that have never been connected while hashing.

The verdict is about the index. A copy on a drive that isn't plugged in still counts, and a copy that went missing since its drive was last scanned would still count too. That's why [freeing up a drive](/docs/guides/free-up-a-drive/) doesn't trust the verdict alone: before deleting a file, it reads the file and its copy again and compares them.

A copy on [another machine](/docs/guides/several-machines/) counts too, more strictly: only by SHA-256, only if that machine confirmed it within the last 7 days (a setting), and, before freeing up relies on it, only after that machine has read it again. `check` says how much of the verified part is only on another machine.

`check` exits with 0 when the path is safe to wipe and 2 when files must be copied first, and lists the folders holding what exists only there (`--list` names the largest 20,000 files).

## Copies across file systems {#copies-across-file-systems}

Copying into an archive, to a mirror, or out of a rescue moves files between drives that don't all follow the same rules. BackDown copies the bytes, checks them, and says what it had to change:

- **Names that would clash.** Before copying, BackDown tests what the destination treats as the same name (`Photo.jpg` and `photo.jpg` on a Mac or Windows drive, or two Unicode spellings of `café`) and which names it can't hold at all (on Windows, FAT and exFAT: `:`, `?`, `*` and the like, trailing dots and spaces, names such as `CON` or `NUL`). The first name keeps its spelling and the others become `photo (2).jpg`; characters the drive can't hold become `-`, and reserved names get `_` in front. Every renaming is written to the journal, and `apply` lists them. Freeing up with `--move-to` keeps names as they are: a file that would clash with a different one is left in place.
- **What else a file carries.** On a Mac, extended attributes (resource forks, Finder info, tags) are copied and checked; a drive that can't hold them gets `._` files. On Windows, NTFS alternate data streams are copied and read back, except the "downloaded from the internet" mark (`Zone.Identifier`), which is left behind on purpose. Linux's extended attributes aren't carried, and neither are permissions, owners or ACLs anywhere: a copy gets the original's modification time. When something couldn't be kept, the journal says what, and freeing up with `--move-to` leaves that file where it is.
- **Streams count for proof.** On Windows, BackDown records each file's alternate data streams when it indexes (their names and sizes; they aren't part of the file's size or hash), shows them in the details pane, and only counts a copy as proof for freeing up if it has the same ones.
- **Long paths.** On Windows, paths longer than 260 characters, and names ending in a dot or a space, are read as they are.
- **As administrator.** Run as administrator on Windows, BackDown can read (never write) files your account otherwise couldn't, such as other users' folders on an old drive.
- **Online-only files.** On Windows, cloud placeholders (OneDrive and the like, whose contents aren't on the drive) are never read, because reading one would download it onto the drive. They're indexed, count as unreadable, and so keep a drive from being "safe to wipe". On a Mac, iCloud Drive's online-only ("dataless") files are left alone the same way.
- **Hardlinks on Windows** count as separate files: `dupes` counts the space they share as reclaimable.

Copies are made the fastest way the destination allows: on btrfs, XFS and APFS as clones that share the source's blocks until one changes, on Linux otherwise by the kernel, and elsewhere by reading and writing. Every copy is then read back and compared with the source's SHA-256 before it counts; on Linux BackDown first asks the system to forget the copy it has in memory, so the read comes from the disk (elsewhere it may come from the system's cache).

## Recognised things and provenance {#recognised-things-and-provenance}

BackDown recognises what things are, from the names and layout of files and folders: mail stores and Maildir mailboxes, Dovecot mdbox and sdbox stores, Thunderbird profiles, Nextcloud data, Time Machine backups, iPhone and Android backups, operating systems, Mac, Linux and Windows home folders, websites, git repositories, database dumps, disk images, photo, music and video libraries, data exports (Google Takeout, Facebook, Instagram, Twitter, LinkedIn), downloads piles and recovered files. Each comes with its size, the years its files span and, where it's less than sure, how sure it is.

Some are looked at more closely, still without reading anyone's messages:

- **Dovecot mdbox and sdbox:** the mailboxes and their names and, with the drive connected, message counts from Dovecot's index headers and the oldest and newest message's dates.
- **Thunderbird profiles:** which folders are copies of server mail (IMAP) and which are kept only here (Local Folders, POP), each mailbox file with its size and, with the drive connected, the accounts, addresses and servers from `prefs.js`.
- **Databases:** dumps (`mysqldump` and MariaDB, phpMyAdmin, `pg_dump` and `pg_dumpall`, SQLite's `.dump`; a folder of three or more is one thing), mongodump folders (`dump/<database>/<collection>.bson`), SQLite files (by their first bytes when the drive is connected; a program's own `.db` files inside a profile or home folder don't count), and the data directories of MySQL or MariaDB, PostgreSQL and MongoDB servers, with the databases and tables in them. A dump's header names its engine, database and host.
- **Websites and Nextcloud servers:** with the drive connected, `wp-config.php`, Joomla's `configuration.php`, Laravel's `.env` and Nextcloud's `config/config.php` say which database each uses, at which address it was served, and (Joomla, Laravel) its contact address.
- **Time Machine:** HFS+ backup sets (`Backups.backupdb`), APFS backups from macOS 11 on (the dated snapshot folders), and network backup bundles, with the computer, the number of snapshots and the span. BackDown doesn't mount snapshots or open the bundles.

Folders that programs can make again are flagged as *regenerable*: caches, thumbnails, `node_modules`, build output, virtual environments, trash and the like (Settings > Junk rules lists them, and takes your own). They still count: a drive is only safe to wipe when they have copies too. A filing plan can leave them out, and then they stay on the old drive when you free it up.

It also works out where things came from, and when, by reading small files on connected drives:

| Source | What it says |
|---|---|
| Backup scripts (`rsync`, `scp`, `rclone`, `cp -a`, `robocopy`, `tar`) | Copied from which machine, with which tool, from which script |
| iPhone backups' `Info.plist` | Backed up from which device, which model, on which day |
| Time Machine folder names and records | Backed up from which computer, last on which day |
| `/etc/hostname`, `/etc/os-release` | Which machine a system disk was, which system |
| git configuration | A clone of which remote, on which branch |
| GNU ddrescue map files | Rescued from which device |
| Database dump headers | Dumped from which host, on which day |
| Sync markers | Kept in sync by Syncthing, Resilio Sync, Dropbox, Nextcloud or ownCloud |

Only small files are read (scripts up to 256 KiB, plists up to 8 MiB), and only on drives that are connected. `backdown entities` lists what's recognised; the background service recognises each drive again after it's been scanned and hashed.

## People and databases {#people-and-databases}

Things are places; people and databases are what they're about, and they span drives. From what's recognised, BackDown gathers:

- **People:** everyone with a mailbox in a mail server's store (`<domain>/<user>` becomes the address), a Maildir of their own, a Nextcloud account (the folder with `files/` in it; the server's `mail_domain` gives the address), a home folder in a system, an identity in a Thunderbird profile (`prefs.js`), or who a site's configuration names as its contact. The same address, or the same login on the same host, on any drive is one person: `backdown people` and the browser's What's here > People show each with their mailboxes (and how many messages), accounts (and how much), homes and identities, on which drives.
- **Databases:** each one with every dump of it, its live data directory, its SQLite file, and the sites and servers configured to use it, under one name per engine family (a MariaDB dump and a MySQL data directory of `shop` are the same database): `backdown databases`, What's here > Databases.

Nothing is read from inside messages or database files for this; only paths, dump headers and the configuration files above, and only on connected drives. Both are gathered right after recognition (so after every scan, in the background), stored per drive and merged when shown.

## Searching inside things {#searching-inside-things}

The search box finds files by name. Its third scope, **Inside**, and `backdown search` find what's in them: mail by subject, sender and recipient; database dumps and SQLite files by table, column and, at full depth, the text in their rows. The hits say where to open: the mailbox folder with the message selected, the dump, the file.

BackDown reuses the indexes already there before reading anything itself:

- **Dovecot's index.** A Dovecot mailbox keeps the headers it cached for IMAP clients in `dovecot.index.cache`. When that file matches the mailbox's `dovecot.index`, the subjects, senders, recipients and dates come from it and no message is opened; a mailbox of a million messages is a few hundred megabytes read once. Messages the cache doesn't cover are read from their files, headers only. Where a Dovecot full-text plugin left its own index (`fts-flatcurve`, `fts-xapian`, `fts-lucene`, Squat), BackDown notes it but can't read it.
- **SQLite's full-text tables.** A SQLite file that holds FTS3, FTS4 or FTS5 tables is searched in place, through SQLite itself: the hit says how many rows match in which table.
- **What a dump says it indexed.** A table's `FULLTEXT KEY` in a MySQL or MariaDB dump names the columns the database itself indexed for search; they're listed with the table. (The engine's own index files in a data directory aren't readable outside the server.)

Depth is a setting (`backdown search --depth off|headers|full`, `headers` by default): headers and schemas are enough to find a message or a table and cost little; `full` indexes the text of rows (text-typed columns only, up to 64 MiB a file) and grows with the data. The index keeps no copy of the text, only where each hit is, so a hit shows the subject, who and when, not a snippet. The background service indexes after each recognition, only what was scanned since; `backdown search --build` does it now.

## Overlaps

`backdown overlaps` shows how much of each drive, top-level folder and recognised thing already exists elsewhere, and where most of it is: "89% elsewhere: 89% on archive". It's the same SHA-256 test as `check`, summed up per place. The report and the [filing plan](/docs/guides/file-into-an-archive/) use it to find what's already archived.

## The background service {#the-daemon}

`backdown daemon` is BackDown running in the background; the app starts it, and `backdown service install` keeps it running. It:

- rescans each drive when it's due (every 24 hours unless Settings says otherwise; `--rescan-every 12h`, or `never`), and as soon as a drive that was away comes back;
- hashes after every scan, then recognises what changed;
- serves the browser on 127.0.0.1 (port 8787), with a private link that `backdown open` and `backdown app` open;
- runs one job at a time, so the index has one writer and each drive one reader; rescues and syncs with [other machines](/docs/guides/several-machines/) run beside the other jobs;
- reads every drive's [health](/docs/guides/drive-health/) every six hours and tells you when one gets worse;
- keeps each mirror in sync and checked, and, if you turned it on, looks for updates once a day.

It's stopped safely at any point: jobs commit as they go and carry on where they stopped.

`backdown service install` runs it for you, as you, with no extra rights: a systemd user service on Linux, a LaunchAgent on macOS, and a scheduled task that starts it when you log on on Windows. On Linux, `sudo backdown service install --system` runs it for the whole machine instead (a NAS or a server): as a `backdown` user that can read every file and write none, sandboxed by systemd, with no network beyond this computer. Members of the `backdown` group can use it. The machine-wide service only reads: it never wipes, rescues, tidies or mounts, and doesn't reach other machines.

While a daemon is running, it owns the index. `index`, `hash`, `dupes`, `check`, `scans`, `status`, `health`, `remote sync` and `apply --go` are handed to it, and your terminal follows the progress; `wipe`, `dedup` and `mail merge` check first, then hand the work to it. `--local` makes a command use the index directly. On Windows, terminal commands always use the index directly: the browser is how you talk to the service there.

## The swarm {#the-swarm}

The machines this BackDown reaches over ssh, plus itself, work as one: each machine's drives and free space, where a file is across all of them, and pool folders on any of them that drives are filed into, one copy of each file. There's no coordinator and nothing new listens on the network; each machine's index stays the authority on its own files. [The swarm and the pool](/docs/guides/the-swarm/) explains it; [Several machines](/docs/guides/several-machines/) the ssh side.

## Drive health {#drive-health}

Drives say how they're doing, through SMART: sectors they've had to replace or can't read, cable errors, wear on SSDs, temperature, and their own verdict. BackDown reads what each physical drive will tell an ordinary account: through udisks on Linux (and `smartctl` where your account may use it), `diskutil` (and `smartctl` if it's installed) on macOS, and Windows' storage cmdlets, which share their counters only with an administrator. A drive's verdict is **ok**, **warning**, **failing** or **unknown** (no health data).

Readings are kept in the index under the drive's serial number, so its history follows it from port to port. `backdown health` shows every drive and exits with 2 when a connected one is failing; the background service reads them every six hours and when a drive appears, and tells you when one gets worse. [Drive health](/docs/guides/drive-health/) has the details, including what each system can and can't read.
