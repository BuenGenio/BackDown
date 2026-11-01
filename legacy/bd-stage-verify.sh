#!/bin/bash
# 1) index the staged tree  2) hash it (resumable)  3) compare each staged sha to its source sha
set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ST=/media/buengenio/FUCKING_BIG/Archive/_staging
sudo find "$ST" -xdev -type f -printf '%s\t%T@\t%p\n' > "$HERE/index/staging.tsv"
echo "staged files: $(wc -l < "$HERE/index/staging.tsv")"
sudo python3 "$HERE/bd-hash.py" --from-tsv "$HERE/index/staging.tsv" --workers 1 > "$HERE/index/hash-staging.log" 2>&1
python3 - <<'PY'
import sqlite3, json
from pathlib import Path
HERE=Path("/home/buengenio/Projects/BackDown"); IDX=HERE/"index"
ST="/media/buengenio/FUCKING_BIG/Archive/_staging/"
SRC={"sda4":"/media/buengenio/sda4/","stuff":"/media/buengenio/Stuff/","verbatim":"/media/buengenio/VERBATIM HD/","sdc1":"/mnt/sdc1/"}
con=sqlite3.connect(f"file:{IDX/'hashes.db'}?mode=ro",uri=True)
sha={p:s for p,s in con.execute("select path,sha256 from h")}
ok=bad=missing=0; badl=[]; per={d:[0,0] for d in SRC}
for line in open(IDX/"staging.tsv",errors="surrogateescape"):
    size,_,p=line.rstrip("\n").split("\t",2)
    rel=p[len(ST):]; d,_,r=rel.partition("/")
    s_dst=sha.get(p); s_src=sha.get(SRC[d]+r)
    if s_dst is None or s_src is None: missing+=1; badl.append(("missing-hash",p)); continue
    if s_dst==s_src: ok+=1; per[d][0]+=int(size); per[d][1]+=1
    else: bad+=1; badl.append(("MISMATCH",p))
print(f"verified identical: {ok:,}   mismatched: {bad}   missing hash: {missing}")
for d,(b,n) in per.items(): print(f"  {d:9} {b/1e9:8.1f} GB {n:8,} files verified")
open(IDX/"staging-verify.txt","w").write("\n".join(f"{k}\t{p}" for k,p in badl))
json.dump({"ok":ok,"bad":bad,"missing":missing,"per":per},open(IDX/"staging-verify.json","w"))
PY
