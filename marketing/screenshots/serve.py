#!/usr/bin/env python3
"""Serve the real BackDown dashboard and report UI with an invented demo dataset, for the landing-page slideshow.

The product pages are used as-is (the dashboard PAGE is imported from legacy/bd-dash.py, the report from
assets/report.html); only the data behind /api/status and /api/report is demo. The report page
hardcodes a few strings from the founding dataset, so PRIVATE_STRINGS swaps those for neutral ones
in the served copy. Nothing here reads index/, the journals or any real drive.

  python3 serve.py [--port 8790]
"""
import argparse
import importlib.util
import json
import re
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SITE_PUBLIC = REPO / 'marketing' / 'public'
TB, GB = 1e12, 1e9

# the report page hardcodes a few details of the founding dataset (its drive count and mail domain);
# these patterns neutralise them in the served copy without this file having to name them
PRIVATE_PATTERNS = [
    (r'Seven drives, one dataset', 'Six drives, one dataset'),
    (r'Mail — mail\.[\w.-]+', 'Mail — mail.example.com'),
    (r"m\.domain==='[^']+'", "m.domain==='example.com'"),
    (r'<div class="l">[\w.-]+\.[a-z]+ mail</div>', '<div class="l">example.com mail</div>'),
]


def _dash_page():
    spec = importlib.util.spec_from_file_location('bd_dash', REPO / 'legacy' / 'bd-dash.py')
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)  # defines functions and PAGE only; the server starts under __main__
    return mod.PAGE


def node(name, tb, mfiles, children=None):
    n = {'name': name, 'bytes': int(tb * TB), 'files': int(mfiles * 1e6)}
    if children:
        n['children'] = children
    return n


BIG_TREE = [
    node('Backups', 5.94, 9.81, [
        node('rsync-daily', 1.46, 3.12, [node('daily.0', 0.52, 1.08), node('daily.1', 0.49, 1.04), node('daily.2', 0.45, 1.0)]),
        node('TimeMachine', 2.31, 4.02, [node('MacBook Pro 2016', 1.62, 2.71), node('MacBook Air 2012', 0.69, 1.31)]),
        node('thinkpad-2019', 1.12, 1.62),
        node('old-desktop-2011', 0.46, 0.51),
        node('iphone', 0.41, 0.36),
        node('android', 0.18, 0.18),
    ]),
    node('Photos', 2.61, 1.42, [node('Takeout', 0.62, 0.41), node('2023', 0.31, 0.12), node('2022', 0.29, 0.11),
                                node('2021', 0.27, 0.1), node('2019', 0.24, 0.09), node('… 16 more', 0.88, 0.59) | {'other': True}]),
    node('Media', 1.98, 0.41, [node('Movies', 1.1, 0.01), node('Music', 0.52, 0.31), node('Shows', 0.36, 0.09)]),
    node('VMs', 0.88, 0.02),
    node('Projects', 0.64, 7.93, [node('git', 0.21, 5.12), node('clients', 0.31, 2.11), node('archive-2012', 0.12, 0.7)]),
    node('Disk images', 0.44, 0.01),
    node('Mail', 0.41, 2.47, [node('mail.example.com', 0.38, 2.31), node('thunderbird-profiles', 0.03, 0.16)]),
]


def report():
    vols = [
        # vol, label, dev, fs, note, size, used, tree
        ('big', 'ARCHIVE-18TB', 'sde1', 'ext4', '18 TB consolidation target', 18 * TB, 12.9 * TB, BIG_TREE, 22.1),
        ('laptop', 'old-laptop', 'sda2', 'ext4', '2019 ThinkPad, home partition', 0.5 * TB, 0.46 * TB,
         [node('home', 0.44, 1.82), node('var', 0.02, 0.08)], 1.9),
        ('macbook', 'MacBook-HD', 'sdb2', 'apfs (read-only)', '2016 MacBook Pro internal', 0.5 * TB, 0.41 * TB,
         [node('Users', 0.38, 1.42), node('Applications', 0.03, 0.18)], 1.6),
        ('passport', 'WD Passport', 'sdc1', 'exfat', 'USB enclosure', 2 * TB, 1.12 * TB,
         [node('Photos', 1.05, 0.81), node('Videos', 0.07, 0.09)], 0.9),
        ('seagate', 'Seagate 2TB', 'sdd1', 'ntfs (read-only)', '2014 Windows backup drive', 2 * TB, 0.31 * TB,
         [node('Backup', 0.29, 0.38), node('Music', 0.02, 0.02)], 0.4),
        ('usb', 'USB-32GB', 'sdf1', 'vfat', 'reformatted, mirror target', 0.032 * TB, 0.0 * TB, [], 0.2),
    ]
    volumes = [{'vol': v, 'mount': f'/mnt/{label.lower()}', 'label': label, 'dev': dev, 'fs': fs, 'note': note,
                'size': int(size), 'used': int(used), 'bytes': int(used), 'files': int(mf * 1e6), 'tree': tree}
               for v, label, dev, fs, note, size, used, tree, mf in vols]
    volumes[-1]['files'] = 0  # the reformatted stick has nothing indexed

    dup = lambda av, ad, bv, bd, tb, kf: {'a_vol': av, 'a_dir': ad, 'b_vol': bv, 'b_dir': bd, 'bytes': int(tb * TB), 'files': int(kf * 1e3)}
    dups = [
        dup('big', 'Backups/TimeMachine', 'big', 'Backups/rsync-daily', 1.21, 412),
        dup('passport', 'Photos/2015-2018', 'big', 'Photos/2017', 0.62, 71),
        dup('big', 'Backups/thinkpad-2019', 'laptop', 'home/alex', 0.43, 1540),
        dup('big', 'Photos/Takeout', 'big', 'Photos/2019', 0.31, 42),
        dup('macbook', 'Users/alex', 'big', 'Backups/TimeMachine', 0.36, 1210),
        dup('seagate', 'Backup/Documents', 'big', 'Backups/old-desktop-2011', 0.27, 318),
        dup('big', 'Media/Music', 'seagate', 'Music/iTunes', 0.02, 9.1),
        dup('big', 'Disk images/thinkpad-sda.img', 'big', 'Backups/thinkpad-2019', 0.19, 0.001),
        dup('passport', 'Videos/2014', 'big', 'Media/Shows', 0.05, 0.4),
        dup('big', 'Projects/clients', 'big', 'Projects/archive-2012', 0.08, 211),
        dup('big', 'Backups/iphone', 'big', 'Photos/2021', 0.07, 18),
        dup('big', 'Mail/thunderbird-profiles', 'big', 'Mail/mail.example.com', 0.02, 96),
    ]

    top = lambda name, v, u: {'top': name, 'vbytes': int(v * GB), 'ubytes': int(u * GB)}
    verify = {
        'hashed_files': 27_100_000, 'hashed_bytes': int(15.2 * TB),
        'vols': {
            'Seagate 2TB': {'tops': [top('Backup', 290, 0), top('Music', 20, 0)]},
            'old-laptop': {'tops': [top('home', 435.8, 4.2), top('var', 20, 0)]},
            'WD Passport': {'tops': [top('Photos', 862, 188), top('Videos', 46, 24)]},
            'MacBook-HD': {'tops': [top('Users', 372, 8.1), top('Applications', 30, 0)]},
        },
        'unique': {'per_vol': {
            'Seagate 2TB': {'unique_bytes': 0, 'unique_files': 0, 'other_small_bytes': 0},
            'old-laptop': {'unique_bytes': int(4.2 * GB), 'unique_files': 11204, 'other_small_bytes': 0},
            'WD Passport': {'unique_bytes': int(212 * GB), 'unique_files': 61230, 'other_small_bytes': int(3.1 * GB)},
            'MacBook-HD': {'unique_bytes': int(8.1 * GB), 'unique_files': 20410, 'other_small_bytes': 0},
        }},
    }

    verdicts = [
        {'drive': 'Seagate 2TB', 'state': 'safe', 'why': 'Every file has a sha256-identical copy on ARCHIVE-18TB: 310 GB in 402 K files, all verified.'},
        {'drive': 'WD Passport', 'state': 'copy', 'why': '81% verified. 212 GB exists nowhere else (Photos/2019–2021 RAW 188 GB, Videos 24 GB): copy those, then wipe.'},
        {'drive': 'old-laptop', 'state': 'copy', 'why': '99.1% verified. 4.2 GB in 11 K files is unique (Documents 3.1 GB, dotfiles, ssh keys): copy those, then wipe.'},
        {'drive': 'MacBook-HD', 'state': 'pending', 'why': 'Hashing is 64% done. The verdict follows once every file has been hashed.'},
        {'drive': 'USB-32GB', 'state': 'done', 'why': 'Wiped after verification; now holds an offline mirror of Archive/Mail.'},
    ]
    plan = [
        ('Mail/', 'all IMAP/maildir data, by server then domain', ['mail.example.com/ ← Mail/mail.example.com (2011–2020, 380 GB)', 'clients/ ← Mail/thunderbird-profiles']),
        ('Machines/', 'computer backups, by machine, dated', ['macbook-pro-2016/2022-06 ← Backups/TimeMachine/MacBook Pro 2016, MacBook-HD/Users', 'thinkpad-2019/2023-01 ← Backups/thinkpad-2019, old-laptop/home', 'desktop-2011/ ← Backups/old-desktop-2011, Seagate 2TB/Backup', 'disk-images/ ← Disk images/*']),
        ('Phones/', '', ['iphone/ ← Backups/iphone', 'android/ ← Backups/android']),
        ('Photos/', 'one copy of every photo, by year', ['2008–2024/ ← Photos/*, WD Passport/Photos, Photos/Takeout (merged)']),
        ('Projects/', '', ['git/ ← Projects/git (143 repositories)', 'clients/ ← Projects/clients, Projects/archive-2012']),
        ('Media/', 'libraries, one copy each', ['Music/, Movies/, Shows/ ← Media/*, Seagate 2TB/Music']),
        ('Exports/', 'cloud takeouts', ['google-takeout-2024/, facebook-2024/']),
    ]
    mail = [{'domain': 'example.com', 'bytes': int(380 * GB), 'files': 2_310_000, 'msgs': 2_240_000, 'mailboxes': 214},
            {'domain': 'example.org', 'bytes': int(31 * GB), 'files': 162_000, 'msgs': 158_000, 'mailboxes': 12}]
    return {'generated': '2026-09-21 14:32', 'volumes': volumes, 'redundant_bytes': int(5.9 * TB),
            'dups': dups, 'containment': [], 'mail': mail, 'verify': verify, 'verdicts': verdicts,
            'plan': [{'dir': d, 'desc': s, 'items': i} for d, s, i in plan],
            'totals': {'files': sum(v['files'] for v in volumes), 'bytes': sum(v['bytes'] for v in volumes)}}


def status():
    vol = lambda v, mount, done, total, files, inflight=(): {'vol': v, 'mount': mount, 'planned': True, 'chunks_total': total,
                                                             'chunks_done': done, 'files': files, 'inflight': list(inflight)}
    mirror = lambda k, used_tb: {'k': k, 'mount': f'/mnt/mirror-{k.lower()}', 'size': int(1 * TB), 'free': int((1 - used_tb) * TB)}
    return {
        'time': '14:32:08', 'load': [3.1, 2.8, 2.6],
        'procs': {'indexer': True, 'hasher': True, 'mail_paused': False, 'finds': 2},
        'index': [
            vol('big', '/mnt/archive-18tb', 686, 743, 14_802_113, [{'dir': './Backups/rsync-daily', 'files': 2_190_442}, {'dir': './Photos/Takeout', 'files': 412_331}]),
            vol('macbook', '/mnt/macbook-hd', 38, 41, 1_480_902, [{'dir': './Users/alex/Library', 'files': 88_110}]),
            vol('laptop', '/mnt/old-laptop', 52, 52, 1_904_220),
            vol('passport', '/mnt/wd-passport', 24, 24, 902_114),
            vol('seagate', '/mnt/seagate-2tb', 12, 12, 402_009),
            {'vol': 'usb', 'mount': '/mnt/usb-32gb', 'planned': False},
        ],
        'hash': {'rows': 2_114_050, 'bytes': int(1.32 * TB), 'mbs': 142, 'errors': 1, 'log': 'hash-small.log',
                 'done_files': 1_412_300, 'todo_files': 2_208_410, 'done_gb': 1318.4, 'todo_gb': 2140.2,
                 'errors_detail': ['ERR /mnt/wd-passport/Videos/2014/clip-0419.mov: Input/output error (queued for retry)']},
        'stage': [
            {'drive': 'old-laptop', 'plan_bytes': int(4.2 * GB), 'plan_files': 11_204, 'bytes': int(4.2 * GB), 'files': 11_204,
             'status': '[14:02] done, verified by re-hash', 'rate': '', 'errors': 0, 'done': True},
            {'drive': 'WD Passport', 'plan_bytes': int(212 * GB), 'plan_files': 61_230, 'bytes': int(131 * GB), 'files': 37_002,
             'status': '[14:31] copying Photos/2020', 'rate': '131.02G  61%  98.21MB/s    0:13:48', 'errors': 0, 'done': False},
        ],
        'mirror': {
            'drives': [mirror('A', 0.82), mirror('B', 0.64), mirror('C', 0.31)],
            'mail': {'units': 214, 'done': 173, 'src_bytes': int(380 * GB), 'done_bytes': int(301 * GB), 'out_bytes': int(212 * GB),
                     'files': 2_310_000, 'done_files': 1_842_000, 'warn': 0,
                     'last': 'mail.example.com/alex → A · 1.4 GB → 0.9 GB zst · 14,021 messages · verified'},
            'plain': [{'name': 'archive-subset', 'bytes': int(402 * GB), 'files': 1_210_442}, {'name': 'photos', 'bytes': int(1.02 * TB), 'files': 188_213}],
        },
        'disks': [
            {'dev': 'sde', 'serial': 'ZL2PX0R1', 'name': 'ARCHIVE-18TB', 'r_iops': 61, 'r_mbs': 1.2, 'w_mbs': 0.4, 'util': 98, 'bogus': False},
            {'dev': 'sdc', 'serial': 'WX51A73K0P2F', 'name': 'WD Passport', 'r_iops': 14, 'r_mbs': 98.4, 'w_mbs': 0.0, 'util': 71, 'bogus': False},
            {'dev': 'sdb', 'serial': 'S3Z9NB0K412', 'name': 'MacBook-HD', 'r_iops': 212, 'r_mbs': 142.0, 'w_mbs': 0.0, 'util': 64, 'bogus': False},
            {'dev': 'sdg', 'serial': 'WD-WXC1E83P', 'name': 'mirror A', 'r_iops': 0, 'r_mbs': 0.0, 'w_mbs': 88.2, 'util': 52, 'bogus': False},
        ],
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--port', type=int, default=8790)
    port = ap.parse_args().port
    page = _dash_page()
    # the app's shell.css ships basic-Latin fonts only; for localised screenshots use the site's unicode-range set
    # (Latin, Latin Extended, Cyrillic stand-ins) so cs/sk/pl/uk render in the brand faces
    site_faces = '\n'.join(l for l in (SITE_PUBLIC / 'site.css').read_text(encoding='utf-8').splitlines() if l.startswith('@font-face'))
    shell = '\n'.join(l for l in (REPO / 'assets' / 'shell.css').read_text(encoding='utf-8').splitlines() if not l.startswith('@font-face'))
    rep = (REPO / 'assets' / 'report.html').read_text(encoding='utf-8')
    for pattern, repl in PRIVATE_PATTERNS:
        rep, n = re.subn(pattern, repl, rep)
        assert n, f'report.html changed: {pattern!r} no longer matches; update PRIVATE_PATTERNS'
    routes = {
        '/': (page.replace('__INIT__', json.dumps(status())), 'text/html; charset=utf-8'),
        '/api/status': (json.dumps(status()), 'application/json'),
        '/report': (rep, 'text/html; charset=utf-8'),
        '/api/report': (json.dumps(report()), 'application/json'),
        '/assets/shell.css': (site_faces + '\n' + shell, 'text/css'),
    }

    class H(BaseHTTPRequestHandler):
        def log_message(self, *a): pass

        def do_GET(self):
            path = self.path.split('?', 1)[0]
            if path in routes:
                body, ct = routes[path]; body = body.encode()
            elif path.startswith(('/assets/', '/fonts/')) and '..' not in path:
                fp = REPO / 'assets' / path.lstrip('/').removeprefix('assets/')
                if not fp.is_file() and path.startswith('/fonts/'):
                    fp = SITE_PUBLIC / path.lstrip('/')
                if not fp.is_file():
                    self.send_response(404); self.end_headers(); return
                body = fp.read_bytes()
                ct = {'.css': 'text/css', '.woff2': 'font/woff2'}.get(fp.suffix, 'application/octet-stream')
            else:
                self.send_response(404); self.end_headers(); return
            self.send_response(200); self.send_header('Content-Type', ct); self.send_header('Content-Length', str(len(body)))
            self.end_headers(); self.wfile.write(body)

    print(f'demo UI → http://127.0.0.1:{port}/ and /report', flush=True)
    ThreadingHTTPServer(('127.0.0.1', port), H).serve_forever()


if __name__ == '__main__':
    main()
