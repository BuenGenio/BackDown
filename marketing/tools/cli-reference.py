#!/usr/bin/env python3
"""Write the docs' CLI reference (docs/cli/*.md) from the real binary's own help.

    python3 marketing/tools/cli-reference.py [--bin PATH] [--released TAG] [--check]

Runs `backdown --help`, then `backdown <command> --help` (and `backdown <command> <subcommand> --help`) for every
command clap lists, in a clean environment so no local path or $BACKDOWN_DB can leak into the pages, and writes
docs/cli/index.md (global options, every command, exit codes) plus docs/cli/backdown-<command>.md for each command.

Commands and options that the released version doesn't have are marked "in the next preview". The released
version is a git tag (default: the version downloads.json offers); a command counts as released when its variant is
in that tag's `enum Cmd`, an option when its field name appears in that tag's source. At release time: build the
binary, update downloads.json, run this again (nothing is marked then), then `python3 build.py`.

Clap doesn't know everything a reader needs, so EXAMPLES, ARGS (for arguments without help text), NOTES and SEE_ALSO
below add examples, argument descriptions, caveats and links. Keep them true to the code.

--check: exit 1 if the generated pages differ from what's on disk (for CI), writing nothing.
Standard library only.
"""
import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path

MARKETING = Path(__file__).resolve().parent.parent
REPO = MARKETING.parent
OUT = MARKETING / 'docs' / 'cli'
MAIN_RS = 'crates/backdown-cli/src/main.rs'
RESCUE_RS = 'crates/backdown-cli/src/rescuing.rs'

# ---------- what clap can't say (keep true to the code)

ARGS = {
    ('index', '<PATHS>...'): 'The drives or folders to index, each as its own root. Roots can’t be nested: index each '
                             'drive or top-level folder once.',
    ('check', '<PATH>'): 'The drive or folder to check. It must be there now (connected and mounted).',
    ('apply', '<PLAN>'): 'The plan’s number, as `backdown plan` and `backdown plans` show it.',
    ('wipe', '<PATHS>...'): 'Drives, or folders on them, to free. Each must be on an indexed drive.',
    ('mirror sync', '[ID]'): 'A mirror’s number from `backdown mirror list`. Without it, every mirror.',
    ('mirror verify', '[ID]'): 'A mirror’s number from `backdown mirror list`. Without it, every mirror.',
    ('mirror remove', '<ID>'): 'A mirror’s number from `backdown mirror list`.',
    ('mirror add', '<FOLDER>'): 'The folder to keep copied, usually part of the archive.',
    ('mirror add', '<MIRROR>'): 'Where the copy goes: a folder on another drive. Neither folder may be inside the other.',
    ('dedup', '<FOLDER>'): 'The folder to tidy, usually the archive. Only files inside it, on the same drive, are linked.',
    ('mail merge', '<SOURCES>...'): 'The copies of the mailbox: Maildirs, mbox files, Thunderbird account folders, or whole mail stores '
                                   '(folders of Maildirs), in any order. They are only read.',
    ('mail export', '<MAILDIR>'): 'The Maildir folder to export (its own messages, not its subfolders).',
    ('remote sync', '[NAME]'): 'A machine’s name from `backdown remote list`. Without it, every machine.',
    ('remote test', '<NAME>'): 'A machine’s name from `backdown remote list`.',
    ('remote remove', '<NAME>'): 'A machine’s name from `backdown remote list`.',
}

# options clap has no help text for
FALLBACK = {'--json': 'Print it as JSON, for scripts.'}

EXAMPLES = {
    'index': ['backdown index /media/old-drive /media/archive',
              '# a NAS share with other file systems mounted inside it',
              'backdown index /mnt/nas --cross-filesystems',
              '# Windows',
              'backdown index E:\\ F:\\Archive'],
    'hash': ['backdown hash', '# an SSD: two readers per device', 'backdown hash --jobs 2',
             '# try the files that couldn’t be read last time again', 'backdown hash --retry-errors'],
    'dupes': ['backdown dupes', 'backdown dupes --min-size 100M --limit 50', 'backdown dupes --json > dupes.json'],
    'check': ['backdown check /media/old-drive', '# every file that has no copy, largest first',
              'backdown check /media/old-drive --list',
              '# in a script: 0 = safe to wipe, 2 = copy first',
              'backdown check /media/old-drive && echo "safe to wipe"'],
    'volumes': ['backdown volumes', 'backdown volumes --json'],
    'scans': ['backdown scans'],
    'serve': ['backdown serve', '# another port, and just print the link', 'backdown serve --port 8788 --no-open',
              '# also look from your other devices, at this computer’s Tailscale address', 'backdown serve --host 100.x.y.z'],
    'status': ['backdown status', 'backdown status --json'],
    'daemon': ['# usually started by `backdown service install`; by hand:', 'backdown daemon',
               'backdown daemon --rescan-every 12h', '# scan only when asked, or when a drive comes back',
               'backdown daemon --rescan-every never'],
    'open': ['backdown open'],
    'service': ['backdown service install', 'backdown service status', '# see what it would install first',
                'backdown service install --print', '# Linux: also look from your other devices over Tailscale',
                'backdown service install --host 100.x.y.z', '# machine-wide, on Linux (NAS, server)',
                'sudo backdown service install --system', 'backdown service uninstall'],
    'health': ['backdown health', '# in a script: 2 = a connected drive is failing',
               'backdown health --json > health.json'],
    'entities': ['backdown entities', 'backdown entities /media/old-drive --kind mail-store',
                 'backdown entities --refresh --json > entities.json'],
    'overlaps': ['backdown overlaps', '# only what is at least 90% elsewhere', 'backdown overlaps --min 90%'],
    'report': ['backdown report > report.md', 'backdown report --html -o report.html'],
    'plan': ['backdown plan --to /media/archive/Archive', '# only one drive', 'backdown plan --to /media/archive/Archive --from /media/old-drive',
             '# your own schema', 'backdown plan --print-schema > schema.toml',
             'backdown plan --to /media/archive/Archive --schema schema.toml'],
    'plans': ['backdown plans'],
    'apply': ['# what plan 3 would do (writes nothing)', 'backdown apply 3', 'backdown apply 3 --go'],
    'rescue': ['backdown rescue image /dev/sdc /media/archive/Rescue/sdc.img',
               'backdown rescue find /media/archive/Rescue/sdc.img --to /media/archive/Rescue/sdc-files --only photos,documents',
               'backdown rescue partitions /dev/sdc',
               '# when your account can’t read the drive',
               'sudo backdown rescue partitions /dev/sdc'],
    'mirror': ['backdown mirror add /media/archive/Archive/Mail /media/backup/Mail', 'backdown mirror list',
               'backdown mirror sync', 'backdown mirror verify 1 --all', 'backdown mirror remove 1'],
    'wipe': ['# what would happen (changes nothing)', 'backdown wipe /media/old-drive',
             '# move what exists only there first, then wipe',
             'backdown wipe /media/old-drive --move-to /media/archive/Unsorted --go --confirm "old-drive"'],
    'license': ['backdown license add BD1.eyJ2Ijox…', '# or from a file or the clipboard, on stdin',
                'backdown license add - < licence.txt', 'backdown license show',
                '# what is free, what is Pro, and what this copy may use', 'backdown license features'],
    'app': ['backdown app', '# only make sure the background service runs (for starting at login)', 'backdown app --no-window',
            '# a particular Chromium-family browser', 'backdown app --browser /usr/bin/chromium'],
    'desktop': ['backdown desktop install', '# Linux and Windows: also start the background service when you log in',
                'backdown desktop install --autostart', 'backdown desktop uninstall'],
    'update': ['# is there a newer version? (exits 10 when there is)', 'backdown update --check',
               '# download it, check it against SHA256SUMS.txt and install it', 'backdown update',
               '# put back the version the last update replaced', 'backdown update --rollback'],
    'mount': ['# a partition `backdown volumes` calls mountable', 'backdown mount /dev/sdb1',
              '# a disk image: attached read-only, and what is on it mounted', 'backdown mount /media/archive/Images/old-laptop.img',
              '# what BackDown has mounted', 'backdown mount'],
    'unmount': ['backdown unmount /dev/sdb1', 'backdown unmount /media/archive/Images/old-laptop.img'],
    'remote': ['backdown remote add nas ssh://you@nas.example.org', '# BackDown isn\'t on the PATH ssh sessions get there',
               'backdown remote add office ssh://office-pc --backdown /opt/backdown/backdown',
               'backdown remote test nas', 'backdown remote sync', 'backdown remote list',
               '# copies there count for 14 days after they were last confirmed there',
               'backdown remote settings --proof-days 14', 'backdown remote remove nas'],
    'dedup': ['# what would be linked (changes nothing)', 'backdown dedup /media/archive/Archive',
              '# on btrfs, XFS or APFS: separate files that share their blocks',
              'backdown dedup /media/archive/Archive --link clone --go'],
    'mail': ['# what the merge would be (writes nothing)',
             'backdown mail merge /media/old-drive/Mail/Inbox /media/laptop-2012/Mail/Inbox --to /media/archive/Mail/merged --dry-run',
             'backdown mail merge /media/old-drive/Mail/Inbox /media/laptop-2012/Mail/Inbox --to /media/archive/Mail/merged',
             'backdown mail export /media/archive/Mail/merged --mbox /media/archive/Mail/inbox.mbox'],
}

NOTES = {
    'index': 'A drive that is suddenly empty although its last scan held files (usually an unplugged drive’s leftover '
             'mount point) is refused and nothing is recorded; pass `--allow-empty` if it really was emptied. Symbolic '
             'links are not followed. When a daemon is running, the scan is handed to it and it hashes the new files next. '
             'With `--method auto`, a rescan reads the drive’s change log where there is one (the USN journal of NTFS '
             'drives on Windows, the change history of APFS and HFS+ drives on macOS), or NTFS’s file table (on Windows as '
             'administrator, on Linux for ntfs-3g mounts whose device your account can read), and walks every folder '
             'whenever either can’t vouch for every file, and anyway every tenth scan or after 30 days (Settings > '
             'Scanning). Linux keeps no change log for drives. Each drive’s line says how it was scanned and, for a walk, '
             'why. `--method journal` or `table` stops with the reason instead of walking.',
    'hash': 'Hashes only what could be a duplicate: a file whose size no other file shares is never read. Then a sample '
            '(the first, middle and last 64 KiB) and, for files whose samples still match, the full SHA-256. Files on '
            'drives that aren’t connected wait for them. See [Hashes and proof](/docs/concepts/#hashes-and-proof).',
    'dupes': 'Hardlinks count once. Only fully hashed files are grouped, and `dupes` runs `hash` first.',
    'check': 'Exits 0 when every file has a SHA-256-identical copy outside PATH (“SAFE TO WIPE”), and 2 when some files '
             'exist nowhere else or couldn’t be read (“COPY FIRST”). Copies on drives that aren’t connected still count: '
             'the verdict is about the index. `--list` lists the largest 20,000 such files. Empty files don’t count '
             'against a drive.',
    'volumes': 'Reads sysfs, udev and mountinfo on Linux, `diskutil` on macOS and the storage cmdlets on Windows. It never '
               'mounts or writes anything, says what BackDown can do with each partition (mountable, mounted, system, '
               'encrypted, part of a volume group, swap, empty), and marks the volumes an existing index covers.',
    'serve': 'The browser listens on 127.0.0.1 and prints a private link with a one-time token. It only reads: copying, '
             'wiping, rescues and rescans need the background service (`backdown app`, or `backdown service install` '
             'then `backdown open`). `--host ADDRESS` also listens on that address, a Tailscale address such as '
             '100.x.y.z say, and prints the link to use from your other devices. From anywhere but this computer, '
             'BackDown only shows what’s there: nothing can be wiped, copied, rescued, mounted, opened or changed, and '
             'the link’s token is still needed. A wildcard such as 0.0.0.0 listens on every address, 127.0.0.1 '
             'included.',
    'daemon': 'One job at a time (scan, hash, recognise, copy, mirror, wipe, tidy, mail merge), so the index has a single '
              'writer and each drive a single reader; rescues and syncs with other machines run beside them, and drive '
              'health is read every six hours. Every scan is followed by hashing, and recognition when something changed. '
              'Without `--rescan-every`, the interval is the one in Settings. `--host` works as for `serve`: the browser '
              'stays on 127.0.0.1 too, and from other devices it only shows and queues rescans (the link is '
              '`http://ADDRESS:PORT/?t=TOKEN`, with the token from `ui.json` in the data folder). On Linux and macOS the '
              'CLI talks to it through a socket in your runtime folder; on Windows, commands use the index directly.',
    'service': 'On Linux, a systemd user service that runs as you; with `--system`, a machine-wide service that runs '
               'as a `backdown` user that can read every file and write none, with no network beyond this machine '
               '(add people to the `backdown` group to let them use it). On macOS, a LaunchAgent '
               '(`~/Library/LaunchAgents/io.backdown.daemon.plist`) that runs while you\'re logged in; its log is in '
               '`~/Library/Logs/BackDown`. On Windows, a scheduled task "BackDown" that starts it, without a window and '
               'with your normal rights, when you log on. `--system` is Linux-only. `--host` and `--port` go into the '
               'Linux unit (see `daemon`); systemd keeps the machine-wide service off the network whatever they say, and '
               'on macOS and Windows they aren\'t passed to the service yet (run `backdown daemon --host …` instead). '
               '`status` exits 3 when the service isn\'t running. Uninstalling keeps the index.',
    'health': 'Reads each drive\'s own health reports (SMART) as you, without administrator rights: through udisks '
              '(and `smartctl` where your account may use it) on Linux, `diskutil` (and `smartctl`, if installed) on '
              'macOS, and the storage cmdlets on Windows, which share error counters only with an administrator. '
              '`smartctl` is asked not to wake a drive that has spun down. What can\'t be read is noted, with the '
              'command that would read it. Readings are kept in the index under the drive\'s serial number, so its '
              'history follows it from port to port. When the daemon runs (Linux and macOS), `health` has it read '
              'every drive now; it also reads them every six hours, when a drive appears and when an indexed drive '
              'comes back. No self-tests are run. Exits 2 when a connected drive is failing.',
    'entities': 'Kinds: `mail-store`, `maildir`, `mdbox`, `thunderbird`, `nextcloud`, `nextcloud-server`, `timemachine`, '
                '`ios-backup`, `android-backup`, `system`, `home-mac`, `home-linux`, `home-windows`, `website`, `git`, '
                '`sql-dump`, `disk-image`, `photos`, `music`, `videos`, `export`, `downloads`, `recovery`. Reading where '
                'things came from needs the drive connected: small files only (backup scripts, plists, git config, dump '
                'headers).',
    'report': 'The report reads the index as it is; run `backdown hash` (or `check`/`dupes`) first so every duplicate '
              'is counted.',
    'plan': 'A plan only proposes. The archive’s own drive is never a source. Nothing is copied until `backdown apply '
            'ID --go`, and nothing is ever deleted. Mailboxes that are at least 90% the same as another of the same '
            'origin are filed once, with the other’s extra files beside them; `--merge-mail` merges them by Message-ID '
            'instead.',
    'apply': 'Every copy is written beside its destination under a temporary name, read back and compared before it '
             'takes its name. An existing destination is never overwritten: if it differs, it’s reported as a conflict. '
             'Runs can be interrupted and resumed. Copies are clones or kernel copies where the drives allow, and are '
             'read back either way. A name the archive’s drive would take for another, or can’t hold, is copied under '
             'another name (`photo (2).jpg`) and journaled; extended attributes (macOS) and alternate data streams '
             '(Windows) are copied, and what couldn’t be kept is listed. Exits 2 when there were conflicts or failures. '
             'With a daemon running, `--go` is handed to it, and it indexes the archive afterwards.',
    'rescue': 'Rescue never opens the index, so `sudo backdown rescue …` is safe to run; what it writes is handed back '
              'to the user who ran sudo. It only reads the source, and refuses to write the image or found files onto '
              'the drive being rescued (or, for an image, onto another partition of the same drive unless '
              '`--same-drive`). Exits 2 when an image is incomplete or nothing was found. Browsing a found partition’s '
              'files, recovering deleted files with their names (FAT, exFAT, NTFS) and copying files out are in the '
              'browser’s Rescue view, not in this command.',
    'mirror': 'A sync never deletes: when a file changed, the mirror’s old copy is moved to `.backdown-versions/` in '
              'the mirror first, and files that exist only on the mirror are kept and counted. The daemon syncs each '
              'mirror daily and re-reads a sample on its schedule, whenever both drives are connected.',
    'wipe': 'Refuses the running system (outside home folders), a whole home folder, BackDown’s own index, and anything '
            'not on an indexed drive. Each file is re-read before it’s deleted, and so is its copy outside the '
            'selection; `--trust-index` checks only that the copy is still there as last scanned. Changed, new or '
            'unreadable files are left alone. Every deletion is journaled with its proof. Folders the wipe empties '
            'are removed (a drive’s own top folder stays). A copy only on another machine (`backdown remote`) counts '
            'when that machine confirmed it within the proof window, and is read again there just before the deletion. '
            '`--move-to` keeps names as they are, and leaves a file in place if its extended attributes or alternate '
            'data streams couldn’t be kept on the copy.',
    'license': 'The key is checked offline, on this machine, and kept in your BackDown data folder. `show` exits 1 when '
               'there’s no licence. `features` lists which features are free and which need Pro; a pre-release version, '
               'such as a beta, unlocks everything.',
    'app': 'Finds the running background service (yours, or on Linux the machine-wide one) or starts one: the installed '
           'service if there is one, else a daemon of its own, logging to `daemon.log` in your data folder. The window is '
           'Chrome, Chromium, Edge or Brave in app mode, with a profile of its own (`app-window` in your data folder) '
           'that remembers its size and place; without one, a tab in your default browser. It opens with a one-time code, '
           'so the session token never goes on a command line. If BackDown’s window is open already, it’s brought forward.',
    'desktop': 'Linux: `io.backdown.BackDown.desktop` and its icons in `~/.local/share` (with `--autostart`, an autostart '
               'entry that runs `backdown app --no-window`). macOS: `~/Applications/BackDown.app`, a small bundle that runs '
               'this program (with `--autostart`, the LaunchAgent `backdown service install` makes). Windows: a Start menu '
               'entry (with `--autostart`, the scheduled task `backdown service install` makes). `uninstall` removes '
               'what `install` added and leaves the background service; `backdown service uninstall` removes that.',
    'update': 'Asks GitHub’s public list of BackDown releases, only when you run it (or when Settings says to look once a '
              'day); the answer is kept for a day. Installing downloads this computer’s archive and `SHA256SUMS.txt`, '
              'refuses unless the SHA-256 matches, checks the new program runs, keeps the old one in your data folder, '
              'swaps them and restarts the background service; if the new one doesn’t start, the old one is put back. '
              'A copy installed by a package manager, an AppImage or Homebrew is updated that way instead, and it says so.',
    'mount': 'Always read-only, and as you, through the system’s own tools: `udisksctl` on Linux (it checks the mount '
             'really is read-only), `diskutil` and `hdiutil` on macOS, `Mount-DiskImage` on Windows. It refuses the '
             'running system’s partitions, locked encrypted volumes, LVM and RAID members, swap and empty partitions. '
             'When the system wants an administrator’s password, it shows the command to run instead. What it mounted '
             'is kept in `mounts.json` in your data folder.',
    'unmount': 'Only what `backdown mount` (or the browser’s Mount read-only) mounted; detaching an image unmounts '
               'everything mounted from it.',
    'remote': 'The other machine needs BackDown installed and an index. BackDown runs your system’s `ssh` as you, with '
              '`BatchMode=yes` (never a password) and your known hosts, and starts `backdown serve-stdio` there, which '
              'only reads its index and hashes a file only when asked; nothing listens on the network, and no file’s '
              'contents are sent. A copy there counts only by SHA-256, and only when confirmed there within the proof '
              'window (7 days unless changed). The background service syncs each machine every 6 hours. `add`, `test` '
              'and `sync` need Pro (unlocked in pre-release versions); `list` and `remove` don’t. Windows machines '
              'can’t be the other end yet.',
    'dedup': 'Only files the index has hashed and that are identical, on the same drive, inside FOLDER; files whose '
             'permissions, owner or extended attributes differ (and, for hardlinks, their modification time), or that '
             'also have names outside FOLDER, are left alone. Clones need btrfs, XFS or APFS. Each pair is read again '
             'first; every link is journaled, and running it again carries on. It refuses the running system, whole '
             'home folders and BackDown’s index, and isn’t available on Windows yet. Index the drive afterwards (the '
             'daemon does). Exits 2 when some files failed.',
    'mail': 'Messages are matched by Message-ID and body (or, without a Message-ID, by headers and body); each lands in '
            'the folder the newest copy has it in, with read, replied and flagged marks combined. The destination must be '
            'new or empty; it becomes a Maildir++ mailbox. Every message is written, read back and journaled; an '
            'interrupted merge carries on when run again. Dovecot mdbox and sdbox aren’t read yet. `export` never '
            'overwrites the mbox file. `merge` exits 2 when some messages failed.',
}

SEE_ALSO = {
    'index': [('Getting started', '/docs/getting-started/'), ('Scans and the time slider', '/docs/concepts/#scans-and-the-time-slider'),
              ('Fast rescans', '/docs/concepts/#fast-rescans'), ('When a rescan walks', '/docs/troubleshooting/#when-a-rescan-walks')],
    'hash': [('Hashes and proof', '/docs/concepts/#hashes-and-proof')],
    'dupes': [('Free up a drive', '/docs/guides/free-up-a-drive/')],
    'check': [('What “safe to wipe” means', '/docs/concepts/#what-safe-to-wipe-means'), ('Free up a drive', '/docs/guides/free-up-a-drive/')],
    'import-legacy': [('Scans and the time slider', '/docs/concepts/#scans-and-the-time-slider')],
    'volumes': [('Mounting drives read-only', '/docs/concepts/#mounting-drives-read-only'), ('Troubleshooting: drives not showing up', '/docs/troubleshooting/#a-drive-doesnt-show-up')],
    'serve': [('Browse without restoring', '/docs/guides/browse-without-restoring/'), ('Look from your other devices', '/docs/guides/browse-without-restoring/#look-from-your-other-devices')],
    'status': [('Where the index lives', '/docs/troubleshooting/#where-the-index-lives')],
    'daemon': [('The background service', '/docs/concepts/#the-daemon'), ('Look from your other devices', '/docs/guides/browse-without-restoring/#look-from-your-other-devices')],
    'open': [('Getting started', '/docs/getting-started/')],
    'service': [('The background service', '/docs/concepts/#the-daemon'), ('The service on each system', '/docs/guides/settings-and-licences/#the-service-on-each-system')],
    'entities': [('Recognised things and provenance', '/docs/concepts/#recognised-things-and-provenance')],
    'overlaps': [('File your old drives into one archive', '/docs/guides/file-into-an-archive/')],
    'report': [('Getting started', '/docs/getting-started/#produce-a-report')],
    'plan': [('File your old drives into one archive', '/docs/guides/file-into-an-archive/'), ('Merge mailboxes', '/docs/guides/merge-mailboxes/#in-a-filing-plan')],
    'plans': [('File your old drives into one archive', '/docs/guides/file-into-an-archive/')],
    'apply': [('File your old drives into one archive', '/docs/guides/file-into-an-archive/')],
    'rescue': [('Rescue a failing drive', '/docs/guides/rescue/'), ('Recover deleted files', '/docs/guides/recover-deleted-files/')],
    'mirror': [('Keep a mirror', '/docs/guides/mirrors/')],
    'wipe': [('Free up a drive', '/docs/guides/free-up-a-drive/'), ('Several machines', '/docs/guides/several-machines/'), ('Principles', '/docs/principles/')],
    'license': [('Settings and licences', '/docs/guides/settings-and-licences/')],
    'health': [('Drive health', '/docs/guides/drive-health/'), ('Rescue a failing drive', '/docs/guides/rescue/')],
    'app': [('Getting started', '/docs/getting-started/#open-backdown'), ('Settings, updates and licences', '/docs/guides/settings-and-licences/')],
    'desktop': [('Getting started', '/docs/getting-started/#install')],
    'update': [('Updates', '/docs/guides/settings-and-licences/#updates')],
    'mount': [('Mounting drives read-only', '/docs/concepts/#mounting-drives-read-only'), ('A drive doesn’t show up', '/docs/troubleshooting/#a-drive-doesnt-show-up')],
    'unmount': [('Mounting drives read-only', '/docs/concepts/#mounting-drives-read-only')],
    'remote': [('Several machines', '/docs/guides/several-machines/')],
    'dedup': [('Tidy the archive', '/docs/guides/file-into-an-archive/#tidy-the-archive')],
    'mail': [('Merge mailboxes', '/docs/guides/merge-mailboxes/')],
    'custody': [('Chain of custody', '/docs/guides/chain-of-custody/')],
}

EXIT_CODES = '''| Code | Meaning |
|---|---|
| `0` | Done. For `check`: safe to wipe. |
| `1` | Something went wrong; the message says what. `license show`: no licence. In a release version, a Pro command without a licence. |
| `2` | Finished, with a result to act on: `check` found files that exist nowhere else (or couldn’t be read); `apply`, `wipe`, `mirror sync`, `mirror verify`, `dedup` or `mail merge` had conflicts, failures or mismatches; `health` found a connected drive failing; `rescue image` couldn’t read everything; `rescue find` or `rescue partitions` found nothing. |
| `3` | `service status`: the service isn’t running. |
| `10` | `update --check`: there’s a newer version. |'''

DAEMON = '''When the daemon is running, it owns the index: `index`, `hash`, `dupes`, `check`, `scans`, `status`, `health`,
`remote sync` and `apply --go` are handed to it and you follow its progress in the terminal; `wipe`, `dedup --go` and
`mail merge` check first, then hand the work to it. Everything else reads the index directly.
`--local` makes any command work on the index itself instead. `import-legacy` refuses to write while a daemon is
running: stop it first. On Windows, commands always use the index directly.'''

ENVIRONMENT = '''`BACKDOWN_DB`
: The index database, like `--db`.

`BACKDOWN_PORT`
: The daemon's port for the browser, like `backdown daemon --port`.

`BACKDOWN_BROWSER`
: The Chromium-family browser `backdown app` uses, like `--browser`.

`BACKDOWN_NO_UPDATE_CHECK`
: When set, the daemon never looks for updates, whatever Settings says.

`XDG_DATA_HOME`, `XDG_RUNTIME_DIR` (Linux)
: Where the data folder and the daemon’s socket go: `$XDG_DATA_HOME/backdown` (default `~/.local/share/backdown`) and `$XDG_RUNTIME_DIR/backdown`.

`LOCALAPPDATA` (Windows)
: The data folder is `%LOCALAPPDATA%\\BackDown`.'''


# ---------- running the binary

def clean_env():
    # nothing from this machine: no $BACKDOWN_DB, no real $HOME, no terminal width
    return {'PATH': '/usr/bin:/bin', 'HOME': '/nonexistent', 'LANG': 'C.UTF-8', 'LC_ALL': 'C.UTF-8'}


def run_help(binary, *words):
    r = subprocess.run([str(binary), *words, '--help'], env=clean_env(), capture_output=True, text=True, timeout=30)
    if r.returncode != 0:
        sys.exit(f'`backdown {" ".join(words)} --help` failed: {r.stderr.strip()}')
    return r.stdout


def version_of(binary):
    return subprocess.run([str(binary), '--version'], env=clean_env(), capture_output=True, text=True).stdout.split()[-1]


SECTION = re.compile(r'^(Usage|Commands|Arguments|Options):\s*(.*)$')
ANNOTATION = re.compile(r'\s*\[(default|env|possible values|aliases): ([^\]]*)\]\s*$')


def parse_help(text):
    """{'about': [paragraphs], 'usage': str, 'commands'/'arguments'/'options': [(term, desc)], 'after': str}"""
    lines = text.rstrip('\n').split('\n')
    out = {'about': [], 'usage': '', 'commands': [], 'arguments': [], 'options': [], 'after': ''}
    i, para = 0, []
    while i < len(lines) and not SECTION.match(lines[i]):
        if lines[i].strip():
            para.append(lines[i].strip())
        elif para:
            out['about'].append(' '.join(para))
            para = []
        i += 1
    if para:
        out['about'].append(' '.join(para))
    section = None
    while i < len(lines):
        line = lines[i]
        m = SECTION.match(line)
        if m:
            section = m[1].lower()
            if section == 'usage':
                out['usage'] = m[2].strip()
                section = None
            i += 1
            continue
        if not line.strip():
            i += 1
            continue
        if not line.startswith(' '):
            # text at column 0 that isn't a section: the after-help, to the end
            out['after'] = '\n'.join(lines[i:]).rstrip()
            break
        if section and line.startswith('  '):
            body = line.strip()
            parts = re.split(r'\s{2,}', body, maxsplit=1)
            term, desc = parts[0], (parts[1] if len(parts) > 1 else '')
            i += 1
            if not desc:  # clap's next-line layout: the description follows, indented further
                paras, cur = [], []
                while i < len(lines) and (not lines[i].strip() or len(lines[i]) - len(lines[i].lstrip()) >= 8):
                    if lines[i].strip():
                        cur.append(lines[i].strip())
                    elif cur:
                        paras.append(' '.join(cur))
                        cur = []
                    # stop at the blank line before the next entry (or the after-help)
                    if not lines[i].strip() and i + 1 < len(lines) and lines[i + 1].strip() and \
                            len(lines[i + 1]) - len(lines[i + 1].lstrip()) < 8:
                        break
                    i += 1
                if cur:
                    paras.append(' '.join(cur))
                desc = ' '.join(paras)
            out[section].append((term, desc))
            continue
        i += 1
    return out


def split_annotations(desc):
    notes = {}
    while (m := ANNOTATION.search(desc)):
        notes[m[1]] = m[2]
        desc = desc[:m.start()]
    return desc.strip(), notes


def long_flag(term):
    m = re.search(r'--([a-z0-9-]+)', term)
    return m[1] if m else None


# ---------- what the released version has

def released_source(tag):
    try:
        return ''.join(subprocess.run(['git', '-C', str(REPO), 'show', f'{tag}:{f}'], capture_output=True, text=True,
                                      check=True).stdout for f in (MAIN_RS,)) + \
            subprocess.run(['git', '-C', str(REPO), 'show', f'{tag}:{RESCUE_RS}'], capture_output=True, text=True).stdout
    except subprocess.CalledProcessError:
        sys.exit(f"can't read {MAIN_RS} at {tag}: is it a tag in this repository? (--released)")


def released_commands(src):
    m = re.search(r'enum Cmd \{(.*?)\n\}', src, re.S)
    names = re.findall(r'^    ([A-Z][A-Za-z]+)\s*[{,(]', m[1], re.M) if m else []
    return {re.sub(r'(?<!^)([A-Z])', r'-\1', n).lower() for n in names}


# ---------- Markdown

def md_text(s):
    """clap's help as Markdown: keep `code`, and don't let a stray * or [x](y) turn into formatting"""
    return s.replace('*', '\\*') if '`' not in s else s


def term_md(term):
    # "-o, --output <OUTPUT>" -> `-o`, `--output <OUTPUT>`
    return ', '.join(f'`{t.strip()}`' for t in term.split(', '))


def entries_md(entries, cmd_key, released_src, is_new_cmd, skip):
    out = []
    for term, desc in entries:
        flag = long_flag(term)
        if flag in skip or term.startswith('-h,') or term == '-h' or term.startswith('-V'):
            continue
        desc, ann = split_annotations(desc)
        if not desc:
            desc = ARGS.get((cmd_key, term), '') or FALLBACK.get(term, '')
        extra = []
        if 'default' in ann:
            extra.append(f'Default: `{ann["default"]}`.')
        if 'possible values' in ann:
            extra.append('One of ' + ', '.join(f'`{v.strip()}`' for v in ann['possible values'].split(',')) + '.')
        if 'env' in ann:
            extra.append(f'Also read from `${ann["env"].split("=")[0]}`.')
        new = ''
        if flag and not is_new_cmd and released_src is not None and flag.replace('-', '_') not in released_src:
            new = ' {preview}'
        text = desc if not desc or desc.endswith(('.', '?', '!')) else desc + '.'
        out.append(f'{term_md(term)}{new}\n: {" ".join(filter(None, [md_text(text), *extra])) or "&nbsp;"}\n')
    return out


def command_page(binary, words, order, released_src, released, globals_):
    top = words[0]
    key = ' '.join(words)
    h = parse_help(run_help(binary, *words))
    is_new = released_src is not None and top not in released
    about = h['about']
    lede = about[0] if about else ''
    lines = []
    if is_new:
        lines += ['> [!PREVIEW]', f'> `backdown {top}` isn’t in {released_label(released_src)}, the current download: it '
                  'comes with the next preview.', '']
    # the first paragraph is the page's lede (its description); any others follow
    lines += [md_text(p) + ('' if p.endswith(('.', '?', ')')) else '.') for p in about[1:]] + ['']
    lines += ['```text', h['usage'], '```', '']
    subs = [(t, d) for t, d in h['commands'] if t != 'help']
    body = []
    if h['arguments']:
        body += ['## Arguments', ''] + entries_md(h['arguments'], key, released_src, is_new, set())
    opts = entries_md(h['options'], key, released_src, is_new, globals_)
    if opts:
        body += ['## Options', ''] + opts
    if not subs:
        body += ['Also takes the [global options](/docs/cli/#global-options) `--db` and `--local`.', '']
    if h['after']:
        body += ['## Details', '', '```text', h['after'], '```', '']
    for t, d in subs:
        sh = parse_help(run_help(binary, *words, t))
        body += [f'## backdown {key} {t} {{#{t}}}', '']
        body += [md_text(p) + ('' if p.endswith(('.', '?', ')')) else '.') for p in sh['about']] + ['']
        body += ['```text', sh['usage'], '```', '']
        if sh['arguments']:
            body += ['### Arguments', ''] + entries_md(sh['arguments'], f'{key} {t}', released_src, is_new, set())
        sopts = entries_md(sh['options'], f'{key} {t}', released_src, is_new, globals_)
        if sopts:
            body += ['### Options', ''] + sopts
    if subs:
        body += ['All of them also take the [global options](/docs/cli/#global-options) `--db` and `--local`.', '']
    if top in NOTES:
        body += ['## Good to know', '', NOTES[top], '']
    if top in EXAMPLES:
        body += ['## Examples', '', '```sh', *EXAMPLES[top], '```', '']
    if top in SEE_ALSO:
        body += ['## See also', ''] + [f'- [{t}]({u})' for t, u in SEE_ALSO[top]] + ['']
    description = re.sub(r'\s+', ' ', lede).strip()
    description = description if description.endswith(('.', '?')) else description + '.'
    head = ['---', f'title: backdown {key}', f'nav: {key}', f'description: {description}', f'order: {order}',
            *(['preview: yes'] if is_new else []), '---', '',
            f'<!-- Generated by tools/cli-reference.py from `backdown {key} --help`. Edit the help in crates/backdown-cli '
            '(or the notes in the script) and run it again. -->', '']
    return '\n'.join(head + lines + body).rstrip() + '\n', description, is_new


_released_label = ''


def released_label(_src):
    return _released_label


def index_page(binary, top, pages, version):
    ahead = any(new for _, _, new in pages)
    source = (f'the development build of the next preview; commands marked {{preview}} aren’t in '
              f'{released_label(None)}, the current download') if ahead else f'version {version}'
    lines = ['---', 'title: CLI reference', 'nav: CLI reference',
             'description: Every backdown command and option, generated from the program’s own help.', 'order: 1',
             '---', '',
             '<!-- Generated by tools/cli-reference.py from `backdown --help`. Run it again after the commands change. -->', '',
             f'Everything below comes from the `backdown` program itself (`backdown --help`), so it matches what it '
             f'accepts: {source}.', '',
             'Run `backdown help <command>` for the same text in a terminal.', '',
             '```text', top['usage'], '```', '',
             '## Commands', '', '| Command | What it does |', '|---|---|']
    for name, desc, new in pages:
        lines.append(f'| [`{name}`](/docs/cli/backdown-{name}/){" {preview}" if new else ""} | {md_text(desc)} |')
    lines += ['', '## Global options', '', 'Every command takes these.', '']
    lines += entries_md(top['options'], '', None, True, {'help', 'version'})
    lines += ['`-h`, `--help`', ': Print help (`--help` shows more than `-h`).', '', '`-V`, `--version`', ': Print the version.', '']
    lines += ['## When the daemon is running', '', DAEMON.replace('\n', ' '), '']
    lines += ['## Exit codes', '', EXIT_CODES, '']
    lines += ['## Environment', '', ENVIRONMENT, '']
    return '\n'.join(lines).rstrip() + '\n'


def find_binary(arg):
    if arg:
        return Path(arg)
    for p in (REPO / 'target/release/backdown', REPO / 'target/debug/backdown'):
        if p.is_file():
            return p
    sys.exit('no backdown binary: build it (cargo build -p backdown-cli) or pass --bin')


def leaks(text):
    """anything that looks like this machine: the checkout, the home folder, the user name"""
    found = []
    for s in {str(REPO), str(Path.home()), os.environ.get('USER', ''), os.environ.get('LOGNAME', '')}:
        if len(s) > 2 and s in text:
            found.append(s)
    return found


def main():
    global _released_label
    ap = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    ap.add_argument('--bin', help='the backdown binary (default: target/release, then target/debug)')
    ap.add_argument('--released', help='git tag of the current download (default: downloads.json)')
    ap.add_argument('--check', action='store_true', help='only compare with docs/cli, write nothing')
    a = ap.parse_args()
    binary = find_binary(a.bin)
    version = version_of(binary)
    released = a.released or json.loads((MARKETING / 'downloads.json').read_text())['platforms'][0]['version']
    _released_label = released
    src = released_source(released)
    released_cmds = released_commands(src)

    top = parse_help(run_help(binary))
    globals_ = {long_flag(t) for t, _ in top['options']} - {None}
    files, listing = {}, []
    for n, (name, _) in enumerate([c for c in top['commands'] if c[0] != 'help'], start=2):
        text, desc, new = command_page(binary, [name], n, src, released_cmds, globals_)
        files[f'backdown-{name}.md'] = text
        listing.append((name, desc, new))
    files['index.md'] = index_page(binary, top, listing, version)
    # the site's own style: straight apostrophes and quotes
    files = {f: t.replace('’', "'").replace('“', '"').replace('”', '"') for f, t in files.items()}

    bad = {f: leaks(t) for f, t in files.items() if leaks(t)}
    if bad:
        sys.exit(f'the help mentions this machine ({bad}); fix the help text or the environment')
    if a.check:
        stale = [f for f, t in files.items() if not (OUT / f).is_file() or (OUT / f).read_text(encoding='utf-8') != t]
        stale += [p.name for p in OUT.glob('*.md') if p.name not in files]
        if stale:
            sys.exit(f'docs/cli is out of date: {", ".join(sorted(stale))} (run tools/cli-reference.py)')
        print('docs/cli is up to date')
        return
    OUT.mkdir(parents=True, exist_ok=True)
    for old in OUT.glob('*.md'):
        if old.name not in files:
            old.unlink()
            print(f'removed docs/cli/{old.name}')
    for f, t in files.items():
        (OUT / f).write_text(t, encoding='utf-8')
    new = [n for n, _, isnew in listing if isnew]
    print(f'docs/cli: {len(files)} pages from backdown {version} ({len(new)} commands newer than {released})')


if __name__ == '__main__':
    main()
