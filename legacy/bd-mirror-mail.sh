#!/bin/bash
# Supervisor for the mail mirror. Runs bd-mirror.py mail; if it exits non-zero
# (a bus drop / device error trips exit 3), it remounts every volume by UUID and
# resumes. Each mailbox is a resumable unit (.ok markers), so a drop costs at most
# the archive in flight. Ends when the mailer reports all units done (exit 0).
set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
L="$HERE/journal/mirror-mail.log"
say() { echo "[$(date '+%m-%d %H:%M:%S')] SUPERVISOR: $*" | sudo tee -a "$L" >/dev/null; }

attempt=0
while :; do
  attempt=$((attempt+1))
  # keep the archive read-only: a drop then can never corrupt it
  if findmnt -rn /media/buengenio/FUCKING_BIG >/dev/null; then
    sudo mount -o remount,ro /media/buengenio/FUCKING_BIG 2>/dev/null || true
  fi
  sudo python3 "$HERE/bd-mirror.py" mail
  rc=$?
  [ $rc -eq 0 ] && { say "mail mirror complete"; break; }
  say "mailer exited rc=$rc (attempt $attempt) — waiting for the bus, remounting"
  # wait until the archive UUID is visible again (bus may re-enumerate slowly)
  for i in $(seq 1 60); do
    blkid -U c7e9ddf3-6073-43c5-97bf-9ae3a315e2b2 >/dev/null 2>&1 && break
    sleep 10
  done
  # clear any stale mounts pointing at vanished devices, then remount by UUID
  for m in /media/buengenio/FUCKING_BIG /mnt/bd1a /mnt/bd1b /mnt/bd1c /mnt/bd500; do
    cur="$(findmnt -rn -o SOURCE "$m" 2>/dev/null || true)"
    [ -n "$cur" ] && [ ! -e "$cur" ] && sudo umount -l "$m" 2>/dev/null
  done
  "$HERE/bd-mount.sh" >/dev/null 2>&1
  sleep 5
  [ $attempt -ge 200 ] && { say "giving up after $attempt attempts"; exit 1; }
done
