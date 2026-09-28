#!/bin/bash
# Finish the consolidation: run batch2b (in-place dedup of the EXTRACT sets — keep keeper, delete
# victims; NO move-to-root, which flattened app/HR 'Documents' dirs), then index the 18TB into
# the Tailscale daemon so the web UI shows the final deduplicated archive.
set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
say(){ echo "[$(date '+%m-%d %H:%M:%S')] FINISH: $*" | tee -a "$HERE/journal/finish.log"; }
say "batch2b (in-place dedup of extract sets)"
"$HERE/bd-delete-run.sh" index/batch2b-extract.tsv sample
say "batch2b done; total freed now $(df -h /media/buengenio/FUCKING_BIG | awk 'NR==2{print $4}') free"
# archive is read-only again (supervisor restores it); indexing only reads
say "indexing 18TB into the daemon"
~/.local/bin/backdown index /media/buengenio/FUCKING_BIG >>"$HERE/journal/finish.log" 2>&1
say "index queued/done; hashing kicks in per the daemon"
~/.local/bin/backdown status >>"$HERE/journal/finish.log" 2>&1
say "COMPLETE"
