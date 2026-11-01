import '../api/api.dart';
import '../api/demo_api.dart';
import '../core/format.dart';
import 'demo_fs.dart';

/// The demo's service queue (GET /api/daemon; POST /api/rescan, /api/job-stop, /api/index), its
/// drive health (GET /api/health, /api/health-machines; POST /api/health-check), and its
/// notifications (GET and POST /api/notifications, /api/notify-settings; POST /api/notify-test).
///
/// The made-up computer: the Seagate is an old USB disk that has started to reallocate sectors
/// ("warning"), the home folder is on an NVMe that's fine, the WD Passport is away (last read
/// healthy, with a couple of cable errors), and an SD card reader gives no health data. Another
/// machine, nextcloud (as in demo_core's copies), has two healthy drives and an old USB disk it
/// hasn't seen for a while.
final Map<String, DemoHandler> demoActivityGets = {
  'daemon': (d, p) async {
    if (_serveOnly(d)) return {'running': false};
    final since = int.tryParse(p['since'] ?? '');
    if (since != null && since >= d.change) await d.nextChange();
    return _snapshot(d);
  },
  'health': (d, p) => _healthOverview(d),
  'health-machines': (d, p) => _machines(d),
  'notifications': (d, p) {
    final h = _health(d);
    final limit = int.tryParse(p['limit'] ?? '') ?? 50;
    return {'notes': h.notes.take(limit).toList(), 'unread': h.unread};
  },
  'notify-settings': (d, p) => _settingsJson(_health(d)),
};

final Map<String, DemoHandler> demoActivityPosts = {
  'rescan': (d, p) {
    final s = _state(d);
    final id = int.tryParse(p['root'] ?? '');
    final roots = DemoFs.instance.roots.where((r) => id == null || r.id == id).toList();
    if (roots.isEmpty) throw ApiException(ApiFailure.refused, 'no such drive in the index', status: 400);
    final jobs = <Object?>[];
    for (final r in roots) {
      final path = r.node.path;
      final already = [?s.current, ...s.queued].any((j) => j['kind'] == 'index' && j['path'] == path);
      if (already) continue;
      final job = _job(s, 'index', path, 'queued', reason: 'requested');
      s.queued.add(job);
      jobs.add(job['id']);
    }
    d.changed();
    return {'ok': true, 'jobs': jobs};
  },
  'job-stop': (d, p) {
    final s = _state(d);
    final id = int.tryParse(p['id'] ?? '');
    final now = nowSeconds();
    final i = s.queued.indexWhere((j) => j['id'] == id);
    if (i >= 0) {
      final j = s.queued.removeAt(i);
      _finish(s, {...j, 'state': 'stopped', 'summary': 'taken off the queue', 'finished_at': now});
      d.changed();
      return {'ok': true, 'state': 'taken off the queue'};
    }
    if (s.side?['id'] == id) {
      _finish(s, {...s.side!, 'state': 'stopped', 'summary': 'stopped', 'finished_at': now, 'counts': null});
      s.side = null;
      d.changed();
      return {'ok': true, 'state': 'stopping'};
    }
    if (s.current?['id'] == id && s.current?['stoppable'] == true) {
      _finish(s, {...s.current!, 'state': 'stopped', 'summary': 'stopped', 'finished_at': now, 'counts': null});
      s.current = null;
      _next(s);
      d.changed();
      return {'ok': true, 'state': 'stopping'};
    }
    throw ApiException(ApiFailure.conflict, 'job $id isn\'t waiting, and can\'t be stopped while it runs', status: 409);
  },
  'index': (d, p) {
    final s = _state(d);
    final path = p['path'];
    if (path == null || path.isEmpty) throw ApiException(ApiFailure.refused, 'missing path', status: 400);
    if (!path.startsWith('/')) throw ApiException(ApiFailure.refused, '$path isn\'t a folder here', status: 400);
    final job = _job(s, 'index', path, 'queued', reason: 'requested');
    s.queued.add(job);
    d.changed();
    return {'ok': true, 'job': job['id']};
  },
  'health-check': (d, p) {
    final h = _health(d);
    h.checkedAt = nowSeconds();
    h.change++;
    d.changed();
    return {'ok': true};
  },
  'notifications': (d, p) {
    final h = _health(d);
    final which = p['read'];
    if (which == null) throw ApiException(ApiFailure.refused, 'read=all or read=<id>', status: 400);
    final id = which == 'all' ? null : int.tryParse(which);
    if (which != 'all' && id == null) throw ApiException(ApiFailure.refused, 'bad id', status: 400);
    var marked = 0;
    for (final n in h.notes) {
      if ((id == null || n['id'] == id) && n['read'] != true) {
        n['read'] = true;
        marked++;
      }
    }
    h.change++;
    d.changed();
    return {'ok': true, 'marked': marked, 'unread': h.unread};
  },
  'notify-settings': (d, p) {
    final h = _health(d);
    for (final k in ['desktop', 'health', 'drift', 'jobs']) {
      final v = p[k];
      if (v == null) continue;
      if (v != '0' && v != '1') throw ApiException(ApiFailure.refused, '$k must be 0 or 1, not "$v"', status: 400);
      h.settings[k] = v == '1';
    }
    return _settingsJson(h);
  },
  'notify-test': (d, p) => {'ok': true},
};

/// Moves the running job along a little (the demo's timer calls this every two seconds).
void demoActivityTick(DemoBackDownApi d) {
  final s = _state(d);
  final c = s.current;
  if (c == null) {
    _next(s);
    return;
  }
  final now = nowSeconds();
  final raw = c['counts'];
  if (raw is! Map) {
    // a job without numbers (recognition): a few steps, then done
    final step = ((c['steps'] as int?) ?? 0) + 1;
    if (step < 6) {
      s.current = {...c, 'steps': step, 'progress': 'looked at ${step * 14} of 84 folders'};
      return;
    }
    _finish(s, {...c, 'state': 'done', 'finished_at': now, 'summary': 'recognised 9 things', 'progress': ''});
    s.current = null;
    _next(s);
    return;
  }
  final counts = Map<String, Object?>.from(raw);
  final total = counts['total_bytes']! as int;
  final done = ((counts['bytes']! as int) + total ~/ 90).clamp(0, total);
  counts['bytes'] = done;
  counts['files'] = ((counts['total_files']! as int) * done / total).round();
  if (done >= total) {
    _finish(s, {
      ...c,
      'state': 'done',
      'finished_at': now,
      'summary': c['kind'] == 'index'
          ? '${count(counts['files'] as int)} files, ${bytes(total)}: nothing changed'
          : 'hashed ${count(counts['files'] as int)} files',
      'counts': null,
      'progress': '',
    });
    s.current = null;
    _next(s);
  } else {
    final what = c['kind'] == 'index' ? 'scanned' : 'hashed';
    s.current = {...c, 'counts': counts, 'progress': '${percent(done / total)} of ${bytes(total)} $what'};
  }
}

/// A sync of the nextcloud machine running beside the main job, as the side lane does (for tests
/// and screenshots).
void demoStartSideJob(DemoBackDownApi d) {
  final s = _state(d);
  s.side = _job(
    s,
    'remote-sync',
    'nextcloud',
    'running',
    reason: 'requested',
    startedAt: nowSeconds() - 140,
    progress: 'nextcloud: 1,204 files new or changed there',
    counts: {'stage': 'syncing', 'files': 0, 'total_files': 0, 'bytes': 0, 'total_bytes': 0},
  );
  d.changed();
}

/// Makes the demo answer as `backdown serve` does: no queue, drive health read when asked.
void demoServeOnly(DemoBackDownApi d) => d.state['activity_serve'] = true;

bool _serveOnly(DemoBackDownApi d) => d.state['activity_serve'] == true;

// ---------- jobs

class _DemoJobs {
  int nextId = 40;
  Map<String, Object?>? current;
  Map<String, Object?>? side;
  final List<Map<String, Object?>> queued = [];
  final List<Map<String, Object?>> recent = [];
}

_DemoJobs _state(DemoBackDownApi d) => (d.state['jobs'] ??= _seed()) as _DemoJobs;

/// A job's end: newest first in Recent, which keeps 30, as the service does.
void _finish(_DemoJobs s, Map<String, Object?> job) {
  s.recent.insert(0, job);
  if (s.recent.length > 30) s.recent.removeRange(30, s.recent.length);
}

/// The next job starts; with nothing waiting, hashing starts again, so the demo always has
/// something running.
void _next(_DemoJobs s) {
  final now = nowSeconds();
  if (s.queued.isNotEmpty) {
    final q = s.queued.removeAt(0);
    final root = DemoFs.instance.roots.where((r) => r.node.path == q['path']).firstOrNull;
    s.current = {
      ...q,
      'state': 'running',
      'started_at': now,
      if (q['kind'] == 'index' && root != null)
        'counts': {
          'stage': 'walk',
          'files': 0,
          'total_files': root.node.files,
          'bytes': 0,
          'total_bytes': root.node.size,
        },
    };
    return;
  }
  final seagate = DemoFs.instance.roots.first.node;
  s.current = _job(
    s,
    'hash',
    seagate.path,
    'running',
    reason: 'after-scan',
    startedAt: now,
    counts: {'stage': 'full', 'files': 0, 'total_files': 52, 'bytes': 0, 'total_bytes': seagate.size},
  );
}

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
  final h = _health(d);
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
    'side': s.side,
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
          'last_how': r.id == 3
              ? 'full walk in 4 s: Linux keeps no change log for drives'
              : 'full walk in 2 min 10 s: Linux keeps no change log for drives',
        },
    ],
    'health': {
      'change': h.change,
      'checked_at': h.checkedAt,
      'next_at': h.checkedAt + _every,
      'running': false,
      'unread': h.unread,
      'worst': _worst(_drives(h)),
    },
  };
}

// ---------- drive health

const _every = 6 * 3600;

class _DemoHealth {
  _DemoHealth(int now) : checkedAt = now - 3000, seededAt = now {
    notes.addAll([
      _note(
        14,
        checkedAt,
        'health',
        'warning',
        'ST2000DM008-2FR102 is getting worse',
        '6 more sectors have gone bad and been replaced by spares (now 8). Make sure what\'s on it has another copy.',
        drive: _seagateId,
        read: false,
      ),
      _note(
        13,
        now - 5380,
        'job',
        'warning',
        'A scheduled sync of the machine nextcloud failed',
        'Nextcloud: ssh: connect to host nextcloud port 22: Connection timed out',
        read: false,
      ),
      _note(
        9,
        now - 9 * 86400,
        'health',
        'warning',
        'ST2000DM008-2FR102 needs watching',
        '2 sectors had gone bad and have been replaced by spares (reallocated). Make sure what\'s on it has another '
            'copy.',
        drive: _seagateId,
      ),
      _note(
        6,
        now - 23 * 86400,
        'health',
        'warning',
        'Transfer errors on WDC WD20SDRW-11VUUS0',
        '2 more transfer errors between the drive and the computer (now 2): check or replace the cable or '
            'enclosure; the drive itself may be fine.',
        drive: _wdId,
        shown: false,
      ),
    ]);
  }

  /// When the drives were last read (a "Check now" reads them again).
  int checkedAt;
  final int seededAt;
  int change = 3;
  final List<Map<String, Object?>> notes = [];
  final Map<String, bool> settings = {'desktop': true, 'health': true, 'drift': true, 'jobs': true};

  int get unread => notes.where((n) => n['read'] != true).length;
}

_DemoHealth _health(DemoBackDownApi d) => (d.state['health'] ??= _DemoHealth(nowSeconds())) as _DemoHealth;

Map<String, Object?> _note(
  int id,
  int at,
  String kind,
  String level,
  String title,
  String body, {
  String? drive,
  bool read = true,
  bool shown = true,
}) => {
  'id': id,
  'at': at,
  'kind': kind,
  'key': '$kind:$id',
  'level': level,
  'title': title,
  'body': body,
  'drive': drive,
  'shown': shown,
  'read': read,
};

Map<String, Object?> _settingsJson(_DemoHealth h) => {...h.settings, 'available': true};

const _seagateId = 'sn:ZFL0A7Q2';
const _wdId = 'sn:WXB1A79H4K2C';

int _rank(Object? v) => switch (v) {
  'failing' => 3,
  'warning' => 2,
  'ok' => 1,
  _ => 0,
};

String _worst(List<Map<String, Object?>> drives) {
  var w = 'unknown';
  for (final d in drives) {
    if (d['present'] == true && _rank(d['verdict']) > _rank(w)) w = d['verdict']! as String;
  }
  return w;
}

/// [n] readings over [days] days, the last at [last]; [f] gives each one's values.
List<Map<String, Object?>> _series(int last, int days, int n, Map<String, Object?> Function(int i) f) => [
  for (var i = 0; i < n; i++) {'at': last - (n - 1 - i) * (days * 86400 ~/ n), ...f(i)},
];

List<Map<String, Object?>> _drives(_DemoHealth h) {
  final t = h.checkedAt, now = h.seededAt;
  final seagate = _series(t, 60, 12, (i) {
    final bad = i < 10 ? 0 : (i == 10 ? 2 : 8);
    return {'temp': 36 + (i * 5) % 9, 'bad': bad, 'crc': 0, 'verdict': bad > 0 ? 'warning' : 'ok'};
  });
  final nvme = _series(t, 60, 12, (i) => {'temp': 33 + (i * 3) % 8, 'wear': i < 7 ? 3 : 4, 'media': 0});
  final wdLast = now - 2 * 86400;
  final wd = _series(wdLast, 30, 4, (i) => {'temp': 32 + i % 3, 'bad': 0, 'crc': i == 0 ? 0 : 2, 'verdict': 'ok'});
  return [
    {
      'id': _seagateId,
      'present': true,
      'health': {
        'id': _seagateId,
        'device': '/dev/sdb',
        'name': 'sdb',
        'model': 'ST2000DM008-2FR102',
        'serial': 'ZFL0A7Q2',
        'transport': 'usb',
        'rotational': true,
        'size': 2000398934016,
        'protocol': 'ata',
        'reported': 'ok',
        'reported_as': 'PASSED',
        'temperature': 42,
        'power_on_hours': 31244,
        'power_cycles': 1822,
        'reallocated': 8,
        'pending': 0,
        'uncorrectable': 0,
        'crc_errors': 0,
        'source': 'udisks2 + smartctl',
        'mounts': [DemoFs.instance.roots[0].node.path],
        'read_at': t,
      },
      'verdict': 'warning',
      'findings': [
        {
          'level': 'warning',
          'code': 'reallocated',
          'text': '8 sectors had gone bad and have been replaced by spares (reallocated)',
        },
      ],
      'changes': [
        {
          'level': 'warning',
          'code': 'verdict',
          'text': 'was ok, now warning: 2 sectors had gone bad and have been replaced by spares (reallocated)',
          'at': seagate[10]['at'],
          'key': 'verdict:warning',
        },
        {
          'level': 'warning',
          'code': 'reallocated',
          'text': '6 more sectors have gone bad and been replaced by spares (now 8)',
          'at': t,
          'key': 'reallocated:8',
        },
      ],
      'trend': ['+8 reallocated', '+1404 h on'],
      'first_at': seagate.first['at'],
      'readings': 18,
      'series': seagate,
    },
    {
      'id': 'sn:S64ANS0T512345K',
      'present': true,
      'health': {
        'id': 'sn:S64ANS0T512345K',
        'device': '/dev/nvme0n1',
        'name': 'nvme0n1',
        'model': 'Samsung SSD 980 1TB',
        'serial': 'S64ANS0T512345K',
        'transport': 'nvme',
        'rotational': false,
        'size': 1000204886016,
        'protocol': 'nvme',
        'temperature': 39,
        'power_on_hours': 6120,
        'power_cycles': 1411,
        'critical_warning': 0,
        'percentage_used': 4,
        'available_spare': 100,
        'spare_threshold': 10,
        'media_errors': 0,
        'unsafe_shutdowns': 57,
        'source': 'udisks2',
        'mounts': ['/', '/home'],
        'read_at': t,
      },
      'verdict': 'ok',
      'findings': <Object?>[],
      'changes': <Object?>[],
      'trend': ['wear +1%', '+412 h on'],
      'first_at': nvme.first['at'],
      'readings': 31,
      'series': [
        for (final p in nvme) {...p, 'verdict': 'ok'},
      ],
    },
    {
      'id': '',
      'present': true,
      'health': {
        'id': '',
        'device': '/dev/sdd',
        'name': 'sdd',
        'model': 'Generic- SD/MMC',
        'transport': 'usb',
        'size': 63864569856,
        'source': 'none',
        'note':
            'its USB enclosure doesn\'t pass health data on (as administrator, sudo smartctl -d sat -a /dev/sdd '
            'sometimes gets through)',
        'mounts': ['/media/sam/CAMERA'],
        'read_at': t,
      },
      'verdict': 'unknown',
      'findings': <Object?>[],
      'changes': <Object?>[],
      'trend': <Object?>[],
      'first_at': null,
      'readings': 0,
      'series': <Object?>[],
    },
    {
      'id': _wdId,
      'present': false,
      'health': {
        'id': _wdId,
        'device': '/dev/sdc',
        'name': 'sdc',
        'model': 'WDC WD20SDRW-11VUUS0',
        'serial': 'WXB1A79H4K2C',
        'transport': 'usb',
        'rotational': true,
        'size': 2000365289472,
        'protocol': 'ata',
        'reported': 'ok',
        'reported_as': 'PASSED',
        'temperature': 34,
        'power_on_hours': 5210,
        'power_cycles': 980,
        'reallocated': 0,
        'pending': 0,
        'uncorrectable': 0,
        'crc_errors': 2,
        'source': 'udisks2 + smartctl',
        'mounts': [DemoFs.instance.roots[1].node.path],
        'read_at': wdLast,
      },
      'verdict': 'ok',
      'findings': [
        {
          'level': 'ok',
          'code': 'crc',
          'text':
              '2 transfer errors between it and the computer so far (a cable or enclosure problem, if it keeps rising)',
        },
      ],
      'changes': [
        {
          'level': 'warning',
          'code': 'crc',
          'text':
              '2 more transfer errors between the drive and the computer (now 2): check or replace the cable or '
              'enclosure; the drive itself may be fine',
          'at': wd[1]['at'],
          'key': 'crc:2',
        },
      ],
      'trend': ['+2 CRC errors', '+36 h on'],
      'first_at': wd.first['at'],
      'readings': 4,
      'series': wd,
    },
  ];
}

Map<String, Object?> _healthOverview(DemoBackDownApi d) {
  final h = _health(d);
  if (_serveOnly(d)) {
    return {
      'drives': _drives(h),
      'notes': <Object?>[],
      'checked_at': nowSeconds(),
      'daemon': false,
      'every_secs': _every,
    };
  }
  return {
    'drives': _drives(h),
    'notes': <Object?>[],
    'error': null,
    'checked_at': h.checkedAt,
    'next_at': h.checkedAt + _every,
    'running': false,
    'every_secs': _every,
    'daemon': true,
  };
}

/// The nextcloud machine's drives, as its own BackDown read them, fetched with its last sync (a
/// day ago: the sync since then failed, as Recent says).
Map<String, Object?> _machines(DemoBackDownApi d) {
  final now = _health(d).seededAt;
  final synced = now - 26 * 3600;
  Map<String, Object?> drive(
    String id,
    Map<String, Object?> health, {
    String verdict = 'ok',
    int? readAt,
    bool current = true,
    List<Map<String, Object?>> findings = const [],
    List<String> trend = const [],
    int readings = 9,
  }) => {
    'id': id,
    'health': {'id': id, ...health, 'read_at': readAt ?? synced - 600},
    'verdict': verdict,
    'findings': findings,
    'changes': <Object?>[],
    'trend': trend,
    'first_at': now - 40 * 86400,
    'readings': readings,
    'machine': 'nextcloud',
    'fetched_at': synced,
    'current': current,
  };
  return {
    'machines': [
      {
        'id': 1,
        'name': 'nextcloud',
        'url': 'ssh://sam@nextcloud',
        'last_sync': synced,
        'last_try': now - 5380,
        'last_error': 'ssh: connect to host nextcloud port 22: Connection timed out',
        'version': '1.0.0-beta.4',
        'health': {'state': 'ok', 'asked_at': synced, 'ok_at': synced, 'error': null, 'note': null},
        'worst': 'ok',
        'drives': [
          drive(
            'sn:WDWCC7K4DEMO01',
            {
              'device': '/dev/sda',
              'name': 'sda',
              'model': 'WDC WD40EFRX-68N32N0',
              'serial': 'WD-WCC7K4DEMO01',
              'transport': 'sata',
              'rotational': true,
              'size': 4000787030016,
              'protocol': 'ata',
              'reported': 'ok',
              'reported_as': 'PASSED',
              'temperature': 37,
              'power_on_hours': 28412,
              'power_cycles': 61,
              'reallocated': 0,
              'pending': 0,
              'uncorrectable': 0,
              'crc_errors': 0,
              'source': 'udisks2',
              'mounts': ['/srv'],
            },
            trend: ['+620 h on'],
          ),
          drive(
            'sn:S3Z9NB0KDEMO',
            {
              'device': '/dev/sdb',
              'name': 'sdb',
              'model': 'Samsung SSD 860 EVO 250GB',
              'serial': 'S3Z9NB0KDEMO',
              'transport': 'sata',
              'rotational': false,
              'size': 250059350016,
              'protocol': 'ata',
              'reported': 'ok',
              'reported_as': 'PASSED',
              'temperature': 31,
              'power_on_hours': 30120,
              'reallocated': 0,
              'pending': 0,
              'uncorrectable': 0,
              'source': 'udisks2',
              'mounts': ['/'],
            },
            trend: ['+620 h on'],
          ),
          drive(
            'sn:Y8K2T1DEMO',
            {
              'device': '/dev/sdc',
              'name': 'sdc',
              'model': 'TOSHIBA MQ04UBF100',
              'serial': 'Y8K2T1DEMO',
              'transport': 'usb',
              'rotational': true,
              'size': 1000204886016,
              'protocol': 'ata',
              'temperature': 35,
              'power_on_hours': 9930,
              'reallocated': 0,
              'pending': 3,
              'uncorrectable': 0,
              'source': 'smartctl',
              'mounts': ['/mnt/old-backup'],
            },
            verdict: 'warning',
            readAt: now - 20 * 86400,
            current: false,
            readings: 3,
            findings: [
              {
                'level': 'warning',
                'code': 'pending',
                'text': '3 sectors can\'t be read and are waiting to be replaced (pending)',
              },
            ],
            trend: ['+3 pending'],
          ),
        ],
        'notes': [
          {'level': 'warning', 'title': 'TOSHIBA MQ04UBF100 needs watching', 'at': now - 21 * 86400},
        ],
      },
    ],
    'worst': 'ok',
    'stale_days': 7,
  };
}
