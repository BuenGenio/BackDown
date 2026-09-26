#!/usr/bin/env python3
"""Resumable content hashing into index/hashes.db (sqlite).

Rows are keyed by absolute path and carry (size, mtime, sha256). A file is
re-hashed only if its size or mtime changed, so the job can be killed and
restarted freely; progress is committed every COMMIT files.

Input: paths on stdin (one per line) or an index .tsv (size\tmtime\tpath).
Workers: one per physical disk is ideal; --workers N runs a pool, and paths are
dealt to workers round-robin by their mount prefix so one slow disk does not
block another.

Usage:
  bd-hash.py --from-tsv index/stuff.tsv [--min-size 1] [--workers 2]
  cat paths.txt | bd-hash.py
  bd-hash.py --status
"""
import argparse, hashlib, os, sqlite3, sys, time, threading, queue
from pathlib import Path

HERE = Path(__file__).parent
DB = HERE / "index" / "hashes.db"
COMMIT = 200
BUF = 4 * 1024 * 1024

def open_db():
    con = sqlite3.connect(str(DB), timeout=60, check_same_thread=False)
    con.execute("PRAGMA journal_mode=WAL")
    con.execute("""CREATE TABLE IF NOT EXISTS h(
        path TEXT PRIMARY KEY, size INTEGER, mtime REAL, sha256 TEXT, hashed_at REAL)""")
    con.execute("CREATE INDEX IF NOT EXISTS h_sha ON h(sha256)")
    con.execute("CREATE INDEX IF NOT EXISTS h_size ON h(size)")
    return con

def sha256_of(path):
    h = hashlib.sha256()
    with open(path, "rb", buffering=0) as f:
        while True:
            b = f.read(BUF)
            if not b: break
            h.update(b)
    return h.hexdigest()

def mount_key(p):
    # group by top-level mount so each worker sticks to one disk where possible
    parts = p.split("/")
    return "/".join(parts[:4]) if p.startswith("/media/") else "/".join(parts[:3])

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--from-tsv", action="append", default=[])
    ap.add_argument("--min-size", type=int, default=1, help="skip files smaller than this (bytes)")
    ap.add_argument("--workers", type=int, default=2)
    ap.add_argument("--status", action="store_true")
    ap.add_argument("--sudo", action="store_true", help="re-exec under sudo (needed for root-owned files)")
    a = ap.parse_args()

    if a.sudo and os.geteuid() != 0:
        os.execvp("sudo", ["sudo", "-E", sys.executable] + [x for x in sys.argv if x != "--sudo"])

    con = open_db()
    if a.status:
        n, b = con.execute("SELECT count(*), coalesce(sum(size),0) FROM h").fetchone()
        print(f"{n:,} files hashed, {b/1e9:.1f} GB"); return

    # ---- collect work
    want = {}   # path -> (size, mtime)
    if a.from_tsv:
        for t in a.from_tsv:
            with open(t, errors="surrogateescape") as f:
                for line in f:
                    try: s, m, p = line.rstrip("\n").split("\t", 2)
                    except ValueError: continue
                    s = int(s)
                    if s >= a.min_size: want[p] = (s, float(m))
    else:
        for line in sys.stdin:
            p = line.rstrip("\n")
            try: st = os.stat(p)
            except OSError: continue
            if st.st_size >= a.min_size: want[p] = (st.st_size, st.st_mtime)

    # ---- drop what is already done (same size+mtime)
    done = 0
    cur = con.execute("SELECT path, size, mtime FROM h")
    for p, s, m in cur:
        w = want.get(p)
        if w and w[0] == s and abs(w[1] - m) < 1.0:
            del want[p]; done += 1
    todo = sorted(want.items())
    total_b = sum(s for _, (s, _) in todo)
    print(f"{done:,} already hashed, {len(todo):,} to do ({total_b/1e9:.1f} GB)", file=sys.stderr)
    if not todo: return

    # ---- deal to per-disk queues
    qs = {}
    for p, sm in todo:
        qs.setdefault(mount_key(p), queue.Queue()).put((p, sm))
    lock = threading.Lock()
    stats = {"n": 0, "b": 0, "err": 0, "t0": time.time()}
    pending = []

    def flush():
        with lock:
            if pending:
                con.executemany("INSERT OR REPLACE INTO h VALUES (?,?,?,?,?)", pending)
                con.commit(); pending.clear()

    def worker(q):
        while True:
            try: p, (s, m) = q.get_nowait()
            except queue.Empty: return
            try:
                d = sha256_of(p)
                with lock:
                    pending.append((p, s, m, d, time.time()))
                    stats["n"] += 1; stats["b"] += s
                    if len(pending) >= COMMIT:
                        con.executemany("INSERT OR REPLACE INTO h VALUES (?,?,?,?,?)", pending)
                        con.commit(); pending.clear()
            except OSError as e:
                with lock: stats["err"] += 1
                print(f"ERR {p}: {e}", file=sys.stderr)

    threads = []
    for q in qs.values():
        for _ in range(max(1, a.workers // max(1, len(qs)))):
            t = threading.Thread(target=worker, args=(q,), daemon=True); t.start(); threads.append(t)

    try:
        while any(t.is_alive() for t in threads):
            time.sleep(10)
            el = time.time() - stats["t0"]
            print(f"  {stats['n']:,}/{len(todo):,} files  {stats['b']/1e9:.1f}/{total_b/1e9:.1f} GB  "
                  f"{stats['b']/1e6/max(el,1):.0f} MB/s  err={stats['err']}", file=sys.stderr)
    finally:
        flush()
    print("done", file=sys.stderr)

if __name__ == "__main__":
    main()
