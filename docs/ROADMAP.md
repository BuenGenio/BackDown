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

**Status, 27 Sep 2026:** previews `v0.2.0-alpha.1` and `alpha.2` are out for Linux (x86-64, ARM64), macOS (universal) and Windows, built by `.github/workflows/release.yml` and published to the public `BuenGenio/backdown-releases` repo by `scripts/publish-release.sh`. Shipped: `index` (each run a scan; file and folder versions kept for a time slider), tiered `hash` (size → sample → SHA-256, one reader per device), `dupes`, `check` (safe-to-wipe, exit 0/2), `scans`, `status`, `serve`, a local file browser with a time slider and read-only viewers (pulled forward from v0.5), and (`alpha.3`) `daemon` + `service install|uninstall|status`: a systemd user or hardened system service that rescans drives when due or when they come back, hashes after every scan, and serves the UI (`backdown open`). Since (on `cli/rust-v0.2`, 28 Sep): the browser's Activity view (current job with progress, queue, per-drive schedule; long-polled, nothing in hidden tabs), right-click menus, open with the system app / show in file manager (never programs or scripts), download a copy, type/size/date filters and hidden files, sorting in every view, a photo gallery, a nested size map, comparing a folder between any two scans, drag-select, typed locations (Ctrl+L), tabs and a shortcuts help; `backdown license add|show` for offline Pro keys; `backdown import-legacy`, which turns the Python scripts' `index/` (merged `<vol>.tsv` or finished chunks, plus `hashes.db`) into one finished scan per drive with its hashes attached, so the next `index` finds the files unchanged and nothing is crawled or hashed twice. Not yet: `volumes`, BLAKE3, recognisers.

Port the scripts into `crates/backdown-core` + `crates/backdown-cli`. Same on-disk contract (chunks, `.done` markers, SQLite), so the Python tools and the Rust binary can be run interchangeably during the transition.

- `backdown volumes` — probe block devices, partitions, fs, health; classify (mountable / ro-only / encrypted / empty / system)
- `backdown scan <mount>` — adaptive chunking, one seeker per spindle, salvage, `--resume` default
- `backdown hash <mount> [--tier size|sample|full]` — tiered hashing; BLAKE3 default, sha256 optional
- `backdown status` — the dashboard's numbers in the terminal
- SQLite schema v1 with migrations
- Import tonight's `index/*.tsv` + `hashes.db` so no work is repeated

**Done when:** the 18TB drive re-indexes from the existing chunk state in < 10 min and a fresh full crawl of a 1TB laptop drive completes in < 20 min with progress and resume.

## v0.3 — Recognisers + generated report (3 weeks, Nov 2026)

**Status, 28 Sep 2026: built** on `cli/rust-v0.2`: `backdown entities` (mail stores, Time Machine, phones, systems, homes, websites, repositories, disk images, dumps, media libraries, exports, downloads piles, recovered files; junk; provenance from backup scripts, plists, hostnames, git remotes, ddrescue mapfiles, dump headers, sync markers), `backdown overlaps` (containment per drive, top folder and entity), `backdown report [--html]`; in the daemon (after every hash) and the browser (What's here, the report). Not yet: mdbox depth, binary plists, Thunderbird account names, the REPORT.md-parity dogfood on the real desk.

- `backdown entities` — the recogniser set in `APP-PLAN.md` §6, with confidence and junk-subtree flags
- Provenance readers: shell scripts with `rsync`/`scp`/`tar` lines, `.sync*` markers, Time Machine plists, iOS `Info.plist`, ddrescue logs
- `backdown overlaps` — directory containment matrix by hash (falls back to name+size where unhashed)
- `backdown report --md|--html` — generates the equivalent of `REPORT.md`: drives table, entities by type, overlaps, wipe-safety per volume
- Dogfood: the report must find the maximalconcepts.com maildir and state its size/mailbox count without any hand-written steps

**Done when:** `backdown report` on this desk reproduces every finding in `REPORT.md` §1–§4 automatically.

## v0.4 — Planner + journaled executor (4 weeks, Dec 2026 – Jan 2027)

**Status, 28 Sep 2026: built** on `cli/rust-v0.2`: `backdown plan` (type → origin → time schema, editable; explanations; already-archived skips; ≥90% twins copy only what's unique; low-value listed not copied), `backdown apply` (dry run, then journaled copies verified by reading back, resumable, never overwrites, never deletes; free-space check), `backdown mirror add|sync|verify` (old versions kept aside, extras reported, sampled re-reads on a schedule); in the daemon and the browser (Plan view; mirrors in Activity). Deleting and wiping stay manual, backed by `check`. Not yet: reflink/copy_file_range fast paths, the privileged helper, per-mailbox merging.

- `backdown plan` — filing proposal (type → origin → time), schema editable as a small TOML, explanation per placement, merge rule for >90 % overlapping entities
- `backdown apply --dry-run` then `apply` — journaled copy (reflink/`copy_file_range` where possible), verify by re-hash, `delete`/`wipe` gated on verified copies; pause/resume; per-op logs
- `backdown mirror add|verify` — standing sync rules, verify by sampling or full
- Privileged helper (Linux: polkit) for ro-mount and root-owned reads; the main binary never runs as root
- Dogfood: execute the real consolidation of these seven drives with it. Free Stuff, sda4, sdc, Verbatim; make BD-500 + one freed drive the mirrors of `Archive/Mail`, `Archive/Companies`, `Archive/Machines`.

**Done when:** the desk is consolidated and every deletion is traceable to a journal line with a matching hash.

## v0.5 — Desktop app (5 weeks, Feb – Mar 2027)

Tauri 2 shell + Svelte 5, sharing Innards' tokens and components.

- Screens: Drives · Progress (port of `bd-dash.py`) · What's here · Overlaps (treemap + matrix) · Plan (drag to re-file) · Apply (journal live view, arm/confirm) · Mirrors
- Settings: index location, hash algorithm, junk rules, schema
- Linux `.deb`/AppImage; auto-update via Tauri updater
- Free/Supporter gating (offline Ed25519 keys, same tooling as Innards)

**Done when:** a non-technical user can plug in two drives, see what's duplicated, and produce a report without the terminal.

## v0.6 — Mirrors, scheduling, health (3 weeks, Apr 2027)

- Scheduled mirror verify (systemd timer / launchd), drift notifications
- SMART trends per drive (reuse `innards-core` probe); "this mirror drive is dying" warnings
- Hardlink/reflink dedup inside the archive as an opt-in post-step
- Pro tier: multi-machine — a headless `backdown agent` on a NAS reports into the desktop app

## v1.0 — macOS + Windows (6 weeks, May – Jun 2027)

- macOS: `diskutil` probing, APFS snapshots, HFS+/APFS Time Machine sets, `hdiutil` for images, notarised `.dmg`
- Windows: WMI/`diskpart` probing, NTFS ADS, long paths, `.msi`; read-only access via backup semantics (no ro mount)
- Case-insensitive / Unicode-normalising fs handling everywhere
- Website (landing page live at backdown.io from `marketing/`; full docs site still to do), press kit, docs

## Later / ideas

- Mail-aware merging: union two maildir snapshots by Message-ID; export to mbox/EML; Thunderbird import helper
- Photo-aware dedup: perceptual hashes, burst grouping, EXIF-date filing into `Media/Photos/YYYY/MM`
- Cloud sources as volumes via rclone (Drive, Dropbox, iCloud Drive, S3)
- "Time-lapse" view: how a directory tree evolved across the snapshots found on different drives
- Recovery mode: feed `testdisk`/`photorec` output and `found.000` dirs into the recognisers

---

## Principles that don't move

1. Never write to a source volume. Ever.
2. Never delete without a verified copy in the journal.
3. Every long job is resumable and shows why it's slow.
4. The free tier answers "what do I have and what's duplicated" completely.
5. Local only. No account, no telemetry, no cloud dependency for core function.
