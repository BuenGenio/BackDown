#!/usr/bin/env python3
"""Generate the invented drives for the BackDown docs demo. Deterministic (seeded); nothing real.

Layout (under DEMO/drives, each top-level folder is one "drive"):
  Old Laptop     an old Linux laptop's disk: home/alex with dotfiles, Documents, Pictures (GPS photos),
                 Music, Videos, Mail (Maildir), Projects (git repo + node_modules junk), Downloads
  Backup 2021    an external backup drive: Time Machine set, a 2020 rsync copy of the laptop, an iPhone
                 backup, a Nextcloud data dir, database dumps, a WordPress site, a 2018 mbox export
  Camera cards   DCIM dumps from SD cards: originals of two trips, plus photos that exist nowhere else
  Archive        empty: `backdown plan`/`apply` fill it
  Mirror         empty: `backdown mirror` fills it
DEMO/nas/volume1, volume2: the NAS's drives (a second BackDown index, reached over the ssh shim).
"""
import io, json, os, random, sqlite3, struct, subprocess, sys, time, zlib
from datetime import datetime, timezone, timedelta
from email.message import EmailMessage
from email.utils import format_datetime, make_msgid
from pathlib import Path
from PIL import Image, ImageDraw, ExifTags

DEMO = Path(sys.argv[1]).resolve()
DR = DEMO / 'drives'
R = random.Random(20261109)
KiB, MiB = 1 << 10, 1 << 20

def ts(y, m=None, d=None, h=None):
    m = m or R.randint(1, 12); d = d or R.randint(1, 28); h = h if h is not None else R.randint(7, 22)
    return int(datetime(y, m, d, h, R.randint(0, 59), R.randint(0, 59), tzinfo=timezone.utc).timestamp())

def put(path, data, mtime, sparse_to=None):
    path = Path(path); path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, 'wb') as f:
        f.write(data)
        if sparse_to and sparse_to > len(data):
            f.truncate(sparse_to)
    os.utime(path, (mtime, mtime))
    return path

def copy(src, dst):
    """a byte-identical copy (sparse tails stay sparse), keeping mtime"""
    src, dst = Path(src), Path(dst); dst.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(['cp', '--sparse=always', '-p', str(src), str(dst)], check=True)

def copytree(src, dst, pick=None):
    n = 0
    for p in sorted(Path(src).rglob('*')):
        if p.is_file() and (pick is None or pick(p)):
            copy(p, Path(dst) / p.relative_to(src)); n += 1
    return n

def rbytes(n, seed):
    """pseudo-random bytes, cheap: a seeded stream"""
    r = random.Random(seed); return bytes(r.getrandbits(8) for _ in range(min(n, 4096))) * (n // 4096 + 1)

def text(n_words, seed):
    r = random.Random(seed)
    W = ('archive drive photo backup copy folder index hash verify keep drop restore mail message plan budget '
         'trip spring summer invoice meeting notes draft final review version lisbon paris kyoto lviv camera lens '
         'battery charger cable laptop desktop server share sync mirror check restore recover rescue').split()
    out, line = [], []
    for i in range(n_words):
        line.append(r.choice(W))
        if len(line) >= r.randint(8, 14): out.append(' '.join(line).capitalize() + '.'); line = []
    return '\n'.join(out) + '\n'

# ---------- photos
TRIPS = [  # name, year, month, lat, lon, count, camera
    ('Lisbon 2017', 2017, 5, 38.7139, -9.1394, 48, ('Canon', 'Canon EOS 6D')),
    ('Paris 2019', 2019, 6, 48.8575, 2.3514, 64, ('Canon', 'Canon EOS 6D')),
    ('Lviv 2021', 2021, 9, 49.8419, 24.0315, 56, ('Apple', 'iPhone 11')),
    ('Kyoto 2023', 2023, 11, 35.0116, 135.7681, 72, ('SONY', 'ILCE-7M3')),
]
PALETTES = [((235, 200, 120), (90, 60, 140)), ((120, 180, 230), (30, 60, 90)), ((210, 110, 80), (250, 230, 200)),
            ((70, 130, 90), (200, 230, 180)), ((40, 40, 60), (220, 160, 90)), ((250, 250, 250), (60, 90, 160))]

def jpeg(seed, when, lat=None, lon=None, camera=('Canon', 'Canon EOS 6D'), size=(600, 450)):
    r = random.Random(seed)
    a, b = r.choice(PALETTES)
    img = Image.new('RGB', size, a); d = ImageDraw.Draw(img)
    for y in range(0, size[1], 4):  # a sky-to-ground gradient
        t = y / size[1]; d.rectangle([0, y, size[0], y + 4], fill=tuple(int(a[i] * (1 - t) + b[i] * t) for i in range(3)))
    for _ in range(r.randint(4, 12)):  # "buildings", "people"
        x0, y0 = r.randint(0, size[0] - 40), r.randint(size[1] // 3, size[1] - 20)
        d.rectangle([x0, y0, x0 + r.randint(20, 160), size[1]], fill=tuple(r.randint(20, 120) for _ in range(3)))
    d.ellipse([r.randint(0, 500), 20, r.randint(540, 640), 120], fill=(255, 240, 200))
    ex = Image.Exif()
    ex[ExifTags.Base.Make], ex[ExifTags.Base.Model] = camera
    dt = datetime.fromtimestamp(when, timezone.utc).strftime('%Y:%m:%d %H:%M:%S')
    ex[ExifTags.Base.DateTime] = dt
    ex.get_ifd(ExifTags.IFD.Exif)[ExifTags.Base.DateTimeOriginal] = dt
    if lat is not None:
        g = ex.get_ifd(ExifTags.IFD.GPSInfo)
        def dms(v):
            v = abs(v); dg = int(v); mn = int((v - dg) * 60); sc = round(((v - dg) * 60 - mn) * 60, 2)
            return (float(dg), float(mn), sc)
        g[ExifTags.GPS.GPSVersionID] = b'\x02\x03\x00\x00'
        g[ExifTags.GPS.GPSLatitudeRef] = 'N' if lat >= 0 else 'S'; g[ExifTags.GPS.GPSLatitude] = dms(lat)
        g[ExifTags.GPS.GPSLongitudeRef] = 'E' if lon >= 0 else 'W'; g[ExifTags.GPS.GPSLongitude] = dms(lon)
    out = io.BytesIO(); img.save(out, 'JPEG', quality=80, exif=ex.tobytes()); return out.getvalue()

def photos(root, trips=TRIPS, prefix='IMG_', start=1000, pad=False):
    """Pictures/<trip>/IMG_nnnn.JPG with GPS; each ~1.2-3.6 MB on paper (a sparse tail), tens of KB on disk"""
    paths = []; n = start
    for name, y, m, lat, lon, count, cam in trips:
        for i in range(count):
            when = ts(y, m, R.randint(2, 12), R.randint(8, 20))
            seed = f'{name}/{i}'
            data = jpeg(seed, when, lat + R.uniform(-0.03, 0.03), lon + R.uniform(-0.04, 0.04), cam)
            p = put(Path(root) / name / f'{prefix}{n:04d}.JPG', data, when, (1200 + R.randint(0, 2400)) * KiB if pad else None)
            paths.append(p); n += 1
    return paths

def home_photos(root):
    """photos without GPS, across the years, in Pictures/<year>/"""
    for y in (2015, 2016, 2018, 2020, 2022, 2024):
        for i in range(R.randint(8, 16)):
            when = ts(y); put(Path(root) / str(y) / f'DSC_{R.randint(100, 9999):04d}.jpg', jpeg(f'home/{y}/{i}', when, camera=('NIKON', 'D7200')), when)

# ---------- documents
def documents(root, years=(2016, 2017, 2018, 2019), n=70):
    names = ['Insurance', 'Lease', 'Camera receipt', 'Trip budget', 'Tax return', 'Notes', 'Reading list', 'Recipes', 'Warranty',
             'Flat plans', 'Club newsletter', 'Minutes', 'Invoice', 'Packing list', 'Journal', 'Letter']
    for i in range(n):
        y = R.choice(years); when = ts(y)
        kind = R.choice(['txt', 'txt', 'md', 'csv', 'json', 'html', 'pdf'])
        base = f'{R.choice(names)} {y}' + (f' ({i})' if i % 7 == 0 else '')
        sub = R.choice(['', '', 'Finance', 'Travel', 'Home', 'Camera club'])
        p = Path(root) / sub / f'{base}.{kind}'
        if kind in ('txt', 'md'): put(p, (f'# {base}\n\n' if kind == 'md' else '') .encode() + text(R.randint(60, 900), base).encode(), when)
        elif kind == 'csv': put(p, ('date,item,amount\n' + ''.join(f'{y}-{R.randint(1,12):02d}-{R.randint(1,28):02d},{R.choice(names)},{R.randint(5,900)}.{R.randint(0,99):02d}\n' for _ in range(R.randint(10, 80)))).encode(), when)
        elif kind == 'json': put(p, json.dumps({'title': base, 'year': y, 'items': [{'n': k, 'v': R.random()} for k in range(R.randint(3, 30))]}, indent=1).encode(), when)
        elif kind == 'html': put(p, f'<!doctype html><title>{base}</title><h1>{base}</h1><p>{text(120, base)}</p>'.encode(), when)
        else:  # a small real PDF (one page, an image), via Pillow
            img = Image.new('RGB', (595, 842), (250, 248, 240)); d = ImageDraw.Draw(img); d.text((60, 60), base, fill=(20, 20, 20))
            for k, line in enumerate(text(160, base).split('\n')[:30]): d.text((60, 110 + 20 * k), line, fill=(50, 50, 50))
            out = io.BytesIO(); img.save(out, 'PDF', resolution=72); put(p, out.getvalue(), when)

def office_docs(root, when):
    """a .docx, .xlsx, .odt and .pptx made by LibreOffice headless from plain sources (real files for the viewers)"""
    tmp = DEMO / 'tmp-office'; tmp.mkdir(exist_ok=True)
    (tmp / 'Camera club constitution.txt').write_text('Camera club constitution\n\n' + text(400, 'constitution'))
    (tmp / 'Trip budget 2019.csv').write_text('item,planned,actual\nflights,420,398.5\nhotel,860,912\nfood,300,344.2\nmuseums,120,96\n')
    (tmp / 'Flat renovation plan.txt').write_text('Flat renovation plan\n\n' + text(300, 'flat'))
    (tmp / 'Club talk 2019.txt').write_text('Club talk 2019\n\n' + text(120, 'talk'))
    conv = [('Camera club constitution.txt', 'docx'), ('Trip budget 2019.csv', 'xlsx'), ('Flat renovation plan.txt', 'odt'), ('Club talk 2019.txt', 'pptx')]
    for src, fmt in conv:
        subprocess.run(['soffice', '--headless', '--convert-to', fmt, '--outdir', str(tmp), str(tmp / src)], check=False, capture_output=True, timeout=180)
    for src, fmt in conv:
        made = tmp / (Path(src).stem + '.' + fmt)
        if made.exists():
            copy(made, Path(root) / made.name); os.utime(Path(root) / made.name, (when, when))

# ---------- mail
PEOPLE = [('Alex Rivera', 'alex@example.net'), ('Maria Kowalska', 'maria@photoclub.example'), ('Jonas Berg', 'jonas.berg@example.org'),
          ('Priya Nair', 'priya@example.org'), ('Camera Store', 'news@camera-store.example'), ('Tomasz Lis', 'tomasz@photoclub.example'),
          ('Hanna Melnyk', 'hanna.melnyk@example.net'), ('Flat Agency', 'lettings@flat-agency.example')]
SUBJECTS = ['Paris trip: dates?', 'Club meeting minutes', 'Lens for sale', 'Re: prints for the exhibition', 'Invoice 2019-0{n}',
            'Your order has shipped', 'Flat inspection', 'Photos from Lisbon', 'Newsletter: spring offers', 'Backup of the old laptop',
            'Re: Re: which drive to buy', 'Kyoto itinerary', 'Lviv weekend', 'Membership renewal', 'Workshop sign-up']

def messages(n, y0, y1, seed):
    """n messages in threads, oldest first: (Message-ID, bytes, timestamp)"""
    r = random.Random(seed); out = []; threads = []
    for i in range(n):
        when = ts(r.randint(y0, y1))
        m = EmailMessage()
        frm = r.choice(PEOPLE); to = ('Alex Rivera', 'alex@example.net') if frm[1] != 'alex@example.net' else r.choice(PEOPLE[1:])
        m['From'] = f'{frm[0]} <{frm[1]}>'; m['To'] = f'{to[0]} <{to[1]}>'
        mid = make_msgid(domain=frm[1].split('@')[1]); m['Message-ID'] = mid
        if threads and r.random() < 0.45:
            parent = r.choice(threads); m['In-Reply-To'] = parent[0]; m['References'] = parent[0]; subj = 'Re: ' + parent[1].removeprefix('Re: ')
        else:
            subj = r.choice(SUBJECTS).replace('{n}', str(r.randint(10, 99)))
        m['Subject'] = subj; m['Date'] = format_datetime(datetime.fromtimestamp(when, timezone.utc))
        m.set_content(text(r.randint(20, 220), mid) + f'\n-- \n{frm[0]}\n')
        if r.random() < 0.12:
            m.add_attachment(jpeg(mid, when), maintype='image', subtype='jpeg', filename=f'IMG_{r.randint(1000,9999)}.jpg')
        threads.append((mid, subj)); out.append((mid, m.as_bytes(), when))
    out.sort(key=lambda t: t[2]); return out

def maildir(root, msgs, folders=('', '.Sent', '.Camera club', '.Receipts'), host='alex-laptop', seed=1):
    r = random.Random(seed)
    for f in folders:
        for sub in ('cur', 'new', 'tmp'): (Path(root) / f / sub).mkdir(parents=True, exist_ok=True)
    for i, (mid, data, when) in enumerate(msgs):
        f = '' if r.random() < 0.6 else r.choice(folders[1:])
        sub, flags = ('new', '') if r.random() < 0.08 else ('cur', ':2,' + ('S' if r.random() < 0.9 else '') + ('R' if r.random() < 0.3 else '') + ('F' if r.random() < 0.05 else ''))
        put(Path(root) / f / sub / f'{when}.M{i}P{r.randint(1000,9999)}.{host}{flags}', data, when)
    # a client's bookkeeping files, like a Dovecot store has
    put(Path(root) / 'dovecot-uidlist', b'3 V1 N%d G%s\n' % (len(msgs) + 1, ''.join(r.choice('0123456789abcdef') for _ in range(32)).encode()), msgs[-1][2])
    put(Path(root) / 'subscriptions', '\n'.join(f.strip('.') for f in folders[1:]).encode() + b'\n', msgs[-1][2])

def mbox(path, msgs):
    out = io.BytesIO()
    for mid, data, when in msgs:
        out.write(b'From alex@example.net ' + datetime.fromtimestamp(when, timezone.utc).strftime('%a %b %d %H:%M:%S %Y').encode() + b'\n')
        out.write(data.replace(b'\nFrom ', b'\n>From ')); out.write(b'\n')
    put(path, out.getvalue(), msgs[-1][2])

# ---------- the rest
def dotfiles(home, when):
    put(home / '.bashrc', b'# ~/.bashrc\nexport EDITOR=vim\nalias ll="ls -la"\n', when)
    put(home / '.profile', b'# ~/.profile\nif [ -n "$BASH_VERSION" ]; then . ~/.bashrc; fi\n', when)
    put(home / '.bash_history', b'ls\ncd Pictures\nrsync -av Pictures /media/alex/Backup\ngit status\n', when)
    put(home / '.config/gtk-3.0/settings.ini', b'[Settings]\ngtk-theme-name=Adwaita\n', when)
    put(home / '.config/user-dirs.dirs', b'XDG_PICTURES_DIR="$HOME/Pictures"\n', when)
    put(home / '.local/share/applications/mimeapps.list', b'[Default Applications]\nimage/jpeg=eog.desktop\n', when)
    put(home / '.ssh/known_hosts', b'nas.local ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDemoKeyNotRealxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\n', when)
    for i in range(240):  # a thumbnail cache: regenerable junk
        put(home / '.cache/thumbnails/normal' / f'{rbytes(16, i).hex()}.png', rbytes(R.randint(2, 9) * KiB, 1000 + i), when)

def git_repo(path, when):
    path.mkdir(parents=True, exist_ok=True)
    env = {**os.environ, 'GIT_AUTHOR_NAME': 'Alex Rivera', 'GIT_AUTHOR_EMAIL': 'alex@example.net', 'GIT_COMMITTER_NAME': 'Alex Rivera',
           'GIT_COMMITTER_EMAIL': 'alex@example.net', 'HOME': str(DEMO / 'tmp-home'), 'GIT_CONFIG_GLOBAL': '/dev/null', 'GIT_CONFIG_NOSYSTEM': '1'}
    (DEMO / 'tmp-home').mkdir(exist_ok=True)
    def g(*a, t=when): subprocess.run(['git', *a], cwd=path, env={**env, 'GIT_AUTHOR_DATE': f'@{t} +0000', 'GIT_COMMITTER_DATE': f'@{t} +0000'}, check=True, capture_output=True)
    g('init', '-q', '-b', 'main')
    (path / 'README.md').write_text('# gallery-site\n\nA static gallery for the camera club.\n'); (path / 'src').mkdir(exist_ok=True)
    (path / 'src/index.js').write_text("console.log('gallery');\n"); (path / 'src/gallery.css').write_text('body{margin:0}\n')
    g('add', '.'); g('commit', '-q', '-m', 'first version')
    for k in range(6):
        t = when + (k + 1) * 86400 * 9
        (path / 'src/index.js').write_text(f"console.log('gallery v{k+2}');\n" + text(30, k)); g('add', '.'); g('commit', '-q', '-m', f'tweak {k+1}', t=t)
    for p in path.rglob('*'):
        if p.is_file(): os.utime(p, (when, when))

def node_modules(path, when):
    for i in range(45):
        pkg = f'pkg-{i:02d}'
        put(path / pkg / 'package.json', json.dumps({'name': pkg, 'version': f'1.{i}.0'}).encode(), when)
        for k in range(R.randint(4, 14)): put(path / pkg / 'lib' / f'mod{k}.js', text(R.randint(20, 200), f'{pkg}{k}').encode(), when)

def music(root):
    for a in range(11):
        artist = f'Artist {chr(65 + a)}'
        for b in range(2):
            album = f'Album {b + 1}'
            for t in range(10):
                when = ts(R.randint(2010, 2019))
                head = b'ID3\x04\x00\x00\x00\x00\x00\x00' + b'\xff\xfb\x90\x00' + rbytes(2048, f'{artist}{album}{t}')
                put(Path(root) / artist / album / f'{t + 1:02d} Track {t + 1}.mp3', head, when, (3 + R.randint(0, 6)) * MiB + R.randint(0, MiB))

def videos(root, names_sizes):
    for name, mb, y in names_sizes:
        when = ts(y); put(Path(root) / name, b'\x00\x00\x00\x18ftypmp42\x00\x00\x00\x00mp42isom' + rbytes(4096, name), when, mb * MiB)

def downloads(root, when):
    inst = ['backdown_1.0.0-beta.4_amd64.deb', 'ubuntu-20.04.6-desktop-amd64.iso', 'firefox-115.0.tar.bz2', 'darktable-4.2.1.dmg',
            'rawtherapee-5.9.AppImage', 'photos-export-2019.zip', 'node-v16.20.0-linux-x64.tar.xz', 'hugin-2021.0.0.zip',
            'lens-profiles.zip', 'manual-eos-6d.zip', 'gimp-2.10.34.tar.bz2', 'inkscape-1.2.2.tar.xz', 'vlc-3.0.18.tar.xz',
            'printer-driver.deb', 'scanner-driver.deb', 'club-site-backup-2018.zip']
    for i, n in enumerate(inst): put(Path(root) / n, rbytes(8 * KiB, n), when - i * 86400 * 17, (12 + R.randint(0, 110)) * MiB)
    for i in range(22): put(Path(root) / f'{R.choice(["invoice", "ticket", "manual", "boarding-pass", "statement"])}-{2017 + i % 3}-{i:02d}.pdf', rbytes(R.randint(40, 400) * KiB, 9000 + i), when - i * 86400 * 5)

def sqlite_db(path, when, tables):
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists(): path.unlink()
    c = sqlite3.connect(path)
    for t, cols, rows in tables:
        c.execute(f'CREATE TABLE {t} ({", ".join(cols)})')
        c.executemany(f'INSERT INTO {t} VALUES ({", ".join("?" * len(cols))})', rows)
    c.commit(); c.close(); os.utime(path, (when, when))

def sql_dumps(root, when):
    for i, db in enumerate(['photoclub', 'gallery_site', 'members', 'bookings']):
        body = [f'-- MySQL dump 10.13  Distrib 8.0.{20 + i}, for Linux (x86_64)\n--\n-- Host: localhost    Database: {db}\n-- ------------------------------------------------------\n',
                f'CREATE DATABASE /*!32312 IF NOT EXISTS*/ `{db}`;\nUSE `{db}`;\n', 'DROP TABLE IF EXISTS `members`;\nCREATE TABLE `members` (\n  `id` int NOT NULL AUTO_INCREMENT,\n  `name` varchar(120) NOT NULL,\n  `email` varchar(200) DEFAULT NULL,\n  `joined` date DEFAULT NULL,\n  PRIMARY KEY (`id`)\n) ENGINE=InnoDB;\n',
                'INSERT INTO `members` VALUES ' + ','.join(f"({k},'{n}','{e}','{2015 + k % 5}-0{1 + k % 9}-1{k % 9}')" for k, (n, e) in enumerate(PEOPLE)) + ';\n',
                'DROP TABLE IF EXISTS `events`;\nCREATE TABLE `events` (\n  `id` int NOT NULL,\n  `title` varchar(200),\n  `held` date,\n  PRIMARY KEY (`id`)\n) ENGINE=InnoDB;\n',
                'INSERT INTO `events` VALUES ' + ','.join(f"({k},'Meeting {k}','{2016 + k // 12}-{1 + k % 12:02d}-05')" for k in range(1, 40)) + ';\n']
        for d in range(3):
            put(Path(root) / f'{db}-{2019 + d}-0{1 + d * 3}-01.sql', ''.join(body).encode(), when + d * 86400 * 90)

def wordpress(root, when):
    put(root / 'wp-config.php', b"<?php\ndefine('DB_NAME', 'gallery_site');\ndefine('DB_USER', 'gallery');\ndefine('DB_PASSWORD', 'not-a-real-password');\n", when)
    put(root / 'index.php', b"<?php\nrequire __DIR__ . '/wp-blog-header.php';\n", when)
    put(root / 'wp-blog-header.php', b"<?php\nrequire __DIR__ . '/wp-load.php';\n", when); put(root / '.htaccess', b'RewriteEngine On\n', when)
    for n in ('wp-admin/index.php', 'wp-admin/admin.php', 'wp-includes/version.php', 'wp-includes/functions.php', 'wp-content/themes/gallery/style.css',
              'wp-content/themes/gallery/index.php', 'wp-content/plugins/akismet/akismet.php'):
        put(root / n, text(R.randint(30, 300), n).encode(), when)
    for y in (2018, 2019):
        for m in range(1, 13, 2):
            for k in range(3):
                put(root / f'wp-content/uploads/{y}/{m:02d}/photo-{k}.jpg', jpeg(f'wp{y}{m}{k}', ts(y, m)), ts(y, m))

def ios_backup(root, when):
    udid = rbytes(20, 'udid').hex()[:40]; b = root / udid
    plist = lambda d: ('<?xml version="1.0" encoding="UTF-8"?>\n<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n<plist version="1.0"><dict>\n'
                       + ''.join(f'<key>{k}</key>' + (f'<string>{v}</string>' if isinstance(v, str) else f'<date>{v[1]}</date>' if isinstance(v, tuple) else f'<integer>{v}</integer>') + '\n' for k, v in d.items()) + '</dict></plist>\n').encode()
    put(b / 'Info.plist', plist({'Device Name': "Alex's iPhone", 'Display Name': "Alex's iPhone", 'Product Type': 'iPhone12,1', 'Product Version': '14.4', 'Serial Number': 'DEMO0IPHONE01',
                                 'Unique Identifier': udid.upper(), 'Last Backup Date': ('date', '2021-03-12T19:41:03Z'), 'iTunes Version': '12.11.0.26'}), when)
    put(b / 'Manifest.plist', plist({'Version': '10.0', 'Date': ('date', '2021-03-12T19:41:03Z'), 'IsEncrypted': 0, 'WasPasscodeSet': 1}), when)
    put(b / 'Status.plist', plist({'BackupState': 'new', 'Date': ('date', '2021-03-12T19:43:10Z'), 'IsFullBackup': 1, 'SnapshotState': 'finished', 'Version': '3.3'}), when)
    rows = []
    for i in range(380):
        fid = rbytes(20, f'f{i}').hex()[:40]; sub = fid[:2]
        size = R.randint(4, 900) * KiB
        put(b / sub / fid, rbytes(2048, fid), when - R.randint(0, 86400 * 400), size)
        rows.append((fid, R.choice(['CameraRollDomain', 'HomeDomain', 'AppDomain-com.example.app']), f'Media/DCIM/100APPLE/IMG_{i:04d}.JPG' if i % 3 else f'Library/Preferences/pref{i}.plist', 1, None))
    sqlite_db(b / 'Manifest.db', when, [('Files', ['fileID TEXT PRIMARY KEY', 'domain TEXT', 'relativePath TEXT', 'flags INTEGER', 'file BLOB'], rows),
                                          ('Properties', ['key TEXT', 'value BLOB'], [('salt', b'demo'), ('passwordHash', None)])])

def timemachine(root, home_src, when):
    host = "Alex's MacBook Air"
    for k, snap in enumerate(['2021-01-10-201512', '2021-02-14-093011', '2021-03-14-093011']):
        base = root / 'Backups.backupdb' / host / snap / 'Macintosh HD' / 'Users' / 'alex'
        copytree(home_src / 'Documents', base / 'Documents')
        copytree(home_src / 'Pictures', base / 'Pictures', pick=lambda p, k=k: p.suffix.lower() in ('.jpg', '.jpeg') and (hash(p.name) % 3 <= k))
        put(base / 'Library/Preferences/com.apple.finder.plist', rbytes(6 * KiB, snap), when)
        put(base / 'Desktop/todo.txt', b'- back up the laptop\n- order prints\n', when)
    put(root / 'Backups.backupdb' / host / 'com.apple.TimeMachine.MachineID.plist',
        b'<?xml version="1.0" encoding="UTF-8"?>\n<plist version="1.0"><dict><key>com.apple.backupd.ModelID</key><string>MacBookAir9,1</string><key>com.apple.backupd.HostUUID</key><string>DEMO-0000-HOST-UUID</string></dict></plist>\n', when)

def nextcloud(root, home_src, when):
    put(root / '.ocdata', b'', when); put(root / 'nextcloud.log', b'{"reqId":"demo","level":1,"time":"2021-02-01T10:00:00+00:00","message":"started"}\n', when)
    put(root / 'index.html', b'', when); put(root / '.htaccess', b'# Nextcloud data directory\nRequire all denied\n', when)
    copytree(home_src / 'Documents', root / 'alex/files/Documents', pick=lambda p: hash(p.name) % 2 == 0)
    copytree(home_src / 'Pictures/Paris 2019', root / 'alex/files/Photos/Paris 2019')
    for i in range(30): put(root / 'alex/files/Notes' / f'note-{i:02d}.md', text(R.randint(10, 80), f'nc{i}').encode(), when - i * 86400 * 3)
    for i in range(60): put(root / 'appdata_ocdemo1234/preview' / str(i) / '256-256.jpg', jpeg(f'prev{i}', when, size=(256, 256)), when)

# ================= build
def main():
    if DR.exists(): subprocess.run(['rm', '-rf', str(DR), str(DEMO / 'nas'), str(DEMO / 'tmp-office'), str(DEMO / 'tmp-home')], check=True)
    laptop = DR / 'Old Laptop'; home = laptop / 'home' / 'alex'
    when19 = ts(2019, 11, 2)
    print('Old Laptop'); dotfiles(home, when19)
    photos(home / 'Pictures'); home_photos(home / 'Pictures')
    for i in range(6):  # stitched panoramas: big TIFFs (a sparse tail), so the library's average size is a real camera's
        put(home / 'Pictures/Panoramas' / f'pano-{2016 + i}.tif', b'II*\x00' + rbytes(4096, f'pano{i}'), ts(2016 + i), (40 + R.randint(0, 40)) * MiB)
    documents(home / 'Documents'); office_docs(home / 'Documents', ts(2019, 3, 8))
    music(home / 'Music'); videos(home / 'Videos', [('Lisbon walk 2017.mp4', 160, 2017), ('Paris boat trip.mp4', 190, 2019), ('Club exhibition opening.mp4', 90, 2018), ('Lviv market.mov', 70, 2021)])
    msgs = messages(420, 2016, 2021, 'mail'); maildir(home / 'Mail', msgs[:300], seed=3)
    git_repo(home / 'Projects/gallery-site', ts(2018, 4, 2)); node_modules(home / 'Projects/gallery-site/node_modules', ts(2018, 6, 1))
    downloads(home / 'Downloads', when19)
    sqlite_db(home / '.local/share/shotwell/data/photo.db', when19, [('PhotoTable', ['id INTEGER PRIMARY KEY', 'filename TEXT', 'timestamp INTEGER', 'rating INTEGER'],
                                                                      [(i, f'/home/alex/Pictures/Paris 2019/IMG_{1048 + i}.JPG', ts(2019, 6), i % 6) for i in range(64)])])
    for n in ('etc/hostname', 'etc/fstab', 'etc/hosts'): put(laptop / n, (b'alex-laptop\n' if n.endswith('hostname') else text(20, n).encode()), when19)

    backup = DR / 'Backup 2021'; when21 = ts(2021, 3, 14, 9)
    print('Backup 2021'); timemachine(backup, home, when21)
    lc = backup / 'Laptop copy 2020'; copytree(home / 'Documents', lc / 'Documents'); copytree(home / 'Pictures', lc / 'Pictures'); copytree(home / 'Music', lc / 'Music')
    photos(lc / 'Pictures', trips=[('Lviv 2020', 2020, 8, 49.8419, 24.0315, 40, ('Apple', 'iPhone 11'))], start=3000)
    maildir(lc / 'Mail', msgs[:380], seed=4)  # the newer snapshot of the same mailbox
    mbox(backup / 'mail-export-2018.mbox', [m for m in msgs if m[2] < ts(2019, 1, 1, 0)])
    ios_backup(backup / 'iPhone backup', when21); nextcloud(backup / 'Nextcloud data', home, when21)
    sql_dumps(backup / 'Server/dumps', ts(2019, 1, 1)); wordpress(backup / 'Server/www/gallery', ts(2019, 5, 20))
    sqlite_db(backup / 'Server/gallery.sqlite', when21, [('photos', ['id INTEGER PRIMARY KEY', 'title TEXT', 'taken TEXT', 'likes INTEGER'], [(i, f'Photo {i}', f'2019-06-{1 + i % 28:02d}', i * 3 % 17) for i in range(120)]),
                                                        ('albums', ['id INTEGER PRIMARY KEY', 'name TEXT'], [(1, 'Paris'), (2, 'Lisbon'), (3, 'Club')])])
    videos(backup / 'Videos', [('Lisbon walk 2017.mp4', 160, 2017), ('Family dinner 2016.mp4', 120, 2016)])
    copy(home / 'Videos/Lisbon walk 2017.mp4', backup / 'Videos/Lisbon walk 2017.mp4')

    cards = DR / 'Camera cards'; print('Camera cards')
    for trip, folder in (('Paris 2019', '100CANON'), ('Lisbon 2017', '101CANON')):
        n = 0
        for p in sorted((home / 'Pictures' / trip).glob('*.JPG')):
            if n % 4 != 3: copy(p, cards / trip / 'DCIM' / folder / p.name)
            n += 1
    photos(cards / 'Kyoto 2023/DCIM/100MSDCF', trips=[('', 2023, 11, 35.0116, 135.7681, 44, ('SONY', 'ILCE-7M3'))], prefix='DSC', start=5100)
    for i in range(9): put(cards / 'Kyoto 2023/DCIM/100MSDCF' / f'DSC{5200 + i}.ARW', b'II*\x00' + rbytes(4096, f'arw{i}'), ts(2023, 11), (24 + R.randint(0, 6)) * MiB)
    for d in ('Archive', 'Mirror'): (DR / d).mkdir(parents=True, exist_ok=True)

    nas = DEMO / 'nas'; print('nas')
    v1, v2 = nas / 'volume1', nas / 'volume2'
    copytree(home / 'Music', v1 / 'Music'); copytree(home / 'Pictures/Paris 2019', v1 / 'Photos/Paris 2019'); copytree(home / 'Pictures/Kyoto 2023', v1 / 'Photos/Kyoto 2023')
    videos(v1 / 'Video', [('Series/S01E01.mkv', 700, 2022), ('Series/S01E02.mkv', 690, 2022), ('Films/Nosferatu (1922).mp4', 450, 2020)])
    copytree(home / 'Documents', v2 / 'Backups/alex-laptop-2019/Documents'); copytree(home / 'Projects/gallery-site', v2 / 'Backups/alex-laptop-2019/Projects/gallery-site', pick=lambda p: 'node_modules' not in p.parts)
    for d in ('Pool',): (v1 / d).mkdir(parents=True, exist_ok=True)
    put(v2 / 'Backups/README.txt', b'rsync copies of the laptops, by year\n', ts(2019, 12, 30))
    subprocess.run(['rm', '-rf', str(DEMO / 'tmp-office'), str(DEMO / 'tmp-home')])
    for d in sorted(DR.iterdir()) + [v1, v2]:
        n = sum(1 for p in d.rglob('*') if p.is_file()); b = sum(p.stat().st_size for p in d.rglob('*') if p.is_file())
        print(f'  {d.name:14s} {n:5d} files  {b / 1e9:6.2f} GB on paper')
    print('disk use:', subprocess.run(['du', '-sh', str(DEMO)], capture_output=True, text=True).stdout.split()[0])

main()
