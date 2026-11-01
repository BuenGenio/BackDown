#!/bin/bash
# Mount every BackDown volume by filesystem UUID (never by /dev/sdX — the enclosure reorders bays).
# Idempotent: already-mounted volumes are skipped. Volumes that are gone (wiped) are reported and skipped.
#   bd-mount.sh          mount all
#   bd-mount.sh status   show what is where
set -u
# UUID | mount point | fs type + options
VOLS=(
  "c7e9ddf3-6073-43c5-97bf-9ae3a315e2b2|/media/buengenio/FUCKING_BIG|ext4|defaults"
  "4166832a-4309-45b4-a5db-11bad1aafe6c|/mnt/bd500|ext4|defaults"
  "c429fc72-d45b-4ed2-bfee-4a0d86a6face|/mnt/bd1a|ext4|defaults"     # BD-1T-A (ex Stuff/sda4/Recovery, wiped 2026-09-22)
  "fd4260d8-de81-4d75-b288-cbc575896ad0|/mnt/bd1b|ext4|defaults"     # BD-1T-B (ex sdc1 NTFS + 22, wiped 2026-09-22)
  "9ee90c10-3e0e-4a97-9c67-53b5bfcea6f3|/mnt/bd1c|ext4|defaults"     # BD-1T-C (ex VERBATIM HD, wiped 2026-09-22)
)
warn_capacity() {
  for d in /sys/block/sd*; do
    b=$(( $(cat "$d/size") * 512 ))
    if (( b > 30 * 1000**4 )); then
      echo "WARNING: /dev/$(basename "$d") reports $(numfmt --to=iec $b) — enclosure capacity bug; do NOT partition/format until re-plugged" >&2
    fi
  done
}
case "${1:-mount}" in
  status) lsblk -o NAME,SIZE,FSTYPE,LABEL,UUID,MOUNTPOINT | grep -vE "loop|nvme0n1p[1-4]"; warn_capacity ;;
  mount)
    warn_capacity
    for v in "${VOLS[@]}"; do
      IFS='|' read -r uuid mnt fs opts <<< "$v"
      dev="$(blkid -U "$uuid" 2>/dev/null || sudo blkid -U "$uuid" 2>/dev/null || true)"
      if [[ -z "$dev" ]]; then echo "absent   $mnt (UUID $uuid)"; continue; fi
      if mountpoint -q "$mnt"; then
        cur="$(findmnt -rn -o SOURCE "$mnt")"
        if [[ -e "$cur" ]]; then echo "mounted  $mnt  ($cur)"; continue; fi
        echo "stale    $mnt  ($cur vanished) — lazy-unmounting"; sudo umount -l "$mnt"
      fi
      sudo mkdir -p "$mnt"
      if sudo mount -t "$fs" -o "$opts" "UUID=$uuid" "$mnt" 2>/dev/null; then echo "mounted  $mnt  ($dev, $fs $opts)"
      else echo "FAILED   $mnt  ($dev)"; fi
    done ;;
esac
