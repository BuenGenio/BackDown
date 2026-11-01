# BackDown — the app

**One line:** plug in every old drive you own; BackDown tells you what's on them, what's duplicated, what each thing *is* (a mail server's maildir, an iPhone backup, a Nextcloud data dir, a music library, a raw disk image), proposes one sane tree to file it all into, executes the move safely and resumably, and then keeps a mirror.

This document is the plan for turning tonight's scripts (`bd-plan.py`, `bd-index.sh`, `bd-hash.py`, `analyze.py`, `bd-dash.py`) into a downloadable desktop application. `ROADMAP.md` sequences it.

---

## 1. Who it is for, and the problem

People with 10–20 years of accumulated storage: a drawer of laptop drives, a couple of externals, a NAS that was migrated twice, a "big drive" that already holds three generations of copies. Typically ex-sysadmins, freelancers, small-business owners, photographers, people who ran their own mail/Nextcloud server for a while. They know roughly what *should* be there ("the server maildir backup"), can't find it, and are afraid to delete anything.

Existing tools cover slices — `fdupes`/`rmlint`/`czkawka` (duplicates), `ncdu`/WizTree (size), Beyond Compare (tree diff), rsync (copy) — but none of them:

- survive being killed halfway through a 13-million-file crawl and resume,
- know that `cur/ new/ tmp/ + dovecot-uidlist` **is a mailbox** and should be treated as one object,
- answer *"what fraction of drive A is already somewhere on drive B?"* at the directory level,
- propose where things should go, or
- execute the move with a journal and hash verification so that "delete the old copy" is provably safe.

## 2. What we learned tonight (design constraints)

| Observation | Consequence for the app |
|---|---|
| A monolithic `find` over 14TB ran 75 min and had to be killed. | **Everything is a resumable unit of work** with atomic completion markers. Nothing runs "to the end" without checkpoints. |
| Fixed-depth chunking is wrong: `hercules-sdb/sdb` alone had 5M files; `Ringtones` had 1. | **Adaptive chunking**: split a directory only when it's known/estimated to be big. Use counts from earlier partial passes. |
| Depth-first traversal means a killed crawl is *mostly complete*; 12.3M of 12.4M files were salvaged. | **Salvage is a first-class operation.** Any partial output can be turned into finished chunks. |
| HDDs saturate at ~60 random reads/s regardless of throughput; two `find`s on one spindle thrash. | **One worker per physical spindle**, work queues keyed by block device, not by mount. Show per-disk saturation in the UI so the user understands why it's slow. |
| The bottleneck is the *source* disk, never the index store. | Index/DB on the fastest local disk; SQLite in WAL mode is enough. |
| Root-owned server backups, damaged NTFS that only `ntfs3` will mount read-only, an encrypted PS4 drive with 31 partitions, an empty exFAT partition. | **Volume probing** must classify partitions (mountable / damaged-ro / encrypted / empty / foreign) and mount read-only via a privileged helper. |
| The valuable thing (maildir) was found by *reading a shell script* (`backup-mail.sh`) that documented the rsync source. | **Recognisers read provenance**: backup scripts, rsync logs, `.sync` markers, Time Machine plists, iTunes `Info.plist`. Provenance is a first-class attribute of an entity. |
| "Duplicates" at file level were dominated by one Nextcloud data dir being a superset of everything else. | The useful unit is **tree containment** (what % of A's bytes exist under B), not pairwise file lists. |
| FAT32 mtimes have 2s granularity; NTFS/QNAP `.streams` produce phantom `:`-named entries; Nextcloud `preview/` caches are 450k worthless files. | Fuzzy mtime compare; ignore-lists per recognised entity type; entity-level "junk" flags. |
| User granted wipe authority conditionally: *only after data is verified elsewhere*. | **The executor never deletes without a hash-verified copy recorded in the journal.** Wiping a volume is a plan step gated on "every file on this volume has ≥1 verified copy". |

## 3. Core concepts (data model)

```
Volume     block device + partition + fs + label + health + mount state (ro/rw/none)
Spindle    physical disk a Volume lives on (work-queue key)
Chunk      resumable unit of indexing: (volume, kind R|F, dir) → done/in-flight/pending
File       (volume, relpath, size, mtime, inode, [sha256], [partial_hash])
Entity     a recognised *thing* spanning many files: type, root path, provenance,
           date range, size, file count, confidence, junk-subtrees
           types: maildir, mdbox, mailcow-vmail, thunderbird-profile, nextcloud-data,
                  nextcloud-app, timemachine, ios-backup, android-backup, macos-home,
                  linux-home, windows-profile, linux-system, disk-image, git-repo,
                  photo-library, music-library, video-library, vhosts, sql-dump,
                  takeout (google/facebook/linkedin), app-installers, downloads-pile
Subject    who or what entities are about, across drives: a Person (aliases: email,
           login@host, name) or a Database (engine family + name); each with its
           *mentions*: (entity, place, role, detail, size, dates). Kinds are a
           registry of extractor functions (subjects.rs); merging is by shared alias.
Document   what's inside an entity, for search (fts.rs): a message (subject, who, when),
           a table (columns), a chunk of a table's text; the FTS index keeps no text,
           only where each hit is. Sources: Dovecot's own index cache, message files,
           dumps, SQLite files (FTS tables searched in place).
Overlap    (entityA or dirA, entityB or dirB) → bytes-in-common, files-in-common,
           % of A contained in B, % of B contained in A, method (name+size|hash)
Plan       ordered list of Ops: copy / verify / delete / wipe-volume / rename-tree
Op         source, destination, state (pending|running|verified|done|failed), journal ref
Journal    append-only log per Op: every file copied, its hash, timestamps; the basis
           for resume and for the "safe to delete" proof
Mirror     a standing rule: keep <archive subtree> mirrored on <volume>, verify on schedule
```

## 4. Architecture

Same shape as Innards: a UI-agnostic Rust core, a thin Tauri 2 shell, a Svelte 5 frontend. Reuse Innards' design tokens and its `probe/` code for drives and SMART.

```
crates/backdown-core/
  probe/        volumes, partitions, fs types, health (reuse innards-core smart probe),
                spindle mapping (/sys/block), mount helper client
  index/        planner (adaptive chunks, salvage), crawler (per-spindle workers),
                store (SQLite WAL: chunks, files)
  hash/         tiered: size → 64KB head+tail sample → full sha256 (BLAKE3 optional);
                per-spindle queues, resumable by (path,size,mtime)
  recognise/    rule-based entity detection over the index (+ provenance readers)
  overlap/      directory containment matrix; entity-aware (compares mailboxes as units)
  plan/         filing proposal: type → origin → time; explains every placement
  exec/         journaled copy (reflink/sendfile where possible), verify, delete,
                wipe; resumable; dry-run
  mirror/       standing sync rules + scheduled verify
  report/       Markdown/HTML export (the REPORT.md we wrote by hand, generated)
crates/backdown-cli/     `backdown scan|hash|dupes|entities|plan|apply|status|report`
                          (the scripts, productised; the GUI is optional)
src-tauri/               commands, privileged-helper broker, settings, license
src/                     Svelte: Drives · Progress · What's here · Overlaps · Plan · Apply · Mirrors
tools/backdown-helper/   tiny privileged helper (pkexec/polkit on Linux, SMJobBless-less
                          osascript prompt on macOS, UAC on Windows): mount ro, read root-owned
```

Privilege model: the app itself runs unprivileged. A separate helper is invoked only for (a) mounting a partition read-only, (b) reading files the user can't. It never writes to source volumes. This keeps the "we never modify your old drives" promise honest and auditable.

Performance targets (from tonight's data): 15M files indexed on a 5400rpm HDD in < 2 h with a single seeker; hashing at disk sequential speed for large files and ≥ 300 files/s for small ones; UI stays responsive with 20M rows (all aggregation in SQLite, UI reads summaries).

## 5. Screens

1. **Drives** — every block device, classified: mountable / mounted ro / damaged (offer ro mount) / encrypted (PS4, BitLocker, LUKS) / empty / system. SMART health inline. One click: "index this".
2. **Progress** — tonight's dashboard: per-volume chunk progress, in-flight dirs, per-spindle busy %, MB/s, ETA. Pause/resume/kill any job. Explains *why* it's slow ("sde is at 98% busy doing 60 seeks/s — this is the disk, not the app").
3. **What's here** — the entity list: "Dovecot maildir, mail.maximalconcepts.com, 142 mailboxes, 2011–2020, 38 GB, rsynced by backup-mail.sh". Filter by type; open provenance; mark junk subtrees.
4. **Overlaps** — a matrix/treemap: for each top-level dir on each volume, how much of it already exists elsewhere (by hash), and where. Drill to file lists. This is the "can I wipe this drive?" view.
5. **Plan** — the proposed archive tree (type → origin → time) with every entity slotted in and an explanation. Drag to re-file; rename; merge; exclude. Shows resulting sizes per target volume, and which source volumes become wipeable.
6. **Apply** — dry-run diff, then execute with the journal scrolling; pause/resume; per-op verify status; "safe to delete" gates lit only when proofs exist.
7. **Mirrors** — standing rules ("keep Archive/Mail on BD-500"), last verified, drift.

## 6. Recognisers (initial set)

| Type | Signature | Provenance hints |
|---|---|---|
| maildir | dir with `cur/ new/ tmp/`; `dovecot-uidlist`, `dovecot.index*`, `subscriptions` | parent named like a domain; sibling `backup-*.sh` mentioning rsync `vmail` |
| mdbox / sdbox | `storage/ m.*`, `dovecot.map.index`; `dovecot.list.index` | |
| mailcow vmail | `<domain>/<user>/Maildir`, `sieve/`, `shared-mailboxes.db` | `mailcowdockerized_vmail-vol-1` in a sync script |
| Thunderbird profile | `ImapMail/<host>/*.msf`, `prefs.js` | host → server name |
| Nextcloud data | `appdata_*/`, `<user>/files/`, `.ocdata`, `updater-*/` | `preview/` and `updater-*/backups` are junk |
| Time Machine | `Backups.backupdb/<host>/YYYY-MM-DD-HHMMSS`, `.com.apple.timemachine.*` | host name, dated snapshots |
| iOS backup | `Manifest.db`, `Info.plist`, `Status.plist`, 2-hex-char dirs | device name/model from Info.plist |
| Android | `crDroid`, `TWRP/BACKUPS/<serial>/<date>`, `Android/data` | |
| Linux system image | `usr/ var/ etc/ home/` under one dir | hostname from `etc/hostname` |
| Linux / macOS home | `.bashrc`/`.zshrc`, `Library/`, `.config/` | username = dir name |
| Windows profile | `AppData/`, `NTUSER.DAT`, `Documents and Settings` | |
| Disk image | `*.img *.dd *.dmg *.iso *.vdi *.qcow2`; sparse; magic bytes; `ddrescue` logs beside | `sdf.log` → source device |
| vhosts / web root | `wp-config.php`, `.htaccess`, `composer.json`, `vhosts/<domain>` | |
| SQL dump | `*.sql *.sql.gz` with `-- MySQL dump` header | database names, host, date; phpMyAdmin, pg_dump(all), SQLite .dump |
| MongoDB dump | `dump/<db>/<coll>.bson` + `.metadata.json` | database and collection names |
| SQLite file | `*.sqlite *.sqlite3 *.db3`, `*.db` by magic bytes | swallowed inside profiles, homes, phones |
| MySQL / MariaDB data dir | `ibdata1` or `ib_logfile0` + `mysql/`; `aria_log_control` → MariaDB | databases from folders with `.frm/.ibd/.MYD` |
| PostgreSQL data dir | `PG_VERSION` + `base/` + `global/` | version; databases counted from `base/<oid>` |
| MongoDB data dir | `WiredTiger` + `storage.bson` | |
| Website config | `wp-config.php`, `configuration.php`, `.env`, Nextcloud `config/config.php` | database, engine, host, address, contact |
| Media libraries | ≥ 200 audio / video files with artist/album or season/episode patterns | |
| Photo library | DCIM structure, `Photos Library.photoslibrary`, Shotwell/Immich dirs | camera EXIF makes |
| Takeout / export | `Takeout/`, `facebook-<user>-<date>`, LinkedIn `Complete_LinkedInDataExport` | date in name |
| Downloads pile | dir named Downloads/Incoming with mixed installers & archives | mark low-value |
| Recovery output | `found.000`, `dir0000.chk`, `Testdisk-*`, `lost+found` | mark low-confidence |

Each recogniser yields a confidence and the subtrees it considers junk (regenerable caches). Junk is excluded from overlap totals and from "must copy before wipe".

## 7. The filing proposal

Default schema (tonight's §5 in `REPORT.md`): first level **type** (`Mail`, `Servers`, `Machines`, `Phones`, `People`, `Companies`, `Exports`, `Media`, `DiskImages`), second **origin** (host / person / company), third **time** (year or dated snapshot). Every placement carries an explanation string ("maildir → Mail; domain mail.maximalconcepts.com → origin; last message 2020-05 → time"). The user edits the schema once; the planner re-slots everything.

Merging rule: when two entities of the same type/origin overlap > 90 % by hash, the newer/larger one is the target and the other becomes a "verify then delete" op, never a second copy.

## 8. Safety model

- Source volumes are mounted read-only and never written (the helper enforces `ro`).
- Every copy is journaled with sha256 of the destination re-read after write.
- `delete` and `wipe-volume` ops are generated only when every non-junk file has ≥ 1 verified journal entry; the UI shows the proof.
- Dry-run is the default; `apply` requires an explicit arm step; wipes require typing the volume label.
- All state lives in one SQLite file the user can back up; the journal alone is enough to reconstruct what happened.

## 9. Platforms and distribution

Linux first (this is where the drives get plugged in; `ntfs3`, `polkit`, `/sys/block` are all available). macOS second (APFS/HFS+ Time Machine sets are a big audience; `diskutil` for probing; ro mounts via `hdiutil`/`mount -r`). Windows third (WMI/`diskpart` probing; NTFS ADS handling; no ro mount, so read via `\\?\` with backup semantics).

Distribution as with Innards: Tauri bundles (`.deb`/`.AppImage`, `.dmg`, `.msi`), plus the CLI crate on crates.io/Homebrew/apt for headless NAS use. Free tier is genuinely useful (inventory + overlaps + report); the executor and mirrors are paid.

| Tier | Unlocks |
|---|---|
| Free | Drives, Progress, What's here, Overlaps, Markdown/HTML report, CLI `scan/hash/dupes/entities/report` |
| Supporter (one-off) | Plan editor, journaled Apply (copy/verify/delete/wipe) |
| Pro (yearly) | Mirrors + scheduled verify, multi-machine (NAS agent), SMART trend alerts |

## 10. Open questions

- BLAKE3 vs sha256: BLAKE3 is 5–10× faster on CPU, but sha256 lets users cross-check with `sha256sum`. Probably: BLAKE3 for dedup, optional sha256 manifest on export.
- Hardlinks/reflinks in the archive: dedupe by hardlinking identical files within the archive (saves space, but complicates "delete one copy")? Default off; offer as a post-step.
- Case-insensitive/normalising filesystems (FAT, HFS+, NTFS): compare relpaths case-folded and NFC-normalised when either side is such a fs.
- Mail-specific merging: two maildir snapshots of the same mailbox at different dates — union by Message-ID rather than by file? (v2.)
- Cloud sources (Google Drive / Dropbox / iCloud) as volumes via rclone? (later; the local-drives problem is big enough.)
