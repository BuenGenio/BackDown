#!/usr/bin/env python3
"""Phase 2 — mirrors of the irreplaceable subset onto the freed drives. Run under sudo.

  bd-mirror.py mail      per-mailbox tar.zst archives of every mail tree, packed first-fit-decreasing
                         across BD-1T-A/B/C (plan: stage/mail-units.tsv). Resumable: a unit whose
                         .ok marker exists on any drive is skipped. Each archive is integrity-tested
                         (zstd -t), its file count compared with the index, and sha256'd into
                         index/mirror-mail.tsv.
  bd-mirror.py plain     rsync mirrors: Archive subset -> BD-500, Photos -> BD-1T-C (journaled).
  bd-mirror.py status    what is where.

Drives are addressed by filesystem UUID (bd-mount.sh mounts them).
"""
import hashlib, json, os, shutil, subprocess, sys, time
from pathlib import Path

HERE = Path(__file__).parent; BIG = Path("/media/buengenio/FUCKING_BIG")
DRIVES = {"A": "/mnt/bd1a", "B": "/mnt/bd1b", "C": "/mnt/bd1c", "S": "/mnt/bd500"}
UUIDS = {"A": "c429fc72-d45b-4ed2-bfee-4a0d86a6face", "B": "fd4260d8-de81-4d75-b288-cbc575896ad0",
         "C": "9ee90c10-3e0e-4a97-9c67-53b5bfcea6f3", "S": "4166832a-4309-45b4-a5db-11bad1aafe6c"}
TAG = {  # source root -> folder name on the mirror
    "hercules-sdb/sdb/mail-maximal/vmail/": "mail.maximalconcepts.com",
    "hercules-sdb/sdb/mail/": "hercules-mail-2015-2023",
    "hercules-sdb/sdb/mail-anthill/": "mail.anthill.hk-2018",
    "hercules-sdb/sdb/mdbox/": "mdbox-eugene-2018",
    "steelhead-mail/mail-steelhead/": "mail-steelhead-legacy-2023",
    "vmail/mail.steelhead-group.com/": "mail.steelhead-group.com-2025",
}
MAIL_DRIVES = ["A", "B", "C"]; MARGIN = 3 * 1024**3
LOG = HERE / "journal" / "mirror-mail.log"; IDX = HERE / "index" / "mirror-mail.tsv"

def check_mounts():
    for k, m in DRIVES.items():
        src = subprocess.run(["findmnt", "-rn", "-o", "SOURCE", m], capture_output=True, text=True).stdout.strip()
        uuid = subprocess.run(["blkid", "-s", "UUID", "-o", "value", src], capture_output=True, text=True).stdout.strip() if src else ""
        if uuid != UUIDS[k]: sys.exit(f"{m} is not the expected filesystem (found {uuid or 'nothing'}) — run bd-mount.sh")

def free(m): st = os.statvfs(m); return st.f_bavail * st.f_frsize
def log(msg):
    line = f"[{time.strftime('%H:%M:%S')}] {msg}"; print(line, flush=True)
    with open(LOG, "a") as f: f.write(line + "\n")

def sha256(p):
    h = hashlib.sha256()
    with open(p, "rb", buffering=0) as f:
        for b in iter(lambda: f.read(8 << 20), b""): h.update(b)
    return h.hexdigest()

def mail():
    check_mounts()
    units = []
    for line in open(HERE / "stage" / "mail-units.tsv", encoding="utf-8", errors="surrogateescape"):
        root, unit, b, n = line.rstrip("\n").split("\t"); units.append((root, unit, int(b), int(n)))
    units.sort(key=lambda u: -u[2])                    # first-fit-decreasing
    done = set()
    for k in MAIL_DRIVES:
        for ok in Path(DRIVES[k]).glob("Mail/**/*.ok"): done.add(str(ok.relative_to(Path(DRIVES[k]) / "Mail"))[:-3])
    todo = [u for u in units if f"{TAG[u[0]]}/{u[1] or '_root'}" not in done]
    log(f"mail: {len(units)} units, {len(done)} already archived, {len(todo)} to do ({sum(u[2] for u in todo)/1e9:.1f} GB)")
    for root, unit, b, n in todo:
        name = f"{TAG[root]}/{unit or '_root'}"
        drive = next((k for k in MAIL_DRIVES if free(DRIVES[k]) > b + MARGIN), None)
        if not drive: log(f"NO SPACE for {name} ({b/1e9:.1f} GB) — stopping"); return
        out = Path(DRIVES[drive]) / "Mail" / (name + ".tar.zst"); out.parent.mkdir(parents=True, exist_ok=True)
        src_parent = BIG / root
        if unit == "":                                  # whole root as one unit (mdbox)
            src_parent, member = src_parent.parent, src_parent.name
            excl = []
        elif unit.endswith("_root"):                    # files directly in the dir, not subdirs
            d = unit[:-len("_root")].rstrip("/"); src_parent = src_parent / d if d else src_parent
            member = "."; excl = ["--no-recursion"]
            members = [e for e in os.listdir(src_parent) if not os.path.isdir(src_parent / e)]
            if not members: Path(str(out)[:-8] + ".ok").write_text("empty\n"); continue
        else:
            member = unit; excl = []
        # a prior failed run may have left a partial
        for junk in (str(out) + ".tmp",):
            try: os.path.exists(junk) and os.unlink(junk)
            except OSError: pass
        t0 = time.time(); tmp = str(out) + ".tmp"
        if unit.endswith("_root"):
            tar_cmd = ["tar", "-C", str(src_parent), "--no-recursion", "-cf", "-", "--", *members]
        else:
            tar_cmd = ["tar", "-C", str(src_parent), "-cf", "-", "--", member]
        with open(tmp, "wb") as f:
            tar = subprocess.Popen(tar_cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            zst = subprocess.Popen(["zstd", "-T0", "-3", "-q"], stdin=tar.stdout, stdout=f, stderr=subprocess.PIPE)
            tar.stdout.close(); zerr = zst.communicate()[1]; terr = tar.stderr.read(); tar.wait()
        if tar.returncode not in (0, 1) or zst.returncode != 0:      # tar rc 1 = "file changed" warnings only
            log(f"FAIL {name}: tar rc={tar.returncode} zstd rc={zst.returncode} {terr[-200:]!r} {zerr[-160:]!r}")
            try: os.unlink(tmp)
            except OSError: pass
            if not os.path.ismount(DRIVES[drive]) or b"Input/output error" in terr + zerr or b"Read error" in zerr:
                log("bus/device error detected — stopping so the supervisor can remount"); sys.exit(3)
            continue
        if subprocess.run(["zstd", "-t", "-q", tmp]).returncode != 0:
            log(f"FAIL {name}: zstd integrity test")
            try: os.unlink(tmp)
            except OSError: pass
            if not os.path.ismount(DRIVES[drive]): log("target unmounted — stopping for remount"); sys.exit(3)
            continue
        cnt = subprocess.run(f"zstd -dc -q {tmp!r} | tar -tf - | grep -vc '/$'", shell=True, capture_output=True, text=True).stdout.strip()
        cnt = int(cnt or 0)
        os.rename(tmp, out); digest = sha256(out); size = out.stat().st_size; dt = time.time() - t0
        Path(str(out)[:-8] + ".ok").write_text(f"{digest}\n")
        with open(IDX, "a") as f: f.write(f"{drive}\t{name}\t{b}\t{n}\t{size}\t{cnt}\t{digest}\n")
        flag = "" if cnt == n else f"  ⚠ count {cnt} vs index {n}"
        log(f"{drive} {name}: {b/1e9:.2f} GB/{n:,} files -> {size/1e9:.2f} GB ({100*size/max(b,1):.0f}%) in {dt/60:.1f} min{flag}")
    log("mail: all units done")

PLAIN = [  # (drive, source, dest-subdir, extra rsync args)
    ("S", BIG / "Archive", "Archive", ["--exclude=/Machines/disk-images/", "--exclude=/Media/"]),
    ("S", BIG / "Archive/Mail", "Archive/Mail", []),
    ("C", BIG / "Photos", "Photos", []),
    ("C", BIG / "Archive/Media/Photos", "Archive-Media-Photos", []),
]
def plain():
    check_mounts()
    for drive, src, sub, extra in PLAIN:
        dst = Path(DRIVES[drive]) / sub; dst.mkdir(parents=True, exist_ok=True)
        logf = HERE / "journal" / f"mirror-plain-{drive}-{sub.replace('/', '_')}.log"
        cmd = ["rsync", "-a", "--partial", "--inplace", "--info=progress2", "--no-motd", f"--log-file={logf}",
               "--log-file-format=%l\t%n", *extra, str(src) + "/", str(dst) + "/"]
        log(f"plain: {src} -> {dst}"); rc = subprocess.run(cmd).returncode; log(f"plain: {sub} rc={rc}")

def status():
    for k, m in DRIVES.items():
        n = sum(1 for _ in Path(m).glob("Mail/**/*.ok")) if k in MAIL_DRIVES else 0
        print(f"{k} {m:12} free {free(m)/1e9:7.1f} GB   mail archives: {n}")
    if IDX.exists():
        rows = [l.split("\t") for l in open(IDX)]
        print(f"archived: {len(rows)} units, {sum(int(r[2]) for r in rows)/1e9:.1f} GB source -> {sum(int(r[4]) for r in rows)/1e9:.1f} GB")

if __name__ == "__main__":
    {"mail": mail, "plain": plain, "status": status}[sys.argv[1] if len(sys.argv) > 1 else "status"]()
