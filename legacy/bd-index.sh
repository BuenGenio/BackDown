#!/bin/bash
# Resumable file indexer (works with the plan produced by bd-plan.py).
#
# index/<vol>/chunks.list : lines "R<TAB>dir" (recursive) or "F<TAB>dir" (direct files only)
# index/<vol>/chunks/<id>.tsv / .done / .path : one atomically-written chunk each
# index/<vol>.tsv          : merge of all finished chunks
#
# Killing and re-running `run` only repeats chunks without a .done marker.
#
# Usage: bd-index.sh plan   <vol> [--salvage old.tsv]   build/refresh chunk plan
#        bd-index.sh run    [vol ...]                    index (resumable)
#        bd-index.sh status                              progress
#        bd-index.sh merge  [vol ...]                    rebuild <vol>.tsv from finished chunks
set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IDX="$HERE/index"
PAR="${PAR:-1}"   # one seeker per spindle; HDDs thrash with more

declare -A VOL=(
  [stuff]="/media/buengenio/Stuff"
  [sda4]="/media/buengenio/sda4"
  [verbatim]="/media/buengenio/VERBATIM HD"
  [big]="/media/buengenio/FUCKING_BIG"
  [sdc1]="/mnt/sdc1"
  [v22]="/media/buengenio/22"
)

cid() { printf '%s\t%s' "$1" "$2" | sha1sum | cut -c1-16; }

do_chunk() {           # $1=vol $2=kind $3=dir
  local vol="$1" kind="$2" dir="$3" out rc
  out="$IDX/$vol/chunks/$(cid "$kind" "$dir")"
  [[ -f "$out.done" ]] && return 0
  printf '%s\t%s\n' "$kind" "$dir" > "$out.path"
  if [[ "$kind" == F ]]; then
    sudo find "$dir" -xdev -mindepth 1 -maxdepth 1 -type f -printf '%s\t%T@\t%p\n' > "$out.tmp" 2>"$out.err"
  else
    sudo find "$dir" -xdev -type f -printf '%s\t%T@\t%p\n' > "$out.tmp" 2>"$out.err"
  fi
  rc=$?
  if [[ $rc -eq 0 || $rc -eq 1 ]]; then
    mv "$out.tmp" "$out.tsv" && touch "$out.done"
    [[ -s "$out.err" ]] || rm -f "$out.err"
  else
    echo "chunk failed rc=$rc: $kind $dir" >&2; rm -f "$out.tmp"
  fi
}
export -f do_chunk cid; export IDX

run_vol() {
  local vol="$1"
  local root="${VOL[$vol]}"
  local list="$IDX/$vol/chunks.list"
  [[ -d "$root" ]] || { echo "skip $vol: not mounted" >&2; return; }
  [[ -f "$list" ]] || python3 "$HERE/bd-plan.py" "$vol" "$root" --index "$IDX"
  mkdir -p "$IDX/$vol/chunks"
  echo "[$vol] $(wc -l < "$list") chunks, $(ls "$IDX/$vol/chunks"/*.done 2>/dev/null | wc -l) already done"
  # F-chunks first (fast), then R-chunks, PAR at a time
  { grep -P '^F\t' "$list"; grep -P '^R\t' "$list"; } \
    | xargs -d '\n' -P "$PAR" -I{} bash -c 'IFS=$'"'"'\t'"'"' read -r k d <<<"$1"; do_chunk "$0" "$k" "$d"' "$vol" {}
  echo "[$vol] pass complete"
}

status() {
  for vol in "${!VOL[@]}"; do
    local list="$IDX/$vol/chunks.list" cdir="$IDX/$vol/chunks"
    [[ -f "$list" ]] || { printf '%-9s not planned\n' "$vol"; continue; }
    printf '%-9s %6d / %-6d chunks  %12d files\n' "$vol" \
      "$(ls "$cdir"/*.done 2>/dev/null | wc -l)" "$(wc -l < "$list")" \
      "$(cat "$cdir"/*.tsv 2>/dev/null | wc -l)"
  done
}

merge() {
  for vol in "$@"; do
    local cdir="$IDX/$vol/chunks"
    : > "$IDX/$vol.tsv.tmp"
    for d in "$cdir"/*.done; do [[ -f "$d" ]] && cat "${d%.done}.tsv" >> "$IDX/$vol.tsv.tmp"; done
    mv "$IDX/$vol.tsv.tmp" "$IDX/$vol.tsv"
    echo "[$vol] merged $(wc -l < "$IDX/$vol.tsv") files"
  done
}

cmd="${1:-run}"; shift || true
case "$cmd" in
  plan)   v="$1"; shift; python3 "$HERE/bd-plan.py" "$v" "${VOL[$v]}" --index "$IDX" "$@" ;;
  run)    vols=("$@"); [[ ${#vols[@]} -eq 0 ]] && vols=("${!VOL[@]}")
          for v in "${vols[@]}"; do run_vol "$v" & done; wait; merge "${vols[@]}" ;;
  status) status ;;
  merge)  vols=("$@"); [[ ${#vols[@]} -eq 0 ]] && vols=("${!VOL[@]}"); merge "${vols[@]}" ;;
  *) echo "usage: $0 plan|run|status|merge" >&2; exit 2 ;;
esac
