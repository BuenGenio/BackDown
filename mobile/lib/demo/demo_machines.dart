import 'dart:convert';

import 'package:crypto/crypto.dart' show sha256;

import '../api/api.dart';
import '../api/demo_api.dart';
import '../api/json.dart';
import '../api/keys.dart';
import '../core/format.dart';
import 'demo_fs.dart';

/// The demo's other machines and its swarm: "studio" (the demo computer, demo_fs.dart's drives),
/// "nextcloud" (a home server with an 8 TB drive, whose last sync failed with an ssh timeout, as
/// demo_activity.dart's jobs say; it holds the 2014 photos under /srv/photos/2014, as
/// demo_core.dart's file details say) and "attic-pi" (a Raspberry Pi with two 2.5" drives, reached
/// two hours ago). Two pool folders are on nextcloud's big drive, one on the Pi.
final Map<String, DemoHandler> demoMachineGets = {
  'machines': (d, p) => _machinesJson(d),
  'machine-ls': (d, p) => _ls(d, p),
  'machine-file': (d, p) => _file(d, p),
  'machine-find': (d, p) => _machineFind(d, p),
  'swarm': (d, p) => _swarm(d),
  'swarm-machine': (d, p) => _swarmMachine(d, p),
  'swarm-where': (d, p) => _where(d, p),
  'pool-plans': (d, p) => {
    'plans': [
      for (final pl in _plans().reversed)
        {
          'id': pl['id'],
          'created_at': pl['created_at'],
          'drive': pl['drive'],
          'root': pl['root'],
          'state': pl['state'],
          'new_bytes': pl['new_bytes'],
          'unknown_bytes': pl['unknown_bytes'],
          'unplaced_bytes': pl['unplaced_bytes'],
        },
    ],
  },
  'pool-plan': (d, p) {
    final id = int.tryParse(p['id'] ?? '');
    return _plans().where((pl) => pl['id'] == id).firstOrNull ??
        (throw ApiException(ApiFailure.refused, 'there\'s no pool plan $id', status: 400));
  },
};

/// Only reached when the demo is told it may change things (as on the computer itself).
final Map<String, DemoHandler> demoMachinePosts = {
  'machine-sync': (d, p) {
    final s = _state(d);
    final ids = p['m'] == null ? [for (final m in _live(d)) m.id] : [_machine(d, p['m']).id];
    final jobs = <int>[];
    for (final id in ids) {
      final job = s.nextJob++;
      s.jobs[id] = {
        'id': job,
        'kind': 'remote-sync',
        'path': _machine(d, '$id').name,
        'state': 'queued',
        'stoppable': true,
        'progress': '',
        'counts': null,
        'summary': '',
        'error': null,
        'auto': false,
        'reason': 'requested',
        'queued_at': nowSeconds(),
        'started_at': null,
        'finished_at': null,
      };
      jobs.add(job);
    }
    d.changed();
    return {'ok': true, 'jobs': jobs};
  },
  'machine-test': (d, p) {
    final m = _machine(d, p['m']);
    _state(d).lastTry[m.id] = nowSeconds();
    if (!m.reachable) return {'ok': true, 'reached': false, 'test_error': _timeout};
    return {
      'ok': true,
      'reached': true,
      'test':
          'Reached ${m.name} (${m.name}): BackDown ${m.version}, index ${m.db}, ${m.roots.length} drives indexed: '
          '${m.roots.map((r) => r.path).join(', ')}',
    };
  },
  'machine-settings': (d, p) {
    final s = _state(d);
    int? n(String k) => int.tryParse(p[k]?.trim() ?? '');
    final days = n('proof_days'), hours = n('sync_hours');
    if (p['proof_days'] != null && (days == null || days < 1 || days > 90)) {
      throw ApiException(ApiFailure.refused, 'proof_days must be 1 to 90', status: 400);
    }
    if (p['sync_hours'] != null && (hours == null || hours < 1 || hours > 168)) {
      throw ApiException(ApiFailure.refused, 'sync_hours must be 1 to 168', status: 400);
    }
    s.proofDays = days ?? s.proofDays;
    s.syncHours = hours ?? s.syncHours;
    return {
      'ok': true,
      'settings': {'proof_days': s.proofDays, 'sync_hours': s.syncHours},
    };
  },
  'machine-remove': (d, p) {
    _state(d).removed.add(_machine(d, p['m']).id);
    d.changed();
    return {'ok': true};
  },
};

// ---------- the machines and what's on them

const _timeout = 'ssh: connect to host nextcloud port 22: Connection timed out';

int _t(int y, [int m = 1, int d = 1, int h = 12]) => DateTime.utc(y, m, d, h).millisecondsSinceEpoch ~/ 1000;

String _hash(String seed) => sha256.convert(utf8.encode(seed)).toString();

/// A file or folder on another machine.
class _Node {
  _Node.dir(this.name, this.children) : size = 0, mtime = 0, sha = null, rid = null, hashed = true;
  _Node.file(this.name, this.size, this.mtime, {this.hashed = true})
    : children = null,
      rid = null,
      sha = _hash('$name/$size/$mtime');

  /// A copy of one of the demo computer's files (same content, so the same hash).
  _Node.copy(DemoNode n, {this.rid})
    : hashed = true,
      name = n.name,
      size = n.size,
      mtime = n.mtime,
      sha = n.hash,
      children = null;

  final String name;
  final List<_Node>? children;
  int size, mtime;
  final String? sha;
  int? rid;

  /// Its SHA-256 is cached here: hashed there for comparison (a file here has its size), or by
  /// that machine's own BackDown (a pool folder's files, read back as they were written).
  final bool hashed;

  late String path;
  _Node? parent;
  int files = 0;

  bool get isDir => children != null;
  String get key => keyOfPath(path);
}

/// An indexed drive on another machine.
class _Root {
  _Root(this.id, this.node, {required this.scannedAt, required this.seenAt});
  final int id;
  final _Node node;

  /// How long ago it was scanned there, and its files last confirmed there (seconds).
  final int scannedAt, seenAt;
  String get path => node.path;
}

class _Machine {
  _Machine({
    required this.id,
    required this.name,
    required this.url,
    required this.version,
    required this.db,
    required this.reachable,
    required this.syncedAgo,
    required this.triedAgo,
    required List<_Root> Function() roots,
    required int ridBase,
  }) : roots = roots() {
    var next = ridBase;
    void walk(_Node n, _Node? parent, String path) {
      n.parent = parent;
      n.path = path;
      if (n.isDir) {
        for (final c in n.children!) {
          walk(c, n, '$path/${c.name}');
        }
        n.size = n.children!.fold(0, (a, c) => a + c.size);
        n.files = n.children!.fold(0, (a, c) => a + (c.isDir ? c.files : 1));
        n.mtime = n.children!.fold(0, (a, c) => c.mtime > a ? c.mtime : a);
      } else {
        n.rid ??= ++next;
        byRid[n.rid!] = n;
        (byHash[n.sha!] ??= []).add(n);
      }
      byKey[n.key] = n;
    }

    for (final r in this.roots) {
      walk(r.node, null, r.node.name);
    }
  }

  final int id;
  final String name, url, version, db;

  /// Whether it answers now (nextcloud's ssh times out).
  final bool reachable;

  /// Seconds since its last good sync, and since it was last tried.
  final int syncedAgo, triedAgo;
  final List<_Root> roots;
  final Map<int, _Node> byRid = {};
  final Map<String, _Node> byKey = {};
  final Map<String, List<_Node>> byHash = {};

  _Root rootOf(_Node n) {
    var top = n;
    while (top.parent != null) {
      top = top.parent!;
    }
    return roots.firstWhere((r) => identical(r.node, top));
  }

  Iterable<_Node> get allFiles => byRid.values;
}

DemoFs get _fs => DemoFs.instance;

List<DemoNode> _local(String path) => _fs.byKey[keyOfPath(path)]!.children!;

List<_Node> _photos(String prefix, int count, int start, int year, {int month = 1, bool hashed = false}) => [
  for (var i = 0; i < count; i++)
    _Node.file(
      '$prefix${start + i}.${prefix == 'PXL_' ? 'jpg' : 'JPG'}',
      2600000 + (i * 211717) % 1800000,
      _t(year, ((month - 1 + i) % 12) + 1, 1 + (i * 5) % 27),
      hashed: hashed,
    ),
];

List<_Node> _mails(int n, int t0, int base) => [
  for (var i = 0; i < n; i++)
    _Node.file(
      '${t0 + i * 5400}.M${i}P${base + i}.nextcloud,S=${3800 + i * 271}:2,S',
      3800 + i * 271,
      t0 + i * 5400,
      hashed: false,
    ),
];

final _machines = <_Machine>[
  _Machine(
    id: 1,
    name: 'nextcloud',
    url: 'ssh://sam@nextcloud',
    version: '1.0.0-beta.4',
    db: '/var/lib/backdown/index.db',
    reachable: false,
    syncedAgo: 90000,
    triedAgo: 5380,
    ridBase: 20000,
    roots: () => [
      // a pool folder that was the photo library already: the 2014 photos, as on the Seagate
      _Root(
        1,
        _Node.dir('/srv/photos', [
          _Node.dir('2014', [
            for (final n in _local('/media/sam/Seagate Expansion/Photos/2014')) _Node.copy(n, rid: 9000 + n.id!),
          ]),
          _Node.dir('2017', _photos('IMG_', 8, 6200, 2017, month: 4)),
          _Node.dir('2018', _photos('IMG_', 6, 7400, 2018, month: 7)),
        ]),
        scannedAt: 93600,
        seenAt: 90000,
      ),
      _Root(
        2,
        _Node.dir('/srv/mail', [
          _Node.dir('example.org', [
            _Node.dir('sam', [
              _Node.dir('cur', _mails(5, _t(2021, 3, 2), 100)),
              _Node.dir('new', _mails(1, _t(2021, 3, 9), 200)),
            ]),
            _Node.dir('info', [_Node.dir('cur', _mails(2, _t(2020, 11, 20), 300))]),
          ]),
        ]),
        scannedAt: 93600,
        seenAt: 90000,
      ),
      // the pool folder that takes anything
      _Root(
        3,
        _Node.dir('/srv/pool', [
          _Node.dir('Machines', [
            _Node.dir('thinkpad-2012', [
              _Node.file('thinkpad-2012.img', 64023257088, _t(2016, 2, 11)),
              _Node.file('README.txt', 412, _t(2016, 2, 11)),
            ]),
          ]),
          _Node.dir('Mail', [
            _Node.dir('old-isp.net', [
              _Node.file('INBOX.mbox', 211812352, _t(2015, 6, 30)),
              _Node.file('Sent.mbox', 48299008, _t(2015, 6, 30)),
            ]),
          ]),
        ]),
        scannedAt: 93600,
        seenAt: 90000,
      ),
    ],
  ),
  _Machine(
    id: 2,
    name: 'attic-pi',
    url: 'ssh://pi@attic-pi',
    version: '1.0.0-beta.4',
    db: '/home/pi/.local/share/backdown/index.db',
    reachable: true,
    syncedAgo: 7200,
    triedAgo: 7200,
    ridBase: 30000,
    roots: () => [
      // the old 2.5" drive: scanned there 12 days ago, so its copies are too old to count
      _Root(
        1,
        _Node.dir('/mnt/old-backup', [
          _Node.dir('laptop-2015', [
            _Node.dir('Documents', [
              _Node.file('thesis.pdf', 2411520, _t(2015, 5, 18)),
              _Node.file('cv-2015.docx', 48211, _t(2015, 9, 2)),
              _Node.file('budget-2015.xlsx', 30455, _t(2015, 12, 30)),
            ]),
            _Node.dir('Pictures', _photos('IMG_', 8, 412, 2015, month: 6, hashed: true)),
          ]),
          _Node.file('dotfiles.tar.gz', 1849311, _t(2015, 11, 3)),
        ]),
        scannedAt: 12 * 86400,
        seenAt: 12 * 86400,
      ),
      // the pool folder on the other 2.5" drive: Media only
      _Root(
        2,
        _Node.dir('/mnt/pool', [
          _Node.dir('Media', [
            _Node.dir('Photos', [_Node.dir('Pixel 3', _photos('PXL_', 14, 20190514, 2019, month: 5, hashed: true))]),
            _Node.dir('Music', [
              _Node.dir('Ólafur Arnalds', [
                _Node.dir('re:member', [
                  _Node.file('01 re:member.flac', 41239112, _t(2018, 8, 24)),
                  _Node.file('02 brot.flac', 28771200, _t(2018, 8, 24)),
                  _Node.file('03 saman.flac', 25103360, _t(2018, 8, 24)),
                ]),
              ]),
            ]),
          ]),
        ]),
        scannedAt: 7300,
        seenAt: 7200,
      ),
    ],
  ),
];

// ---------- state kept between requests

class _State {
  int proofDays = 7, syncHours = 6, nextJob = 900;
  final Map<int, Map<String, Object?>> jobs = {};
  final Map<int, int> lastTry = {};
  final Set<int> removed = {};
}

_State _state(DemoBackDownApi d) => (d.state['machines'] ??= _State()) as _State;

Iterable<_Machine> _live(DemoBackDownApi d) => _machines.where((m) => !_state(d).removed.contains(m.id));

_Machine _machine(DemoBackDownApi d, String? id) {
  final n = int.tryParse(id ?? '');
  return _live(d).where((m) => m.id == n).firstOrNull ??
      (throw ApiException(ApiFailure.refused, 'there\'s no machine $id', status: 400));
}

int _lastTry(DemoBackDownApi d, _Machine m) => _state(d).lastTry[m.id] ?? nowSeconds() - m.triedAgo;
int _lastSync(_Machine m) => nowSeconds() - m.syncedAgo;

// ---------- GET /api/machines

Map<String, Object?> _rootJson(_Machine m, _Root r, int proofDays) {
  final now = nowSeconds();
  final files = m.allFiles.where((f) => identical(m.rootOf(f), r));
  return {
    'id': r.id,
    'key': r.node.key,
    'name': baseName(r.path),
    'path': r.path,
    'last_scan': 40 + r.id,
    'scanned_at': now - r.scannedAt,
    'files': r.node.files,
    'bytes': r.node.size,
    'synced_scan': 40 + r.id,
    'seen_at': now - r.seenAt,
    'cached': files.length,
    'hashed': files.where((f) => f.hashed).length,
    'counts': r.seenAt <= proofDays * 86400,
  };
}

Map<String, Object?> _machineJson(DemoBackDownApi d, _Machine m) {
  final s = _state(d);
  final synced = _lastSync(m);
  return {
    'id': m.id,
    'name': m.name,
    'url': m.url,
    'db': null,
    'command': null,
    'created_at': _t(2026, 6, 14),
    'last_try': _lastTry(d, m),
    'last_sync': synced,
    'last_error': m.reachable ? null : _timeout,
    'info': {
      'db': m.db,
      'host': m.name,
      'os': 'unix',
      'proto': 1,
      'read_only': true,
      'version': m.version,
      'seen': synced,
      'now': synced + 2,
      'skew': 2,
    },
    'job': s.jobs[m.id],
    'roots': [for (final r in m.roots) _rootJson(m, r, s.proofDays)],
  };
}

Map<String, Object?> _machinesJson(DemoBackDownApi d) {
  final s = _state(d);
  return {
    'machines': [for (final m in _live(d)) _machineJson(d, m)],
    'settings': {'proof_days': s.proofDays, 'sync_hours': s.syncHours},
    'allowed': true,
    'why_not': null,
    'daemon': true,
  };
}

// ---------- browsing a machine: live (attic-pi) or as last synced (nextcloud)

Map<String, Object?> _ls(DemoBackDownApi d, Map<String, String> p) {
  final m = _machine(d, p['m']);
  final dir = p['dir'];
  final s = _state(d);
  final now = nowSeconds();
  if (dir == null || dir.isEmpty) {
    if (!m.reachable) {
      return {
        'roots': [for (final r in m.roots) _rootJson(m, r, s.proofDays)],
        'cached': true,
        'synced_at': _lastSync(m),
        'live': false,
        'why_not_live': _timeout,
      };
    }
    return {
      'roots': [
        for (final r in m.roots)
          {
            'id': r.id,
            'name': baseName(r.path),
            'path': r.path,
            'key': r.node.key,
            'last_scan': 40 + r.id,
            'scanned_at': now - r.scannedAt,
            'files': r.node.files,
            'bytes': r.node.size,
          },
      ],
      'live': true,
    };
  }
  final n = m.byKey[dir];
  if (m.reachable) {
    if (n == null || !n.isDir) throw ApiException(ApiFailure.refused, 'that folder isn\'t in the index', status: 400);
    final r = m.rootOf(n);
    return {
      'root_id': r.id,
      'scan': 40 + r.id,
      'latest_scan': 40 + r.id,
      'first_scan': 30 + r.id,
      'scanned_at': now - r.scannedAt,
      'name': n.parent == null ? n.path : n.name,
      'path': n.path,
      'key': n.key,
      'crumbs': _crumbs(n),
      'files': n.files,
      'bytes': n.size,
      'entries': [
        for (final c in n.children!)
          {
            'kind': c.isDir ? 'dir' : 'file',
            'name': c.name,
            'key': c.key,
            'id': c.rid,
            'size': c.size,
            'files': c.isDir ? c.files : null,
            'mtime': c.mtime,
            'sha256': c.isDir ? null : c.sha,
            'copies': c.isDir ? 0 : m.byHash[c.sha]!.length - 1,
            'versions': 1,
            'new_here': false,
            'later': 'kept',
          },
      ],
      'needs_reindex': false,
      'live': true,
    };
  }
  // as last synced: only files are cached, so folders come without sizes
  if (n == null || !n.isDir) {
    throw ApiException(ApiFailure.refused, '${pathOfKey(dir)} isn\'t on a drive ${m.name} has indexed', status: 400);
  }
  final r = m.rootOf(n);
  return {
    'cached': true,
    'root_id': r.id,
    'name': n.parent == null ? n.path : n.name,
    'path': n.path,
    'key': n.key,
    'crumbs': _crumbs(n),
    'entries': [
      for (final c in n.children!)
        if (c.isDir)
          {'kind': 'dir', 'name': c.name, 'key': c.key, 'id': null, 'size': 0, 'mtime': 0, 'sha256': null}
        else
          {
            'kind': 'file',
            'name': c.name,
            'key': c.key,
            'id': c.rid,
            'size': c.size,
            'mtime': c.mtime,
            'sha256': c.hashed ? c.sha : null,
          },
    ],
    'truncated': false,
    'seen_at': now - r.seenAt,
    'synced_at': _lastSync(m),
    'live': false,
    'why_not_live': _timeout,
  };
}

List<Map<String, Object?>> _crumbs(_Node n) {
  final out = <Map<String, Object?>>[];
  for (_Node? c = n; c != null; c = c.parent) {
    out.insert(0, {'key': c.key, 'name': c.parent == null ? c.path : c.name});
  }
  return out;
}

// ---------- a file there, and where else its content is

Map<String, Object?> _file(DemoBackDownApi d, Map<String, String> p) {
  final m = _machine(d, p['m']);
  final rid = int.tryParse(p['id'] ?? '') ?? 0;
  final n = m.byRid[rid];
  if (n == null) throw ApiException(ApiFailure.refused, '${m.name} has no file $rid (as last synced)', status: 400);
  final r = m.rootOf(n);
  final now = nowSeconds();
  final sha = m.reachable || n.hashed ? n.sha : null;
  final Map<String, Object?> v;
  if (m.reachable) {
    v = {
      'id': rid,
      'name': n.name,
      'path': n.path,
      'key': n.key,
      'dir_key': n.parent!.key,
      'size': n.size,
      'mtime': n.mtime,
      'sha256': sha,
      'error': null,
      'current': true,
      'versions': [
        {
          'id': rid,
          'size': n.size,
          'mtime': n.mtime,
          'sha256': sha,
          'first_scan': 30 + r.id,
          'last_scan': 40 + r.id,
          'from': now - r.scannedAt - 86400 * 30,
          'to': now - r.scannedAt,
          'current': true,
          'readable': true,
        },
      ],
      'copies': [
        for (final c in m.byHash[n.sha] ?? const <_Node>[])
          if (!identical(c, n)) {'id': c.rid, 'path': c.path, 'key': c.key, 'dir_key': c.parent!.key},
      ],
      'content_from': '',
      'live': true,
    };
  } else {
    v = {
      'cached': true,
      'id': rid,
      'name': n.name,
      'path': n.path,
      'key': n.key,
      'dir_key': n.parent!.key,
      'size': n.size,
      'mtime': n.mtime,
      'sha256': sha,
      'error': null,
      'hashed_at': sha == null ? null : _lastSync(m) - 3600,
      'versions': <Object?>[],
      'copies': <Object?>[],
      'live': false,
      'why_not_live': _timeout,
    };
  }
  if (sha != null) {
    v['local_copies'] = [
      for (final c in _fs.byHash[sha] ?? const <DemoNode>[])
        {'id': c.id, 'path': c.path, 'key': c.key, 'dir_key': c.parent!.key},
    ];
    v['remote_copies'] = [
      for (final o in _live(d))
        for (final c in o.byHash[sha] ?? const <_Node>[])
          if (!(o.id == m.id && c.rid == rid) && c.hashed) _copyJson(d, o, c),
    ];
  }
  v['machine'] = {'id': m.id, 'name': m.name, 'url': m.url};
  return v;
}

Map<String, Object?> _copyJson(DemoBackDownApi d, _Machine m, _Node c) {
  final r = m.rootOf(c);
  final counts = r.seenAt <= _state(d).proofDays * 86400;
  return {
    'machine_id': m.id,
    'machine': m.name,
    'url': m.url,
    'rid': c.rid,
    'root_id': r.id,
    'path': c.path,
    'key': c.key,
    'dir_key': c.parent!.key,
    'confirmed_at': nowSeconds() - r.seenAt,
    'counts': counts,
    'why_not': counts ? null : 'confirmed more than ${_state(d).proofDays} days ago',
  };
}

// ---------- searching by name

int _rank(String name, String q) {
  final n = name.toLowerCase(), t = q.trim().toLowerCase();
  if (t.isEmpty || !n.contains(t)) return 0;
  if (n == t || n.split('.').first == t) return 4;
  if (n.startsWith(t)) return 3;
  if (n.split(RegExp(r'[\s._-]')).any((w) => w.startsWith(t))) return 2;
  return 1;
}

Map<String, Object?> _hitJson(_Machine m, _Node n, int rank, {bool cached = false}) {
  final r = m.rootOf(n);
  final folder = n.parent!.path.substring(r.path.length).replaceFirst(RegExp('^/'), '');
  return {
    'kind': n.isDir ? 'dir' : 'file',
    'id': n.rid,
    'name': n.name,
    'path': n.path,
    'key': n.key,
    'dir_key': n.parent!.key,
    'size': n.size,
    'mtime': n.mtime,
    'root': r.id,
    'drive': cached ? baseName(r.path) : r.path,
    'folder': folder,
    'rank': rank,
    'gone': false,
    'seen': cached ? nowSeconds() - r.seenAt : null,
    'scan': null,
    if (cached) 'cached': true,
  };
}

List<(int, _Node)> _search(_Machine m, String q, {bool filesOnly = false}) {
  final hits = <(int, _Node)>[
    for (final n in m.byKey.values)
      if (n.parent != null && !(filesOnly && n.isDir))
        if (_rank(n.name, q) case final rk when rk > 0) (rk, n),
  ];
  hits.sort((a, b) => b.$1 != a.$1 ? b.$1.compareTo(a.$1) : b.$2.mtime.compareTo(a.$2.mtime));
  return hits;
}

Map<String, Object?> _machineFind(DemoBackDownApi d, Map<String, String> p) {
  final m = _machine(d, p['m']);
  if (!m.reachable) throw ApiException(ApiFailure.refused, _timeout, status: 400);
  final q = p['q'] ?? '';
  final within = p['dir'] == null ? null : m.byKey[p['dir']];
  final hits = [
    for (final (rk, n) in _search(m, q))
      if (within == null || n.path.startsWith('${within.path}/')) (rk, n),
  ];
  return {
    'hits': [for (final (rk, n) in hits.take(200)) _hitJson(m, n, rk)],
    'total': hits.length,
    'gone': 0,
    'ms': 41,
  };
}

// ---------- the swarm: every machine's drives, free space and pool folders

Map<String, Object?> _disk(
  String name,
  String model,
  String serial,
  int size,
  String transport, {
  required bool rotational,
  bool system = false,
  bool removable = false,
  String? health,
  List<Map<String, Object?>> volumes = const [],
}) => {
  'name': name,
  'device': '/dev/$name',
  'model': model,
  'serial': serial,
  'size': size,
  'transport': transport,
  'rotational': rotational,
  'removable': removable,
  'system': system,
  'backing_file': null,
  'health': health,
  'volumes': volumes,
};

Map<String, Object?> _vol(
  String name,
  String? label,
  String fs,
  String fsName,
  int size, {
  String state = 'mounted',
  List<String> mounts = const [],
  ({int total, int free})? space,
  List<Map<String, Object?>> indexed = const [],
  List<Map<String, Object?>> pool = const [],
}) => {
  'device': '/dev/$name',
  'name': name,
  'label': label,
  'part_label': null,
  'fs': fs,
  'fs_name': fsName,
  'uuid': _hash('uuid/$name/$label').substring(0, 32),
  'size': size,
  'state': state,
  'via': null,
  'mounts': mounts,
  'read_only': false,
  'space': space == null ? null : {'total': space.total, 'free': space.free},
  'system': state == 'system',
  'indexed': indexed,
  if (pool.isNotEmpty) 'pool': pool,
};

Map<String, Object?> _identity(Map<String, Object?> disk, Map<String, Object?> vol, {bool? present}) => {
  'disk': {
    'name': disk['name'],
    'model': disk['model'],
    'serial': disk['serial'],
    'size': disk['size'],
    'transport': disk['transport'],
    'rotational': disk['rotational'],
    'removable': disk['removable'],
  },
  'volume': {
    'device': vol['device'],
    'label': vol['label'],
    'uuid': vol['uuid'],
    'fs': vol['fs'],
    'fs_name': vol['fs_name'],
    'size': vol['size'],
    'mount': (vol['mounts']! as List).firstOrNull,
  },
  'present': ?present,
};

/// A machine's storage, as its probe would say it.
class _Storage {
  _Storage(this.disks, {this.away = const [], this.pool = const []});
  final List<Map<String, Object?>> disks;
  final List<Map<String, Object?>> away;
  final List<Map<String, Object?>> pool;

  /// The disk and volume a path is on (the deepest mount above it).
  (Map<String, Object?>, Map<String, Object?>)? placeOf(String path) {
    (Map<String, Object?>, Map<String, Object?>)? best;
    var depth = -1;
    for (final d in disks) {
      for (final v in (d['volumes']! as List).cast<Map<String, Object?>>()) {
        for (final mt in (v['mounts']! as List).cast<String>()) {
          final inside = mt == '/' || path == mt || path.startsWith('$mt/');
          if (inside && mt.length > depth) {
            depth = mt.length;
            best = (d, v);
          }
        }
      }
    }
    return best;
  }
}

Map<String, Object?> _indexedHere(int id, String path, int files, int bytes, int scannedAt) => {
  'id': id,
  'path': path,
  'key': keyOfPath(path),
  'relation': 'inside',
  'name': baseName(path),
  'files': files,
  'bytes': bytes,
  'scanned_at': scannedAt,
};

Map<String, Object?> _indexedThere(_Machine m, String path) {
  final r = m.roots.firstWhere((r) => r.path == path);
  return _indexedHere(r.id, path, r.node.files, r.node.size, nowSeconds() - r.scannedAt);
}

final _seagateDisk = _disk(
  'sda',
  'ST4000DM004-2CV104',
  'ZFN0QX3L',
  4000787030016,
  'usb',
  rotational: true,
  removable: true,
);
final _wdDisk = _disk(
  'sdb',
  'WDC WD20SDRW-11VUUS0',
  'WX21A87H2RPL',
  2000365289472,
  'usb',
  rotational: true,
  removable: true,
);

_Storage _studio() {
  final seagate = _fs.roots[0], wd = _fs.roots[1], home = _fs.roots[2];
  const total = 4000650883072;
  final seagateVol = _vol(
    'sda1',
    'Seagate Expansion',
    'ntfs',
    'NTFS',
    total,
    mounts: [seagate.node.path],
    space: (total: total, free: total - seagate.node.size - 1450000000),
    indexed: [_indexedHere(1, seagate.node.path, seagate.node.files, seagate.node.size, seagate.scans.last.finishedAt)],
  );
  final wdVol = _vol('sdb1', 'WD Passport', 'exfat', 'exFAT', 2000363192320, mounts: [wd.node.path]);
  return _Storage(
    [
      _disk(
        'nvme0n1',
        'Samsung SSD 980 1TB',
        'S649NF0R812345K',
        1000204886016,
        'nvme',
        rotational: false,
        system: true,
        health: 'ok',
        volumes: [
          _vol(
            'nvme0n1p2',
            null,
            'ext4',
            'ext4',
            999129481216,
            state: 'system',
            mounts: ['/'],
            space: (total: 983349346304, free: 412133679104),
            indexed: [_indexedHere(3, home.node.path, home.node.files, home.node.size, home.scans.last.finishedAt)],
          ),
        ],
      ),
      {
        ..._seagateDisk,
        'volumes': [seagateVol],
      },
    ],
    away: [
      {
        'id': wd.id,
        'name': baseName(wd.node.path),
        'path': wd.node.path,
        'key': wd.node.key,
        'files': wd.node.files,
        'bytes': wd.node.size,
        'scanned_at': wd.scans.last.finishedAt,
        'space': null,
        'drive': _identity(_wdDisk, wdVol),
      },
    ],
  );
}

final _nextcloudData = _disk(
  'sdb',
  'WDC WD80EFAX-68KNBN0',
  'VGH5ZT2G',
  8001563222016,
  'sata',
  rotational: true,
  health: 'ok',
);
final _piPoolDisk = _disk(
  'sda',
  'WDC WD10SPZX-22Z10T1',
  'WXE1A39C5TL2',
  1000204886016,
  'usb',
  rotational: true,
  removable: true,
  health: 'ok',
);
final _piOldDisk = _disk(
  'sdb',
  'ST2000LM015-2E8174',
  'ZDZ5K1RW',
  2000398934016,
  'usb',
  rotational: true,
  removable: true,
  health: 'warning',
);

_Storage _storageOf(_Machine m) {
  if (m.id == 1) {
    const total = 7937047539712, free = 5041192062976;
    final vol = _vol(
      'sdb1',
      'data',
      'ext4',
      'ext4',
      8001561124864,
      mounts: ['/srv'],
      space: (total: total, free: free),
      indexed: [
        for (final p in ['/srv/mail', '/srv/photos', '/srv/pool']) _indexedThere(m, p),
      ],
      pool: [
        {'id': 1, 'path': '/srv/photos'},
        {'id': 2, 'path': '/srv/pool'},
      ],
    );
    final disk = {
      ..._nextcloudData,
      'volumes': [vol],
    };
    return _Storage(
      [
        _disk(
          'sda',
          'Samsung SSD 870 EVO 500GB',
          'S62ANJ0R304512B',
          500107862016,
          'sata',
          rotational: false,
          system: true,
          health: 'ok',
          volumes: [
            _vol(
              'sda2',
              null,
              'ext4',
              'ext4',
              499033423872,
              state: 'system',
              mounts: ['/'],
              space: (total: 490577010688, free: 402653184000),
            ),
          ],
        ),
        disk,
      ],
      pool: [
        _poolFolder(m, 1, '/srv/photos', disk, vol, reserve: 400000000000, takes: ['Media']),
        _poolFolder(m, 2, '/srv/pool', disk, vol, reserve: 400000000000),
      ],
    );
  }
  const poolTotal = 983349346304, oldTotal = 1968355647488;
  final poolVol = _vol(
    'sda1',
    'pi-pool',
    'ext4',
    'ext4',
    1000203837440,
    mounts: ['/mnt/pool'],
    space: (total: poolTotal, free: poolTotal - m.roots[1].node.size - 21000000000),
    indexed: [_indexedThere(m, '/mnt/pool')],
    pool: [
      {'id': 1, 'path': '/mnt/pool'},
    ],
  );
  final poolDisk = {
    ..._piPoolDisk,
    'volumes': [poolVol],
  };
  return _Storage(
    [
      _disk(
        'mmcblk0',
        'SC32G',
        '0x8a2b4c11',
        31914983424,
        'mmc',
        rotational: false,
        system: true,
        volumes: [
          _vol(
            'mmcblk0p2',
            'rootfs',
            'ext4',
            'ext4',
            31645499392,
            state: 'system',
            mounts: ['/'],
            space: (total: 31064010752, free: 22548578304),
          ),
        ],
      ),
      poolDisk,
      {
        ..._piOldDisk,
        'volumes': [
          _vol(
            'sdb1',
            'old-backup',
            'ext4',
            'ext4',
            2000396836864,
            mounts: ['/mnt/old-backup'],
            space: (total: oldTotal, free: 1712301234176),
            indexed: [_indexedThere(m, '/mnt/old-backup')],
          ),
        ],
      },
    ],
    pool: [
      _poolFolder(m, 1, '/mnt/pool', poolDisk, poolVol, reserve: 49167467315, takes: ['Media']),
    ],
  );
}

Map<String, Object?> _poolFolder(
  _Machine m,
  int id,
  String path,
  Map<String, Object?> disk,
  Map<String, Object?> vol, {
  required int reserve,
  List<String> takes = const [],
}) {
  final r = m.roots.firstWhere((r) => r.path == path);
  final space = vol['space']! as Map<String, Object?>;
  return {
    'id': id,
    'path': path,
    'key': keyOfPath(path),
    'reserve': reserve,
    'takes': takes,
    'created_at': _t(2026, 10, 3),
    'present': true,
    'answering': true,
    'space': space,
    'room': (space['free']! as int) - reserve,
    'indexed': true,
    'root': {'id': r.id, 'path': path, 'key': keyOfPath(path), 'last_scan': 40 + r.id, 'scanned_at': null},
    'files': r.node.files,
    'bytes': r.node.size,
    'drive': _identity(disk, vol),
    'fs': (vol['mounts']! as List).first,
  };
}

/// One machine's part of the overview, from its storage (swarm.rs's machine_view).
Map<String, Object?> _view(_Storage s, {String os = 'linux'}) {
  var capacity = 0, free = 0, files = 0, size = 0;
  for (final d in s.disks) {
    for (final v in (d['volumes']! as List).cast<Map<String, Object?>>()) {
      final sp = v['space'] as Map<String, Object?>?;
      if (sp != null) {
        capacity += sp['total']! as int;
        free += sp['free']! as int;
      }
      for (final i in (v['indexed']! as List).cast<Map<String, Object?>>()) {
        files += i['files']! as int;
        size += i['bytes']! as int;
      }
    }
  }
  for (final r in s.away) {
    files += r['files']! as int;
    size += r['bytes']! as int;
  }
  // room per file system, less the biggest reserve there
  final byFs = <String, (int, int)>{};
  for (final a in s.pool) {
    final f = (a['space']! as Map<String, Object?>)['free']! as int;
    final fs = a['fs']! as String, reserve = a['reserve']! as int;
    final e = byFs[fs] ?? (f, 0);
    byFs[fs] = (e.$1, reserve > e.$2 ? reserve : e.$2);
  }
  return {
    'os': os,
    'at': nowSeconds(),
    'disks': s.disks,
    'elsewhere': <Object?>[],
    'away': s.away,
    'unanswered': <Object?>[],
    'notes': <Object?>[],
    'pool': s.pool,
    'totals': {
      'disks': s.disks.length,
      'capacity': capacity,
      'free': free,
      'files': files,
      'bytes': size,
      'pool': {
        'areas': s.pool.length,
        'files': s.pool.fold<int>(0, (a, p) => a + (p['files']! as int)),
        'bytes': s.pool.fold<int>(0, (a, p) => a + (p['bytes']! as int)),
        'room': byFs.values.fold<int>(0, (a, e) => a + e.$1 - e.$2),
      },
    },
  };
}

Map<String, Object?> _remoteView(DemoBackDownApi d, _Machine m, {bool asked = false}) {
  final now = nowSeconds();
  final okAt = m.reachable && asked ? now - 1 : _lastSync(m);
  return {
    ..._view(_storageOf(m)),
    'name': m.name,
    'here': false,
    'id': m.id,
    'url': m.url,
    'version': m.version,
    'state': m.reachable ? 'ok' : 'error',
    'error': m.reachable ? null : _timeout,
    'ok_at': okAt,
    'asked_at': m.reachable ? okAt : _lastTry(d, m),
    'last_sync': _lastSync(m),
    'last_try': _lastTry(d, m),
  };
}

Map<String, Object?> _swarm(DemoBackDownApi d) {
  final here = {
    ..._view(_studio()),
    'name': 'studio',
    'here': true,
    'id': null,
    'state': 'here',
    'error': null,
    'version': '1.0.0-beta.4',
  };
  final machines = [here, for (final m in _live(d)) _remoteView(d, m)];
  int sum(String k) => machines.fold(0, (a, m) => a + asJson(m['totals']).integer(k));
  int pool(String k) => machines.fold(0, (a, m) => a + asJson(m['totals']).obj('pool').integer(k));
  return {
    'here': 'studio',
    'machines': machines,
    'totals': {
      'machines': machines.length,
      'reporting': machines.where((m) => (m['disks']! as List).isNotEmpty).length,
      'disks': sum('disks'),
      'capacity': sum('capacity'),
      'free': sum('free'),
      'files': sum('files'),
      'bytes': sum('bytes'),
      'pool': {'areas': pool('areas'), 'files': pool('files'), 'bytes': pool('bytes'), 'room': pool('room')},
    },
    'can_ask': true,
    'why_not': null,
    'daemon': true,
  };
}

Map<String, Object?> _swarmMachine(DemoBackDownApi d, Map<String, String> p) {
  final m = _machine(d, p['m']);
  if (!m.reachable) _state(d).lastTry[m.id] = nowSeconds();
  return {..._remoteView(d, m, asked: true), 'asked': m.reachable, 'ask_error': m.reachable ? null : _timeout};
}

// ---------- where is it: every machine at once

Map<String, Object?>? _placeHere(String path) {
  final s = _studio();
  final wd = _fs.roots[1].node.path;
  if (path == wd || path.startsWith('$wd/')) {
    return {...(s.away.first['drive']! as Map<String, Object?>), 'present': false};
  }
  final at = s.placeOf(path);
  return at == null ? null : _identity(at.$1, at.$2, present: true);
}

Future<Map<String, Object?>> _where(DemoBackDownApi d, Map<String, String> p) async {
  final q = (p['q'] ?? '').trim();
  if (q.isEmpty) throw ApiException(ApiFailure.refused, 'find what? Give some letters of the name', status: 400);
  final limit = (int.tryParse(p['limit'] ?? '') ?? 200).clamp(1, 500);
  final here = asJson(await d.get('find', {'q': q, 'limit': limit}));
  final hits = <Map<String, Object?>>[
    for (final h in here.list('hits'))
      {...h, 'machine': 'studio', 'machine_id': null, 'here': true, 'place': _placeHere(h.str('path'))},
  ];
  final searched = <Map<String, Object?>>[
    {'name': 'studio', 'id': null, 'here': true, 'how': 'here', 'total': here.integer('total'), 'complete': true},
  ];
  for (final m in _live(d)) {
    final storage = _storageOf(m);
    Map<String, Object?>? place(String path) {
      final at = storage.placeOf(path);
      return at == null ? null : _identity(at.$1, at.$2, present: true);
    }

    final found = _search(m, q, filesOnly: !m.reachable);
    for (final (rk, n) in found.take(limit)) {
      hits.add({
        ..._hitJson(m, n, rk, cached: !m.reachable),
        'machine': m.name,
        'machine_id': m.id,
        'here': false,
        'place': place(n.path),
      });
    }
    searched.add(
      m.reachable
          ? {
              'name': m.name,
              'id': m.id,
              'here': false,
              'how': 'live',
              'total': found.length,
              'complete': true,
              'ms': 38,
            }
          : {
              'name': m.name,
              'id': m.id,
              'here': false,
              'how': 'cached',
              'total': found.length,
              'complete': true,
              'error': _timeout,
              'synced_at': _lastSync(m),
            },
    );
  }
  hits.sort((a, b) {
    final r = b.integer('rank').compareTo(a.integer('rank'));
    return r != 0 ? r : b.integer('mtime').compareTo(a.integer('mtime'));
  });
  return {'q': q, 'hits': hits.take(limit).toList(), 'machines': searched};
}

// ---------- the pool's plans: the Seagate (made two days ago) and the WD (before it was unplugged)

int _size(String path) => _fs.byKey[keyOfPath(path)]!.size;
int _files(String path) => _fs.byKey[keyOfPath(path)]!.files;

Map<String, Object?> _at(String machine, int machineId, int area, String path) => {
  'machine': machine,
  'machine_id': machineId,
  'area': area,
  'path': path,
};

final _photosAt = _at('nextcloud', 1, 1, '/srv/photos');
final _poolAt = _at('nextcloud', 1, 2, '/srv/pool');
final _piAt = _at('attic-pi', 2, 1, '/mnt/pool');

Map<String, Object?> _thing(
  String what,
  String kind,
  String source,
  String dest, {
  int? files,
  int? size,
  int inPool = 0,
  int inPoolFiles = 0,
  String? inPoolAt,
  int unknown = 0,
  int left = 0,
  Map<String, Object?>? to,
  String whyTo = '',
}) {
  final f = files ?? _files(source), b = size ?? _size(source);
  final unknownFiles = unknown > 0 ? 1 : 0, leftFiles = left > 0 ? 1 : 0;
  return {
    'item': {
      'what': what,
      'kind': kind,
      'source': source,
      'source_key': keyOfPath(source),
      'drive': '',
      'is_file': false,
      'only_files': false,
      'dest': dest,
      'files': f,
      'bytes': b,
      'action': 'copy-unique',
      'why': '',
      'exclude': <Object?>[],
    },
    'in_pool_files': inPoolFiles,
    'in_pool_bytes': inPool,
    'in_pool_at': inPoolAt,
    'twice_files': 0,
    'twice_bytes': 0,
    'new_files': f - inPoolFiles - unknownFiles - leftFiles,
    'new_bytes': b - inPool - unknown - left,
    'unknown_files': unknownFiles,
    'unknown_bytes': unknown,
    'left_files': leftFiles,
    'left_bytes': left,
    'to': to,
    'why_to': whyTo,
    'skip': false,
  };
}

Map<String, Object?> _planOf(int id, int ago, DemoRoot root, List<Map<String, Object?>> things, List<String> notes) {
  int sum(String k) => things.fold(0, (a, t) => a + (t[k]! as int));
  Map<String, Object?> area(Map<String, Object?> at, String drive, List<String> takes, int room) {
    final mine = things.where((t) => identical(t['to'], at));
    return {
      'at': at,
      'drive': drive,
      'takes': takes,
      'room': room,
      'assigned_files': mine.fold<int>(0, (a, t) => a + (t['new_files']! as int) + (t['unknown_files']! as int)),
      'assigned_bytes': mine.fold<int>(0, (a, t) => a + (t['new_bytes']! as int) + (t['unknown_bytes']! as int)),
      'indexed': true,
      'present': true,
    };
  }

  final newBytes = sum('new_bytes'), unknown = sum('unknown_bytes');
  return {
    'id': id,
    'created_at': nowSeconds() - ago,
    'machine': 'studio',
    'root_id': root.id,
    'root': root.node.path,
    'drive': baseName(root.node.path),
    'scan': root.scans.last.id,
    'things': things,
    'areas': [
      area(_photosAt, 'data (WDC WD80EFAX-68KNBN0)', ['Media'], 4641192062976),
      area(_poolAt, 'data (WDC WD80EFAX-68KNBN0)', [], 4641192062976),
      area(_piAt, 'pi-pool (WDC WD10SPZX-22Z10T1)', ['Media'], 810000000000),
    ],
    'files': root.node.files,
    'bytes': root.node.size,
    'new_bytes': newBytes,
    'in_pool_bytes': sum('in_pool_bytes'),
    'twice_bytes': 0,
    'unknown_bytes': unknown,
    'left_bytes': sum('left_bytes'),
    'copy_bytes': newBytes + unknown,
    'unplaced_bytes': 0,
    'notes': notes,
    'state': 'new',
  };
}

List<Map<String, Object?>> _plans() {
  const s = '/media/sam/Seagate Expansion', w = '/media/sam/WD Passport';
  const iphone = "$s/Backups/Sam's iPhone";
  const nextcloudOld =
      'nextcloud didn\'t answer when this plan was made: its pool folders are as it last said, the day before.';
  return [
    _planOf(1, 9 * 86400, _fs.roots[1], [
      _thing(
        'Photos: 19 photos from 2014 and 2015',
        'photos',
        '$w/Photos backup',
        'Media/Photos/WD Passport',
        inPool: _size('$w/Photos backup/2014'),
        inPoolFiles: _files('$w/Photos backup/2014'),
        inPoolAt: 'nextcloud:/srv/photos/2014/IMG_2100.JPG',
        to: _photosAt,
        whyTo: 'takes Media, and has the most room',
      ),
      _thing('Code: shoebox', 'git', '$w/Projects/shoebox', 'Code/shoebox', to: _poolAt, whyTo: 'the most room'),
      _thing(
        'Loose files on the drive',
        'unsorted',
        w,
        'Machines/WD Passport',
        files: 2,
        size: _size('$w/site-backup.zip') + _size('$w/shop-2018.sql'),
        to: _poolAt,
        whyTo: 'the most room',
      ),
    ], []),
    _planOf(
      2,
      2 * 86400,
      _fs.roots[0],
      [
        _thing(
          'Photos: 28 photos from 2014 to 2016',
          'photos',
          '$s/Photos',
          'Media/Photos/Seagate Expansion',
          inPool: _size('$s/Photos/2014'),
          inPoolFiles: _files('$s/Photos/2014'),
          inPoolAt: 'nextcloud:/srv/photos/2014/IMG_2100.JPG',
          to: _photosAt,
          whyTo: 'takes Media, and has the most room',
        ),
        _thing(
          'Mail server store: 1 mailbox in 1 domain',
          'mail-store',
          '$s/Mail/mail.example.org',
          'Mail/example.org',
          to: _poolAt,
          whyTo: 'takes anything; the most room',
        ),
        _thing(
          'Mailbox: archive-2012.mbox',
          'mbox',
          '$s/Mail/archive-2012.mbox',
          'Mail/archive-2012',
          files: 1,
          to: _poolAt,
          whyTo: 'next to Mail/example.org',
        ),
        _thing(
          "iPhone backup: Sam's iPhone, 14 Aug 2019",
          'iphone-backup',
          iphone,
          "Machines/Sam's iPhone/2019-08-14",
          unknown: _size('$iphone/Manifest.db'),
          left: _size('$iphone/Status.plist'),
          to: _poolAt,
          whyTo: 'the most room',
        ),
        _thing(
          'Folder: Documents',
          'unsorted',
          '$s/Documents',
          'Documents/Seagate Expansion',
          to: _poolAt,
          whyTo: 'the most room',
        ),
        _thing(
          'Music: Nils Frahm, Spaces',
          'music',
          '$s/Music/Nils Frahm',
          'Media/Music/Nils Frahm',
          to: _piAt,
          whyTo: 'chosen',
        ),
        _thing(
          'Disk image: old-laptop.img',
          'disk-image',
          '$s/Disk images',
          'Machines/old-laptop',
          to: _poolAt,
          whyTo: 'the most room',
        ),
        _thing(
          'Folder: Backups',
          'unsorted',
          '$s/Backups/backup-mail.sh',
          'Machines/Seagate Expansion/Backups',
          files: 1,
          to: _poolAt,
          whyTo: 'the most room',
        ),
      ],
      [nextcloudOld],
    ),
  ];
}
