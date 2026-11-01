import '../api/demo_api.dart';
import '../core/format.dart';
import 'demo_fs.dart';

/// The demo's service queue (GET /api/daemon, POST /api/rescan and /api/job-stop), and, with the
/// Activity and Health screens, its drive health and notifications.
final Map<String, DemoHandler> demoActivityGets = {
  'daemon': (d, p) async {
    final since = int.tryParse(p['since'] ?? '');
    if (since != null && since >= d.change) await d.nextChange();
    return _snapshot(d);
  },
};

final Map<String, DemoHandler> demoActivityPosts = {
  'rescan': (d, p) {
    final s = _state(d);
    final root = DemoFs.instance.roots.where((r) => r.id == int.tryParse(p['root'] ?? '')).firstOrNull;
    final path = root?.node.path ?? DemoFs.instance.roots.first.node.path;
    s.queued.add(_job(s, 'index', path, 'queued', reason: 'requested'));
    d.changed();
    return {'queued': true, 'path': path};
  },
  'job-stop': (d, p) {
    final s = _state(d);
    final id = int.tryParse(p['id'] ?? '');
    if (s.current?['id'] == id) {
      s.recent.insert(0, {...s.current!, 'state': 'stopped', 'finished_at': nowSeconds()});
      s.current = s.queued.isEmpty ? null : {...s.queued.removeAt(0), 'state': 'running', 'started_at': nowSeconds()};
    } else {
      s.queued.removeWhere((j) => j['id'] == id);
    }
    d.changed();
    return {'stopped': id};
  },
};

/// Moves the running job along a little (the demo's timer calls this every two seconds).
void demoActivityTick(DemoBackDownApi d) {
  final s = _state(d);
  final c = s.current;
  if (c == null) return;
  final counts = Map<String, Object?>.from(c['counts']! as Map);
  final total = counts['total_bytes']! as int;
  final done = ((counts['bytes']! as int) + total ~/ 90).clamp(0, total);
  counts['bytes'] = done;
  counts['files'] = ((counts['total_files']! as int) * done / total).round();
  if (done >= total) {
    s.recent.insert(0, {
      ...c,
      'state': 'done',
      'finished_at': nowSeconds(),
      'summary': 'hashed ${count(counts['files'] as int)} files',
      'counts': null,
    });
    // start again from the beginning, so the demo always has something running
    counts['bytes'] = 0;
    counts['files'] = 0;
    s.current = {...c, 'started_at': nowSeconds(), 'counts': counts};
  } else {
    s.current = {...c, 'counts': counts, 'progress': '${percent(done / total)} of ${bytes(total)} hashed'};
  }
}

class _DemoJobs {
  int nextId = 40;
  Map<String, Object?>? current;
  final List<Map<String, Object?>> queued = [];
  final List<Map<String, Object?>> recent = [];
}

_DemoJobs _state(DemoBackDownApi d) => (d.state['jobs'] ??= _seed()) as _DemoJobs;

Map<String, Object?> _job(
  _DemoJobs s,
  String kind,
  String? path,
  String state, {
  String reason = 'scheduled',
  int? startedAt,
  int? finishedAt,
  String summary = '',
  String? error,
  Map<String, Object?>? counts,
  String progress = '',
}) {
  final now = nowSeconds();
  return {
    'id': s.nextId++,
    'kind': kind,
    'path': path,
    'state': state,
    'stoppable': kind == 'index' || kind == 'hash' || kind == 'remote-sync',
    'progress': progress,
    'counts': counts,
    'summary': summary,
    'error': error,
    'auto': reason != 'requested',
    'reason': reason,
    'queued_at': (startedAt ?? now) - 30,
    'started_at': startedAt,
    'finished_at': finishedAt,
  };
}

_DemoJobs _seed() {
  final s = _DemoJobs();
  final now = nowSeconds();
  final fs = DemoFs.instance;
  final seagate = fs.roots[0].node, wd = fs.roots[1].node, home = fs.roots[2].node;
  s.current = _job(
    s,
    'hash',
    seagate.path,
    'running',
    reason: 'after-scan',
    startedAt: now - 1260,
    progress: '42% of 128.4 GB hashed',
    counts: {
      'stage': 'full',
      'files': 21,
      'total_files': 52,
      'bytes': (seagate.size * 0.42).round(),
      'total_bytes': seagate.size,
    },
  );
  s.queued.add(_job(s, 'entities', seagate.path, 'queued', reason: 'after-hash'));
  s.recent.addAll([
    _job(
      s,
      'index',
      seagate.path,
      'done',
      startedAt: now - 1900,
      finishedAt: now - 1270,
      summary: '${count(seagate.files)} files, ${bytes(seagate.size)}: 3 new, 1 changed',
    ),
    _job(
      s,
      'remote-sync',
      'nextcloud',
      'failed',
      startedAt: now - 5400,
      finishedAt: now - 5380,
      error: 'nextcloud: ssh: connect to host nextcloud port 22: Connection timed out',
    ),
    _job(
      s,
      'index',
      home.path,
      'done',
      startedAt: now - 86000,
      finishedAt: now - 85940,
      summary: '${count(home.files)} files, ${bytes(home.size)}: nothing changed',
    ),
    _job(
      s,
      'index',
      wd.path,
      'done',
      startedAt: now - 90000,
      finishedAt: now - 89100,
      summary: '${count(wd.files)} files, ${bytes(wd.size)}',
    ),
  ]);
  return s;
}

Map<String, Object?> _snapshot(DemoBackDownApi d) {
  final s = _state(d);
  final now = nowSeconds();
  final fs = DemoFs.instance;
  String driveState(DemoRoot r) {
    if (s.current?['kind'] == 'index' && s.current?['path'] == r.node.path) return 'scanning';
    if (s.queued.any((j) => j['kind'] == 'index' && j['path'] == r.node.path)) return 'queued';
    if (r.id == 2) return 'away';
    return 'scheduled';
  }

  return {
    'running': true,
    'version': '1.0.0-beta.4',
    'change': d.change,
    'now': now,
    'pid': 4242,
    'db': '/home/sam/.local/share/backdown/index.db',
    'started_at': now - 3 * 86400,
    'checked_at': now - 600,
    'rescan_every_secs': 86400,
    'current': s.current,
    'side': null,
    'queued': s.queued,
    'recent': s.recent,
    'drives': [
      for (final r in fs.roots)
        {
          'path': r.node.path,
          'present': r.id != 2,
          'last_scan_at': r.scans.last.finishedAt,
          'files': r.node.files,
          'bytes': r.node.size,
          'next_at': r.scans.last.finishedAt + 86400,
          'state': driveState(r),
          'last_method': 'walk',
          'last_how': 'full walk in 2 min 10 s: Linux keeps no change log for drives',
        },
    ],
    'health': {
      'change': 3,
      'checked_at': now - 3000,
      'next_at': now + 18600,
      'running': false,
      'unread': 1,
      'worst': 'watch',
    },
  };
}
