#!/usr/bin/env python3
"""Find, on the big drive, candidate twins of every hashed small-drive file.

Stage 1 (candidates):  bd-match.py candidates  > index/big-candidates.txt
    streams index/big.tsv once; emits big-drive paths whose (basename, size)
    matches a hashed small-drive file. Those are what bd-hash.py should hash next.
    With --size-only also emits size-only matches for small files >= --min-size
    that had no name match (catches renamed copies of large files).

Stage 2 (verify):      bd-match.py verify
    after hashing candidates, joins by sha256 and writes:
      index/verify-summary.md      per small volume / top dir: verified vs unverified
      index/unverified.tsv         small-drive files with no hash twin on big
      index/verified.tsv           small path -> one big twin path
"""
import argparse, collections, os, sqlite3, sys
from pathlib import Path

HERE = Path(__file__).parent; IDX = HERE / "index"
BIG = "/media/buengenio/FUCKING_BIG/"
SMALL = {"stuff": "/media/buengenio/Stuff/", "sda4": "/media/buengenio/sda4/",
         "sdc1": "/mnt/sdc1/", "verbatim": "/media/buengenio/VERBATIM HD/"}

def small_rows(con):
    q = " OR ".join("path LIKE ?" for _ in SMALL)
    return con.execute(f"SELECT path,size,sha256 FROM h WHERE {q}", [p + "%" for p in SMALL.values()])

def vol_of(p):
    for v, r in SMALL.items():
        if p.startswith(r): return v
    return "?"

def human(n):
    for u in "BKMGT":
        if n < 1024: return f"{n:.1f}{u}"
        n /= 1024
    return f"{n:.1f}P"

def candidates(a):
    con = sqlite3.connect(f"file:{IDX/'hashes.db'}?mode=ro", uri=True)
    want = set(); sizes_big = set()
    for p, s, _ in small_rows(con):
        want.add((os.path.basename(p), s))
        if a.size_only and s >= a.min_size: sizes_big.add(s)
    print(f"{len(want):,} distinct (name,size) keys; {len(sizes_big):,} large sizes for size-only", file=sys.stderr)
    n1 = n2 = 0
    with open(IDX / "big.tsv", "rb") as f:
        for raw in f:
            try: s, _, p = raw.rstrip(b"\n").split(b"\t", 2)
            except ValueError: continue
            s = int(s)
            if s == 0: continue
            name = os.path.basename(p).decode(errors="surrogateescape")
            if (name, s) in want:
                sys.stdout.buffer.write(p + b"\n"); n1 += 1
            elif s in sizes_big:
                sys.stdout.buffer.write(p + b"\n"); n2 += 1
    print(f"emitted {n1:,} name+size candidates, {n2:,} size-only candidates", file=sys.stderr)

def verify(a):
    con = sqlite3.connect(f"file:{IDX/'hashes.db'}?mode=ro", uri=True)
    # sha -> one big path
    big_by_sha = {}
    for p, sha in con.execute("SELECT path, sha256 FROM h WHERE path LIKE ?", (BIG + "%",)):
        big_by_sha.setdefault(sha, p)
    # also allow a twin on a *different* small drive (still a second copy, but weaker)
    stats = collections.defaultdict(lambda: [0, 0, 0, 0])  # (vol, top) -> [vfiles, vbytes, ufiles, ubytes]
    unv = open(IDX / "unverified.tsv", "w", errors="surrogateescape")
    ver = open(IDX / "verified.tsv", "w", errors="surrogateescape")
    for p, s, sha in small_rows(con):
        v = vol_of(p); rel = p[len(SMALL[v]):]; top = rel.split("/", 1)[0] if "/" in rel else "(root files)"
        twin = big_by_sha.get(sha)
        k = stats[(v, top)]
        if twin:
            k[0] += 1; k[1] += s; ver.write(f"{p}\t{twin}\n")
        else:
            k[2] += 1; k[3] += s; unv.write(f"{s}\t{p}\n")
    unv.close(); ver.close()
    with open(IDX / "verify-summary.md", "w") as out:
        out.write("# Verification: small-drive files with a sha256 twin on the 18TB drive\n\n")
        for v in SMALL:
            rows = sorted(((t, k) for (vv, t), k in stats.items() if vv == v), key=lambda x: -(x[1][1] + x[1][3]))
            tv = sum(k[1] for _, k in rows); tu = sum(k[3] for _, k in rows)
            fv = sum(k[0] for _, k in rows); fu = sum(k[2] for _, k in rows)
            pct = 100 * tv / max(1, tv + tu)
            out.write(f"## {v} — {pct:.1f}% of bytes verified ({human(tv)} verified, {human(tu)} NOT; {fv:,} / {fu:,} files)\n\n")
            out.write("| Top dir | verified | not verified | files not verified |\n|---|---|---|---|\n")
            for t, k in rows:
                out.write(f"| `{t}` | {human(k[1])} | **{human(k[3])}** | {k[2]:,} |\n")
            out.write("\n")
    import json
    json.dump({"vols": {v: {"tops": [{"top": t, "vfiles": k[0], "vbytes": k[1], "ufiles": k[2], "ubytes": k[3]}
                                    for (vv, t), k in sorted(stats.items(), key=lambda x: -(x[1][1] + x[1][3])) if vv == v]}
                        for v in SMALL}}, open(IDX / "verify.json", "w"))
    print(open(IDX / "verify-summary.md").read())

ap = argparse.ArgumentParser(); ap.add_argument("cmd", choices=["candidates", "verify"])
ap.add_argument("--size-only", action="store_true"); ap.add_argument("--min-size", type=int, default=1 << 20)
a = ap.parse_args(); {"candidates": candidates, "verify": verify}[a.cmd](a)
