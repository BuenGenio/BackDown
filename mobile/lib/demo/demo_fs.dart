import 'dart:convert';

import 'package:crypto/crypto.dart' show sha256;

import '../api/keys.dart';

/// The demo's drives: a made-up person's old externals and home folder, with the kinds of things
/// BackDown is for (photos twice over, a mail server's maildir, an iPhone backup, invoices, a
/// project, a disk image). Every file has an index id, so the viewers' demo answers can find one
/// by name with [DemoFs.byName].
class DemoNode {
  DemoNode.dir(this.name, this.children, {this.mtime = 0})
    : size = 0,
      id = null,
      hash = null,
      asset = null,
      text = null;

  DemoNode.file(this.name, this.size, {required this.mtime, String? same, this.asset, this.text})
    : children = null,
      id = null,
      hash = sha256.convert(utf8.encode(same ?? '$name/$size')).toString();

  final String name;
  final List<DemoNode>? children;
  int size;
  int mtime;
  int? id;

  /// Files with the same `same` (or name and size) share a hash, so they're duplicates.
  final String? hash;

  /// A bundled file whose bytes stand for this one's content (assets/demo/…).
  final String? asset;

  /// Text content, for text and code files.
  final String? text;

  late String path;
  DemoNode? parent;
  int files = 0;

  bool get isDir => children != null;
  String get key => keyOfPath(path);
}

/// A finished scan, for the time slider.
class DemoScan {
  const DemoScan(this.id, this.finishedAt, this.files, this.bytes);
  final int id, finishedAt, files, bytes;
}

class DemoRoot {
  DemoRoot(this.id, this.node, this.scans);
  final int id;
  final DemoNode node;
  final List<DemoScan> scans;
}

/// Unix seconds for a date, UTC.
int _t(int y, [int m = 1, int d = 1, int h = 12]) => DateTime.utc(y, m, d, h).millisecondsSinceEpoch ~/ 1000;

class DemoFs {
  DemoFs._(this.roots) {
    var id = 1000;
    void walk(DemoNode n, DemoNode? parent, String path) {
      n.parent = parent;
      n.path = path;
      if (n.isDir) {
        for (final c in n.children!) {
          walk(c, n, path.endsWith('/') ? '$path${c.name}' : '$path/${c.name}');
        }
        n.size = n.children!.fold(0, (a, c) => a + c.size);
        n.files = n.children!.fold(0, (a, c) => a + (c.isDir ? c.files : 1));
        n.mtime = n.children!.fold(0, (a, c) => c.mtime > a ? c.mtime : a);
      } else {
        n.id = ++id;
        byId[n.id!] = n;
      }
      byKey[n.key] = n;
    }

    for (final r in roots) {
      walk(r.node, null, r.node.name);
    }
    for (final n in byId.values) {
      (byHash[n.hash!] ??= []).add(n);
    }
  }

  final List<DemoRoot> roots;
  final Map<int, DemoNode> byId = {};
  final Map<String, DemoNode> byKey = {};
  final Map<String, List<DemoNode>> byHash = {};

  static final DemoFs instance = DemoFs._(_build());

  /// The first file called [name] (for the viewers' demo answers).
  DemoNode? byName(String name) {
    for (final n in byId.values) {
      if (n.name == name) return n;
    }
    return null;
  }

  DemoRoot rootOf(DemoNode n) {
    var top = n;
    while (top.parent != null) {
      top = top.parent!;
    }
    return roots.firstWhere((r) => identical(r.node, top));
  }

  /// Other files with the same content.
  int copiesOf(DemoNode n) => n.isDir ? 0 : (byHash[n.hash]!.length - 1);

  Iterable<DemoNode> get files => byId.values;
}

List<DemoNode> _photos(String year, int count, int start, {String? sameAs}) => [
  for (var i = 0; i < count; i++)
    DemoNode.file(
      'IMG_${start + i}.JPG',
      2800000 + (i * 137411) % 2100000,
      mtime: _t(int.parse(year), 1 + i % 12, 1 + (i * 7) % 27),
      same: 'photo-${sameAs ?? year}-$i',
      asset: 'assets/demo/photo${1 + i % 4}.jpg',
    ),
];

DemoNode _f(String name, int size, int mtime, {String? same, String? asset, String? text}) =>
    DemoNode.file(name, size, mtime: mtime, same: same, asset: asset, text: text);

const _readme = '''# Shoebox

The scripts that sorted the old photo drives in 2019. `sort.py` reads EXIF dates and files
each photo under Photos/YEAR/MONTH.

- `sort.py --dry-run` to see what it would do
- the log goes to sort.log
''';

const _sortPy = '''#!/usr/bin/env python3
"""File photos by the date they were taken."""
import sys
from pathlib import Path

from exif import Image


def taken(path: Path) -> str:
    with path.open("rb") as f:
        img = Image(f)
    return img.datetime_original[:7].replace(":", "/")


def main(src: str, dst: str, dry_run: bool = False) -> int:
    moved = 0
    for p in Path(src).rglob("*.JPG"):
        target = Path(dst) / taken(p) / p.name
        if dry_run:
            print(f"{p} -> {target}")
        else:
            target.parent.mkdir(parents=True, exist_ok=True)
            p.rename(target)
        moved += 1
    return moved


if __name__ == "__main__":
    print(main(*sys.argv[1:3], dry_run="--dry-run" in sys.argv), "photos")
''';

const _backupSh = '''#!/bin/sh
# nightly copy of the mail server's maildir to the Seagate
rsync -aH --delete root@mail.example.org:/var/vmail/example.org/sam/ \\
  "/media/sam/Seagate Expansion/Mail/mail.example.org/"
''';

const _notes = '''Things on the old drives

- the Seagate has the 2014-2016 photos, and the WD has a copy of most of them
- mail.example.org was shut down in May 2020; its maildir is on the Seagate
- old-laptop.img is the ThinkPad's disk, taken with ddrescue before it died
''';

List<DemoRoot> _build() {
  final seagate = DemoNode.dir('/media/sam/Seagate Expansion', [
    DemoNode.dir('Photos', [
      DemoNode.dir('2014', _photos('2014', 9, 2100)),
      DemoNode.dir('2015', _photos('2015', 12, 3400)),
      DemoNode.dir('2016', _photos('2016', 7, 5100)),
    ]),
    DemoNode.dir('Mail', [
      DemoNode.dir('mail.example.org', [
        DemoNode.dir('cur', [
          for (var i = 0; i < 6; i++)
            _f('${1588200000 + i * 3600}.M${i}P1234.mail,S=${4000 + i * 311}:2,S', 4000 + i * 311, _t(2020, 4, 20 + i)),
        ]),
        DemoNode.dir('new', [_f('1590900000.M1P99.mail,S=5120', 5120, _t(2020, 5, 31))]),
        DemoNode.dir('tmp', []),
        _f('dovecot-uidlist', 2310, _t(2020, 5, 31)),
        _f('subscriptions', 41, _t(2018, 3, 2)),
      ]),
      _f('archive-2012.mbox', 48231944, _t(2013, 1, 4)),
    ]),
    DemoNode.dir('Backups', [
      DemoNode.dir("Sam's iPhone", [
        _f('Manifest.db', 2457600, _t(2019, 8, 14)),
        _f('Info.plist', 18322, _t(2019, 8, 14)),
        _f('Status.plist', 189, _t(2019, 8, 14)),
        DemoNode.dir('0a', [_f('0a1b2c3d4e5f60718293a4b5c6d7e8f901234567', 1048576, _t(2019, 8, 14))]),
      ]),
      _f('backup-mail.sh', 196, _t(2018, 11, 2), text: _backupSh),
    ]),
    DemoNode.dir('Documents', [
      DemoNode.dir('Invoices', [
        for (var m = 1; m <= 6; m++)
          _f('invoice-2019-0$m.pdf', 48000 + m * 1312, _t(2019, m, 28), asset: 'assets/demo/invoice.pdf'),
      ]),
      _f('Taxes 2019.xlsx', 38211, _t(2020, 3, 30)),
      _f('notes.md', _notes.length, _t(2020, 6, 1), text: _notes),
      _f('contacts.sqlite', 204800, _t(2018, 9, 9)),
    ]),
    DemoNode.dir('Music', [
      DemoNode.dir('Nils Frahm', [
        DemoNode.dir('Spaces', [
          for (final (i, t) in ['An Aborted Beginning', 'Says', 'Hammers', 'Improvisation for Coughs'].indexed)
            _f('0${i + 1} $t.mp3', 6200000 + i * 911000, _t(2014, 2, 3)),
        ]),
      ]),
    ]),
    DemoNode.dir('Disk images', [_f('old-laptop.img', 128035676160, _t(2017, 10, 12))]),
  ]);

  final wd = DemoNode.dir('/media/sam/WD Passport', [
    DemoNode.dir('Photos backup', [
      DemoNode.dir('2014', _photos('2014', 9, 2100)),
      DemoNode.dir('2015', _photos('2015', 10, 3400)),
    ]),
    DemoNode.dir('Projects', [
      DemoNode.dir('shoebox', [
        _f('README.md', _readme.length, _t(2019, 2, 2), text: _readme),
        _f('sort.py', _sortPy.length, _t(2019, 2, 2), text: _sortPy),
        _f('sort.log', 88213, _t(2019, 2, 3)),
      ]),
    ]),
    _f('site-backup.zip', 734003200, _t(2018, 5, 5)),
    _f('shop-2018.sql', 52428800, _t(2018, 12, 31)),
  ]);

  final home = DemoNode.dir('/home/sam', [
    DemoNode.dir('Documents', [
      _f('notes.md', _notes.length, _t(2020, 6, 1), text: _notes),
      _f('invoice-2019-01.pdf', 49312, _t(2019, 1, 28), asset: 'assets/demo/invoice.pdf'),
    ]),
    DemoNode.dir('Pictures', _photos('2016', 4, 5100)),
    DemoNode.dir('Downloads', [_f('ubuntu-24.04-desktop-amd64.iso', 6114656256, _t(2024, 4, 25))]),
  ]);

  final now = DateTime.now().millisecondsSinceEpoch ~/ 1000;
  List<DemoScan> scans(int first, DemoNode n, int count) => [
    for (var i = 0; i < count; i++)
      DemoScan(
        first + i,
        now - (count - i) * 86400 + 3600,
        n.files - (count - 1 - i) * 3,
        n.size - (count - 1 - i) * 9000000,
      ),
  ];
  // sizes and counts are only known after the walk in DemoFs._, so the scans are made lazily below
  return [DemoRoot(1, seagate, []), DemoRoot(2, wd, []), DemoRoot(3, home, [])].map((r) {
    // compute totals for the scans (the walk in DemoFs._ fills these in again; it's the same sum)
    int total(DemoNode n) => n.isDir ? n.children!.fold(0, (a, c) => a + total(c)) : n.size;
    int nfiles(DemoNode n) => n.isDir ? n.children!.fold(0, (a, c) => a + nfiles(c)) : 1;
    r.node.size = total(r.node);
    r.node.files = nfiles(r.node);
    r.scans.addAll(scans(r.id * 10, r.node, r.id == 1 ? 6 : 3));
    return r;
  }).toList();
}
