#!/usr/bin/env python3
"""Build an adaptive, resumable chunk plan for one volume, salvaging a partial crawl.

Chunk kinds (index/<vol>/chunks.list, tab-separated, one per line):
  R <dir>   index <dir> recursively
  F <dir>   index only files directly inside <dir> (for internal nodes)

A directory becomes an R-chunk unless it is known to hold > SPLIT files, in which
case it is an F-chunk and its children are planned recursively. Directories not
seen in the partial crawl are split to at least depth MIN_DEPTH.

If --salvage <old.tsv> is given, every chunk fully covered by the old crawl
(find is depth-first, so everything before the in-flight directory is complete)
is written out as done. Only R-chunks are salvaged; F-chunks are cheap to redo.
"""
import argparse, collections, hashlib, os, sys
from pathlib import Path

ap = argparse.ArgumentParser()
ap.add_argument("vol"); ap.add_argument("root")
ap.add_argument("--salvage", help="partial tsv from an earlier crawl")
ap.add_argument("--split", type=int, default=300_000)
ap.add_argument("--min-depth", type=int, default=2)
ap.add_argument("--index", default=str(Path(__file__).parent / "index"))
a = ap.parse_args()

root = a.root.rstrip("/")
cdir = Path(a.index) / a.vol / "chunks"; cdir.mkdir(parents=True, exist_ok=True)

# --- 1. count files per directory from the partial crawl (and note the in-flight dir)
count = collections.Counter()
lines_by_dir = collections.defaultdict(list) if a.salvage else None
last_dir = None
if a.salvage and os.path.exists(a.salvage):
    with open(a.salvage, "rb") as f:
        for raw in f:
            if not raw.endswith(b"\n"): break            # truncated final line
            try: size, mt, path = raw[:-1].split(b"\t", 2)
            except ValueError: continue
            d = path.rsplit(b"/", 1)[0]
            last_dir = d
            lines_by_dir[d].append(raw)
            # bump every ancestor up to root
            while len(d) >= len(root):
                count[d] += 1
                if d == root.encode(): break
                d = d.rsplit(b"/", 1)[0]
inflight = set()
if last_dir:
    d = last_dir
    while len(d) >= len(root):
        inflight.add(d)
        if d == root.encode(): break
        d = d.rsplit(b"/", 1)[0]
    print(f"in-flight (incomplete) path: {last_dir.decode(errors='replace')}", file=sys.stderr)

# --- 2. plan
def listdirs(d):
    try:
        with os.scandir(d) as it:
            return sorted(e.path for e in it if e.is_dir(follow_symlinks=False))
    except PermissionError:
        # fall back to sudo listing
        import subprocess
        out = subprocess.run(["sudo", "find", d, "-xdev", "-mindepth", "1", "-maxdepth", "1", "-type", "d", "-print0"],
                             capture_output=True).stdout
        return sorted(p.decode(errors="surrogateescape") for p in out.split(b"\0") if p)

plan = []
def visit(d, depth):
    seen = count.get(d.encode(errors="surrogateescape"), 0)
    must_split = seen > a.split or (seen == 0 and depth < a.min_depth)
    if depth == 0: must_split = True
    if not must_split:
        plan.append(("R", d)); return
    plan.append(("F", d))
    for c in listdirs(d):
        visit(c, depth + 1)
visit(root, 0)

with open(Path(a.index) / a.vol / "chunks.list", "w") as f:
    for k, d in plan: f.write(f"{k}\t{d}\n")
print(f"{a.vol}: {sum(1 for k,_ in plan if k=='R')} R-chunks, {sum(1 for k,_ in plan if k=='F')} F-chunks", file=sys.stderr)

# --- 3. salvage complete R-chunks
def cid(kind, d): return hashlib.sha1(f"{kind}\t{d}".encode(errors="surrogateescape")).hexdigest()[:16]
if lines_by_dir is not None:
    salvaged = 0; sfiles = 0
    # group old lines by chunk: for each R-chunk dir, collect lines of all dirs under it
    rchunks = [d for k, d in plan if k == "R"]
    rchunks_b = sorted((d.encode(errors="surrogateescape") for d in rchunks), reverse=True)
    def owner(d):
        # longest R-chunk prefix of d
        for r in rchunks_b:
            if d == r or d.startswith(r + b"/"): return r
        return None
    bucket = collections.defaultdict(list)
    for d, ls in lines_by_dir.items():
        o = owner(d)
        if o is not None: bucket[o].extend(ls)
    for rb, ls in bucket.items():
        if rb in inflight: continue                   # was being crawled when killed
        if count.get(rb, 0) == 0: continue
        d = rb.decode(errors="surrogateescape"); i = cid("R", d)
        out = cdir / i
        with open(str(out) + ".tsv", "wb") as f: f.writelines(ls)
        (cdir / f"{i}.path").write_text(f"R\t{d}\n", errors="surrogateescape")
        (cdir / f"{i}.done").touch()
        salvaged += 1; sfiles += len(ls)
    print(f"salvaged {salvaged} complete chunks, {sfiles:,} files", file=sys.stderr)
