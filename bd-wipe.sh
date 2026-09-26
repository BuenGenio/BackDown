#!/bin/bash
# Wipe the emptied laptop drives and reformat each as ONE ext4 partition (mirror drives).
#
# Safety model — nothing is addressed by /dev/sdX:
#   1. each target is resolved from its ATA model+serial (/dev/disk/by-id/ata-*), stable across re-plugs
#   2. the target must carry EXACTLY the expected filesystem UUIDs (proof it is the disk we surveyed),
#      or already be converted (single partition with the new label) -> skipped
#   3. no PROTECTED filesystem UUID (FUCKING_BIG, BD-500) may live on a target device
#   4. the kernel-reported disk size must match the drive's real capacity within 1 %
#      (the JMicron JMS567 enclosure reports 115.5 PiB for every bay after a power cycle)
#   5. no copy/hash job may be running
#
# Preconditions were met on 2026-09-22: every file on these disks is sha256-verified present on
# FUCKING_BIG (index/staging-verify.json, REPORT.md §4.3). THIS IS IRREVERSIBLE — run it yourself.
#
# History: 2026-09-22 run 1 converted HGST -> BD-1T-A and WD -> BD-1T-B; it stopped before the
# Seagate because the unmount loop split "VERBATIM HD" on the space (fixed below) — re-run does the rest.
set -eu

# by-id link name -> new label | mount point | real bytes | expected fs UUIDs on it (space-separated)
declare -A TARGET=(
  [ata-HGST_HTS721010A9E630_JR10004M172WLF]="BD-1T-A|/mnt/bd1a|1000204886016|E7E0-D3E6 7475f7fa-98f5-44f6-b420-d8733fd4a8f2 ab5e7ca9-8c91-4590-8ecf-7f962ef41c86 bf983514-1716-4e9e-b0ad-541c21e9d6e1"
  [ata-WDC_WD10JPVX-60JC3T0_WD-WXJ1A63C9646]="BD-1T-B|/mnt/bd1b|1000204886016|01CEFCD7A901D1C0 5490-4FCF"
  [ata-ST1000LM024_HN-M101MBB_S2RXJ9CC504324]="BD-1T-C|/mnt/bd1c|1000204886016|1110-0E4D"
)
PROTECTED="c7e9ddf3-6073-43c5-97bf-9ae3a315e2b2 4166832a-4309-45b4-a5db-11bad1aafe6c"   # FUCKING_BIG, BD-500

die() { echo "ABORT: $*" >&2; exit 1; }
sorted() { tr ' ' '\n' | grep -v '^$' | sort | tr '\n' ' ' | sed 's/ $//'; }

if pgrep -fa "rsync|bd-hash" | grep -v pgrep | grep -qv "bash -c"; then die "copy/hash jobs still running"; fi

# ---- resolve + verify every target before touching any of them
declare -A DEV TODO
for id in "${!TARGET[@]}"; do
  link="/dev/disk/by-id/$id"; [[ -e "$link" ]] || die "$id not present"
  dev="$(readlink -f "$link")"; DEV[$id]="$dev"
  IFS='|' read -r label mnt realbytes uuids <<< "${TARGET[$id]}"

  kbytes="$(sudo blockdev --getsize64 "$dev")"
  if (( kbytes > realbytes + realbytes/100 || kbytes < realbytes - realbytes/100 )); then
    die "$dev ($id) reports $(numfmt --to=iec "$kbytes") but the drive is $(numfmt --to=iec "$realbytes"). Enclosure is lying — power-cycle it (remove DC power), re-run."
  fi
  have="$(lsblk -nro UUID "$dev" | sorted)"; want="$(sorted <<< "$uuids")"
  labels="$(lsblk -nro LABEL "$dev" | sorted)"
  for p in $PROTECTED; do grep -q "$p" <<< "$have" && die "$dev carries protected fs $p"; done
  if [[ "$labels" == "$label" && "$(lsblk -nro TYPE "$dev" | grep -c part)" == 1 ]]; then
    echo "already converted: $dev = $id -> $label (fs $have) — skipping"; TODO[$id]=0; continue
  fi
  [[ "$have" == "$want" ]] || die "$dev ($id) carries fs UUIDs [$have], expected [$want] — not the surveyed disk"
  echo "verified: $dev = $id -> $label ($(numfmt --to=iec "$kbytes"), fs: $have)"; TODO[$id]=1
done
for p in $PROTECTED; do
  src="$(findmnt -rn -o SOURCE "UUID=$p" 2>/dev/null || true)"
  [[ -n "$src" ]] || die "protected fs $p is not mounted — refusing to proceed blind"
  for id in "${!TARGET[@]}"; do [[ "$src" == "${DEV[$id]}"* ]] && die "protected fs $p is on target ${DEV[$id]}"; done
done
n=0; for id in "${!TARGET[@]}"; do (( TODO[$id] )) && n=$((n+1)); done
(( n > 0 )) || { echo "nothing to do"; exit 0; }

echo; echo "All checks passed. Wiping $n drive(s) in 10 s — Ctrl-C to abort."; sleep 10

# ---- unmount (space-safe), then wipe
for id in "${!TARGET[@]}"; do
  (( TODO[$id] )) || continue
  dev="${DEV[$id]}"
  while IFS= read -r m; do
    [[ -n "$m" ]] || continue
    sudo umount "$m" || die "cannot unmount $m"
    echo "unmounted $m"
  done < <(lsblk -nl -o MOUNTPOINT "$dev")
done
for id in "${!TARGET[@]}"; do
  (( TODO[$id] )) || continue
  dev="${DEV[$id]}"; IFS='|' read -r label mnt realbytes uuids <<< "${TARGET[$id]}"
  echo "== wiping $dev ($id) -> $label"
  while IFS= read -r part; do [[ -n "$part" ]] && sudo wipefs -a "$part" >/dev/null; done < <(lsblk -nl -o PATH "$dev" | tail -n +2)
  sudo wipefs -a "$dev" >/dev/null
  sudo sgdisk -Z "$dev" >/dev/null 2>&1 || true
  sudo parted -s "$dev" mklabel gpt mkpart primary ext4 1MiB 100%
  sleep 1; sudo partprobe "$dev"; sleep 1
  part="$(lsblk -nl -o PATH "$dev" | sed -n 2p)"
  sudo wipefs -a "$part" >/dev/null
  sudo mkfs.ext4 -q -F -L "$label" -m 0 "$part"
  sudo mkdir -p "$mnt"; sudo mount "$part" "$mnt"; sudo chown buengenio:buengenio "$mnt"
done
lsblk -o NAME,SIZE,FSTYPE,LABEL,UUID,MOUNTPOINT $(printf '%s ' "${DEV[@]}")
df -h /mnt/bd1a /mnt/bd1b /mnt/bd1c 2>/dev/null || true
