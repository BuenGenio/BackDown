#!/bin/bash
# Serialise the long jobs on the 18 TB spindle: wait for the mirrors, then run phase-3 dedup.
# Each step is resumable, so killing this script loses at most the file in flight.
set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
L="$HERE/journal/queue.log"
say() { echo "[$(date '+%m-%d %H:%M:%S')] $*" | tee -a "$L"; }

say "queue armed; waiting for mirror jobs"
while pgrep -f "bd-mirror.py (mail|plain)" >/dev/null; do sleep 120; done
say "mirrors finished"

# phase 3 — dedup the big drive against itself
say "dedup: plan"
sudo python3 "$HERE/bd-dedup.py" plan      >>"$HERE/journal/dedup-plan.log" 2>&1; say "plan rc=$?"
say "dedup: sketch pass (cheap discriminator)"
sudo python3 "$HERE/bd-dedup.py" sketch    >>"$HERE/journal/dedup-sketch.log" 2>&1; say "sketch rc=$?"
say "dedup: full sha256 pass (highest value first)"
sudo python3 "$HERE/bd-dedup.py" full      >>"$HERE/journal/dedup-full.log" 2>&1; say "full rc=$?"
say "dedup: report"
sudo python3 "$HERE/bd-dedup.py" report    >>"$HERE/journal/dedup-report.log" 2>&1; say "report rc=$?"
python3 "$HERE/bd-report.py"               >>"$HERE/journal/dedup-report.log" 2>&1
say "queue complete"
