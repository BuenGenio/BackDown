#!/usr/bin/env python3
"""Refile Archive/_staging/<drive>/... into the Archive/ tree by same-filesystem rename.

  bd-refile.py            dry run: print what would move where (with sizes)
  bd-refile.py --apply    do it (run under sudo: staged files keep their original owners)

Semantics: a mapping moves a staged path to a destination. If the destination does not
exist -> one rename. If it does (merge) -> recurse; files that already exist at the
destination with the same size are dropped from staging (content is already there),
anything else that collides goes to Archive/_conflicts/<dest-path>. Every rename is
appended to journal/refile.log as "<src>\t<dst>". Re-running is safe: finished sources
no longer exist and are skipped.
"""
import os, sys, time, shutil
from pathlib import Path

BIG = Path("/media/buengenio/FUCKING_BIG"); ST = BIG / "Archive" / "_staging"; AR = BIG / "Archive"
HERE = Path(__file__).parent; JOURNAL = HERE / "journal" / "refile.log"

# (staged path relative to _staging, destination relative to Archive)
MAP = [
    # sda3 — the "Recovery" partition
    ("sda3/Kate.tgz",                    "Machines/linux-home/2022-08-recovery/Kate.tgz"),
    ("sda3/Scaut",                       "Machines/linux-home/2022-08-recovery/Scaut"),
    # sda4 remnants (rest of sda4 already lives in FUCKING_BIG/673GB)
    ("sda4/HR",                          "Companies/Maximal/HR"),
    ("sda4/Nextcloud",                   "Servers/nextcloud/sda4-Nextcloud"),
    ("sda4/8,2",                         "Machines/linux-home/8,2"),
    ("sda4/MacBookPro-12,1",             "Machines/macbookpro-12,1/sda4-remnants"),
    # stuff
    ("stuff/Shows",                      "Media/Shows"),
    ("stuff/buengenio.tgz",              "Machines/linux-home/2020-11/buengenio.tgz"),
    ("stuff/buengenio",                  "Machines/linux-home/2020-11/buengenio"),
    ("stuff/buengenio (2)",              "Machines/linux-home/2020-11/buengenio (2)"),
    ("stuff/sherborne.sh",               "Companies/Sherborne/sherborne.sh"),
    # sdc1 — Backups-Mates
    ("sdc1/Backups-Mates/tom",           "People/tom"),
    ("sdc1/Backups-Mates/Damian",        "People/damian"),
    ("sdc1/Backups-Mates/vinoselo",      "People/vinoselo"),
    ("sdc1/Backups-Mates/PW Backup",     "People/pw"),
    ("sdc1/Backups-Mates/image.dd",      "Machines/disk-images/2021-01-Backups-Mates-image.dd"),
    ("sdc1/Backups-Mates/swapfile.gz",   "Machines/disk-images/2021-01-Backups-Mates-swapfile.gz"),
    # sdc1 — My Backups (Gene's Windows era)
    ("sdc1/My Backups/iPhone",           "Phones/iphone/2014"),
    ("sdc1/My Backups/Photographs",      "Media/Photos/2014-Photographs"),
    ("sdc1/My Backups/Mail",             "Mail/clients/thunderbird-lentongrp-2014"),
    ("sdc1/My Backups",                  "Machines/windows-gene/2014"),          # remainder: Gene, Documents, Documents and Settings, dmg…
    # sdc1 — movies/ split
    ("sdc1/movies/Samsung",              "Phones/samsung-2014"),
    ("sdc1/movies/Steve Jobs - The Biography", "Media/Books/Steve Jobs - The Biography"),
    ("sdc1/movies/Daft Punk Discography (320Kbs)", "Media/Music/Daft Punk Discography (320Kbs)"),
    ("sdc1/movies/Dethklok Discography.rar", "Media/Music/Dethklok Discography.rar"),
    ("sdc1/movies/Mojinos Escozios - Mena Chatrua.2011[www.lokotorrents.com][mp3]", "Media/Music/Mojinos Escozios - Mena Chatrua.2011"),
    ("sdc1/movies/System Of A Down",     "Media/Music/System Of A Down"),
    ("sdc1/movies/videos/series",        "Media/Shows/_from-videos"),
    ("sdc1/movies/videos",               "Media/Videos/2014-videos"),
    *[(f"sdc1/movies/{d}", f"Media/Shows/{d}") for d in [
        "BBC.A.Year.in.Tibet.2008", "Emma BBC (1972) DVDrip with Eng. subs [missix]", "Family Guy - All seasons plus extras",
        "Family Guy Season 3", "Futurama - Season 1", "Futurama - Season 2", "Futurama - Season 3", "Futurama - Season 4",
        "Futurama - Season 5", "Futurama - Season 6", "Futurama - Season 7", "Terriers"]],
    ("sdc1/movies",                      "Media/Movies"),                        # everything left = films
    ("sdc1/Snow White And The Huntsman 2012 DVDRip  XViD - DivXTeam", "Media/Movies/Snow White And The Huntsman 2012 DVDRip  XViD - DivXTeam"),
    # sdc1 — the rest
    ("sdc1/Music",                       "Media/Music"),
    ("sdc1/Downloads",                   "Machines/windows-gene/Downloads"),
    ("sdc1/Apps",                        "Software/installers"),
    ("sdc1/games",                       "Software/games"),
    ("sdc1/Books",                       "Media/Books"),
    ("sdc1/Dropbox/Dropbox (Maximal Concepts)", "Companies/Maximal/Dropbox"),
    ("sdc1/vhosts",                      "Servers/maximal-web/vhosts-2014"),
    ("sdc1/Sherborne",                   "Companies/Sherborne/2013"),
    ("sdc1/found.000",                   "Machines/windows-gene/chkdsk-found.000"),
    ("sdc1/found.001",                   "Machines/windows-gene/chkdsk-found.001"),
    # remnants found after the first pass
    ("sda4/Maximal-Netcloud-Users",      "Companies/Maximal/Netcloud-Users-remnants"),
    ("sda4/neo4j",                       "Servers/databases/neo4j-sda4"),
    ("sda4/Sherborne",                   "Companies/Sherborne/sda4"),
    ("sda4/Carbon X1",                   "Machines/carbon-x1/sda4-remnants"),
    ("sda4/Радіо НВ",                    "Media/Podcasts-UA/Радіо НВ"),
    ("stuff/Music",                      "Media/Music"),
    ("sdc1/VC_RED.cab",                  "Software/installers/VC_RED.cab"),
    ("sdc1/VC_RED.MSI",                  "Software/installers/VC_RED.MSI"),
    # verbatim
    ("verbatim/Maximal",                 "Companies/Maximal/Office"),
    ("verbatim/Maximal-WWW",             "Servers/maximal-web/2021"),
    ("verbatim/Nextcloud-master",        "Servers/nextcloud/verbatim-master"),
    ("verbatim/Guille",                  "People/guille"),
]
JUNK = {".DS_Store", "._.DS_Store", "Thumbs.db", "desktop.ini", ".Spotlight-V100", ".Trashes", ".fseventsd", ".TemporaryItems",
        "System Volume Information", "$RECYCLE.BIN", ".apdisk", ".com.apple.timemachine.donotpresent", ".Trash-1000", "2885"}
def is_junk(name): return name in JUNK or name.startswith("._")

apply = "--apply" in sys.argv
stats = {"moved": 0, "merged_dirs": 0, "dropped_dupes": 0, "conflicts": 0, "bytes": 0}
jf = open(JOURNAL, "a") if apply else None
def log(src, dst):
    if jf: jf.write(f"{src}\t{dst}\n")
def du(p):
    t = 0
    for r, ds, fs in os.walk(p):
        for f in fs:
            try: t += os.lstat(os.path.join(r, f)).st_size
            except OSError: pass
    return t

def move(src: Path, dst: Path):
    """rename src onto dst, merging directories."""
    if not dst.exists():
        if apply:
            dst.parent.mkdir(parents=True, exist_ok=True); os.rename(src, dst); log(src, dst)
        stats["moved"] += 1; return
    if src.is_dir() and dst.is_dir():
        stats["merged_dirs"] += 1
        for child in sorted(os.listdir(src)):
            move(src / child, dst / child)
        if apply:
            try: os.rmdir(src)
            except OSError: pass
        return
    # file collision
    try: same = src.lstat().st_size == dst.lstat().st_size
    except OSError: same = False
    if same:
        stats["dropped_dupes"] += 1
        if apply: src.unlink(); log(src, "<dropped: identical size at " + str(dst) + ">")
        return
    c = AR / "_conflicts" / dst.relative_to(AR)
    stats["conflicts"] += 1
    if apply:
        c.parent.mkdir(parents=True, exist_ok=True); os.rename(src, c); log(src, c)

for rel_src, rel_dst in MAP:
    src = ST / rel_src; dst = AR / rel_dst
    if not src.exists():
        continue
    size = du(src) if src.is_dir() else src.lstat().st_size
    stats["bytes"] += size
    print(f"{size/1e9:8.2f} GB  _staging/{rel_src}  ->  Archive/{rel_dst}" + ("" if not dst.exists() else "   (merge)"))
    move(src, dst)

# sweep: junk files and empty dirs left in _staging
for r, ds, fs in os.walk(ST, topdown=False):
    for f in fs:
        if is_junk(f) and apply: os.unlink(os.path.join(r, f))
    for d in ds:
        p = os.path.join(r, d)
        if apply and is_junk(d): shutil.rmtree(p, ignore_errors=True); continue
        try:
            if apply and not os.listdir(p): os.rmdir(p)
        except OSError: pass
left = sum(len(fs) for _, _, fs in os.walk(ST))
print(f"\n{'APPLIED' if apply else 'DRY RUN'}: {stats['moved']} renames, {stats['merged_dirs']} dir merges, "
      f"{stats['dropped_dupes']} same-size dupes dropped, {stats['conflicts']} conflicts -> _conflicts/, {stats['bytes']/1e9:.1f} GB; "
      f"{left} files still in _staging")
