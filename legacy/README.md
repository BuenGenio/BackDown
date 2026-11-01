# The pre-Rust scripts

These are the scripts BackDown started as, written on 27–28 Sep 2026 to consolidate one particular set of drives before the `backdown` binary existed. They are kept for the record and because `backdown import-legacy` reads what three of them wrote. **They aren't maintained or tested, and nothing in the product runs them.**

Most of them hard-code that consolidation: drive labels, mount points under `/media/buengenio/…` and `/mnt/bd*`, and an `index/` folder beside the script (they were run from the repository root, so after the move they'd look in `legacy/index`). Several need `sudo` and delete or wipe things. Don't run them as a tool; `backdown` does all of this, safely and for any drives.

They don't read BackDown's index. They kept their own state: `index/<volume>/` chunk lists and TSVs, `index/hashes.db` (table `h`) and `index/dedup.db` (table `f`).

## What `backdown import-legacy` reads

| Script | Wrote | Read by |
|---|---|---|
| `bd-plan.py` | `index/<vol>/chunks.list`: the crawl plan, one `R<TAB>dir` or `F<TAB>dir` per line | `crates/backdown-core/src/legacy.rs` |
| `bd-index.sh` | `index/<vol>/chunks/<id>.tsv` (with `.done` and `.path` beside each) and, after `merge`, `index/<vol>.tsv`: `find -printf '%s\t%T@\t%p\n'` | the same |
| `bd-hash.py` | `index/hashes.db`: `h(path, size, mtime, sha256, hashed_at)` | the same |

`crates/backdown-core/tests/legacy.rs` writes those formats itself, so the importer's tests don't depend on these files.

## The rest, by phase

| Phase | Scripts | Became |
|---|---|---|
| Look | `analyze.py` (sizes, name+size duplicate pairs, mirrored trees), `bd-report.py` (`report.json` for the dashboard), `bd-dash.py` (the live dashboard on :8787) | `backdown dupes`, `overlaps`, `report`, `serve` |
| Match | `bd-match.py` (cross-drive twins by hash) | `backdown dupes`, `check` |
| Copy | `bd-stage.py`, `bd-stage.sh`, `bd-stage-verify.sh` (stage unique files, verify by SHA-256), `bd-refile.py` (file the staged tree) | `backdown plan` / `apply` |
| Mirror | `bd-mirror.py`, `bd-mirror-mail.sh`, `bd-resume-mail.sh`, `bd-queue.sh` | `backdown mirror` |
| Deduplicate | `bd-dedup.py`, `bd-dedup-run.sh`, `bd-plan-delete.py`, `bd-delete.py`, `bd-delete-run.sh`, `bd-extract.py`, `bd-finish.sh` | `backdown dedup`, Free up |
| Drives | `bd-mount.sh` (mount by UUID), `bd-wipe.sh` (wipe and reformat by serial) | `backdown volumes`, `wipe` |

`marketing/screenshots/serve.py` imports the dashboard page from `bd-dash.py` for the site's slideshow, which is the only thing outside this folder that loads one of these files.
