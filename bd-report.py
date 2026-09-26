#!/usr/bin/env python3
"""Distil the index (+ hashes, when present) into index/report.json for the /report page.

Streams the TSVs once; memory stays small (aggregates by directory to depth 4,
duplicate pairs only for files >= 1 MB). Re-run any time; the page reads the JSON.
"""
import collections, json, os, re, sqlite3, subprocess, sys, time
from pathlib import Path

HERE = Path(__file__).parent; IDX = HERE / "index"
VOLS = {  # vol -> (mount, label, dev, fs, note)
    "big":      ("/media/buengenio/FUCKING_BIG", "FUCKING_BIG", "sde1", "ext4", "18 TB consolidation target"),
    "sda4":     ("/media/buengenio/sda4", "sda4", "sda4", "ext4", "laptop drive, partition 2"),
    "stuff":    ("/media/buengenio/Stuff", "Stuff", "sda2", "ext4", "laptop drive, partition 1"),
    "sdc1":     ("/mnt/sdc1", "Stuff (NTFS)", "sdc1", "ntfs (damaged $MFTMirr, mounted ro via ntfs3)", "2013-14 Windows/Mac era"),
    "verbatim": ("/media/buengenio/VERBATIM HD", "VERBATIM HD", "sdd1", "vfat", "external enclosure"),
    "v22":      ("/media/buengenio/22", "22", "sdc2", "exfat", "empty"),
}
DEPTH = 4; MIN = 1 << 20

def df(mount):
    try:
        o = subprocess.run(["df", "-B1", "--output=size,used", mount], capture_output=True, text=True).stdout.split("\n")[1].split()
        return int(o[0]), int(o[1])
    except Exception: return None, None

# ---- stream
agg = collections.defaultdict(lambda: [0, 0])      # (vol, path-tuple) -> [bytes, files]
big = []                                            # (vol, rel, size) for >= MIN
tot = collections.defaultdict(lambda: [0, 0])
mail = collections.defaultdict(lambda: [0, 0, 0, set()])  # domain -> bytes, files, msgs, mailboxes
MAILROOT = b"hercules-sdb/sdb/mail-maximal/vmail/"
for vol, (mount, *_) in VOLS.items():
    p = IDX / f"{vol}.tsv"
    if not p.exists(): continue
    mb = mount.encode()
    with open(p, "rb") as f:
        for raw in f:
            try: size, _, path = raw.rstrip(b"\n").split(b"\t", 2)
            except ValueError: continue
            size = int(size); rel = path[len(mb):].lstrip(b"/")
            tot[vol][0] += size; tot[vol][1] += 1
            parts = rel.split(b"/")
            for d in range(1, min(DEPTH, len(parts) - 1) + 1):
                k = (vol, tuple(parts[:d])); a = agg[k]; a[0] += size; a[1] += 1
            if size >= MIN: big.append((vol, rel, size))
            if vol == "big" and rel.startswith(MAILROOT):
                q = rel[len(MAILROOT):].split(b"/")
                dom = q[0].decode(errors="replace")
                if dom == "archive" and len(q) > 2: dom = "archive/" + q[1].decode(errors="replace"); q = q[1:]
                m = mail[dom]; m[0] += size; m[1] += 1
                if len(q) > 2 and (b"/cur/" in rel or b"/new/" in rel): m[2] += 1
                if len(q) > 1 and b"." not in q[1][:1]: m[3].add(q[1])

# ---- tree (depth-limited, pruned)
def build(vol, prefix, depth):
    kids = [(k[1][-1], v) for k, v in agg.items() if k[0] == vol and len(k[1]) == depth + 1 and k[1][:depth] == prefix]
    kids.sort(key=lambda x: -x[1][0])
    out = []
    other = [0, 0]
    for i, (name, (b, n)) in enumerate(kids):
        if i < 40 and (b >= 100 << 20 or i < 8):
            node = {"name": name.decode(errors="replace"), "bytes": b, "files": n}
            if depth + 1 < DEPTH and b >= 1 << 30:
                node["children"] = build(vol, prefix + (name,), depth + 1)
            out.append(node)
        else:
            other[0] += b; other[1] += n
    if other[0]: out.append({"name": f"… {len(kids) - len(out)} more", "bytes": other[0], "files": other[1], "other": True})
    return out

volumes = []
for vol, (mount, label, dev, fs, note) in VOLS.items():
    sz, used = df(mount)
    volumes.append({"vol": vol, "mount": mount, "label": label, "dev": dev, "fs": fs, "note": note,
                    "size": sz, "used": used, "files": tot[vol][1], "bytes": tot[vol][0],
                    "tree": build(vol, (), 0)})

# ---- duplicate pairs (name+size, >= 1MB), by depth-2 dir pair; also small-vs-big containment
key = collections.defaultdict(list)
for vol, rel, size in big: key[(os.path.basename(rel), size)].append((vol, rel))
pairs = collections.defaultdict(lambda: [0, 0]); redundant = 0
contain = collections.defaultdict(lambda: [0, 0])  # (small vol, top) -> [bytes matched on big, bytes total]
for vol, rel, size in big:
    if vol != "big": contain[(vol, rel.split(b"/")[0])][1] += size
for (name, size), locs in key.items():
    if len(locs) < 2: continue
    redundant += size * (len(locs) - 1)
    dirs = sorted(set((v, b"/".join(r.split(b"/")[:2])) for v, r in locs))
    for i in range(len(dirs)):
        for j in range(i + 1, len(dirs)):
            k = (dirs[i], dirs[j]); pairs[k][0] += size; pairs[k][1] += 1
    if any(v == "big" for v, _ in locs):
        for v, r in locs:
            if v != "big": contain[(v, r.split(b"/")[0])][0] += size
dups = sorted(({"a_vol": a[0], "a_dir": a[1].decode(errors="replace"), "b_vol": b[0], "b_dir": b[1].decode(errors="replace"),
                "bytes": v[0], "files": v[1]} for (a, b), v in pairs.items()), key=lambda x: -x["bytes"])[:400]
containment = sorted(({"vol": v, "top": t.decode(errors="replace"), "matched": m, "total": tt}
                      for (v, t), (m, tt) in contain.items() if tt >= 100 << 20), key=lambda x: (x["vol"], -x["total"]))

# ---- mail table
mail_rows = sorted(({"domain": d, "bytes": m[0], "files": m[1], "msgs": m[2], "mailboxes": len(m[3])} for d, m in mail.items()
                    if m[1] > 1), key=lambda x: -x["bytes"])

# ---- hashes / verification (if present)
verify = None
db = IDX / "hashes.db"
if db.exists():
    con = sqlite3.connect(f"file:{db}?mode=ro", uri=True, timeout=5)
    n, b = con.execute("SELECT count(*), coalesce(sum(size),0) FROM h").fetchone()
    nb, bb = con.execute("SELECT count(*), coalesce(sum(size),0) FROM h WHERE path LIKE '/media/buengenio/FUCKING_BIG/%'").fetchone()
    verify = {"hashed_files": n, "hashed_bytes": b, "big_hashed_files": nb, "big_hashed_bytes": bb}
    vj = IDX / "verify.json"; uj = IDX / "unique.json"
    if vj.exists(): verify["vols"] = json.load(open(vj))["vols"]
    if uj.exists(): verify["unique"] = json.load(open(uj))

# ---- narrative bits (kept in one place so the page and REPORT.md agree)
verdicts = [
    {"drive": "sdb (PS4)", "verdict": "wiped", "state": "done", "why": "Unreadable console drive; now ext4 'BD-500' at /mnt/bd500."},
    {"drive": "sdc2 (22)", "verdict": "safe", "state": "safe", "why": "Empty — only macOS metadata."},
    {"drive": "sda4", "verdict": "safe after 1.1 GB copy", "state": "copy", "why": "99.8% hash-verified on the big drive (673GB/ is a straight copy). 1.1 GB / 18 K files unique (HR 578 MB, Nextcloud 209 MB, 8,2, MacBookPro) — copy those, then wipe."},
    {"drive": "Stuff", "verdict": "safe after 61 GB copy", "state": "copy", "why": "Music fully verified. Unique: Shows 46 GB, buengenio.tgz 10 GB, buengenio/ 3.7 GB; another 50 GB of Shows exists only here and on sdc1."},
    {"drive": "sdc1", "verdict": "safe after 677 GB copy", "state": "copy", "why": "Only 17.5% verified. Unique: Backups-Mates 342 GB (image.dd), movies 140 GB, My Backups 130 GB, Music 26 GB, Apps 17 GB, Downloads 10 GB, Dropbox 7 GB."},
    {"drive": "Verbatim", "verdict": "safe after 215 GB copy", "state": "copy", "why": "Only 20.6% verified. Unique: Guille 133 GB, Maximal-WWW 39 GB, Maximal 38 GB, Nextcloud-master 4 GB."},
]
plan = [
    ("Mail/", "all IMAP/maildir data, by server then domain", ["mail.maximalconcepts.com/ ← hercules-sdb/sdb/mail-maximal/vmail (2011–2020, 2.0 TB)", "mail.steelhead-group.com/ ← vmail/ (mailcow, 2021–2025)", "mail-steelhead-legacy/ ← steelhead-mail + hercules-sdb/sdb/mail (after dedup)", "mail.anthill.hk/ ← hercules-sdb/sdb/mail-anthill", "clients/ ← Thunderbird profiles, mdbox, sieve"]),
    ("Servers/", "non-mail server data, by host", ["hercules/ ← hercules-sdb/sdb minus mail", "maximal-web/ ← Verbatim/Maximal-WWW, sdc1/vhosts", "nextcloud/ ← nc-data, nextcloud, sda4/Nextcloud, Verbatim/Nextcloud-master (drop preview/)", "databases/ ← mysql, neo4j, LI_DB"]),
    ("Machines/", "computer backups, by machine, dated", ["carbon-x1/2022-12 ← X1, sda4/Carbon X1", "macbookpro-12,1/2022-06 ← sda4/MacBookPro-12,1, buengenio-MacBookPro, TimeMachine", "turbojet/ ← turbojet", "linux-home/2020-11 ← Stuff/buengenio*, sda4/8,2", "windows-gene/2014 ← sdc1/My Backups", "disk-images/ ← kingston.img, image.dd, *.dmg, Testdisk-2TB"]),
    ("Phones/", "", ["oneplus-6t/, iphone/, ipad/ ← Backups/*", "samsung-ernesto/ ← SAMSUNG_ERNESTO/Int_storage", "crdroid/ ← hercules-sdb/sdb/crDroid"]),
    ("People/", "other people's data kept for them", ["ernesto/, leina/, damian/, tom/, vinoselo/, guille/"]),
    ("Companies/", "business documents", ["Maximal/ ← Verbatim/Maximal, Dropbox (Maximal Concepts), sda4/HR, Maximal-Netcloud-Users", "Steelhead/", "Sherborne/"]),
    ("Exports/", "cloud takeouts", ["google-takeout-photos/, facebook-2024-08/, linkedin/"]),
    ("Datasets/", "third-party datasets", ["leaks/ ← Leaks + SAMSUNG_ERNESTO/LEAKS (deduplicated: ~470 GB saved)", "li_db/"]),
    ("Media/", "libraries, one copy each", ["Music/, Movies/, Shows/, Videos/, Photos/ (InstantUpload, iCloud, Google Photos merged), Podcasts-UA/, Books/"]),
]
out = {"generated": time.strftime("%Y-%m-%d %H:%M"), "volumes": volumes, "redundant_bytes": redundant, "dups": dups,
       "containment": containment, "mail": mail_rows, "verify": verify, "verdicts": verdicts,
       "plan": [{"dir": d, "desc": s, "items": i} for d, s, i in plan],
       "totals": {"files": sum(v[1] for v in tot.values()), "bytes": sum(v[0] for v in tot.values())}}
(IDX / "report.json").write_text(json.dumps(out))
print(f"report.json: {out['totals']['files']:,} files, {out['totals']['bytes']/1e12:.2f} TB, {len(dups)} dup pairs, {len(mail_rows)} mail domains, redundant {redundant/1e12:.2f} TB")
