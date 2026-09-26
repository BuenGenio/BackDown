#!/bin/bash
# Supervisor for the dedup full pass + report. Resumable: on any failure (bus drop),
# wait for the archive UUID to reappear, remount everything by UUID (read-only archive),
# and re-run. bd-dedup.py full only hashes WHERE sha256 IS NULL, so finished files are skipped.
set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
UUID=c7e9ddf3-6073-43c5-97bf-9ae3a315e2b2
say() { echo "[$(date '+%m-%d %H:%M:%S')] DEDUP-SUP: $*" | tee -a "$HERE/journal/dedup-full.log"; }

attempt=0
while :; do
  attempt=$((attempt+1))
  # ensure archive is mounted read-only
  if ! findmnt -rn /media/buengenio/FUCKING_BIG >/dev/null; then
    for i in $(seq 1 60); do blkid -U "$UUID" >/dev/null 2>&1 && break; sleep 10; done
    for m in /media/buengenio/FUCKING_BIG /mnt/bd1a /mnt/bd1b /mnt/bd1c /mnt/bd500; do
      cur="$(findmnt -rn -o SOURCE "$m" 2>/dev/null || true)"; [ -n "$cur" ] && [ ! -e "$cur" ] && sudo umount -l "$m" 2>/dev/null
    done
    "$HERE/bd-mount.sh" >/dev/null 2>&1
  fi
  sudo mount -o remount,ro /media/buengenio/FUCKING_BIG 2>/dev/null || true

  sudo python3 "$HERE/bd-dedup.py" full >>"$HERE/journal/dedup-full.log" 2>&1
  rc=$?
  # count how many candidates still lack a hash
  left=$(python3 - <<'PY'
import sqlite3
c=sqlite3.connect('/home/buengenio/Projects/BackDown/index/dedup.db')
c.execute("CREATE TEMP TABLE grp AS SELECT size,sketch FROM f WHERE sketch IS NOT NULL GROUP BY size,sketch HAVING count(*)>1")
print(c.execute("SELECT count(*) FROM f JOIN grp g ON f.size=g.size AND f.sketch=g.sketch WHERE f.sha256 IS NULL").fetchone()[0])
PY
)
  say "full pass returned rc=$rc, $left candidates still unhashed (attempt $attempt)"
  [ "${left:-1}" = "0" ] && break
  [ "$attempt" -ge 100 ] && { say "giving up after $attempt attempts"; exit 1; }
  sleep 15
done
say "full pass complete — generating report"
sudo python3 "$HERE/bd-dedup.py" report >>"$HERE/journal/dedup-report.log" 2>&1
say "report done: $(python3 -c "import json;d=json.load(open('$HERE/index/dedup-report.json'));print(f\"{d['total_sets']:,} sets, {d['reclaim']/1e12:.2f} TB reclaimable\")")"
