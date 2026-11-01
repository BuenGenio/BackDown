# BackDown — screens

Reference screenshots of the current tooling, captured from the live dashboard (`bd-dash.py`) with headless Firefox. Regenerate with `docs/screenshots/capture.sh` while the dashboard is running (dashboard + report, light + dark, desktop + phone).

Visual language: Innards' — warm off-white ground (`#f5f3ec`) / near-black dark ground (`#141317`), accent `#d8432b` (`#f0603f` dark), Bricolage Grotesque for display type, Instrument Sans for body, 14 px radius cards. Frosted glass: the fixed header and every card sit on `backdrop-filter: blur(18–20px) saturate(140–145%)` over a 58–70 %-opaque surface, with three blurred colour fields behind the page so the translucency reads. Honors `prefers-color-scheme`, `prefers-reduced-transparency` (glass becomes opaque) and `prefers-reduced-motion`. `?theme=dark|light` forces a theme.

## Dashboard — "What the machine is doing"

The single screen that exists today (it becomes the **Progress** screen of the app, see `APP-PLAN.md` §5).

| | Light | Dark |
|---|---|---|
| Desktop 1280 px | ![Dashboard, light](screenshots/dashboard-light.png) | ![Dashboard, dark](screenshots/dashboard-dark.png) |
| Phone 420 px | ![Dashboard on a phone, light](screenshots/dashboard-mobile-light.png) | ![Dashboard on a phone, dark](screenshots/dashboard-mobile-dark.png) |

Anatomy, top to bottom:

1. **Header (fixed, glass).** Logo mark + "BackDown"; horizontal nav — Overview · Indexing · Hashing · Disks · Report — with an accent underline on the section currently in view (IntersectionObserver); a status pill on the right (`working · HH:MM:SS` with a green dot, `idle`, or `server unreachable` in red).
2. **Title block.** Mono eyebrow `DRIVE CONSOLIDATION`, display headline, one muted line: load average and how many crawler processes are on the big drive.
3. **Four stat tiles.** *Files indexed on the 18TB drive* (hero figure, e.g. `14.80M`, with `686 of 743 chunks complete`); *Bytes hashed* (`1.32 TB`, files and current MB/s); *Indexer* running/stopped; *Hasher* running/stopped with read-error count.
4. **Indexing table.** One row per volume: name, mount, chunks done/total, a progress bar (accent while unfinished, blue when complete), file count. Under `big`, the in-flight chunks are listed as `code` paths with their running file counts (`./X1/home 2.19M`).
5. **Hashing card.** Two meters for the current run — by file count and by bytes — an ETA at the current rate, and the last read errors in red.
6. **Disks table.** Per physical disk: role, read IOPS, read MB/s, write MB/s, busy %. The caption explains the key insight: a spinning disk at 98 % busy and 1 MB/s is seek-bound on small files — the disk, not the app.
7. Footer: what the page reads and how often.

Data source: `GET /api/status` (JSON) every 5 s; the first state is embedded server-side so the page renders without a loading flash.

## Report — "Seven drives, one dataset"

`http://localhost:8787/report` — the interactive findings page, fed by `index/report.json` (regenerate with `python3 legacy/bd-report.py`). Becomes the **What's here / Overlaps / Verdicts** screens of the app.

| Light | Dark |
|---|---|
| ![Report, light](screenshots/report-light.png) | ![Report, dark](screenshots/report-dark.png) |

Sections (the header nav jumps between them, active one underlined):

1. **Hero tiles** — redundant bytes (name + size ≥ 1 MB), maximalconcepts.com mail size / messages / mailboxes, all mail in the vmail tree, bytes that must be copied before wiping (hash-proven unique).
2. **Drives** — one glass card per volume with a used/size meter, indexed file count, filesystem and a note; clicking a card selects that volume in the size tree.
3. **Where the bytes are** — segmented volume switcher; an expandable directory tree to depth 4 with bars relative to the parent (blue sequential; orange for directories over 1 TB); size and file count per row; children pruned to the top 40 / ≥ 100 MB with a "… n more" remainder row.
4. **Mail** — sortable table of the vmail domains (size, messages, mailboxes, files) with a filter box.
5. **Duplicates** — sortable table of directory pairs sharing files by name + size, with chips to show all / small ↔ big / big ↔ itself, a path filter, and a "show all 400" toggle (50 by default). Volume badges (`big` in accent) on each side.
6. **Hash verification** — per small drive: share of bytes with a sha256-identical twin on the 18 TB drive, unique bytes/files, and per-top-dir bars (orange when > 10 GB unverified).
7. **Wipe verdicts** — cards with a status pill: ✓ wiped / ✓ safe / ◔ pending / ✗ copy first, and the reason with exact sizes.
8. **Proposed filing** — the `Archive/` tree as collapsible groups (type → origin → time) with the source directories each group absorbs.

## Screens that exist only as plans

See `APP-PLAN.md` §5: Drives · What's here (entities) · Overlaps · Plan · Apply · Mirrors. No screenshots yet.

## Related projects

- **Innards** (`~/Projects/innards`) — the desktop app whose site supplied this design language; its own screenshots live in `docs/press-kit/screenshots/`.
