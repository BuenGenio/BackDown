#!/usr/bin/env python3
"""Execute deletions from delete-plan.tsv — safely, journaled, resumable. Run under sudo.

  bd-delete.py --tops SAMSUNG_ERNESTO,.Trash-1000            dry run (default): show what would go
  bd-delete.py --tops SAMSUNG_ERNESTO,.Trash-1000 --apply    actually delete

Safety, per victim:
  1. a keeper (surviving copy) must exist and re-hash to the recorded sha256 → proves an intact
     identical copy remains BEFORE the victim is removed. Keeper hashes are cached (hash once,
     reuse for all its victims).
  2. the victim must still exist; if its size no longer matches the plan it is SKIPPED (changed).
  3. never deletes if the keeper is missing or mismatches — logs SKIP and moves on.
Every removal is appended to journal/deleted.log as: DEL <sha> <size> <victim>\t(keeper=<keeper>)
Re-running skips victims already in the journal or already gone, so a bus drop just needs a re-run.
Requires the archive mounted READ-WRITE (this is the only writing step).
"""
import argparse, hashlib, os, sys, time
from pathlib import Path

HERE = Path(__file__).parent; IDX = HERE / "index"; BIG = "/media/buengenio/FUCKING_BIG/"
JOURNAL = HERE / "journal" / "deleted.log"

def sha256(p):
    h = hashlib.sha256()
    with open(p, "rb", buffering=0) as f:
        for b in iter(lambda: f.read(8 << 20), b""): h.update(b)
    return h.hexdigest()

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tops", default="", help="comma-separated victim top-level dirs to act on")
    ap.add_argument("--prefix", default="", help="comma-separated victim path prefixes (finer than --tops)")
    ap.add_argument("--apply", action="store_true")
    a = ap.parse_args()
    tops = set(t for t in a.tops.split(",") if t)
    prefixes = tuple(p for p in a.prefix.split(",") if p)
    if not tops and not prefixes: sys.exit("give --tops or --prefix")
    if a.apply and not os.access(BIG, os.W_OK):
        # not fatal (root may still write), but warn if archive looks read-only
        pass
    already = set()
    if JOURNAL.exists():
        for l in open(JOURNAL, errors="surrogateescape"):
            if l.startswith("DEL "):
                already.add(l.split("\t", 1)[0].split(" ", 3)[3].rstrip("\n"))
    keeper_ok = {}   # keeper_rel -> True/False (verified identical)
    jf = open(JOURNAL, "a", errors="surrogateescape") if a.apply else None
    n_del = n_skip = 0; freed = 0; t0 = time.time()
    for line in open(IDX / "delete-plan.tsv", errors="surrogateescape"):
        sha, size, action, keeper, victim = line.rstrip("\n").split("\t")
        size = int(size)
        if tops and victim.split("/")[0] not in tops: continue
        if prefixes and not victim.startswith(prefixes): continue
        if victim in already: continue
        vabs = BIG + victim; kabs = BIG + keeper
        try: vst = os.lstat(vabs)
        except OSError: continue                      # already gone
        if vst.st_size != size:
            print(f"SKIP changed-size {victim}"); n_skip += 1; continue
        # verify keeper (cache per keeper)
        ok = keeper_ok.get(keeper)
        if ok is None:
            try: ok = (os.path.exists(kabs) and sha256(kabs) == sha)
            except OSError: ok = False
            keeper_ok[keeper] = ok
        if not ok:
            print(f"SKIP keeper-unverified {victim}  (keeper={keeper})"); n_skip += 1; continue
        if a.apply:
            try:
                os.unlink(vabs); jf.write(f"DEL {sha} {size} {victim}\t(keeper={keeper})\n"); jf.flush()
            except OSError as e:
                print(f"SKIP unlink-failed {victim}: {e}"); n_skip += 1; continue
        n_del += 1; freed += size
        if n_del % 500 == 0:
            print(f"  {'deleted' if a.apply else 'would delete'} {n_del:,}  {freed/1e9:.1f} GB  {n_del/max(time.time()-t0,1):.0f}/s  skip={n_skip}", flush=True)
    if jf: jf.close()
    print(f"{'DELETED' if a.apply else 'DRY RUN — would delete'} {n_del:,} files, {freed/1e12:.3f} TB; {n_skip} skipped")

if __name__ == "__main__": main()
