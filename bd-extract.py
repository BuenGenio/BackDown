#!/usr/bin/env python3
"""batch 2b — 'extract media to root, thin hercules'. Run under sudo (archive must be read-write).

For each EXTRACT set in the plan (personal media with no clean root copy yet):
  1. MOVE the keeper (a nested media path, e.g. hercules-sdb/.../files/Music/X/y.mp3) to a clean
     ROOT category dir (Music/X/y.mp3) by same-filesystem rename. This is the surviving copy.
  2. Then DELETE every other copy (the victims), each gated on the moved copy existing + matching.
Category = the first of Photos/Music/Documents/Videos/Movies/Shows in the keeper path; the root
destination is that segment onward. Moves and deletes are journaled (journal/extract.log,
journal/deleted.log). Resumable: a keeper already at its destination is not re-moved.

  bd-extract.py            dry run: show moves + deletes, flag collisions
  bd-extract.py --apply    do it
"""
import argparse, collections, hashlib, os, sys
from pathlib import Path

HERE = Path(__file__).parent; IDX = HERE / "index"; BIG = "/media/buengenio/FUCKING_BIG/"
PLAN = IDX / "batch2b-extract.tsv"
XJOURNAL = HERE / "journal" / "extract.log"; DJOURNAL = HERE / "journal" / "deleted.log"
MEDIA = ("Photos", "Music", "Documents", "Videos", "Movies", "Shows")

def dest_of(keeper_rel):
    segs = keeper_rel.split("/")
    for i, s in enumerate(segs):
        if s in MEDIA:
            return "/".join([s] + segs[i + 1:])   # e.g. Music/Unsorted/.../y.mp3
    return None

def sha256(p):
    h = hashlib.sha256()
    with open(p, "rb", buffering=0) as f:
        for b in iter(lambda: f.read(8 << 20), b""): h.update(b)
    return h.hexdigest()

def main():
    ap = argparse.ArgumentParser(); ap.add_argument("--apply", action="store_true"); a = ap.parse_args()
    # group victims by keeper
    groups = collections.defaultdict(lambda: {"sha": None, "size": 0, "victims": []})
    for line in open(PLAN, errors="surrogateescape"):
        sha, size, action, keeper, victim = line.rstrip("\n").split("\t")
        g = groups[keeper]; g["sha"] = sha; g["size"] = int(size); g["victims"].append(victim)
    dest_taken = {}   # dest_rel -> sha (collision guard)
    moved = deleted = skipped = 0; mv_bytes = del_bytes = 0
    xf = open(XJOURNAL, "a", errors="surrogateescape") if a.apply else None
    df = open(DJOURNAL, "a", errors="surrogateescape") if a.apply else None
    for keeper, g in groups.items():
        sha, size, victims = g["sha"], g["size"], g["victims"]
        dest = dest_of(keeper)
        if not dest:
            print(f"SKIP no-category {keeper}"); skipped += 1; continue
        kabs, dabs = BIG + keeper, BIG + dest
        # collision: two different files want the same root path
        if dest in dest_taken and dest_taken[dest] != sha:
            print(f"SKIP dest-collision {dest}"); skipped += 1; continue
        dest_taken[dest] = sha
        # 1. move keeper -> dest (unless already done)
        if os.path.lexists(dabs):
            if os.path.lexists(kabs) and os.path.getsize(kabs) == size and os.path.getsize(dabs) == size:
                # dest already present (prior run or pre-existing) — treat keeper source as a victim
                victims = victims + [keeper]
            # else assume dest is the moved copy; fall through to delete victims
        else:
            if not os.path.lexists(kabs):
                print(f"SKIP keeper-gone {keeper}"); skipped += 1; continue
            if os.path.getsize(kabs) != size:
                print(f"SKIP keeper-size {keeper}"); skipped += 1; continue
            print(f"MOVE {keeper}  ->  {dest}")
            if a.apply:
                os.makedirs(os.path.dirname(dabs), exist_ok=True)
                os.rename(kabs, dabs); xf.write(f"MV {sha} {size} {keeper}\t{dest}\n"); xf.flush()
            moved += 1; mv_bytes += size
        # 2. delete victims, gated on the surviving root copy
        if not (os.path.lexists(dabs) or not a.apply):
            print(f"SKIP no-survivor-for {dest}"); skipped += 1; continue
        for v in victims:
            vabs = BIG + v
            try: vsz = os.path.getsize(vabs)
            except OSError: continue
            if vsz != size: print(f"SKIP victim-size {v}"); skipped += 1; continue
            if a.apply:
                # survivor must exist + match size before we remove another copy
                if not (os.path.lexists(dabs) and os.path.getsize(dabs) == size):
                    print(f"SKIP survivor-missing {v}"); skipped += 1; continue
                os.unlink(vabs); df.write(f"DEL {sha} {size} {v}\t(extracted->{dest})\n"); df.flush()
            deleted += 1; del_bytes += size
    if xf: xf.close()
    if df: df.close()
    print(f"\n{'APPLIED' if a.apply else 'DRY RUN'}: moved {moved:,} ({mv_bytes/1e9:.1f} GB) to root, "
          f"deleted {deleted:,} ({del_bytes/1e9:.1f} GB) copies, {skipped:,} skipped")

if __name__ == "__main__": main()
