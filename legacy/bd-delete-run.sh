#!/bin/bash
# Supervisor for a deletion batch: ensure archive is read-WRITE, run bd-delete --apply,
# retry on failure (bus drop) after remounting by UUID. Resumable via journal/deleted.log.
# Usage: bd-delete-run.sh <plan.tsv> [verify]   (verify: full|sample|stat, default sample)
set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLAN="$1"; VERIFY="${2:-sample}"; UUID=c7e9ddf3-6073-43c5-97bf-9ae3a315e2b2
base="$(basename "$PLAN" .tsv)"
say(){ echo "[$(date '+%m-%d %H:%M:%S')] DEL-SUP: $*" | tee -a "$HERE/journal/delete.log"; }
a=0
while :; do
  a=$((a+1))
  if ! findmnt -rn /media/buengenio/FUCKING_BIG >/dev/null; then
    for i in $(seq 1 60); do blkid -U "$UUID" >/dev/null 2>&1 && break; sleep 10; done
    for m in /media/buengenio/FUCKING_BIG /mnt/bd1a /mnt/bd1b /mnt/bd1c /mnt/bd500; do
      cur="$(findmnt -rn -o SOURCE "$m" 2>/dev/null || true)"
      [ -n "$cur" ] && [ ! -e "$cur" ] && sudo umount -l "$m" 2>/dev/null
    done
    "$HERE/bd-mount.sh" >/dev/null 2>&1
  fi
  sudo mount -o remount,rw /media/buengenio/FUCKING_BIG 2>/dev/null || true
  say "starting $base (verify=$VERIFY, attempt $a)"
  sudo python3 "$HERE/bd-delete.py" --plan "$PLAN" --verify "$VERIFY" --apply >>"$HERE/journal/delete-$base.out" 2>&1
  rc=$?
  say "$base rc=$rc (attempt $a)"
  [ "$rc" -eq 0 ] && break
  [ "$a" -ge 100 ] && { say "giving up on $base"; exit 1; }
  sleep 15
done
sudo mount -o remount,ro /media/buengenio/FUCKING_BIG 2>/dev/null || true
say "$base complete; archive back to read-only"
