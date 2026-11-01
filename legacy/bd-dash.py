#!/usr/bin/env python3
"""BackDown live dashboard — http://localhost:8787

Reads the resumable job state (index chunks, hashes.db, logs) and /proc/diskstats
and serves a small auto-refreshing page. No dependencies beyond the stdlib.

  bd-dash.py [--port 8787]
"""
import argparse, glob, json, os, re, sqlite3, subprocess, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

HERE = Path(__file__).parent
IDX = HERE / "index"
VOLS = {  # vol -> (mount, block device)
    "big": ("/media/buengenio/FUCKING_BIG", "sde"),
    "sda4": ("/media/buengenio/sda4", "sda"),
    "stuff": ("/media/buengenio/Stuff", "sda"),
    "sdc1": ("/mnt/sdc1", "sdc"),
    "verbatim": ("/media/buengenio/VERBATIM HD", "sdd"),
    "v22": ("/media/buengenio/22", "sdc"),
}
# physical disks by stable ATA model+serial (the enclosure reorders /dev/sdX between plugs)
SERIALS = {"ata-HGST_HTS721010A9E630_JR10004M172WLF": "BD-1T-A mirror",
           "ata-WDC_WD10JPVX-60JC3T0_WD-WXJ1A63C9646": "BD-1T-B mirror",
           "ata-ST1000LM024_HN-M101MBB_S2RXJ9CC504324": "BD-1T-C mirror",
           "ata-HGST_HTS545050A7E380_141008TM85A3TC13YHAL": "BD-500 mirror",
           "ata-WDC_WD181KFGX-68AFPN0_4BK73J0Y": "FUCKING_BIG (archive)",
           "nvme-KINGSTON_SNV2S2000G_50026B76868F7BCB": "NVMe (index)"}
REAL_BYTES = {"ata-WDC_WD181KFGX-68AFPN0_4BK73J0Y": 18000207937536, "ata-HGST_HTS545050A7E380_141008TM85A3TC13YHAL": 500107862016,
              "nvme-KINGSTON_SNV2S2000G_50026B76868F7BCB": 2000398934016}
def DISKS_now():
    """{kernel name: (label, id, kernel_bytes, bogus?)} resolved from /dev/disk/by-id at call time."""
    out = {}
    for sid, label in SERIALS.items():
        p = f"/dev/disk/by-id/{sid}"
        if not os.path.exists(p): continue
        dev = os.path.basename(os.path.realpath(p))
        try: kb = int(open(f"/sys/block/{dev}/size").read()) * 512
        except OSError: kb = 0
        real = REAL_BYTES.get(sid, 1000204886016)
        out[dev] = (label, sid, kb, abs(kb - real) > real // 100)
    return out
DISKS = {d: v[0] for d, v in DISKS_now().items()}

# ---------- disk throughput from /proc/diskstats (sampled between requests)
_last = {"t": 0, "s": {}}
def diskstats():
    out = {}
    with open("/proc/diskstats") as f:
        for line in f:
            p = line.split()
            if p[2] in DISKS:
                # fields: reads(3) rd_sectors(5) writes(7) wr_sectors(9) io_ms(12)
                out[p[2]] = (int(p[3]), int(p[5]) * 512, int(p[7]), int(p[9]) * 512, int(p[12]))
    return out
def disk_rates():
    global DISKS
    now = time.time(); dn = DISKS_now(); DISKS = {d: v[0] for d, v in dn.items()}
    cur = diskstats()
    res = []
    dt = now - _last["t"]
    for d, name in DISKS.items():
        if d in cur and d in _last["s"] and 0 < dt < 120:
            a, b = _last["s"][d], cur[d]
            res.append({"dev": d, "name": name, "serial": dn[d][1].split("_")[-1], "bogus": dn[d][3],
                        "r_iops": (b[0]-a[0])/dt, "r_mbs": (b[1]-a[1])/dt/1e6,
                        "w_mbs": (b[3]-a[3])/dt/1e6, "util": min(100.0, (b[4]-a[4])/dt/10)})
        else:
            res.append({"dev": d, "name": name, "serial": dn[d][1].split("_")[-1], "bogus": dn[d][3], "r_iops": 0, "r_mbs": 0, "w_mbs": 0, "util": 0})
    _last["t"], _last["s"] = now, cur
    return res

# ---------- index state
_lc = {}  # path -> (mtime, size, lines); finished chunks never change, so count once
def _count_lines(p):
    try:
        st = os.stat(p); key = (st.st_mtime, st.st_size)
        c = _lc.get(p)
        if c and c[0] == key: return c[1]
        n = 0
        with open(p, "rb") as f:
            for blk in iter(lambda: f.read(1 << 20), b""): n += blk.count(b"\n")
        _lc[p] = (key, n); return n
    except OSError:
        return 0

def index_state():
    vols = []
    for vol, (mount, dev) in VOLS.items():
        d = IDX / vol; lst = d / "chunks.list"
        if not lst.exists():
            vols.append({"vol": vol, "mount": mount, "planned": False}); continue
        total = sum(1 for _ in open(lst, errors="replace"))
        done_files = glob.glob(str(d / "chunks" / "*.done"))
        files = 0
        for t in glob.glob(str(d / "chunks" / "*.tsv")):
            files += _count_lines(t)
        inflight = []
        for tmp in glob.glob(str(d / "chunks" / "*.tmp")):
            try:
                path = open(tmp[:-4] + ".path", errors="replace").read().split("\t", 1)[1].strip()
                n = _count_lines(tmp)
                inflight.append({"dir": path.replace(mount, "."), "files": n})
            except OSError: pass
        vols.append({"vol": vol, "mount": mount, "planned": True, "chunks_total": total,
                     "chunks_done": len(done_files), "files": files, "inflight": inflight})
    return vols

# ---------- hash state
def hash_state():
    st = {"rows": 0, "bytes": 0}
    db = IDX / "hashes.db"
    if db.exists():
        try:
            con = sqlite3.connect(f"file:{db}?mode=ro", uri=True, timeout=5)
            st["rows"], st["bytes"] = con.execute("SELECT count(*), coalesce(sum(size),0) FROM h").fetchone()
            con.close()
        except sqlite3.Error as e:
            st["error"] = str(e)
    logs = sorted(IDX.glob("hash-*.log"), key=lambda p: p.stat().st_mtime)
    log = logs[-1] if logs else IDX / "hash-small.log"
    st["log"] = log.name
    if log.exists():
        lines = log.read_text(errors="replace").splitlines()
        prog = [l for l in lines if "/" in l and "GB" in l]
        if prog:
            m = re.search(r"([\d,]+)/([\d,]+) files\s+([\d.]+)/([\d.]+) GB\s+(\d+) MB/s\s+err=(\d+)", prog[-1])
            if m:
                st.update(done_files=int(m[1].replace(",", "")), todo_files=int(m[2].replace(",", "")),
                          done_gb=float(m[3]), todo_gb=float(m[4]), mbs=int(m[5]), errors=int(m[6]))
        st["errors_detail"] = [l for l in lines if l.startswith("ERR")][-5:]
    return st

# ---------- staging (rsync journals)
_sj = {}  # log path -> (size, bytes, files) incremental parse
def stage_state():
    plan_p = HERE / "stage" / "plan.json"
    if not plan_p.exists(): return None
    plan = json.load(open(plan_p)); out = []
    for d, pl in plan.items():
        log = HERE / "journal" / f"stage-{d}.log"; b = n = 0
        if log.exists():
            st = log.stat(); prev = _sj.get(str(log))
            if prev and prev[0] <= st.st_size:
                b, n, pos = prev[1], prev[2], prev[0]
            else:
                b, n, pos = 0, 0, 0
            with open(log, "rb") as f:
                f.seek(pos)
                for line in f:
                    if b"\t" not in line or line.rstrip(b"\n").endswith(b"/"): continue  # skip directory entries
                    head = line.split(b"\t", 1)[0].rsplit(b" ", 1)[-1]
                    if head.isdigit(): b += int(head); n += 1
                pos = f.tell()
            _sj[str(log)] = (pos, b, n)
        status = ""; sp = HERE / "journal" / f"stage-{d}.status"
        if sp.exists():
            lines = sp.read_text().strip().splitlines(); status = lines[-1] if lines else ""
        rate = ""; pp = HERE / "journal" / f"stage-{d}.progress"
        if pp.exists():
            tail = pp.read_bytes()[-400:].decode(errors="replace").replace("\r", "\n").strip().splitlines()
            rate = tail[-1].strip() if tail else ""
        err = 0; ep = HERE / "journal" / f"stage-{d}.err"
        if ep.exists(): err = sum(1 for l in open(ep, errors="replace") if l.strip())
        out.append({"drive": d, "plan_bytes": pl["bytes"], "plan_files": pl["files"], "bytes": b, "files": n,
                    "status": status, "rate": rate, "errors": err, "done": "done" in status})
    return out

# ---------- mirrors (phase 2)
MIRROR_DRIVES = {"A": "/mnt/bd1a", "B": "/mnt/bd1b", "C": "/mnt/bd1c", "S": "/mnt/bd500"}
def _rsync_log_bytes(p):
    b = n = 0
    if not p.exists(): return 0, 0
    st = p.stat(); prev = _sj.get(str(p))
    if prev and prev[0] <= st.st_size: b, n, pos = prev[1], prev[2], prev[0]
    else: b, n, pos = 0, 0, 0
    with open(p, "rb") as f:
        f.seek(pos)
        for line in f:
            if b"\t" not in line or line.rstrip(b"\n").endswith(b"/"): continue
            head = line.split(b"\t", 1)[0].rsplit(b" ", 1)[-1]
            if head.isdigit(): b += int(head); n += 1
        pos = f.tell()
    _sj[str(p)] = (pos, b, n); return b, n
def mirror_state():
    out = {"drives": [], "mail": None, "plain": []}
    for k, m in MIRROR_DRIVES.items():
        try: st = os.statvfs(m); out["drives"].append({"k": k, "mount": m, "free": st.f_bavail * st.f_frsize, "size": st.f_blocks * st.f_frsize})
        except OSError: out["drives"].append({"k": k, "mount": m, "free": 0, "size": 0})
    units = HERE / "stage" / "mail-units.tsv"; idx = IDX / "mirror-mail.tsv"
    if units.exists():
        tot_b = tot_n = 0; cnt = 0
        for l in open(units, errors="replace"):
            p = l.rstrip("\n").split("\t"); tot_b += int(p[2]); tot_n += int(p[3]); cnt += 1
        done_b = done_n = out_b = 0; done = 0; warn = 0
        if idx.exists():
            for l in open(idx, errors="replace"):
                p = l.rstrip("\n").split("\t"); done += 1; done_b += int(p[2]); done_n += int(p[3]); out_b += int(p[4]); warn += (p[5] != p[3])
        last = ""; lg = HERE / "journal" / "mirror-mail.log"
        if lg.exists():
            ls = lg.read_text(errors="replace").strip().splitlines(); last = ls[-1] if ls else ""
        out["mail"] = {"units": cnt, "done": done, "src_bytes": tot_b, "done_bytes": done_b, "out_bytes": out_b, "files": tot_n, "done_files": done_n, "warn": warn, "last": last[-160:]}
    for lg in sorted((HERE / "journal").glob("mirror-plain-*.log")):
        b, n = _rsync_log_bytes(lg); out["plain"].append({"name": lg.stem[13:], "bytes": b, "files": n})
    return out

def procs():
    ps = subprocess.run(["pgrep", "-fa", "bd-index.sh run|bd-hash.py|^find /media|^find /mnt"], capture_output=True, text=True).stdout
    st = subprocess.run(["ps", "-eo", "stat,args"], capture_output=True, text=True).stdout
    mail_paused = any(l.split(None, 1)[0].startswith("T") and "tar -C /media/buengenio/FUCKING_BIG" in l for l in st.splitlines())
    return {"indexer": "bd-index.sh run" in ps, "hasher": "bd-hash.py" in ps, "mail_paused": mail_paused,
            "finds": len([l for l in ps.splitlines() if re.search(r"\d+ find /", l)])}

def status():
    la = os.getloadavg()
    return {"time": time.strftime("%H:%M:%S"), "load": la, "procs": procs(),
            "index": index_state(), "hash": hash_state(), "stage": stage_state(), "mirror": mirror_state(), "disks": disk_rates()}

PAGE = r"""<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>BackDown</title>
<link rel="icon" href="/assets/favicon.svg" type="image/svg+xml">
<script>(function(){var t=new URLSearchParams(location.search).get('theme');if(t==='dark'||t==='light'){document.documentElement.dataset.theme=t}})()</script>
<link rel="stylesheet" href="/assets/shell.css"></head><body>
<div class="field" aria-hidden="true"><i></i><i></i><i></i></div>
<header class="hdr"><div class="nav-glass" aria-hidden="true"></div>
 <div class="wrap bar">
  <a href="#top" class="home"><span class="mark" aria-hidden="true"><svg viewBox="0 0 80 60" fill="currentColor" fill-rule="evenodd"><path d="M1 0H10A1 1 0 0 1 11 1V19.7383A21.5 21.5 0 0 1 40 27.5455A21.5 21.5 0 0 1 69 19.7383V1A1 1 0 0 1 70 0H79A1 1 0 0 1 80 1V38.5A21.5 21.5 0 0 1 40 49.4545A21.5 21.5 0 0 1 0 38.5V1A1 1 0 0 1 1 0ZM32.5 38.5A11 11 0 1 0 10.5 38.5A11 11 0 1 0 32.5 38.5ZM69.5 38.5A11 11 0 1 0 47.5 38.5A11 11 0 1 0 69.5 38.5Z"/></svg></span>backdown</a>
  <nav aria-label="Sections"><a href="#top" data-s="top">Overview</a><a href="#staging" data-s="staging">Staging</a><a href="#mirrors" data-s="mirrors">Mirrors</a><a href="#indexing" data-s="indexing">Indexing</a><a href="#hashing" data-s="hashing">Hashing</a><a href="#disks" data-s="disks">Disks</a><a href="/report" target="_blank" rel="noopener">Report</a></nav>
  <span class="pill" id="live"><span class="dot"></span>connecting…</span>
 </div>
</header>
<main class="wrap" id="top">
 <div class="head"><span class="eyebrow">Drive consolidation</span><h1>What the machine is doing</h1><p class="muted" id="sub">—</p></div>
 <div class="grid" id="tiles"></div>
 <section id="staging"><div class="head"><h2>Staging</h2><p class="muted small">Copying every small-drive file that has no sha256 twin on the 18 TB drive into <code>Archive/_staging/&lt;drive&gt;/</code>, layout preserved. Journaled rsync — re-running resumes.</p></div><div class="card" id="stage"></div></section>
 <section id="mirrors"><div class="head"><h2>Mirrors</h2><p class="muted small">Second copies of the irreplaceable subset on the freed drives: mail as per-mailbox <code>tar.zst</code> packed across BD-1T-A/B/C; Archive subset → BD-500; Photos → BD-1T-C.</p></div><div class="grid" id="mirror-drives" style="margin-bottom:12px"></div><div class="card" id="mirror-t"></div></section>
 <section id="indexing"><div class="head"><h2>Indexing</h2><p class="muted small">Resumable chunks per volume. Finished chunks never re-run.</p></div><div class="card" id="index"></div></section>
 <section id="hashing"><div class="head"><h2>Hashing</h2><p class="muted small">sha256 of every file on the small drives → <code>hashes.db</code>. This is the proof required before anything is wiped.</p></div><div class="card tile" id="hash"></div></section>
 <section id="disks"><div class="head"><h2>Disks</h2><p class="muted small">Identified by ATA serial, not /dev name (the enclosure reorders bays). Spinning disks saturate at ~60 random reads/s; busy% near 100 with low MB/s means seek-bound on small files — the disk, not the app.</p></div><div id="disks-warn" style="margin-bottom:12px"></div><div class="card" id="disks-t"></div></section>
 <footer>BackDown · local dashboard · reads <code>index/</code> and <code>/proc/diskstats</code> every 5 s</footer>
</main>
<script>window.__init=__INIT__</script>
<script>
const $=s=>document.querySelector(s);
const fmt=n=>n>=1e6?(n/1e6).toFixed(2)+'M':n>=1e3?(n/1e3).toFixed(1)+'K':String(n);
const gb=b=>b>=1e12?(b/1e12).toFixed(2)+' TB':(b/1e9).toFixed(1)+' GB';
const pct=(a,b)=>b?Math.min(100,100*a/b):0;
function render(s){
 const p=s.procs, big=s.index.find(v=>v.vol==='big')||{}, h=s.hash, busy=p.indexer||p.hasher;
 $('#live').innerHTML=`<span class="dot ${busy?'on':''}"></span>${busy?'working':'idle'} · ${s.time}`;
 $('#sub').textContent=`load ${s.load.map(x=>x.toFixed(1)).join(' / ')} · ${p.finds} crawler${p.finds==1?'':'s'} on the 18TB drive`;
 $('#tiles').innerHTML=`
  <div class="card tile"><div class="l">Files indexed on the 18TB drive</div><div class="v hero">${fmt(big.files||0)}</div><div class="d">${big.chunks_done||0} of ${big.chunks_total||0} chunks complete</div></div>
  <div class="card tile"><div class="l">Bytes hashed</div><div class="v">${gb(h.bytes||0)}</div><div class="d">${fmt(h.rows||0)} files · ${h.mbs||0} MB/s now</div></div>
  <div class="card tile"><div class="l">Indexer</div><div class="v"><span class="dot ${p.indexer?'on':'off'}"></span>${p.indexer?'running':'stopped'}</div><div class="d">one seeker per spindle</div></div>
  <div class="card tile"><div class="l">Hasher</div><div class="v"><span class="dot ${p.hasher?'on':'off'}"></span>${p.hasher?'running':'stopped'}</div><div class="d">${h.errors||0} read error${h.errors==1?'':'s'}</div></div>`;
 const mi=s.mirror||{drives:[],plain:[]};
 $('#mirror-drives').innerHTML=mi.drives.map(d=>`<div class="card tile"><div class="l">${d.k} · ${d.mount}</div><div class="v" style="font-size:22px">${gb(d.size-d.free)} <span class="small">used</span></div><div class="meter"><i style="width:${d.size?100*(d.size-d.free)/d.size:0}%"></i></div><div class="d">${gb(d.free)} free</div></div>`).join('');
 const mm=mi.mail; $('#mirror-t').innerHTML=(mm?`<div class="tile"><div class="l">Mail archives · ${mm.done} of ${mm.units} mailboxes${s.procs.mail_paused?' · <b style="color:var(--warn)">PAUSED</b> (waiting for the rsync legs — one reader per spindle)':''}${mm.warn?` · <span class="err">${mm.warn} count mismatch</span>`:''}</div><div class="meter"><i style="width:${pct(mm.done_bytes,mm.src_bytes)}%"></i></div><div class="d">${gb(mm.done_bytes)} of ${gb(mm.src_bytes)} source → ${gb(mm.out_bytes)} compressed · ${fmt(mm.done_files)} / ${fmt(mm.files)} files</div><div class="small mono" style="margin-top:6px">${mm.last}</div></div>`:'')+
  (mi.plain.length?`<table><tr><th>rsync mirror</th><th class="n">copied</th><th class="n">files</th></tr>`+mi.plain.map(p=>`<tr><td><b>${p.name}</b></td><td class="n">${gb(p.bytes)}</td><td class="n">${p.files.toLocaleString()}</td></tr>`).join('')+`</table>`:'');
 const sg=s.stage||[];
 $('#stage').innerHTML=sg.length?`<table><tr><th>Drive</th><th class="n">Copied</th><th style="width:30%">Progress</th><th class="n">Files</th><th>Now</th></tr>`+sg.map(x=>`<tr><td><b>${x.drive}</b><div class="small">${x.status.replace(/^\[.*?\] /,'')}${x.errors?` · <span class="err">${x.errors} error line${x.errors==1?'':'s'}</span>`:''}</div></td><td class="n">${gb(x.bytes)} <span class="small">of ${gb(x.plan_bytes)}</span></td><td><div class="bar2 ${x.done?'':'hot'}"><i style="width:${pct(x.bytes,x.plan_bytes)}%"></i></div></td><td class="n">${x.files.toLocaleString()} <span class="small">/ ${x.plan_files.toLocaleString()}</span></td><td class="small mono">${x.rate.slice(0,60)}</td></tr>`).join('')+`</table>`:'<div style="padding:14px 16px" class="small">no staging plan</div>';
 $('#index').innerHTML=`<table><tr><th>Volume</th><th>Mount</th><th class="n">Chunks</th><th style="width:28%">Progress</th><th class="n">Files</th></tr>`+
  s.index.map(v=>v.planned?`<tr><td><b>${v.vol}</b>${v.inflight.length?'<div class="inflight">'+v.inflight.map(i=>`<code>${i.dir}</code> ${fmt(i.files)}`).join('<br>')+'</div>':''}</td><td class="small">${v.mount}</td><td class="n">${v.chunks_done} / ${v.chunks_total}</td><td><div class="bar2 ${v.chunks_done<v.chunks_total?'hot':''}"><i style="width:${pct(v.chunks_done,v.chunks_total)}%"></i></div></td><td class="n">${v.files.toLocaleString()}</td></tr>`
   :`<tr><td><b>${v.vol}</b></td><td class="small">${v.mount}</td><td colspan=3 class="small">not planned</td></tr>`).join('')+`</table>`;
 const hf=h.done_files||0, ht=h.todo_files||0, hg=h.done_gb||0, hgt=h.todo_gb||0;
 const eta=h.mbs&&hgt>hg?`≈ ${((hgt-hg)*1e3/h.mbs/3600).toFixed(1)} h left at the current rate`:'';
 $('#hash').innerHTML=`<div class="l">${h.log||''} · ${hf.toLocaleString()} of ${ht.toLocaleString()} files</div><div class="meter"><i style="width:${pct(hf,ht)}%"></i></div>
  <div class="l" style="margin-top:12px">${hg.toFixed(1)} of ${hgt.toFixed(1)} GB</div><div class="meter"><i style="width:${pct(hg,hgt)}%"></i></div><div class="d">${eta}</div>
  ${(h.errors_detail||[]).length?'<div class="small err" style="margin-top:10px">'+h.errors_detail.map(e=>e.slice(0,140)).join('<br>')+'</div>':''}`;
 const bog=s.disks.filter(d=>d.bogus).map(d=>d.dev);
 $('#disks-warn').innerHTML=bog.length?`<div class="card tile" style="border-color:color-mix(in srgb,var(--crit) 45%,transparent)"><div class="l err">⚠ Enclosure capacity bug</div><div class="d">${bog.join(', ')} report an impossible size (JMS567 after power-cycle). Filesystems are fine; <b>do not partition or format</b> until the enclosure is re-plugged. <code>bd-wipe.sh</code> refuses to run in this state.</div></div>`:'';
 $('#disks-t').innerHTML=`<table><tr><th>Disk</th><th>Serial</th><th>Role</th><th class="n">read IOPS</th><th class="n">read MB/s</th><th class="n">write MB/s</th><th style="width:28%">Busy</th></tr>`+
  s.disks.map(d=>`<tr><td><b>${d.dev}</b>${d.bogus?' <span class="err" title="enclosure reports a bogus capacity — do not partition until re-plugged">⚠</span>':''}</td><td class="small mono">${d.serial}</td><td class="small">${d.name}</td><td class="n">${d.r_iops.toFixed(0)}</td><td class="n">${d.r_mbs.toFixed(1)}</td><td class="n">${d.w_mbs.toFixed(1)}</td><td><div class="bar2"><i style="width:${d.util.toFixed(0)}%"></i></div><span class="small">${d.util.toFixed(0)}%</span></td></tr>`).join('')+`</table>`;
}
async function tick(){let s;try{s=await (await fetch('/api/status')).json()}catch(e){$('#live').innerHTML='<span class="dot off"></span>server unreachable';return}render(s)}
if(window.__init){render(window.__init)}else{tick()}setInterval(tick,5000);
/* active section in the navbar */
const links=[...document.querySelectorAll('nav a[data-s]')];
const io=new IntersectionObserver(es=>{es.forEach(e=>{if(e.isIntersecting){links.forEach(a=>a.classList.toggle('active',a.dataset.s===e.target.id))}})},{rootMargin:'-40% 0px -55% 0px'});
['top','staging','mirrors','indexing','hashing','disks'].forEach(id=>{const el=document.getElementById(id);if(el)io.observe(el)});
</script></body></html>"""

class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_GET(self):
        path = self.path.split("?", 1)[0]
        if path == "/api/status":
            body = json.dumps(status()).encode(); ct = "application/json"
        elif path == "/":
            body = PAGE.replace("__INIT__", json.dumps(status())).encode(); ct = "text/html; charset=utf-8"
        elif path.startswith("/fonts/") and "/" not in path[7:]:
            fp = HERE / "assets" / "fonts" / path[7:]
            if not fp.exists(): self.send_response(404); self.end_headers(); return
            body = fp.read_bytes(); ct = "font/woff2"
        elif path == "/report":
            body = (HERE / "assets" / "report.html").read_bytes(); ct = "text/html; charset=utf-8"
        elif path == "/report.md":
            body = (HERE / "REPORT.md").read_bytes(); ct = "text/markdown; charset=utf-8"
        elif path == "/api/report":
            fp = IDX / "report.json"
            if not fp.exists(): self.send_response(404); self.end_headers(); return
            body = fp.read_bytes(); ct = "application/json"
        elif path.startswith("/assets/") and "/" not in path[8:] and ".." not in path:
            fp = HERE / "assets" / path[8:]
            if not fp.exists(): self.send_response(404); self.end_headers(); return
            body = fp.read_bytes(); ct = {"css": "text/css", "html": "text/html; charset=utf-8", "js": "text/javascript", "svg": "image/svg+xml"}.get(fp.suffix[1:], "application/octet-stream")
        else:
            self.send_response(404); self.end_headers(); return
        self.send_response(200); self.send_header("Content-Type", ct)
        self.send_header("Content-Length", str(len(body))); self.send_header("Cache-Control", "public, max-age=86400" if ct == "font/woff2" else "no-store")
        self.end_headers(); self.wfile.write(body)

if __name__ == "__main__":
    ap = argparse.ArgumentParser(); ap.add_argument("--port", type=int, default=8787)
    ap.add_argument("--host", default="127.0.0.1", help="bind address (e.g. a Tailscale IP)")
    a = ap.parse_args()
    disk_rates()  # prime the sampler
    print(f"BackDown dashboard → http://{a.host}:{a.port}")
    ThreadingHTTPServer((a.host, a.port), H).serve_forever()
