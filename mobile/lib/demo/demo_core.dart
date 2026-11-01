import 'dart:convert';
import 'dart:typed_data';

import 'package:flutter/services.dart' show rootBundle;

import '../api/api.dart';
import '../api/demo_api.dart';
import '../api/keys.dart';
import 'demo_fs.dart';

/// The demo's answers for browsing: drives, folders, a file's details, search, file bytes.
final Map<String, DemoHandler> demoCoreGets = {
  'roots': (d, p) => {
    'roots': [for (final r in _fs.roots) _root(r)],
    'status': null,
  },
  'index-status': (d, p) {
    final files = _fs.files.length;
    final bytes = _fs.roots.fold<int>(0, (a, r) => a + r.node.size);
    return {
      'bytes': bytes,
      'files': files,
      'hashed': files - 3,
      'sampled': files,
      'roots': [for (final r in _fs.roots) r.node.path],
      'scans': _fs.roots.fold<int>(0, (a, r) => a + r.scans.length),
      'unreadable': 1,
      'versions': files + 4,
    };
  },
  'ls': (d, p) => _ls(p['dir'] ?? '', int.tryParse(p['scan'] ?? '')),
  'file': (d, p) => _file(int.tryParse(p['id'] ?? '') ?? 0),
  'timeline': (d, p) {
    final r = _fs.roots.where((r) => r.id == int.tryParse(p['root'] ?? '')).firstOrNull;
    if (r == null) throw ApiException(ApiFailure.refused, 'no such drive', status: 400);
    return [
      for (final s in r.scans) {'id': s.id, 'finished_at': s.finishedAt, 'files': s.files, 'bytes': s.bytes},
    ];
  },
  'find': (d, p) => _find(p['q'] ?? '', p['dir'], int.tryParse(p['limit'] ?? '') ?? 200),
  'search': (d, p) => [for (final h in (_find(p['q'] ?? '', p['dir'], 300)['hits'] as List)) h],
  'resolve': (d, p) {
    final n = p['key'] != null ? _fs.byKey[p['key']] : _fs.byKey[keyOfPath(p['path'] ?? '')];
    if (n == null) throw ApiException(ApiFailure.refused, "that isn't in the index", status: 400);
    if (n.isDir) return {'dir': n.key};
    return {'dir': n.parent!.key, 'select': n.key};
  },
};

final Map<String, DemoHandler> demoCorePosts = {};

final Map<String, DemoBytesHandler> demoCoreBytes = {
  'content': (d, p) => demoContent(int.tryParse(p['id'] ?? '') ?? 0),
};

DemoFs get _fs => DemoFs.instance;

/// The bytes of a demo file: its bundled stand-in or its text.
Future<Uint8List> demoContent(int id) async {
  final n = _fs.byId[id];
  if (n == null) throw ApiException(ApiFailure.notFound, 'no such file', status: 404);
  if (n.text != null) return Uint8List.fromList(utf8.encode(n.text!));
  if (n.asset != null) return (await rootBundle.load(n.asset!)).buffer.asUint8List();
  throw ApiException(ApiFailure.notFound, 'The demo has no content for ${n.name}.', status: 404);
}

Map<String, Object?> _root(DemoRoot r) => {
  'id': r.id,
  'name': baseName(r.node.path),
  'path': r.node.path,
  'key': r.node.key,
  'last_scan': r.scans.last.id,
  'scanned_at': r.scans.last.finishedAt,
  'files': r.node.files,
  'bytes': r.node.size,
};

Map<String, Object?> _entry(DemoNode n) => {
  'kind': n.isDir ? 'dir' : 'file',
  'name': n.name,
  'key': n.key,
  'id': n.id,
  'size': n.size,
  'files': n.isDir ? n.files : null,
  'mtime': n.mtime,
  'sha256': n.isDir ? null : n.hash,
  'copies': _fs.copiesOf(n),
  'versions': n.name == 'notes.md' ? 2 : 1,
  'new_here': false,
  'later': 'kept',
};

Map<String, Object?> _ls(String key, int? scan) {
  final n = _fs.byKey[key];
  if (n == null || !n.isDir) throw ApiException(ApiFailure.refused, "that folder isn't in the index", status: 400);
  final root = _fs.rootOf(n);
  final crumbs = <Map<String, Object?>>[];
  for (DemoNode? c = n; c != null; c = c.parent) {
    crumbs.insert(0, {'key': c.key, 'name': c.parent == null ? c.path : c.name});
  }
  final latest = root.scans.last;
  final at = root.scans.where((s) => s.id == scan).firstOrNull ?? latest;
  return {
    'root_id': root.id,
    'scan': at.id,
    'latest_scan': latest.id,
    'first_scan': root.scans.first.id,
    'scanned_at': at.finishedAt,
    'name': n.parent == null ? n.path : n.name,
    'path': n.path,
    'key': n.key,
    'crumbs': crumbs,
    'files': n.files,
    'bytes': n.size,
    'entries': [for (final c in n.children!) _entry(c)],
    'needs_reindex': false,
  };
}

Map<String, Object?> _file(int id) {
  final n = _fs.byId[id];
  if (n == null) throw ApiException(ApiFailure.refused, 'no such file', status: 400);
  final root = _fs.rootOf(n);
  final latest = root.scans.last;
  final others = [
    for (final c in _fs.byHash[n.hash]!)
      if (!identical(c, n)) {'id': c.id, 'path': c.path, 'key': c.key, 'dir_key': c.parent!.key},
  ];
  final versions = [
    if (n.name == 'notes.md')
      {
        'id': id + 100000,
        'size': n.size - 120,
        'mtime': n.mtime - 86400 * 40,
        'sha256': '${n.hash!.substring(0, 60)}beef',
        'first_scan': root.scans.first.id,
        'last_scan': root.scans.first.id + 1,
        'from': root.scans.first.finishedAt,
        'to': root.scans[1].finishedAt,
        'current': false,
        'readable': false,
      },
    {
      'id': id,
      'size': n.size,
      'mtime': n.mtime,
      'sha256': n.hash,
      'first_scan': n.name == 'notes.md' ? root.scans.first.id + 2 : root.scans.first.id,
      'last_scan': latest.id,
      'from': root.scans.first.finishedAt,
      'to': latest.finishedAt,
      'current': true,
      'readable': true,
    },
  ];
  // the 2014 photos, and the last two of 2015, are on the nextcloud machine too
  final elsewhere = n.path.contains('/2014/') || const {'IMG_3410.JPG', 'IMG_3411.JPG'}.contains(n.name);
  final year = n.path.contains('/2014/') ? '2014' : '2015';
  final remote = elsewhere
      ? [
          {
            'machine_id': 1,
            'machine': 'nextcloud',
            'url': 'ssh://sam@nextcloud',
            'rid': 9000 + id,
            'root_id': 1,
            'path': '/srv/photos/$year/${n.name}',
            'key': keyOfPath('/srv/photos/$year/${n.name}'),
            'dir_key': keyOfPath('/srv/photos/$year'),
            'confirmed_at': latest.finishedAt - 7200,
            'counts': true,
            'why_not': null,
          },
        ]
      : const <Map<String, Object?>>[];
  return {
    'id': id,
    'name': n.name,
    'path': n.path,
    'key': n.key,
    'dir_key': n.parent!.key,
    'size': n.size,
    'mtime': n.mtime,
    'sha256': n.name.endsWith('.img') ? null : n.hash,
    'error': n.name == 'Status.plist' ? 'Permission denied (os error 13)' : null,
    'current': true,
    'versions': versions,
    'copies': others,
    'content_from': '',
    'streams': null,
    if (remote.isNotEmpty) 'remote_copies': remote,
    if (remote.isNotEmpty) 'proof_days': 7,
  };
}

Map<String, Object?> _find(String q, String? within, int limit) {
  final needle = q.trim().toLowerCase();
  if (needle.isEmpty) return {'hits': <Object?>[], 'total': 0, 'gone': 0, 'ms': 0};
  final scope = within == null || within.isEmpty ? null : _fs.byKey[within];
  final hits = <(int, DemoNode)>[];
  for (final n in _fs.byKey.values) {
    if (n.parent == null) continue;
    if (scope != null && !n.path.startsWith('${scope.path}/')) continue;
    final name = n.name.toLowerCase();
    final rank = name == needle || name.split('.').first == needle
        ? 4
        : name.startsWith(needle)
        ? 3
        : RegExp(r'[\s._-]').hasMatch(name) && name.split(RegExp(r'[\s._-]')).any((w) => w.startsWith(needle))
        ? 2
        : name.contains(needle)
        ? 1
        : 0;
    if (rank > 0) hits.add((rank, n));
  }
  hits.sort((a, b) => b.$1.compareTo(a.$1) != 0 ? b.$1.compareTo(a.$1) : b.$2.mtime.compareTo(a.$2.mtime));
  return {
    'hits': [
      for (final (rank, n) in hits.take(limit))
        {
          'kind': n.isDir ? 'dir' : 'file',
          'id': n.id,
          'name': n.name,
          'path': n.path,
          'key': n.key,
          'dir_key': n.parent!.key,
          'size': n.size,
          'mtime': n.mtime,
          'root': _fs.rootOf(n).id,
          'drive': _fs.rootOf(n).node.path,
          'folder': n.parent!.path.substring(_fs.rootOf(n).node.path.length).replaceFirst(RegExp('^/'), ''),
          'rank': rank,
          'gone': false,
          'seen': null,
          'scan': null,
        },
    ],
    'total': hits.length,
    'gone': 0,
    'ms': 3,
  };
}
