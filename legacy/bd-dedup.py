#!/usr/bin/env python3
"""Phase 3 — find true duplicates *within* the 18 TB drive. Run under sudo.

Three passes, each resumable (state in index/dedup.db); stop and restart at any time.

  bd-dedup.py plan     build the work list: every file whose size collides with another file
                       on the drive, ordered by the bytes its group could reclaim.
  bd-dedup.py sketch   cheap discriminator: >=1 MB -> blake2b of head+middle+tail 64 KB;
                       <1 MB -> full blake2b (a seek costs the same as the read).
                       Groups whose sketches all differ are PROVEN not duplicates and drop out.
  bd-dedup.py full     sha256 only for files still sharing (size, sketch), highest-value first.
                       This is the proof required before anything is deleted.
  bd-dedup.py report   duplicate sets, reclaimable bytes, and where each copy lives
                       -> index/dedup-report.json (+ the /report page).

Nothing is ever deleted here; deletion is a separate, journaled step.
"""
import collections, hashlib, os, sqlite3, sys, time
from pathlib import Path

HERE = Path(__file__).parent; IDX = HERE / "index"; BIG = "/media/buengenio/FUCKING_BIG/"
DB = IDX / "dedup.db"; SKETCH_MIN = 1 << 20; CHUNK = 64 << 10; COMMIT = 2000

def db():
    c = sqlite3.connect(str(DB), timeout=120); c.execute("PRAGMA journal_mode=WAL"); c.execute("PRAGMA synchronous=NORMAL")
    c.execute("""CREATE TABLE IF NOT EXISTS f(path BLOB PRIMARY KEY, size INTEGER, prio INTEGER,
                 sketch TEXT, sha256 TEXT)""")  # path is BLOB: filenames may hold non-UTF-8 bytes
    c.execute("CREATE INDEX IF NOT EXISTS f_prio ON f(prio DESC)")
    c.execute("CREATE INDEX IF NOT EXISTS f_grp  ON f(size, sketch)")
    c.execute("CREATE INDEX IF NOT EXISTS f_sha  ON f(sha256)")
    return c

def busy():
    import subprocess
    ps = subprocess.run(["pgrep", "-fa", "bd-mirror.py|rsync -a|tar -C /media/buengenio/FUCKING_BIG"],
                        capture_output=True, text=True).stdout
    return [l for l in ps.splitlines() if "pgrep" not in l and "bash -c" not in l]

def plan():
    sizes = collections.Counter()
    for l in open(IDX / "big.tsv", "rb"):
        try: s, _, p = l.rstrip(b"\n").split(b"\t", 2)
        except ValueError: continue
        s = int(s)
        if s: sizes[s] += 1
    con = db(); n = 0; rows = []
    for l in open(IDX / "big.tsv", "rb"):
        try: s, _, p = l.rstrip(b"\n").split(b"\t", 2)
        except ValueError: continue
        s = int(s)
        if not s or sizes[s] < 2: continue
        rows.append((p, s, s * (sizes[s] - 1))); n += 1   # p stays raw bytes → BLOB
        if len(rows) >= 50000:
            con.executemany("INSERT OR IGNORE INTO f(path,size,prio) VALUES (?,?,?)", rows); con.commit(); rows.clear()
    con.executemany("INSERT OR IGNORE INTO f(path,size,prio) VALUES (?,?,?)", rows); con.commit()
    # reuse full hashes already computed in hashes.db
    h = sqlite3.connect(f"file:{IDX/'hashes.db'}?mode=ro", uri=True)
    got = 0
    for p, sha in h.execute("SELECT path, sha256 FROM h WHERE path LIKE ?", (BIG + "%",)):
        pb = p.encode("utf-8", "surrogateescape")   # match the BLOB column
        got += con.execute("UPDATE f SET sha256=? WHERE path=? AND sha256 IS NULL", (sha, pb)).rowcount
    con.commit()
    tot, = con.execute("SELECT count(*) FROM f").fetchone()
    print(f"work list: {tot:,} files ({n:,} inserted); {got:,} full hashes reused from hashes.db")

def sketch_of(path, size):
    h = hashlib.blake2b(digest_size=16)
    with open(path, "rb", buffering=0) as f:
        if size < SKETCH_MIN:
            while True:
                b = f.read(1 << 20)
                if not b: break
                h.update(b)
        else:
            for off in (0, size // 2 - CHUNK // 2, size - CHUNK):
                f.seek(max(0, off)); h.update(f.read(CHUNK))
    return h.hexdigest()

def sha256_of(path):
    h = hashlib.sha256()
    with open(path, "rb", buffering=0) as f:
        for b in iter(lambda: f.read(8 << 20), b""): h.update(b)
    return h.hexdigest()

def run(kind):
    if busy():
        print("mirror/rsync still running on the 18 TB spindle — refusing to compete:"); [print("  ", b[:100]) for b in busy()]; sys.exit(1)
    con = db()
    if kind == "sketch":
        q = "SELECT path,size FROM f WHERE sketch IS NULL AND size >= 1048576 ORDER BY prio DESC, path"  # cap: skip <1MB tail (22M tiny files = days of seeks for marginal reclaim; sweep later if 100% wanted)
        fn = lambda p, s: ("sketch", sketch_of(p, s))
    else:
        # only files that still share (size, sketch) with another file need a full hash
        con.execute("""CREATE TEMP TABLE grp AS SELECT size, sketch FROM f
                       WHERE sketch IS NOT NULL GROUP BY size, sketch HAVING count(*)>1""")
        q = """SELECT f.path, f.size FROM f JOIN grp g ON f.size=g.size AND f.sketch=g.sketch
               WHERE f.sha256 IS NULL ORDER BY f.prio DESC, f.path"""
        fn = lambda p, s: ("sha256", sha256_of(p))
    todo = con.execute(q).fetchall()
    tb = sum(s for _, s in todo)
    print(f"{kind}: {len(todo):,} files, {tb/1e12:.2f} TB nominal")
    t0 = time.time(); done = b = err = 0; pend = []
    for p, s in todo:
        try:
            col, v = fn(p, s); pend.append((v, p)); done += 1; b += s
        except OSError as e:
            err += 1; print(f"ERR {p}: {e}", file=sys.stderr)
        if len(pend) >= COMMIT:
            con.executemany(f"UPDATE f SET {col}=? WHERE path=?", pend); con.commit(); pend.clear()
            el = time.time() - t0
            print(f"  {done:,}/{len(todo):,}  {b/1e12:.2f} TB  {b/1e6/max(el,1):.0f} MB/s  {done/max(el,1):.0f} files/s  err={err}", flush=True)
    if pend: con.executemany(f"UPDATE f SET {col}=? WHERE path=?", pend); con.commit()
    print(f"{kind} complete: {done:,} files, {b/1e12:.2f} TB, {err} errors")

def report():
    import json
    con = db(); sets = []; bl = len(BIG.encode()); hardlink_groups = 0
    for sha, n, sz in con.execute("""SELECT sha256, count(*), max(size) FROM f WHERE sha256 IS NOT NULL
                                     GROUP BY sha256 HAVING count(*)>1 ORDER BY max(size)*(count(*)-1) DESC"""):
        rawpaths = [p for (p,) in con.execute("SELECT path FROM f WHERE sha256=? ORDER BY path", (sha,))]
        # collapse hard links: paths sharing one inode are ONE physical copy → reclaim only across distinct inodes.
        by_inode = {}
        for rp in rawpaths:
            try: st = os.lstat(rp); key = (st.st_dev, st.st_ino)
            except OSError: key = ("missing", rp)          # gone since indexing → treat as its own
            by_inode.setdefault(key, []).append(rp[bl:].decode("utf-8", "replace"))
        copies = len(by_inode)                              # distinct physical copies
        if copies < 2:                                     # all names were hard links to ONE inode → not a real dup
            hardlink_groups += 1; continue
        reps = [names[0] for names in by_inode.values()]   # one representative name per physical copy
        s = {"sha256": sha, "size": sz, "copies": copies, "reclaim": sz * (copies - 1), "paths": reps}
        if any(len(v) > 1 for v in by_inode.values()):
            s["hardlinked"] = [v for v in by_inode.values() if len(v) > 1]  # names that share an inode
        sets.append(s)
    tot = sum(s["reclaim"] for s in sets)
    pair = collections.Counter()
    for s in sets:
        tops = sorted({"/".join(p.split("/")[:2]) for p in s["paths"]})
        for i in range(len(tops)):
            for j in range(i + 1, len(tops)): pair[(tops[i], tops[j])] += s["reclaim"]
    json.dump({"sets": sets[:2000], "total_sets": len(sets), "reclaim": tot, "hardlink_only_groups": hardlink_groups,
               "pairs": [{"a": a, "b": b, "bytes": v} for (a, b), v in pair.most_common(200)]},
              open(IDX / "dedup-report.json", "w"))
    print(f"{len(sets):,} verified duplicate sets, {tot/1e12:.2f} TB reclaimable "
          f"(hard links collapsed by inode; {hardlink_groups:,} groups were hard-link-only → 0 reclaim)")
    for (a, b), v in pair.most_common(15): print(f"  {v/1e9:8.1f} GB  {a}  <->  {b}")

if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else "report"
    {"plan": plan, "sketch": lambda: run("sketch"), "full": lambda: run("full"), "report": report}[cmd]()
