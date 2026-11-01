#!/usr/bin/env python3
"""Analyze the file index TSVs: sizes per top-level dir, cross-volume duplicates."""
import os, sys, collections
from pathlib import Path

S = Path(__file__).parent / "index"
VOLS = {
    "stuff": "/media/buengenio/Stuff",
    "sda4": "/media/buengenio/sda4",
    "verbatim": "/media/buengenio/VERBATIM HD",
    "big": "/media/buengenio/FUCKING_BIG",
    "sdc1": "/mnt/sdc1",
    "v22": "/media/buengenio/22",
}

def human(n):
    for u in ["B", "K", "M", "G", "T"]:
        if n < 1024: return f"{n:.1f}{u}"
        n /= 1024
    return f"{n:.1f}P"

# load — stream; aggregate sizes as we go, keep only >=1MB rows for pair analysis
MIN = 1 * 2**20
files = []  # (vol, relpath, size, mtime) for files >= MIN only
by_top = collections.defaultdict(lambda: [0, 0])
by_top2 = collections.defaultdict(lambda: [0, 0])
total_n = total_b = 0
for vol, root in VOLS.items():
    p = S / f"{vol}.tsv"
    if not p.exists(): continue
    with open(p, "rb") as f:
        for raw in f:
            try:
                size, mtime, path = raw.rstrip(b"\n").split(b"\t", 2)
            except ValueError:
                continue
            size = int(size); total_n += 1; total_b += size
            rel = path[len(root):].lstrip(b"/")
            parts = rel.split(b"/")
            top = parts[0].decode(errors="replace") if len(parts) > 1 else "(root files)"
            by_top[(vol, top)][0] += size; by_top[(vol, top)][1] += 1
            if len(parts) > 2:
                k = (vol, top, parts[1].decode(errors="replace"))
                by_top2[k][0] += size; by_top2[k][1] += 1
            if size >= MIN:
                files.append((vol, rel.decode(errors="replace"), size, float(mtime)))

print(f"Total files indexed: {total_n:,}  total bytes: {human(total_b)}  (>=1MB rows kept: {len(files):,})\n")

# --- per volume / top-level dir sizes
print("=" * 70)
print("SIZE BY VOLUME / TOP-LEVEL DIRECTORY (depth 1, then depth 2 for big ones)")
print("=" * 70)
for vol in VOLS:
    rows = sorted([(k, v) for k, v in by_top.items() if k[0] == vol], key=lambda x: -x[1][0])
    tot = sum(v[0] for _, v in rows)
    print(f"\n## {vol} ({VOLS[vol]}) — {human(tot)}, {sum(v[1] for _, v in rows):,} files")
    for (v, top), (sz, n) in rows:
        print(f"  {human(sz):>8}  {n:>8,}  {top}")
        if sz > 40 * 2**30:
            sub = sorted([(k, vv) for k, vv in by_top2.items() if k[0] == vol and k[1] == top], key=lambda x: -x[1][0])[:12]
            for (_, _, s2), (sz2, n2) in sub:
                print(f"           {human(sz2):>8}  {n2:>8,}    └ {s2}")

# --- cross-volume duplicates by (basename, size), files >= 1MB
print("\n" + "=" * 70)
print("CROSS-LOCATION DUPLICATES (same basename + size, >=1MB) — grouped by directory pair")
print("=" * 70)
key2 = collections.defaultdict(list)
for vol, rel, size, mt in files:
    if size >= MIN:
        key2[(os.path.basename(rel), size)].append((vol, rel))
dup_bytes = 0
pair_bytes = collections.Counter()
pair_count = collections.Counter()
for (name, size), locs in key2.items():
    if len(locs) < 2: continue
    dup_bytes += size * (len(locs) - 1)
    # directory pairs (by top 2 levels)
    dirs = sorted(set((v, "/".join(r.split("/")[:2])) for v, r in locs))
    for i in range(len(dirs)):
        for j in range(i + 1, len(dirs)):
            pair_bytes[(dirs[i], dirs[j])] += size
            pair_count[(dirs[i], dirs[j])] += 1
print(f"Redundant bytes (>=1MB files, name+size match): {human(dup_bytes)}\n")
print("Top directory pairs sharing files:")
for (a, b), sz in pair_bytes.most_common(60):
    print(f"  {human(sz):>8} {pair_count[(a,b)]:>6} files   {a[0]}:{a[1]}  <->  {b[0]}:{b[1]}")

# --- exact same relative path across volumes (mirror detection)
print("\n" + "=" * 70)
print("MIRRORED TREES: identical relative path+size on multiple volumes")
print("=" * 70)
byrel = collections.defaultdict(list)
for vol, rel, size, mt in files:
    byrel[(rel, size)].append(vol)
mirror = collections.Counter(); mirror_n = collections.Counter()
for (rel, size), vols in byrel.items():
    if len(set(vols)) > 1:
        top = rel.split("/")[0]
        k = (top, tuple(sorted(set(vols))))
        mirror[k] += size; mirror_n[k] += 1
for (top, vols), sz in mirror.most_common(40):
    print(f"  {human(sz):>8} {mirror_n[(top,vols)]:>8,} files  '{top}' on {'+'.join(vols)}")
