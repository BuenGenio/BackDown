import 'dart:convert';
import 'dart:typed_data';

import 'package:crypto/crypto.dart' show sha256;

import '../api/api.dart';
import '../api/demo_api.dart';
import '../api/keys.dart';
import '../core/format.dart';
import 'demo_fs.dart';

/// The demo's answers for what's here, duplicates, the tidying history (one finished plan, one
/// freed-up drive, one rescue, one mirror, one manifest) and the service's editions and updates,
/// all from the made-up drives in demo_fs.dart.
final Map<String, DemoHandler> demoLibraryGets = {
  'entities': (d, p) => {
    'roots': [for (final r in _fs.roots) _rootEntities(r)],
    'daemon': true,
  },
  'overlaps': (d, p) => _overlaps(),
  'dupes': (d, p) => _dupes(p),
  'dedups': (d, p) => <Object?>[],
  'plans': (d, p) => [_planSummary()],
  'plan': (d, p) {
    if (p['id'] != '1') throw ApiException(ApiFailure.refused, 'there\'s no plan ${p['id']}', status: 400);
    return {'id': 1, 'state': 'done', 'plan': _plan(), 'journal': _journal(), 'mergeable': <int>[]};
  },
  'plan-dry': (d, p) => {
    'files': 6,
    'bytes': _planBytes(),
    'would_copy': 0,
    'would_copy_bytes': 0,
    'would_merge': 0,
    'resumed': 0,
    'duplicates': 6,
    'conflicts': 0,
    'failed': 0,
    'first_problems': <String>[],
  },
  'wipes': (d, p) => [_wipe()],
  'rescues': (d, p) => [_rescue()],
  'mirrors': (d, p) => [_mirror()],
  'manifests': (d, p) => {
    'manifests': [_manifest()],
  },
  'features': (d, p) => _features(),
  'update': (d, p) => _update(),
  'scan-settings': (d, p) => _scanSettings,
};

final Map<String, DemoHandler> demoLibraryPosts = {};

/// Documents the service makes as files: the report, the chain of custody, a signed manifest.
/// (Read by the library screens through documentBytes in features/library/data.dart.)
final Map<String, DemoBytesHandler> demoLibraryBytes = {
  'report': (d, p) => _utf8(_report()),
  'custody': (d, p) => _utf8(const JsonEncoder.withIndent('  ').convert(_custody())),
  'manifest': (d, p) => _utf8('${const JsonEncoder.withIndent('  ').convert(_manifestDoc())}\n'),
};

DemoFs get _fs => DemoFs.instance;
DemoRoot get _seagate => _fs.roots[0];
DemoRoot get _home => _fs.roots[2];

Uint8List _utf8(String s) => Uint8List.fromList(utf8.encode(s));

DemoNode _at(String path) => _fs.byKey[keyOfPath(path)] ?? (throw StateError('no $path in the demo'));

Iterable<DemoNode> _filesUnder(DemoNode n) sync* {
  if (!n.isDir) {
    yield n;
    return;
  }
  for (final c in n.children!) {
    yield* _filesUnder(c);
  }
}

bool _under(String path, String dir) => path == dir || path.startsWith(dir.endsWith('/') ? dir : '$dir/');

String _hex(String seed) => sha256.convert(utf8.encode(seed)).toString();

// ---------- what's here

class _Thing {
  const _Thing(
    this.root,
    this.rel,
    this.kind,
    this.label,
    this.title, {
    this.confidence = 0.9,
    this.facts = const [],
    this.provenance = const [],
    this.lowValue = false,
    this.junk = const [],
  });

  final int root;
  final String rel, kind, label, title;
  final double confidence;
  final List<(String, String)> facts;
  final List<String> provenance;
  final bool lowValue;
  final List<_JunkAt> junk;

  String get path => rel.isEmpty ? _fs.roots[root].node.path : '${_fs.roots[root].node.path}/$rel';
  DemoNode get node => _at(path);
}

class _JunkAt {
  const _JunkAt(this.path, this.why, this.files, this.bytes);
  final String path, why;
  final int files, bytes;
  Map<String, Object?> get json => {'path': path, 'key': keyOfPath(path), 'why': why, 'files': files, 'bytes': bytes};
}

const List<_Thing> _things = [
  _Thing(
    0,
    'Mail/mail.example.org',
    'maildir',
    'Mailbox (Maildir)',
    'sam@example.org: 7 messages',
    confidence: 0.95,
    facts: [('Messages', '7'), ('Unread', '1'), ('Server', 'Dovecot'), ('Folders', 'Inbox')],
    provenance: [
      'Copied from mail.example.org:/var/vmail/example.org/sam/ with rsync (Backups/backup-mail.sh).',
      'The mail server for example.org: its last message is from 31 May 2020.',
    ],
  ),
  _Thing(
    0,
    "Backups/Sam's iPhone",
    'ios-backup',
    'iPhone or iPad backup',
    "Sam's iPhone (iPhone 8, iOS 12.4)",
    confidence: 0.99,
    facts: [('Device', 'iPhone 8'), ('iOS', '12.4'), ('Last backup', '14 Aug 2019'), ('Encrypted', 'no')],
    provenance: ["Backed up from Sam's iPhone by iTunes 12.9."],
  ),
  _Thing(
    0,
    'Photos',
    'photos',
    'Photo library',
    '28 photos, 2014–2016',
    facts: [('Photos', '28'), ('Folders', 'by year')],
  ),
  _Thing(
    0,
    'Music',
    'music',
    'Music library',
    '1 artist, 1 album, 4 tracks',
    confidence: 0.85,
    facts: [('Artists', '1'), ('Albums', '1'), ('Tracks', '4')],
  ),
  _Thing(
    0,
    'Disk images/old-laptop.img',
    'disk-image',
    'Disk image',
    'Raw disk image, 128 GB: GPT, 2 partitions',
    confidence: 0.95,
    facts: [('Format', 'raw'), ('Partition table', 'GPT'), ('File systems', 'FAT32, ext4')],
  ),
  _Thing(1, 'Photos backup', 'photos', 'Photo library', '19 photos, 2014–2015', facts: [('Photos', '19')]),
  _Thing(
    1,
    'shop-2018.sql',
    'sql-dump',
    'Database dump',
    'MySQL dump of shop, 31 Dec 2018',
    confidence: 0.99,
    facts: [('Engine', 'MySQL 5.7'), ('Database', 'shop'), ('Tables', '42'), ('Dumps', '1')],
    provenance: ['Dumped from db.example.org by mysqldump 10.13.'],
  ),
  _Thing(
    2,
    '',
    'home-linux',
    'Linux home folder',
    "sam's home folder",
    confidence: 0.8,
    facts: [('User', 'sam')],
    junk: [_JunkAt('/home/sam/Pictures/.thumbnails', 'thumbnail cache', 4, 61440)],
  ),
  _Thing(2, 'Downloads', 'downloads', 'Downloads pile', '1 file, an installer', confidence: 0.75, lowValue: true),
  _Thing(
    2,
    'Downloads/ubuntu-24.04-desktop-amd64.iso',
    'disk-image',
    'Disk image',
    'Ubuntu 24.04 installer (ISO 9660)',
    confidence: 0.95,
    facts: [('Format', 'ISO 9660'), ('Volume', 'Ubuntu 24.04 LTS amd64')],
  ),
];

/// Junk that isn't inside anything recognised, by drive.
const _looseJunk = {
  1: [_JunkAt('/media/sam/WD Passport/Projects/shoebox/__pycache__', 'Python bytecode cache', 2, 9312)],
};

Map<String, Object?> _entity(_Thing t) {
  final n = t.node;
  final times = [for (final f in _filesUnder(n)) f.mtime]..sort();
  String? inside;
  var depth = -1;
  for (final o in _things) {
    if (identical(o, t) || !o.node.isDir) continue;
    if (_under(t.path, o.path) && t.path != o.path && o.path.length > depth) {
      inside = o.node.key;
      depth = o.path.length;
    }
  }
  return {
    'kind': t.kind,
    'label': t.label,
    'title': t.title,
    'path': n.path,
    'key': n.key,
    'is_file': !n.isDir,
    'confidence': t.confidence,
    'files': n.isDir ? n.files : 1,
    'bytes': n.size,
    'oldest': times.first,
    'newest': times.last,
    'facts': [
      for (final (k, v) in t.facts) [k, v],
    ],
    'provenance': t.provenance,
    'junk': [for (final j in t.junk) j.json],
    'inside': inside,
    'low_value': t.lowValue,
  };
}

Map<String, Object?> _rootEntities(DemoRoot r) {
  final i = _fs.roots.indexOf(r);
  return {
    'root_id': r.id,
    'root': r.node.path,
    'scan': r.scans.last.id,
    'computed_at': r.scans.last.finishedAt + 900,
    'read_drive': true,
    'entities': [
      for (final t in _things)
        if (t.root == i) _entity(t),
    ],
    'junk': [for (final j in _looseJunk[i] ?? const <_JunkAt>[]) j.json],
    'fresh': true,
  };
}

// ---------- overlaps: how much of each place exists elsewhere, worked out from the demo's hashes

/// Files the nextcloud machine has a copy of (as demo_core's file details say for 2014's photos),
/// and the two 2015 photos that are on this computer only once.
bool _onNextcloud(DemoNode f) => f.path.contains('/2014/') || _acrossNames.contains(f.name);
const _acrossNames = {'IMG_3410.JPG', 'IMG_3411.JPG'};

List<Map<String, Object?>> _overlaps() {
  // the units: each drive, its top-level folders, and what's recognised (folders only)
  final units = <(String, String, DemoNode)>[
    for (final r in _fs.roots) ...[
      ('drive', baseName(r.node.path), r.node),
      for (final c in r.node.children!)
        if (c.isDir) ('folder', c.name, c),
    ],
    for (final t in _things)
      if (t.node.isDir) ('entity', '${t.label}: ${t.title}', t.node),
  ];
  final entityUnits = [
    for (final u in units)
      if (u.$1 == 'entity') u,
  ]..sort((a, b) => b.$3.path.length.compareTo(a.$3.path.length));

  // where a copy is: the drive, for a drive; else the most specific place holding it
  (String, String, String) placeOf(DemoNode copy, String level) {
    final root = _fs.rootOf(copy).node;
    if (level != 'drive') {
      for (final e in entityUnits) {
        if (_under(copy.path, e.$3.path)) return (e.$2, e.$3.path, e.$3.key);
      }
      for (final c in root.children!) {
        if (c.isDir && _under(copy.path, c.path)) return (c.name, c.path, c.key);
      }
    }
    return (baseName(root.path), root.path, root.key);
  }

  Map<String, Object?> overlap(String level, String label, DemoNode node) {
    var elsewhere = 0, files = 0;
    final places = <(String, String, String), int>{};
    for (final f in _filesUnder(node)) {
      final outside = [
        for (final c in _fs.byHash[f.hash]!)
          if (!_under(c.path, node.path)) c,
      ];
      final there = _onNextcloud(f);
      if (outside.isEmpty && !there) continue;
      elsewhere += f.size;
      files++;
      for (final p in {for (final c in outside) placeOf(c, level)}) {
        places[p] = (places[p] ?? 0) + f.size;
      }
      if (there) {
        const m = ('the machine nextcloud', 'ssh://sam@nextcloud', '');
        places[m] = (places[m] ?? 0) + f.size;
      }
    }
    final containers = places.entries.toList()..sort((a, b) => b.value.compareTo(a.value));
    return {
      'unit': {
        'level': level,
        'label': label,
        'path': node.path,
        'key': node.key,
        'files': node.files,
        'bytes': node.size,
      },
      'elsewhere': elsewhere,
      'elsewhere_files': files,
      'containers': [
        for (final c in containers.take(5)) {'label': c.key.$1, 'path': c.key.$2, 'key': c.key.$3, 'bytes': c.value},
      ],
    };
  }

  return [for (final (level, label, node) in units) overlap(level, label, node)];
}

// ---------- duplicates

int _size(String? s, int fallback) {
  if (s == null || s.isEmpty) return fallback;
  final m = RegExp(r'^(\d+(?:\.\d+)?)\s*([KMGT]?)B?$', caseSensitive: false).firstMatch(s.trim());
  if (m == null) return fallback;
  final mul = switch (m[2]!.toUpperCase()) {
    'K' => 1e3,
    'M' => 1e6,
    'G' => 1e9,
    'T' => 1e12,
    _ => 1.0,
  };
  return (double.parse(m[1]!) * mul).round();
}

Map<String, Object?> _remote(DemoNode f, int latest) {
  final folder = f.path.contains('/2014/') ? '2014' : '2015';
  final path = '/srv/photos/$folder/${f.name}';
  return {
    'machine_id': 1,
    'machine': 'nextcloud',
    'url': 'ssh://sam@nextcloud',
    'rid': 9000 + f.id!,
    'root_id': 1,
    'path': path,
    'key': keyOfPath(path),
    'dir_key': keyOfPath('/srv/photos/$folder'),
    'confirmed_at': latest - 7200,
    'counts': true,
    'why_not': null,
  };
}

Map<String, Object?> _dupes(Map<String, String> p) {
  final min = _size(p['min'], 1000000).clamp(1, 1 << 62);
  final from = int.tryParse(p['from'] ?? '') ?? 0;
  final below = int.tryParse(p['below'] ?? '');
  final ext = p['ext'] ?? '';
  final exts = ext.isEmpty ? null : ext.split(',').map((e) => e.trim().toLowerCase()).toSet();
  final except = p['except'] == '1';
  final after = int.tryParse(p['after'] ?? '');
  final before = int.tryParse(p['before'] ?? '');
  final noHidden = p['nohidden'] == '1';
  final limit = (int.tryParse(p['limit'] ?? '') ?? 200).clamp(1, 5000);
  final perPath = exts != null || after != null || before != null || noHidden;
  final latest = _seagate.scans.last.finishedAt;

  bool passes(DemoNode f) {
    final dot = f.name.lastIndexOf('.');
    final e = dot < 0 ? f.name.toLowerCase() : f.name.substring(dot + 1).toLowerCase();
    if (exts != null && exts.contains(e) == except) return false;
    if (after != null && f.mtime < after) return false;
    if (before != null && f.mtime >= before) return false;
    final rel = f.path.substring(_fs.rootOf(f).node.path.length);
    if (noHidden && rel.split('/').any((n) => n.startsWith('.') || n.endsWith('~'))) return false;
    return true;
  }

  final all = <Map<String, Object?>>[];
  final listed = <Map<String, Object?>>[];
  for (final MapEntry(key: hash, value: copies) in _fs.byHash.entries) {
    if (copies.length < 2) continue;
    final size = copies.first.size;
    if (size < min) continue;
    final sorted = [...copies]..sort((a, b) => a.path.compareTo(b.path));
    final files = [
      for (final (i, f) in sorted.indexed)
        {
          'path': f.path,
          'key': f.key,
          'dir': f.parent!.key,
          'id': f.id,
          'mtime': f.mtime,
          'copy': i,
          'listed': !perPath || passes(f),
        },
    ];
    final g = {
      'sha256': hash,
      'size': size,
      'copies': sorted.length,
      'reclaimable': size * (sorted.length - 1),
      'paths': [for (final f in sorted) f.path],
      'files': files,
      'remote': [if (sorted.any(_onNextcloud)) _remote(sorted.first, latest)],
    };
    all.add(g);
    final sizeOk = size >= from && (below == null || size < below);
    if (sizeOk && files.any((f) => f['listed'] == true)) listed.add(g);
  }
  int byFree(Map<String, Object?> a, Map<String, Object?> b) {
    final c = (b['reclaimable']! as int).compareTo(a['reclaimable']! as int);
    return c != 0 ? c : (a['paths']! as List).first.toString().compareTo((b['paths']! as List).first.toString());
  }

  listed.sort(byFree);
  int free(List<Map<String, Object?>> l) => l.fold(0, (a, g) => a + (g['reclaimable']! as int));
  final filtered = from > 0 || below != null || perPath;

  // here once, and on the nextcloud machine too
  final across = [
    for (final f in _fs.files)
      if (_fs.copiesOf(f) == 0 && _onNextcloud(f) && f.size >= min && passes(f)) f,
  ]..sort((a, b) => b.size.compareTo(a.size));
  final acrossBytes = across.fold<int>(0, (a, f) => a + f.size);

  return {
    'groups': listed.length,
    'reclaimable': free(listed),
    'largest': listed.take(limit).toList(),
    if (filtered) 'unfiltered': {'groups': all.length, 'reclaimable': free(all)},
    'across': {
      'files': across.length,
      'bytes': acrossBytes,
      'proven_files': across.length,
      'proven_bytes': acrossBytes,
      'largest': [
        for (final f in across.take(200))
          {
            'sha256': f.hash,
            'size': f.size,
            'path': f.path,
            'remote': [_remote(f, latest)],
          },
      ],
    },
  };
}

// ---------- the finished plan: sam's home folder filed into the Seagate

String get _pictures => '${_home.node.path}/Pictures';
String get _documents => '${_home.node.path}/Documents';
String get _downloads => '${_home.node.path}/Downloads';

int _planBytes() => _at(_pictures).size + _at(_documents).size;

int get _planMade => _home.scans.last.finishedAt - 6 * 86400;

Map<String, Object?> _planSummary() => {
  'id': 1,
  'state': 'done',
  'created_at': _planMade,
  'target': _seagate.node.path,
  'copy_bytes': 0,
  'copy_files': 0,
  'items': 3,
  'drives': [
    {'drive': baseName(_home.node.path), 'covered': false},
  ],
};

Map<String, Object?> _plan() {
  final pics = _at(_pictures), docs = _at(_documents), dl = _at(_downloads);
  Map<String, Object?> item(
    DemoNode n,
    String what,
    String kind,
    String action,
    String dest,
    String why, {
    List<String> exclude = const [],
  }) => {
    'what': what,
    'kind': kind,
    'source': n.path,
    'source_key': n.key,
    'drive': baseName(_home.node.path),
    'is_file': false,
    'only_files': false,
    'dest': dest,
    'files': n.files,
    'bytes': n.size,
    'action': action,
    'why': why,
    'exclude': exclude,
  };
  return {
    'target': _seagate.node.path,
    'created_at': _planMade,
    'schema': 'built-in schema',
    'items': [
      item(
        pics,
        'Photo library: ${plural(pics.files, 'photo')} from 2016',
        'photos',
        'copy-unique',
        'Photos/2016',
        'Photos go under Photos by the year they were taken. Every one of these is in the archive already, so only '
            'what isn\'t would be copied.',
        exclude: ['$_pictures/.thumbnails'],
      ),
      item(
        docs,
        'Folder',
        'unsorted',
        'copy-unique',
        'Documents',
        'Loose documents go to Documents. Both are in the archive already (notes.md, and the January invoice under '
            'Documents/Invoices).',
      ),
      item(
        dl,
        'Downloads pile: 1 file, an installer',
        'downloads',
        'skip',
        'Downloads',
        'Probably not worth keeping as a unit: an installer that can be downloaded again.',
      ),
    ],
    'drives': [
      {
        'drive': baseName(_home.node.path),
        'path': _home.node.path,
        'bytes': _home.node.size,
        'copy': 0,
        'already': _planBytes(),
        'not_copied': dl.size,
        'covered': false,
      },
    ],
    'copy_files': 0,
    'copy_bytes': 0,
  };
}

List<List<Object>> _journal() => [
  ['duplicate', 6, _planBytes()],
];

// ---------- freeing up, rescues, mirrors, manifests

int get _wipedAt => nowSeconds() - 12 * 86400;

Map<String, Object?> _wipe() => {
  'id': 1,
  'created_at': _wipedAt,
  'paths': ['/media/sam/Old Toshiba'],
  'move_to': '${_seagate.node.path}/Documents',
  'state': 'done',
  'summary':
      'deleted 38 files (176.4 MB), 2 of them moved to the chosen place first; left 0 changed or new, 0 with no copy, '
      '0 unreadable',
};

String get _laptopImage => '${_seagate.node.path}/Disk images/old-laptop.img';

Map<String, Object?> _rescue() {
  final image = _at(_laptopImage);
  final at = nowSeconds() - 5 * 86400;
  final linux = image.size - 537919488 - 1048576;
  return {
    'id': 1,
    'created_at': at,
    'updated_at': at + 41,
    'mode': 'parts',
    'source': image.path,
    'dest': null,
    'options': {'deep': false},
    'state': 'done',
    'summary': '2 file systems found',
    'result': {
      'size': image.size,
      'sector': 512,
      'table': {
        'scheme': 'gpt',
        'sector': 512,
        'entries': [
          {
            'index': 1,
            'start': 1048576,
            'size': 536870912,
            'kind': 'EFI System',
            'code': 'C12A7328-F81F-11D2-BA4B-00A0C93EC93B',
            'name': 'EFI System Partition',
            'guid': null,
            'bootable': false,
          },
          {
            'index': 2,
            'start': 537919488,
            'size': linux,
            'kind': 'Linux filesystem',
            'code': '0FC63DAF-8483-4772-8E79-3D69D8477DE4',
            'name': null,
            'guid': null,
            'bootable': false,
          },
        ],
        'problems': ['the backup GPT at the end of the disk is damaged'],
      },
      'found': [
        {
          'start': 1048576,
          'size': 536870912,
          'fs': 'FAT32',
          'label': 'ESP',
          'via': 'boot sector',
          'in_table': true,
          'truncated': false,
          'nested': false,
        },
        {
          'start': 537919488,
          'size': linux,
          'fs': 'ext4',
          'label': 'ubuntu',
          'via': 'superblock',
          'in_table': true,
          'truncated': false,
          'nested': false,
        },
      ],
      'unreadable': 1228800,
      'stopped': false,
    },
  };
}

Map<String, Object?> _mirror() {
  final pics = _at(_pictures);
  final now = nowSeconds();
  return {
    'id': 1,
    'source': pics.path,
    'target': '${_seagate.node.path}/Photos/2016',
    'created_at': now - 60 * 86400,
    'verify_every_days': 30,
    'last_sync': now - 86400 + 1200,
    'last_sync_summary':
        '${pics.files} files: 0 copied (0 B), 0 updated (old copies kept), ${pics.files} unchanged, 3 only on the mirror',
    'last_verify': now - 9 * 86400,
    'last_verify_summary': 're-read ${pics.files} files (${bytes(pics.size)}, 100%): all identical',
    'last_verify_ok': true,
    'connected': true,
  };
}

const _fingerprint = '3f9a-c021-7be4-d518';

Map<String, Object?> _manifest() {
  final r = _seagate;
  return {
    'id': 1,
    'root_id': r.id,
    'scan_id': r.scans.last.id,
    'created_at': r.scans.last.finishedAt + 300,
    'merkle': _hex('merkle ${r.node.path} ${r.scans.last.id}'),
    'drive': r.node.path,
    'files': r.node.files,
    'bytes': r.node.size,
    'hashed': r.node.files - 1,
    'fingerprint': _fingerprint,
  };
}

Map<String, Object?> _manifestDoc() {
  final r = _seagate;
  return {
    'v': 1,
    'kind': 'backdown-manifest',
    'note': 'The demo\'s made-up manifest: its signature doesn\'t verify.',
    'created': r.scans.last.finishedAt + 300,
    'drive': {'path': r.node.path, 'scan': r.scans.last.id},
    'counts': {'files': r.node.files, 'bytes': r.node.size, 'hashed': r.node.files - 1},
    'merkle': _hex('merkle ${r.node.path} ${r.scans.last.id}'),
    'files': [
      for (final f in _filesUnder(r.node).take(12))
        {'path': f.path.substring(r.node.path.length + 1), 'size': f.size, 'mtime': f.mtime, 'sha256': f.hash},
    ],
    'key': _hex('demo key'),
    'sig': 'demo',
  };
}

Map<String, Object?> _custody() {
  var prev = '0' * 64;
  final events = <Map<String, Object?>>[];
  for (final name in ['Taxes 2019.xlsx', 'contacts.sqlite']) {
    final f = _fs.byName(name)!;
    final e = {
      'kind': 'move',
      'at': _wipedAt + 60,
      'ref': 'wipe:1',
      'src': '/media/sam/Old Toshiba/$name',
      'dest': f.path,
      'size': f.size,
      'src_sha256': f.hash,
      'dest_sha256': f.hash,
    };
    final digest = _hex(jsonEncode(e));
    final hash = _hex('$prev$digest');
    events.add({...e, 'seq': events.length + 1, 'prev': prev, 'hash': hash});
    prev = hash;
  }
  return {
    'v': 1,
    'kind': 'backdown-custody',
    'created': nowSeconds(),
    'log_starts': _wipedAt - 30 * 86400,
    'key': _hex('demo key'),
    'note': 'The demo\'s made-up chain of custody: its signature doesn\'t verify.',
    'events': events,
    'seals': [
      {'kind': 'backdown-custody-seal', 'seq': events.length, 'hash': prev, 'at': _wipedAt + 120},
    ],
    'sig': 'demo',
  };
}

// ---------- this BackDown

const _version = '1.0.0-beta.4';

Map<String, Object?> _features() => {
  'version': _version,
  'tier': 'community',
  'tier_name': 'Community',
  'preview': true,
  'summary': 'Pro features are free during the preview: everything is unlocked.',
  'note': 'Pro features are free during the preview',
  'licence': null,
  'licence_problem': null,
  'licence_file': '/home/sam/.local/share/backdown/license.txt',
  'pricing': 'https://backdown.io/pricing',
  'features': [
    for (final (key, name, unlocks, tier, ready) in _featureTable)
      {
        'key': key,
        'name': name,
        'unlocks': unlocks,
        'tier': tier,
        'tier_name': '${tier[0].toUpperCase()}${tier.substring(1)}',
        'ready': ready,
        'allowed': true,
        'views': <String>[],
        'commands': <String>[],
      },
  ],
};

/// BackDown's own table of what each edition has (tiers.rs).
const _featureTable = [
  (
    'inventory',
    'Backup, restore and dedup',
    'Index every drive, hash what could be a duplicate, list duplicates, check a drive is safe to wipe, keep mirrors.',
    'community',
    true,
  ),
  (
    'ui',
    'CLI and local web UI',
    'The command line, the browser with its time slider, and the desktop app.',
    'community',
    true,
  ),
  (
    'fast-rescans',
    'Rescans in seconds from the drive\'s change log',
    'Rescans from the drive\'s own change log (macOS FSEvents, the Windows USN journal) and NTFS\'s file table, with '
        'a full walk now and then.',
    'community',
    true,
  ),
  ('sizes', 'Size analytics', 'Folder sizes, the size map and what changed between scans.', 'community', true),
  (
    'recognition',
    'Basic recognition',
    'What\'s on each drive: mail stores, backups, phones, systems, repositories, libraries.',
    'community',
    true,
  ),
  (
    'export',
    'Markdown and JSON export',
    'The report as HTML or Markdown, and --json from every command.',
    'community',
    true,
  ),
  (
    'multi-machine',
    'Several machines',
    'A NAS or another computer with BackDown on it, read over ssh with your keys: browse it, and let its copies count '
        'for duplicates, overlaps and freeing drives.',
    'community',
    true,
  ),
  (
    'swarm',
    'The swarm',
    'Every machine as one: each drive\'s size and free space, which machine and drive holds any file, and a pool of '
        'folders across machines that drives are filed into, each file once.',
    'community',
    true,
  ),
  (
    'provenance',
    'Full recognition and provenance',
    'Where each thing came from: the host, domain, device or person, read from backup scripts, plists, git remotes '
        'and dump headers.',
    'pro',
    true,
  ),
  (
    'everywhere',
    'Dedup across every source',
    'How much of each drive, folder and recognised thing already exists elsewhere, and where.',
    'pro',
    true,
  ),
  (
    'reclaim',
    'Reclaim simulator',
    'Free up: see what freeing drives or folders would delete, move what exists only there, then delete only what has '
        'a verified copy.',
    'pro',
    true,
  ),
  (
    'filing',
    'Filing suggestions',
    'Plan: where everything on the old drives goes in one archive and why, then verified, journaled copies.',
    'pro',
    true,
  ),
  (
    'viewers',
    'Data viewers',
    'Open databases, spreadsheets, presentations, mailboxes and SQL dumps right in the browser.',
    'pro',
    true,
  ),
  (
    'cloud',
    'Encrypted cloud targets',
    'Mirror the archive to cloud storage, encrypted before it leaves this computer.',
    'pro',
    false,
  ),
  ('fleet', 'Hosted fleet dashboard', 'Every machine\'s drives in one place.', 'team', false),
  ('catalogs', 'Shared catalogs and roles', 'One catalog for a team, with who may see and do what.', 'team', false),
  (
    'health-alerts',
    'Drive health alerts',
    'Every machine\'s drive-health warnings gathered here, from the machines you\'ve added. Each machine\'s own '
        'health readings, warnings and notifications are free.',
    'team',
    true,
  ),
  ('attestations', 'Scheduled attestations', 'Regular signed statements of what each drive holds.', 'team', true),
  ('manifests', 'Signed, tamper-evident manifests', 'Manifests anyone can verify were not changed.', 'forensic', true),
  (
    'custody',
    'Chain-of-custody export',
    'Every copy, move, deletion, link and merge BackDown made, in a hash-chained, signed log: exported for '
        'evidence handling, and verifiable by anyone.',
    'forensic',
    true,
  ),
  (
    'airgap',
    'Air-gapped deployment',
    'Licensing and updates without any network: install a signed release from a folder carried over, with '
        '`backdown update --from`.',
    'forensic',
    true,
  ),
];

Map<String, Object?> _update() => {
  'enabled': true,
  'can_install': true,
  'why_not': null,
  'install': {'state': '', 'message': '', 'version': ''},
  'status': {
    'current': _version,
    'channel': 'preview',
    'checked_at': nowSeconds() - 3 * 3600,
    'newer': true,
    'error': null,
    'blocked_until': null,
    'platform': 'linux-x64',
    'source': 'https://api.github.com/repos/BuenGenio/backdown-releases',
    'latest': {
      'version': '1.0.0-rc.1',
      'tag': 'v1.0.0-rc.1',
      'notes_url': 'https://github.com/BuenGenio/backdown-releases/releases/tag/v1.0.0-rc.1',
      'prerelease': true,
      'published_at': '2026-10-30T09:12:44Z',
      'signed': true,
      'size': 2512096,
      'archive': 'backdown-v1.0.0-rc.1-linux-x64.tar.gz',
    },
  },
};

const _scanSettings = {
  'journal': 'none: Linux keeps no change log for drives',
  'table': 'NTFS drives mounted with ntfs-3g, when the drive can be read directly',
  'settings': {
    'fast': true,
    'walk_every_scans': 10,
    'walk_every_days': 30,
    'note':
        'A rescan reads only what changed when the drive\'s change log says what that is (none: Linux keeps no change '
        'log for drives), or reads NTFS\'s file table instead of every folder (NTFS drives mounted with ntfs-3g, when '
        'the drive can be read directly); either way it records exactly what walking every folder would, and walks '
        'whenever there\'s any doubt.',
  },
  'labels': {
    'fast': 'Use the drive\'s change log and file table when possible',
    'walk_every_scans': 'Walk every folder anyway, every … scans',
    'walk_every_days': '… or when the last walk is this many days old',
  },
  'help': {
    'fast': 'Off: every scan walks every folder (slower, never different).',
    'walk_every_scans': '0: not for this reason.',
    'walk_every_days':
        'Catches changes made where no change log was kept (the drive plugged into another computer). 0: not for this '
        'reason.',
  },
};

// ---------- the report (as summary.rs writes it in Markdown)

String _report() {
  final o = StringBuffer();
  final files = _fs.files.length;
  final total = _fs.roots.fold<int>(0, (a, r) => a + r.node.size);
  final ov = {
    for (final u in _overlaps()) '${(u['unit']! as Map)['key']}:${(u['unit']! as Map)['level']}': u['elsewhere'],
  };
  String pct(int a, int b) => b == 0 ? '0%' : '${(a * 100 / b).floor()}%';
  int elsewhere(DemoNode n) => (ov['${n.key}:drive'] as int?) ?? 0;
  o.writeln('# BackDown report\n');
  o.writeln(
    'Generated ${day(nowSeconds())} · ${_fs.roots.length} drives · ${count(files)} files · ${bytes(total)} · '
    '${pct(files - 3, files)} hashed\n',
  );
  o.writeln('## Drives\n');
  o.writeln('| Drive | Files | Size | Last scan | Elsewhere | Verdict |\n|---|---:|---:|---|---:|---|');
  for (final r in _fs.roots) {
    final e = elsewhere(r.node);
    o.writeln(
      '| ${baseName(r.node.path)} (${r.node.path}) | ${count(r.node.files)} | ${bytes(r.node.size)} | '
      '${day(r.scans.last.finishedAt)} | ${pct(e, r.node.size)} | ${e >= r.node.size ? 'safe to wipe' : 'keep'} |',
    );
  }
  for (final (i, r) in _fs.roots.indexed) {
    final e = elsewhere(r.node);
    o.writeln('\n## ${baseName(r.node.path)}\n');
    o.writeln(
      '`${r.node.path}` · ${count(r.node.files)} files · ${bytes(r.node.size)} · scanned '
      '${day(r.scans.last.finishedAt)}${r.id == 2 ? ' · not connected' : ''}\n',
    );
    o.writeln(
      '**Verdict.** ${e >= r.node.size ? 'Everything on it has a copy elsewhere.' : '${bytes(r.node.size - e)} exists '
                'only here: keep it, or copy that first.'}\n',
    );
    final things = [
      for (final t in _things)
        if (t.root == i) t,
    ];
    if (things.isEmpty) continue;
    o.writeln("### What's on it\n");
    o.writeln('| What | Where | Size | Years | Notes |\n|---|---|---:|---|---|');
    for (final t in things) {
      final n = t.node;
      final times = [for (final f in _filesUnder(n)) f.mtime]..sort();
      final a = DateTime.fromMillisecondsSinceEpoch(times.first * 1000).year;
      final b = DateTime.fromMillisecondsSinceEpoch(times.last * 1000).year;
      final notes = [...t.provenance, if (t.lowValue) 'probably not worth keeping as a unit'];
      o.writeln(
        '| **${t.label}**: ${t.title} | `${t.rel.isEmpty ? '.' : t.rel}` | ${bytes(n.size)} | '
        '${a == b ? '$a' : '$a–$b'} | ${notes.join(' ')} |',
      );
    }
  }
  final d = _dupes(const {'min': '1M'});
  final groups = (d['largest']! as List).cast<Map<String, Object?>>();
  o.writeln('\n## Duplicates\n');
  o.writeln(
    '${count(d['groups']! as int)} groups of identical files (1 MB and up); ${bytes(d['reclaimable']! as int)} could be '
    'freed.\n',
  );
  o.writeln('| Copies | Each | Reclaimable | Where |\n|---:|---:|---:|---|');
  for (final g in groups.take(8)) {
    final paths = (g['paths']! as List).take(3).map((p) => '`$p`').join('<br>');
    o.writeln('| ${g['copies']} | ${bytes(g['size']! as int)} | ${bytes(g['reclaimable']! as int)} | $paths |');
  }
  o.writeln(
    '\n---\nMade by BackDown · every "elsewhere" and verdict is backed by SHA-256; files without a hash count as '
    'unique: usually no other file has their size, or hashing hasn\'t reached them yet.',
  );
  return o.toString();
}
