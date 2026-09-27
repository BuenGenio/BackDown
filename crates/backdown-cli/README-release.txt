BackDown command-line preview
=============================

Backup that thinks: find out what's on your drives, what's duplicated, and
which drives are safe to wipe, with a SHA-256 hash behind every claim.
https://backdown.io

This is an early preview of the `backdown` command-line tool. It only ever
reads your drives; the only file it writes is its index database.


Quick start
-----------

  1. Index the drives you care about (each one is a separate scan root):

       backdown index /media/old-drive /media/archive

  2. Is the old drive safe to wipe? Every file needs a SHA-256-identical copy
     somewhere else in the index:

       backdown check /media/old-drive
       backdown check /media/old-drive --list      # every file with no copy

     Exit code 0 = safe to wipe, 2 = copy the listed files first.

  3. What's duplicated, largest reclaimable space first:

       backdown dupes --min-size 10M

  On Windows use drive paths, e.g.  backdown index E:\ F:\Archive


Commands
--------

  index PATH...   record every file under PATH as a new scan (re-run to update;
                  unchanged files keep their hashes, history is kept)
  hash            hash only what could be a duplicate: size, then a
                  head/middle/tail sample, then full SHA-256
                  (dupes and check run this for you)
  dupes           duplicated content and the space it wastes
  check PATH      is PATH safe to wipe?
  scans           the scans on record, oldest first
  status          what's in the index

  --db FILE       index database (default: backdown.db in the current folder,
                  or $BACKDOWN_DB)
  --jobs N        readers per drive for hashing (default 1, right for spinning
                  disks; raise it for SSDs)

Run `backdown help <command>` for details.


Notes for this preview
----------------------

- These builds are not signed yet.
  macOS:   run  xattr -d com.apple.quarantine backdown  once after unpacking,
           or allow it in System Settings > Privacy & Security.
  Windows: SmartScreen may ask for confirmation ("More info" > "Run anyway").
- Hardlinks are recognised on Linux and macOS. On Windows each path counts
  as its own file for now.
- Provided as-is, without warranty, for evaluation. Verify before you wipe.
