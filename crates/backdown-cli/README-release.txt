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

  4. Browse it all in your web browser: folders as they were at any scan
     (time slider), every version and copy of a file, and read-only viewers
     for PDF, Word, SQLite, SQL dumps, code, images, audio/video, git history
     and diffs. It runs on this computer only and prints a private link:

       backdown serve

  5. Or keep it running in the background (Linux, systemd): it rescans your
     drives daily and whenever one is plugged back in, hashes after every
     scan, and keeps the browser available:

       backdown service install        # a user service, no root needed
       backdown open                   # the browser, any time
       backdown service status

     For a machine-wide service (NAS, server), install the binary in
     /usr/local/bin and run `sudo backdown service install --system`: it runs
     as a `backdown` user that can read every file but write none, sandboxed
     by systemd with no network beyond this machine.

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
  serve           the file browser (local web UI on 127.0.0.1, read-only)
  daemon          run in the background (usually via `service install`)
  service         install | uninstall | status the systemd service
  open            open the running daemon's browser
  status          what's in the index

  --db FILE       index database (default: ~/.local/share/backdown/index.db on
                  Linux, ~/Library/Application Support/BackDown on macOS,
                  %LOCALAPPDATA%\BackDown on Windows; or $BACKDOWN_DB)
  --local         work on the index directly even if a daemon is running
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
- The index moved: 0.2.0-alpha.1/2 kept backdown.db in the current folder.
  Keep using it with --db backdown.db, or index again.
- An index refuses to record a drive that is suddenly empty after holding
  files (usually an unplugged drive's leftover mount point); pass
  --allow-empty if it really was emptied.
- The time slider can open an old version only while its exact content
  still exists somewhere indexed; hash after each index (`backdown hash`,
  or `check`/`dupes`) so old versions keep a provable fingerprint.
- Provided as-is, without warranty, for evaluation. Verify before you wipe.
- Third-party licenses: THIRD-PARTY-NOTICES.txt.
