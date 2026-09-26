# BackDown — product & commercial strategy

**One line:** the backup tool that also *understands* what it's backing up — content-verified dedup, entity recognition, forensic-grade provenance, and a data intelligence layer, in one installable daemon with a web UI.

This document is the commercial thesis: who it's for, why it wins against the incumbents, the feature ladder, the daemon + web-UI architecture, and pricing. The engineering plan is in `APP-PLAN.md`; the phasing in `ROADMAP.md`. This project began as a real 27-million-file, 15 TB consolidation (see git history) — every feature below was proven against that dataset before it was a product idea.

---

## 1. The problem the incumbents don't solve

Backup tools answer "can I restore byte X." They do **not** answer "**what do I actually have, what is it, what's duplicated, and what's safe to delete.**" That second question is the one that matters to anyone with 10-20 years of accumulated storage — and it's unserved.

| Tool | What it does well | The gap BackDown fills |
|---|---|---|
| **Time Machine** | Effortless local snapshots on macOS | macOS-only; opaque; no dedup insight; no "what is this"; a black box you can't query |
| **Borg / Borgmatic** | Excellent chunk-level dedup, encryption, compression | CLI-only; a repo is opaque — you can't browse *what's inside* or *why* something is there; no entity understanding; no cross-repo intelligence |
| **restic / Kopia** | Fast, modern, cross-platform, dedup | Same opacity — great vaults, zero intelligence about content; Kopia's UI is a file browser, not an analyst |
| **Bacula / Bareos** | Enterprise scheduling, tape, catalogs | Heavyweight, ops-team-only; catalog is metadata, not *understanding*; no content analysis |
| **rsync / rclone** | The plumbing everyone builds on | No dedup, no verification story, no UI, no intelligence |
| **Duplicati / Vorta** | Friendly GUIs over dedup engines | Still just "browse the vault"; no recognition, no forensics, no analytics |

**The insight from the founding dataset:** the hard part was never copying bytes. It was that 5 of 6 drives held three generations of the *same* personal dataset, the valuable thing (a mail server's maildir) was found by *reading a shell script that documented an rsync*, and 5.9 TB of the "big drive" was silently duplicating itself. **No backup tool on the market would have told you any of that.** BackDown does.

## 2. Positioning

**"Backup that thinks."** Two audiences, one engine:

- **Prosumer / homelab / creator (self-serve):** the person with a drawer of old drives, a NAS migrated twice, a photo library scattered across four devices. They want to *consolidate and understand*, not just protect. Nobody serves them well today.
- **SMB / MSP / forensics & compliance (team + hosted):** legal hold, eDiscovery triage, "prove this file has a verified second copy," data-inventory for GDPR/PDPO, ransomware-recovery verification. The provenance and forensic layers are a wedge into a market that currently pays for expensive, ugly enterprise tooling.

The moat isn't the dedup math (Borg has that). It's the **intelligence layer on top** — recognition, provenance, analytics, forensics — plus a UI people actually enjoy using. That combination doesn't exist.

## 3. Feature ladder — what ships, and what makes us ahead

### Table stakes (par with Borg/restic/Kopia)
- Content-defined chunking + dedup, compression (zstd), client-side encryption
- Incremental, scheduled, verified backups; prune/retention policies
- Cross-platform (Linux/macOS/Windows), S3/B2/local/SFTP targets
- Restore to point-in-time; single-file restore; mount-as-filesystem

### The differentiators (nobody bundles these)

**A. Smart, intelligent deduplication**
- Three-tier dedup proven here: size-collision → cheap head/middle/tail *sketch* → full hash on survivors. Turns "hash everything" (days) into "hash what can possibly collide" (hours).
- **Cross-source dedup**: dedup a new drive *against everything you already have*, not just within one repo — the thing that revealed 5.9 TB of self-duplication.
- **Inode/hardlink-aware and symlink-aware** so reclaim numbers are truthful (Time Machine sets, rsync-without-`-H` copies).
- **Reclaim simulator**: "if I delete these, I free X TB and lose nothing" — with a keep-policy you set (canonical source, newest, tidiest path).

**B. Data intelligence**
- **Entity recognition** (the killer feature): the daemon *knows what things are* — a Dovecot maildir, a mailcow vmail volume, a Nextcloud data dir, a Time Machine set, an iOS/Android backup, a Linux/Windows home, a disk image, a git repo, a photo/music/video library, a SQL dump, a Google/Facebook/LinkedIn takeout. Each with confidence, date range, size, and *junk-subtree* flags (regenerable caches excluded from backup automatically).
- **Provenance readers**: parse the backup scripts, rsync logs, `.sync` markers, TM plists, `Info.plist` that document where data *came from* — so the tool can say "this maildir was rsynced from mail.example.com on 2020-05-31," not just "these are files."
- **Storage analytics**: where the bytes are (interactive size tree), duplication heatmap, growth over time, cost-per-category, "what's junk."
- **Filing intelligence**: proposes a clean tree (type → origin → time) and executes it as instant same-filesystem moves; learns your corrections.

**C. Forensic & compliance layer** (the SMB/legal wedge)
- **Tamper-evident manifests**: every file's sha256 + a signed, append-only journal of every copy/move/delete — a chain of custody you can hand to a court or auditor.
- **"Prove it" reports**: verified-second-copy attestation per file; "this drive is safe to wipe because every byte is elsewhere, hash-confirmed" (the exact guarantee this project produced before wiping three disks).
- **Exposure scan**: search your own corpus for your addresses/domains/secrets across breach-shaped data you legitimately hold — *your own* exposure, not other people's data.
- **Timeline & diff**: how a tree evolved across the snapshots found on different drives.

**D. Data viewers, out of the box**
- **Browse a backup like a live filesystem** — no restore-to-look. Mailboxes render as threaded mail; photos as a gallery with EXIF/map; SQL dumps show their schema + row counts; disk images are loop-mounted read-only and browsed.
- **Search across every backup and drive at once** — full-path, by-entity, by-hash ("where else does this exact file live").
- **Relationship graph** (roadmap): entities and their derivation — this backup came from that machine came from that server; correspondents in a mail archive; the same photo across six locations.

**E. Reliability that survives bad hardware** (proven, not aspirational)
- Every long job is **resumable**; a killed crawl/hash/copy loses only the unit in flight.
- **Read-only source mounts** during analysis so a flaky enclosure can never corrupt the source.
- **UUID/serial addressing** — never trusts `/dev/sdX`; detects and refuses the "enclosure lies about capacity" failure mode.
- **Auto-remount supervisors** — a USB bus drop self-heals and the job resumes.
- One-per-spindle scheduling so parallel jobs don't thrash a single disk.

## 4. Architecture: installable daemon + web UI

```
backdownd (daemon, Rust)                          bd (CLI, Rust)        Web UI (Svelte)
├── probe/      volumes, SMART, spindles          scriptable, headless   local: served by the
├── index/      resumable adaptive crawl → SQLite  (NAS/CI/cron)          daemon on 127.0.0.1
├── hash/       tiered sketch+full, per-spindle                           hosted: our cloud pane
├── recognise/  entity + provenance engine                               over a read-only agent
├── dedup/      cross-source, inode-aware, simulator                      link (see §5)
├── plan/       filing proposal + executor (journaled)
├── backup/     chunked, encrypted, scheduled, S3/B2/local
├── forensic/   signed manifests, chain-of-custody, attestations
├── intel/      analytics, search, viewers, graph
└── api/        local HTTP+WS (the web UI and CLI both speak this)
```

- **Daemon-first.** `backdownd` runs as a service (systemd/launchd/Windows service), owns the schedule, the catalog, and the long jobs. The current Python tools are the working prototype of every module above; the productized daemon is the Rust rewrite (Innards-style core, per `APP-PLAN.md`).
- **The web UI is a client of the daemon's API**, not a bolt-on. Local by default (daemon serves `127.0.0.1`); the same UI runs in our cloud for the hosted tier, talking to a customer-run **agent** that exposes a read-only, scoped API (data stays on their disks; only metadata/telemetry crosses, unless they opt into cloud backup targets).
- **Privacy model:** local tier does zero network I/O. Hosted tier is metadata + UI only by default; content never leaves without an explicit cloud-target opt-in. This is a selling point, not a footnote — it's the opposite of every SaaS backup.

## 5. Deployment modes

1. **Local desktop app** (Tauri shell over the daemon) — the prosumer entry point; the web UI in a native window.
2. **Headless daemon on a NAS/server** — `bd` CLI + browse the UI from any browser on the LAN.
3. **Hosted control plane** — our cloud runs the UI and multi-machine fleet view; a lightweight agent on each machine reports in. Content optional; intelligence centralized.
4. **Air-gapped / forensic appliance** — signed manifests, no network, court-admissible chain of custody.

## 6. Pricing & tiers

Open-core. The engine (index/hash/dedup/backup/restore) is source-available and free forever — that's how we beat "just use Borg" and earn trust. The **intelligence, forensics, hosted, and fleet** layers are paid.

| Tier | Price | For | Unlocks |
|---|---|---|---|
| **Free / Community** | $0, source-available | anyone | Full backup/restore, dedup, CLI + local web UI, size analytics, basic entity recognition, Markdown/JSON export |
| **Pro** | $8/mo or $80/yr per machine | prosumer / homelab | Full entity + provenance engine, reclaim simulator, filing intelligence, all data viewers, cross-source dedup, scheduled verified backups, encrypted cloud targets |
| **Team** | $24/mo per machine (5+ seats) | SMB / MSP | Hosted control plane, multi-machine fleet view, shared catalogs, RBAC, SMART fleet health + alerts, scheduled attestations |
| **Forensic / Compliance** | from $2k/yr per site (quote) | legal / IR / regulated | Signed tamper-evident manifests, chain-of-custody export, "prove it" attestations, exposure scans, air-gapped appliance, audit log, priority support |
| **Hosted backup storage** | usage-based, resold B2/S3 + margin | any paid tier | Optional managed cloud target with client-side encryption |

Rationale: the free tier is genuinely useful (kills the "why not Borg" objection and seeds trust); Pro monetizes the *intelligence* an individual can't get anywhere; Team/Forensic monetize the layers businesses already pay ugly-enterprise money for. Per-machine (not per-GB) pricing aligns with value and undercuts the per-GB SaaS incumbents on large local corpora.

## 7. Go-to-market

- **Wedge:** the consolidation story. "Plug in your old drives; find out what you actually have and what's safe to delete." Nobody else offers it; it's a viral demo (people love watching a tool reclaim TBs).
- **Distribution:** open-core on GitHub for developer trust and inbound; Homebrew/apt/AUR for the CLI; a polished desktop app for the prosumer; content marketing around the forensic/"prove it" angle for SMB.
- **Landing page** (`marketing/`, served at backdown.io): hero = "Backup that thinks," the reclaim demo, the intelligence screenshots, the privacy stance, pricing, a downloadable free tier. Same visual language as the current dashboard/report (Innards-derived, in `assets/`).
- **Moat over time:** the recogniser library and provenance readers compound — every new format we understand is a feature competitors would each have to build. The forensic chain-of-custody, once trusted/certified, is sticky.

## 8. What exists today vs. what's next

**Proven (this repo, against a real 27M-file / 15 TB run):** resumable adaptive index; tiered content-verified dedup (cross-drive *and* internal); inode/symlink-correct reclaim; entity provenance discovery (found the maildir by reading its backup script); journaled staging/refile/mirror; UUID-safe disk ops; bus-drop-surviving supervisors; the live dashboard + interactive report web UI; verified "safe to wipe" attestations.

**Next (see `ROADMAP.md`):** Rust daemon rewrite; scheduled/encrypted backup targets; the full recogniser catalog + provenance readers as a library; forensic signed manifests; hosted control plane + agent; desktop app; the relationship graph; the landing page and paid tiers.
