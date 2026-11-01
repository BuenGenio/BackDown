#!/usr/bin/env python3
"""Build rsync file lists for staging every small-drive file that has no sha256 twin
on the big drive. Files present on two small drives (but not on big) are staged once,
from the first drive in ORDER.

Output: stage/<drive>.list0   NUL-separated relative paths for rsync --files-from --from0
        stage/plan.json       bytes/files per drive
"""
import json, sqlite3, collections
from pathlib import Path
HERE = Path(__file__).parent; IDX = HERE / "index"; ST = HERE / "stage"; ST.mkdir(exist_ok=True)
SMALL = {"sda4": "/media/buengenio/sda4/", "stuff": "/media/buengenio/Stuff/",
         "verbatim": "/media/buengenio/VERBATIM HD/", "sdc1": "/mnt/sdc1/"}
ORDER = ["sda4", "stuff", "verbatim", "sdc1"]
con = sqlite3.connect(f"file:{IDX/'hashes.db'}?mode=ro", uri=True)
sha_of = {}
for p, sha in con.execute("SELECT path, sha256 FROM h"): sha_of[p] = sha
rows = collections.defaultdict(list)
for line in open(IDX / "unverified.tsv", "rb"):
    size, path = line.rstrip(b"\n").split(b"\t", 1)
    p = path.decode("utf-8", "surrogateescape")
    for v, root in SMALL.items():
        if p.startswith(root): rows[v].append((int(size), p, root)); break
staged = set(); plan = {}
for v in ORDER:
    out = open(ST / f"{v}.list0", "wb"); n = b = skipped = 0; mine = set()
    for size, p, root in sorted(rows[v], key=lambda x: x[1]):
        sha = sha_of.get(p)
        if sha and sha in staged: skipped += size; continue   # already staged from an earlier drive
        if sha: mine.add(sha)
        out.write(p[len(root):].encode("utf-8", "surrogateescape") + b"\0"); n += 1; b += size
    out.close(); staged |= mine; plan[v] = {"files": n, "bytes": b, "skipped_dup_bytes": skipped, "src": SMALL[v]}
    print(f"{v:9} {n:8,} files  {b/1e9:7.1f} GB   (skipped {skipped/1e9:.1f} GB already staged from another drive)")
json.dump(plan, open(ST / "plan.json", "w"), indent=1)
print(f"TOTAL {sum(x['bytes'] for x in plan.values())/1e9:.1f} GB, {sum(x['files'] for x in plan.values()):,} files")
