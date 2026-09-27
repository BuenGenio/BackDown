#!/usr/bin/env python3
"""Turn the verified dedup sets into a REVIEWABLE keep/delete plan. Deletes nothing.

Keeper rules (per user, 2026-09-26):
  - personal media (Photos/Music/Documents/Videos/…) -> ONE copy in a clean ROOT category dir
    ("thin hercules": extract to root, drop all others incl. the hercules copy)
  - Leaks/  beats  SAMSUNG_ERNESTO/LEAKS
  - everything else -> hercules-sdb is canonical (keep it), then Archive/, then anything
Outputs (index/):
  delete-plan.tsv     sha \t size \t action \t keeper_rel \t victim_rel     (action: KEEP | EXTRACT)
  delete-plan.md      human summary: reclaim total, by victim top-dir, wholesale-deletable dirs
Nothing is removed here; a separate journaled executor does deletions after review.
"""
import collections, json, os, sqlite3
from pathlib import Path

HERE = Path(__file__).parent; IDX = HERE / "index"; BIG = "/media/buengenio/FUCKING_BIG/"
MEDIA_SEGS = {"Photos", "Music", "Documents", "Videos", "Movies", "Shows"}
ROOT_MEDIA = {"Photos", "Music", "Documents", "Videos", "Movies", "Shows", "Ringtones"}

def segs(rel): return rel.split("/")
def top(rel): return segs(rel)[0]
def is_media(rel): return any(s in MEDIA_SEGS for s in segs(rel))

def media_category(rel):
    for s in segs(rel):
        if s in MEDIA_SEGS: return s
    return "Documents"

def keep_rank(rel):
    """Lower = more preferred to KEEP as the surviving copy."""
    t = top(rel)
    if t in ROOT_MEDIA:      return 0          # already a clean root media dir
    if t == "Leaks":         return 1          # beats SAMSUNG_ERNESTO/LEAKS
    if t == "Archive":       return 2          # mail + Maximal-old canonical home
    if t == "hercules-sdb":  return 3          # canonical server/system master
    return 5                                    # nc-data, X1, Backups-2022, turbojet, 673GB, SAMSUNG_ERNESTO, TimeMachine, Backups…

def main():
    con = sqlite3.connect(f"file:{IDX/'dedup.db'}?mode=ro", uri=True)
    bl = len(BIG.encode())
    plan = open(IDX / "delete-plan.tsv", "w", errors="surrogateescape")
    reclaim = 0; nvictims = 0
    by_victim_top = collections.defaultdict(lambda: [0, 0])   # top-dir -> [bytes, files] deleted
    extract_bytes = extract_n = 0
    victims_by_dir_prefix = collections.defaultdict(lambda: [0, 0])  # depth-2 prefix -> [bytes,files] deletable

    for sha, n, sz in con.execute("""SELECT sha256, count(*), max(size) FROM f WHERE sha256 IS NOT NULL
                                     GROUP BY sha256 HAVING count(*)>1"""):
        rows = [p for (p,) in con.execute("SELECT path FROM f WHERE sha256=?", (sha,))]
        # collapse hard links by inode -> distinct physical copies only
        by_inode = {}
        for rp in rows:
            try: st = os.lstat(rp); k = (st.st_dev, st.st_ino)
            except OSError: k = ("x", rp)
            by_inode.setdefault(k, rp)
        copies = [rp[bl:].decode("utf-8", "replace") for rp in by_inode.values()]
        if len(copies) < 2: continue
        copies.sort(key=lambda r: (keep_rank(r), len(r), r))
        keeper = copies[0]; victims = copies[1:]
        media = is_media(keeper)
        # media not already in a root category dir -> keeper is EXTRACTed to root/<Category>/…
        if media and keep_rank(keeper) != 0:
            action = "EXTRACT"; extract_bytes += sz; extract_n += 1
        else:
            action = "KEEP"
        for v in victims:
            plan.write(f"{sha}\t{sz}\t{action}\t{keeper}\t{v}\n")
            reclaim += sz; nvictims += 1
            by_victim_top[top(v)][0] += sz; by_victim_top[top(v)][1] += 1
            pref = "/".join(segs(v)[:2])
            victims_by_dir_prefix[pref][0] += sz; victims_by_dir_prefix[pref][1] += 1
    plan.close()

    # wholesale-deletable check: for each top-level source dir, compare total (big.tsv) vs deletable
    totals = collections.defaultdict(lambda: [0, 0])
    for l in open(IDX / "big.tsv", "rb"):
        try: s, _, p = l.rstrip(b"\n").split(b"\t", 2)
        except ValueError: continue
        rel = p[bl:].decode("utf-8", "replace"); totals[top(rel)][0] += int(s); totals[top(rel)][1] += 1
    del_by_top = collections.defaultdict(lambda: [0, 0])
    for t, (b, n2) in by_victim_top.items(): del_by_top[t] = [b, n2]

    with open(IDX / "delete-plan.md", "w") as f:
        f.write("# BackDown — keep/delete plan (review before executing; nothing deleted yet)\n\n")
        f.write(f"**{nvictims:,} files to delete, {reclaim/1e12:.2f} TB reclaimable.** "
                f"{extract_n:,} sets ({extract_bytes/1e9:.0f} GB) are personal media whose surviving copy is "
                f"EXTRACTED to a clean root category dir before the others are removed.\n\n")
        f.write("## Reclaim by source directory (where the deletions fall)\n\n")
        f.write("| Source dir | delete GB | delete files | dir total GB | % of dir removed |\n|---|--:|--:|--:|--:|\n")
        for t, (b, n2) in sorted(del_by_top.items(), key=lambda x: -x[1][0]):
            tb = totals[t][0]
            f.write(f"| `{t}` | {b/1e9:.0f} | {n2:,} | {tb/1e9:.0f} | {100*b/max(tb,1):.0f}% |\n")
        f.write("\n## Wholesale-deletable candidates (≥95% of the dir is redundant)\n\n")
        wholesale = []
        for t, (b, n2) in del_by_top.items():
            tb = totals[t][0]
            if tb > 10e9 and b/max(tb,1) >= 0.95:
                wholesale.append((t, b, tb));
        if wholesale:
            for t, b, tb in sorted(wholesale, key=lambda x: -x[2]):
                f.write(f"- **`{t}`** — {100*b/tb:.0f}% redundant ({b/1e9:.0f} of {tb/1e9:.0f} GB); "
                        f"unique remainder {(tb-b)/1e9:.1f} GB. Candidate for wholesale removal after spot-check.\n")
        else:
            f.write("_(none ≥95%; deletions are file-level, not whole-directory)_\n")
        f.write("\n## Not touched (canonical keepers)\n\n`hercules-sdb/`, `Archive/`, `Leaks/`, and root media dirs are keep-priority; "
                "files unique in size were never candidates and are untouched.\n")
    print(f"{nvictims:,} deletions planned, {reclaim/1e12:.2f} TB reclaimable; {extract_n:,} media-extract sets")
    print("wholesale-deletable:", ", ".join(f"{t} ({100*del_by_top[t][0]/max(totals[t][0],1):.0f}%)"
          for t,_ in sorted(del_by_top.items(), key=lambda x:-x[1][0]) if totals[t][0]>10e9 and del_by_top[t][0]/max(totals[t][0],1)>=0.95) or "none")

if __name__ == "__main__": main()
