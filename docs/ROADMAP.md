# BackDown — roadmap

Companion to `APP-PLAN.md`. Each phase ends with something usable on the real dataset (the seven drives on this desk); nothing ships that hasn't been run against 15M files.

Effort is in focused weeks for one person with AI assistance. Dates assume a start in October 2026.

---

## v0.1 — Scripts (done, 21 Sep 2026)

What exists tonight, in `Projects/BackDown/`:

- `bd-plan.py` adaptive chunk planner with salvage of a killed crawl
- `bd-index.sh` resumable chunked indexer (`plan | run | status | merge`)
- `bd-hash.py` resumable sha256 → SQLite, per-spindle queues
- `analyze.py` size breakdown, name+size duplicate pairs, mirrored-tree detection
- `bd-dash.py` local live dashboard (Innards visual language)
- `REPORT.md` hand-written findings + proposed filing tree

Lessons captured in `APP-PLAN.md` §2.

## v0.2 — `backdown` CLI in Rust (3 weeks, Oct 2026)

**Status, 27 Sep 2026:** previews `v0.2.0-alpha.1` and `alpha.2` are out for Linux (x86-64, ARM64), macOS (universal) and Windows, built by `.github/workflows/release.yml` and published to the public `BuenGenio/backdown-releases` repo by `scripts/publish-release.sh`. Shipped: `index` (each run a scan; file and folder versions kept for a time slider), tiered `hash` (size → sample → SHA-256, one reader per device), `dupes`, `check` (safe-to-wipe, exit 0/2), `scans`, `status`, `serve`, a local file browser with a time slider and read-only viewers (pulled forward from v0.5), and (`alpha.3`) `daemon` + `service install|uninstall|status`: a systemd user or hardened system service that rescans drives when due or when they come back, hashes after every scan, and serves the UI (`backdown open`). Since (on `cli/rust-v0.2`, 28 Sep): the browser's Activity view (current job with progress, queue, per-drive schedule; long-polled, nothing in hidden tabs), right-click menus, open with the system app / show in file manager (never programs or scripts), download a copy, type/size/date filters and hidden files, sorting in every view, a photo gallery, a nested size map, comparing a folder between any two scans, drag-select, typed locations (Ctrl+L), tabs and a shortcuts help; `backdown license add|show` for offline Pro keys; `backdown import-legacy`, which turns the Python scripts' `index/` (merged `<vol>.tsv` or finished chunks, plus `hashes.db`) into one finished scan per drive with its hashes attached, so the next `index` finds the files unchanged and nothing is crawled or hashed twice; `backdown volumes` (and `GET /api/volumes` for the UI): every disk (model, serial, transport, spinning or not) and partition (fs, label, UUID, mounts ro/rw), classified system / mounted / mountable / encrypted / member / swap / empty / unknown, with the volumes the index covers marked; read from sysfs, udev and mountinfo on Linux, `diskutil` on macOS, the storage cmdlets on Windows, never as root. Not yet: SMART health, BLAKE3.

Port the scripts into `crates/backdown-core` + `crates/backdown-cli`. Same on-disk contract (chunks, `.done` markers, SQLite), so the Python tools and the Rust binary can be run interchangeably during the transition.

- `backdown volumes` — probe block devices, partitions, fs, health; classify (mountable / ro-only / encrypted / empty / system)
- `backdown scan <mount>` — adaptive chunking, one seeker per spindle, salvage, `--resume` default
- `backdown hash <mount> [--tier size|sample|full]` — tiered hashing; BLAKE3 default, sha256 optional
- `backdown status` — the dashboard's numbers in the terminal
- SQLite schema v1 with migrations
- Import tonight's `index/*.tsv` + `hashes.db` so no work is repeated

**Done when:** the 18TB drive re-indexes from the existing chunk state in < 10 min and a fresh full crawl of a 1TB laptop drive completes in < 20 min with progress and resume.

## v0.3 — Recognisers + generated report (3 weeks, Nov 2026)

**Status, 28 Sep 2026: built** on `cli/rust-v0.2`: `backdown entities` (mail stores, Time Machine, phones, systems, homes, websites, repositories, disk images, dumps, media libraries, exports, downloads piles, recovered files; junk; provenance from backup scripts, plists, hostnames, git remotes, ddrescue mapfiles, dump headers, sync markers), `backdown overlaps` (containment per drive, top folder and entity), `backdown report [--html]`; in the daemon (after every hash) and the browser (What's here, the report). Since (29 Sep): Dovecot mdbox and sdbox in depth (mailboxes and their names, message counts from the index headers, the oldest and newest message from the first and last storage files, never reading the messages) and Thunderbird profiles (accounts, addresses and servers from prefs.js; IMAP caches told apart from local mail; which mbox file is which folder, with sizes). Not yet: binary plists, the REPORT.md-parity dogfood on the real desk. Since (1 Nov 2026, `feat/people-and-databases`): databases in every form (mongodump folders, SQLite files by magic, MySQL/MariaDB, PostgreSQL and MongoDB data directories; dump headers from phpMyAdmin, pg_dump(all) and SQLite too), site and server configuration read for the database behind each (`wp-config.php`, Joomla, Laravel `.env`, Nextcloud `config.php`), and *subjects* (`subjects.rs`): People and Databases gathered across drives from the entities (mailboxes, Nextcloud accounts, homes, Thunderbird identities, site contacts; dumps, live data, SQLite files, "used by"), merged by shared alias, stored per root, shown by `backdown people|databases`, `GET /api/subjects` and What's here > People | Databases. New kinds of subject are one extractor function each. Also (1 Nov 2026): search inside things (`fts.rs`, `content.rs`): an SQLite FTS5 index (contentless) over mail headers, dump and SQLite schemas and, at full depth, the text in rows; Dovecot's `dovecot.index.cache` read directly for headers (`dovecot.rs`, verified against the format in lib-index) so no message is opened where the cache is there, SQLite FTS tables searched in place, `FULLTEXT KEY` columns noted; `backdown search`, `GET /api/contents`, the search box's "Inside" scope; a daemon job after recognition, depth `off|headers|full` as a setting.

- `backdown entities` — the recogniser set in `APP-PLAN.md` §6, with confidence and junk-subtree flags
- Provenance readers: shell scripts with `rsync`/`scp`/`tar` lines, `.sync*` markers, Time Machine plists, iOS `Info.plist`, ddrescue logs
- `backdown overlaps` — directory containment matrix by hash (falls back to name+size where unhashed)
- `backdown report --md|--html` — generates the equivalent of `REPORT.md`: drives table, entities by type, overlaps, wipe-safety per volume
- Dogfood: the report must find the maximalconcepts.com maildir and state its size/mailbox count without any hand-written steps

**Done when:** `backdown report` on this desk reproduces every finding in `REPORT.md` §1–§4 automatically.

## v0.4 — Planner + journaled executor (4 weeks, Dec 2026 – Jan 2027)

**Status, 28 Sep 2026: built** on `cli/rust-v0.2`: `backdown plan` (type → origin → time schema, editable; explanations; already-archived skips; ≥90% twins copy only what's unique; low-value listed not copied), `backdown apply` (dry run, then journaled copies verified by reading back, resumable, never overwrites, never deletes; free-space check), `backdown mirror add|sync|verify` (old versions kept aside, extras reported, sampled re-reads on a schedule); in the daemon and the browser (Plan view; mirrors in Activity). `backdown wipe` and the browser's Free up view: tick drives, folders or recognised things; see how much has an identical copy outside the selection and where; move what exists only there to a chosen folder or leave it; confirm by typing the drive names. The daemon re-reads each file and its copy before deleting it (or, trusting the index, still checks the copy is there as scanned), journals every deletion, then rescans. Never the running system outside home folders, never BackDown's index. Since (29 Sep): every verified copy clones where the filesystem shares blocks (FICLONE on btrfs/XFS, `clonefile` on APFS) or lets the kernel copy (`copy_file_range`, hashing the source alongside), falling back to the loop, and is read back from the disk rather than the page cache (on Linux; macOS and Windows may answer the read-back from their cache); overlapping mailboxes in a plan can be merged by Message-ID instead of copied twice (`plan --merge-mail`, "Merge these mailboxes"), store by store and mailbox by mailbox, and `backdown mail merge|export` do it by hand. Read-only mounts go through udisks (polkit) as the user, and reading a whole drive through udisks' OpenForBackup, so there is no helper of BackDown's own running as root.

- `backdown plan` — filing proposal (type → origin → time), schema editable as a small TOML, explanation per placement, merge rule for >90 % overlapping entities
- `backdown apply --dry-run` then `apply` — journaled copy (reflink/`copy_file_range` where possible), verify by re-hash, `delete`/`wipe` gated on verified copies; pause/resume; per-op logs
- `backdown mirror add|verify` — standing sync rules, verify by sampling or full
- Privileged helper (Linux: polkit) for ro-mount and root-owned reads; the main binary never runs as root
- Dogfood: execute the real consolidation of these seven drives with it. Free Stuff, sda4, sdc, Verbatim; make BD-500 + one freed drive the mirrors of `Archive/Mail`, `Archive/Companies`, `Archive/Machines`.

**Done when:** the desk is consolidated and every deletion is traceable to a journal line with a matching hash.

## Rescue — failing drives, deleted files, lost partitions (pulled forward, Sep 2026)

For people who've never heard of ddrescue, PhotoRec or TestDisk: a Rescue view in the browser (which drive, what happened, where rescued things go) and `backdown rescue image|find|partitions`. Only ever reads the source.

**Status, 28 Sep 2026: built** on `cli/rescue` (PR #3):
- **Copy a failing drive** into an image (`rescue::image`): ddrescue's passes (copy with skipping forwards and backwards, trim, scrape, retry) and its mapfile format, resumable without re-reading. It refuses to write onto the source or elsewhere on its physical drive.
- **Find files by content** (`rescue::carve`): photos (JPEG with EXIF, PNG, HEIC, camera RAW), videos (MP4/MOV, MKV, AVI and more), documents (PDF, Office old and new, OpenDocument, EPUB), audio, archives and SQLite, saved by kind with an index; recovered files are then indexed so they open in the browser.
- **Find lost partitions** (`rescue::parts`): MBR/GPT and their damage, file systems by primary and backup structures; "Find files in it" searches just that partition.

Rescues run in the daemon's side lane beside scans, can be stopped and resumed, and stay on record. When a drive can't be read directly, BackDown says so and gives the one command that lets this account read that device only; `sudo backdown rescue …` works too, and never opens the index.

**Since (29 Sep):** a found partition's file system opens directly (FAT, exFAT, ext, NTFS, read by BackDown itself), and chosen files and folders are copied out to another drive, each checked by reading it back; deleted files come back with their names (FAT, exFAT, NTFS; ext3/4 can't, so Find files by content), each saying how likely it is intact; "Allow BackDown to read this drive" asks udisks for a read-only handle (the system's password dialog) instead of the terminal command, which stays as the other way. Partitions (and disk images BackDown can't read itself: XFS, btrfs, HFS+, APFS…) mount read-only through udisks as the user, `backdown mount|unmount`, and only what BackDown mounted is unmounted.

Not yet: reassembling fragmented files, SMART health.

## v0.5 — Desktop app (5 weeks, Feb – Mar 2027)

**Status, 29 Sep 2026: built around the browser UI** (Tauri later, as decided): `backdown app` starts this user's service or daemon if none runs and opens the UI in a Chromium-family app window (Chrome, Chromium, Edge, Brave; a profile of its own that remembers the window; else the default browser), finding an open window instead of starting anything twice; the session token never goes on a command line (one-time launch codes). `backdown desktop install|uninstall`: applications menu and icons (Linux, optional autostart), `~/Applications/BackDown.app` (macOS), Start menu (Windows); icons from the brand mark. A Settings view (licence and features, updates, rescan interval applied live, hashing, junk rules incl. your own folder names, the filing schema with validation, notifications, index location, about). `backdown update`: GitHub releases of `backdown-releases`, cached a day, verified against `SHA256SUMS.txt`, atomic swap with rollback, daemon restarted; a quiet banner and "Update now" in the browser. Free/Pro gating from one table (`tiers.rs`), enforced by the server, the CLI and the UI; previews unlock everything ("Pro features are free during the preview"). First run: Activity welcomes you with the connected drives to index. Installers (`scripts/package-local.sh`, from `build-local.sh`'s archives, no root): `.deb` and AppImage for x86-64 and ARM64, a `.dmg` and `.zip` of an ad-hoc-signed `BackDown.app`, a per-user `.msi` and a setup `.zip` for Windows. Not yet: notarisation and code signing (they need Apple and Windows certificates), a native shell. The installers ship with releases from `v1.0.0-beta.1` (`publish-release.sh --with-packages`).

Tauri 2 shell + Svelte 5, sharing Innards' tokens and components.

- Screens: Drives · Progress (port of `bd-dash.py`) · What's here · Overlaps (treemap + matrix) · Plan (drag to re-file) · Apply (journal live view, arm/confirm) · Mirrors
- Settings: index location, hash algorithm, junk rules, schema
- Linux `.deb`/AppImage; auto-update via Tauri updater
- Free/Supporter gating (offline Ed25519 keys, same tooling as Innards)

**Done when:** a non-technical user can plug in two drives, see what's duplicated, and produce a report without the terminal.

## v0.6 — Mirrors, scheduling, health (3 weeks, Apr 2027)

**Status, 29 Sep 2026: health, notifications and scheduling built** (pulled forward): `backdown health [--json]` and the daemon read every drive's SMART data without root (Linux: udisks2 over D-Bus, smartctl where allowed, hwmon; macOS: `diskutil info`, smartctl; Windows: `Get-PhysicalDisk`, `Get-StorageReliabilityCounter`), keep readings in the index by serial so a drive's history follows it, and turn changes into warnings (new reallocated/pending/uncorrectable sectors, rising cable errors, NVMe wear and spare, temperature spikes, the drive saying it's failing). In the browser: a health pill per drive (Activity, the schedule, the sidebar), details with sparklines, a banner for a failing drive with "Copy it now" into Rescue, "this mirror drive is dying" on mirrors. Desktop notifications (notify-send, osascript, a PowerShell toast), deduplicated and rate-limited, for a drive getting worse, a mirror that no longer matches, a scheduled job that failed; listed in Activity with read/unread; `GET/POST /api/notify-settings`. `backdown service install` on macOS (LaunchAgent) and Windows (a scheduled task at logon) too. Not yet: SMART self-tests, a timer-based mirror verify outside the daemon.

- Scheduled mirror verify (systemd timer / launchd), drift notifications
- SMART trends per drive (reuse `innards-core` probe); "this mirror drive is dying" warnings
- Hardlink/reflink dedup inside the archive as an opt-in post-step (built 29 Sep: `backdown dedup`, "Tidy the archive" in the Plan view)
- Pro tier: multi-machine — a headless `backdown agent` on a NAS reports into the desktop app (built 29 Sep, over ssh rather than an agent that listens: `backdown remote add|list|remove|sync|test|settings` reaches another machine with the system's `ssh` and your keys (BatchMode, host key checking on) and runs `backdown serve-stdio` there, a read-only JSON-lines answerer; the daemon syncs each machine's files and hashes incrementally by its scan ids, has files there that could be copies of files here hashed there, and hashes here against what's there. Duplicates, overlaps, `check` and Free up count a copy there only by SHA-256 and only when confirmed there within 7 days (a setting); a wipe has that machine re-read the copy just before deleting. The browser's Machines section browses them read-only. Not yet: previews of files there, Windows machines as the other end)

## v1.0 — macOS + Windows (6 weeks, May – Jun 2027)

- macOS: `diskutil` probing, APFS snapshots, HFS+/APFS Time Machine sets, `hdiutil` for images, notarised `.dmg`
- Windows: WMI/`diskpart` probing, NTFS ADS, long paths, `.msi`; read-only access via backup semantics (no ro mount)
- Case-insensitive / Unicode-normalising fs handling everywhere
- Website (landing page live at backdown.io from `marketing/`; full docs site still to do), press kit, docs

**Status, 29 Sep 2026: `v1.0.0-beta.1` released** (built locally with `scripts/build-local.sh` and `package-local.sh`, since Actions is blocked on billing; every test run on Linux, the macOS and Windows targets linted with clippy and cross-built with zig, not yet run on real Macs or PCs). Built for this milestone: case- and Unicode-safe names (`names.rs`: NFC/NFD and case collisions renamed rather than overwritten), `\\?\` long paths and opening sources for reading only (`paths.rs`), NTFS alternate data streams listed and carried (`streams.rs`), extended attributes and resource forks carried (`extras.rs`), Time Machine on APFS as well as HFS+ (`timemachine.rs`), reading as a backup operator when elevated on Windows (`win.rs`), and online-only files never read (Windows cloud placeholders, iCloud's dataless files on macOS). Also: fast rescans, free in every edition (the USN journal on Windows, FSEvents on macOS, NTFS's MFT read directly; a full walk whenever the log can't vouch for the whole tree, and every 10 scans or 30 days regardless), and several machines over SSH (Pro: `backdown remote`, `serve-stdio`, copies elsewhere counted as proof by SHA-256 and re-read before a wipe relies on them). The website has docs at `/docs/` and a press kit at `/press/`. Not yet: signing and notarisation, trying the builds on real macOS and Windows machines.

## Phone app (Android and iOS, started 1 Nov 2026)

A Flutter app in `mobile/` (its README says how to build and test it), a client of the service's HTTP API over a tailnet or LAN, paired with `backdown token --qr`. It does drives and folders (earlier scans too), a file's copies and versions, search across every drive, viewers (pictures, video, text and code, PDF, mail and mailboxes, archives, disk images, SQLite, SQL dumps, spreadsheets, slides), Activity, drive health, notifications from a background check, other machines and the swarm, what's here, duplicates, the history of plans, freeing up and rescues, and the service's version and edition. A built-in demo needs no network. From a phone the service only lets you look (and queue rescans), and the app follows that.

**Status, 1 Nov 2026:** built and tested: widget tests and screenshots against the demo, the real app headless against a real service (`integration_test/`), release APKs on an Android 15 emulator. Not yet: an iOS build (it needs a Mac), a release signing key, store listings, and a decision on whether paired phones may do more than look.

## Later / ideas

- Mail-aware merging: union two maildir snapshots by Message-ID; export to mbox/EML; Thunderbird import helper (merging and mbox export built 29 Sep; EML export and the import helper not yet)
- Photo-aware dedup: perceptual hashes, burst grouping, EXIF-date filing into `Media/Photos/YYYY/MM`
- Cloud sources as volumes via rclone (Drive, Dropbox, iCloud Drive, S3)
- "Time-lapse" view: how a directory tree evolved across the snapshots found on different drives
- Recovery mode: feed `testdisk`/`photorec` output and `found.000` dirs into the recognisers (BackDown's own Rescue now does the finding; see below)

---

## Principles that don't move

1. Never write to a source volume. Ever.
2. Never delete without a verified copy in the journal.
3. Every long job is resumable and shows why it's slow.
4. The free tier answers "what do I have and what's duplicated" completely.
5. Local only. No account, no telemetry, no cloud dependency for core function.
