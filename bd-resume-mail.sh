#!/bin/bash
# Wait for the plain rsync mirrors to finish, then SIGCONT the paused mail archiver
# so only one job reads the 18 TB spindle at a time.
set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
while pgrep -f "bd-mirror.py plain" >/dev/null; do sleep 60; done
echo "[$(date +%T)] plain mirrors finished; resuming mail archiver" | sudo tee -a "$HERE/journal/mirror-mail.log" >/dev/null
# resume children first, parent last; retry until nothing is left in T state
for _ in 1 2 3; do
  for p in $(pgrep -x zstd) $(pgrep -f "tar -C /media/buengenio/FUCKING_BIG/hercules") $(pgrep -f "bd-mirror.py mail"); do
    sudo kill -CONT "$p" 2>/dev/null
  done
  sleep 3
  ps -o stat= -p $(pgrep -d, -f "bd-mirror.py mail|tar -C /media/buengenio/FUCKING_BIG/hercules|^zstd" 2>/dev/null) 2>/dev/null | grep -q '^T' || break
done
sleep 10
ps -o pid,stat,comm -p $(pgrep -d, -f "bd-mirror.py mail|tar -C /media/buengenio/FUCKING_BIG/hercules|^zstd") 2>/dev/null | sudo tee -a "$HERE/journal/mirror-mail.log" >/dev/null
