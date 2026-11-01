#!/bin/bash
# Stage unique small-drive files into FUCKING_BIG/Archive/_staging/<drive>/ (layout preserved).
# Resumable: re-run the same command; rsync skips what is already there.
# Journal: journal/stage-<drive>.log  (rsync log, one line per file: "<bytes>\t<relpath>")
set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DST=/media/buengenio/FUCKING_BIG/Archive/_staging
declare -A SRC=( [sda4]="/media/buengenio/sda4/" [stuff]="/media/buengenio/Stuff/" [verbatim]="/media/buengenio/VERBATIM HD/" [sdc1]="/mnt/sdc1/" )
one() {  # $1 = drive
  local d="$1"
  sudo mkdir -p "$DST/$d"
  echo "[$(date +%T)] $d start" >> "$HERE/journal/stage-$d.status"
  sudo rsync -a --partial --inplace --from0 --files-from="$HERE/stage/$d.list0" \
       --log-file="$HERE/journal/stage-$d.log" --log-file-format='%l	%n' \
       --info=progress2 --no-motd "${SRC[$d]}" "$DST/$d/" > "$HERE/journal/stage-$d.progress" 2> "$HERE/journal/stage-$d.err"
  echo "[$(date +%T)] $d done rc=$?" >> "$HERE/journal/stage-$d.status"
}
case "${1:-all}" in
  all)  ( one sda4; one stuff ) & one verbatim & one sdc1 & wait ;;
  *)    one "$1" ;;
esac
