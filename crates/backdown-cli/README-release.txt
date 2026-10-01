BackDown 1.0 beta
=================

Backup that thinks: find out what's on your drives, what's duplicated, where
it came from and which drives are safe to wipe, with a SHA-256 hash behind
every claim. Then file everything into one archive, free the old drives and
rescue failing ones.
https://backdown.io  ·  documentation: https://backdown.io/docs/

Scanning only ever reads your drives. BackDown writes elsewhere only when you
ask it to: copying into your archive, freeing up a drive (and then only files
with a verified copy elsewhere), or saving what Rescue found. It makes no
network requests unless you ask it to.


Quick start
-----------

  The installers (.deb, AppImage, .dmg, .msi) put BackDown in your
  applications menu. From this archive, put `backdown` on your path, then:

       backdown app

  It opens in a window of its own (in Chrome, Chromium, Edge or Brave when
  one is installed, else your browser), starts the background service, and
  shows the drives you can index. `backdown desktop install` adds it to your
  applications menu, ~/Applications (macOS) or the Start menu (Windows).

  The same from a terminal:

  1. Index the drives you care about (each one is a separate scan root):

       backdown index /media/old-drive /media/archive

  2. Is the old drive safe to wipe? Every file needs a SHA-256-identical copy
     somewhere else in the index:

       backdown check /media/old-drive
       backdown check /media/old-drive --list      # the files with no copy

     Exit code 0 = safe to wipe, 2 = copy the listed files first.

  3. What's duplicated, largest reclaimable space first; what things are and
     where they came from; the whole picture as a document:

       backdown dupes --min-size 10M
       backdown entities
       backdown report --html -o report.html

  4. Browse it all: folders as they were at any scan, every version and copy
     of a file, mailboxes as conversations, disk images without mounting, and
     read-only viewers for PDFs, office documents, databases and git history:

       backdown serve

  5. Keep it running in the background (systemd on Linux, a login item on
     macOS, a task at logon on Windows): it rescans drives when they're due
     or plugged back in, hashes after every scan, and watches drive health:

       backdown service install
       backdown open                   # the browser, any time

     For a machine-wide service on Linux (NAS, server): install the binary in
     /usr/local/bin and run `sudo backdown service install --system`. It runs
     as a `backdown` user that can read every file but write none.

  On Windows use drive paths, e.g.  backdown index E:\ F:\Archive


Commands
--------

  index, hash, dupes, check   scan, fingerprint, find duplicates, safe to wipe?
  scans, status, volumes      what's on record, and every disk on this machine
  entities, overlaps, report  what things are, what exists elsewhere, the lot
  plan, plans, apply          file everything into one archive, verified and
                              journaled (apply never deletes anything)
  mirror                      keep a second, checked copy of the archive
  wipe                        free drives: delete only what has a verified
                              copy elsewhere (without --go, only shows)
  dedup, mail                 tidy the archive; merge copies of a mailbox
  rescue                      copy a failing drive, find deleted files and
                              lost partitions (only reads the drive)
  health                      SMART readings and warnings, without root
  mount, unmount              read-only mounts and disk images
  remote                      other machines over ssh (Pro)
  serve, open, app, daemon,   the browser, the app and the background service
  service, desktop, token     (token: its browser link, e.g. for another device)
  update                      a newer BackDown, verified against SHA256SUMS.txt
  license                     your BackDown Pro licence

  --db FILE       index database (default: ~/.local/share/backdown/index.db on
                  Linux, ~/Library/Application Support/BackDown on macOS,
                  %LOCALAPPDATA%\BackDown on Windows; or $BACKDOWN_DB)
  --local         work on the index directly even if a daemon is running

Run `backdown help <command>` for details.


Notes for the beta
------------------

- These builds are not signed yet.
  macOS:   right-click BackDown.app and choose Open the first time; for the
           command-line binary, run  xattr -d com.apple.quarantine backdown
  Windows: SmartScreen may ask for confirmation ("More info" > "Run anyway").
- Tested on Linux; the macOS and Windows builds are checked but haven't yet
  run on those systems. Please report what doesn't work:
  https://github.com/BuenGenio/backdown-releases/issues
- Pro features are free during the beta.
- Hardlinks are recognised on Linux and macOS. On Windows each path counts
  as its own file for now.
- An index refuses to record a drive that is suddenly empty after holding
  files (usually an unplugged drive's leftover mount point); pass
  --allow-empty if it really was emptied.
- Provided as-is, without warranty, for evaluation. Verify before you wipe.
- Third-party licences: THIRD-PARTY-NOTICES.txt.
