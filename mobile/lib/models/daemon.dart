import '../api/json.dart';

/// Where a running job is. For a scan the totals are the drive's previous scan (an estimate, so it
/// can pass 100%); for hashing they're exact.
class JobCounts {
  JobCounts.fromJson(Json j)
    : stage = j.str('stage'),
      files = j.integer('files'),
      totalFiles = j.integer('total_files'),
      bytes = j.integer('bytes'),
      totalBytes = j.integer('total_bytes');
  final String stage;
  final int files, totalFiles, bytes, totalBytes;
}

/// A job in the service's queue: a scan, hashing, recognition, a copy, a machine's sync…
class Job {
  Job.fromJson(Json j)
    : id = j.integer('id'),
      kind = j.str('kind'),
      path = j.strOrNull('path'),
      state = j.str('state'),
      stoppable = j.flag('stoppable'),
      progress = j.str('progress'),
      counts = j.objOrNull('counts') == null ? null : JobCounts.fromJson(j.obj('counts')),
      summary = j.str('summary'),
      error = j.strOrNull('error'),
      auto = j.flag('auto'),
      reason = j.str('reason'),
      queuedAt = j.integer('queued_at'),
      startedAt = j.intOrNull('started_at'),
      finishedAt = j.intOrNull('finished_at');

  final int id;

  /// "index", "hash", "entities", "remote-sync", "apply", "wipe", "rescue", "dedup", "mail-merge",
  /// "mirror-sync", "mirror-verify", "attest", "indexes", …
  final String kind;
  final String? path;

  /// "queued", "running", "done", "failed" or "stopped".
  final String state;
  final bool stoppable;
  final String progress;
  final JobCounts? counts;
  final String summary;
  final String? error;

  /// Queued by the scheduler rather than asked for.
  final bool auto;

  /// Why it was queued: "requested", "scheduled", "returned", "after-scan", …
  final String reason;
  final int queuedAt;
  final int? startedAt, finishedAt;

  bool get running => state == 'running';
  bool get failed => state == 'failed';

  /// How far it is, 0..1, or null when there's nothing to compare with (a drive's first scan). The
  /// web UI's rule: rescues and hashing by bytes (hashing's quick samples by files), scans by
  /// files against the previous scan, capped below 100%.
  double? get fraction {
    final c = counts;
    if (c == null) return null;
    if (kind == 'rescue') return c.totalBytes > 0 ? c.bytes / c.totalBytes : null;
    if (kind == 'hash') {
      if (c.stage != 'sample' && c.totalBytes > 0) return c.bytes / c.totalBytes;
      return c.totalFiles > 0 ? c.files / c.totalFiles : null;
    }
    return c.totalFiles > 0 ? (c.files / c.totalFiles).clamp(0, 0.99).toDouble() : null;
  }

  /// Seconds left, from the pace so far, when there's enough to go on.
  int? secondsLeft(int now) {
    final p = fraction, start = startedAt;
    if (p == null || start == null || p < 0.02 || now - start < 15) return null;
    return ((now - start) * (1 - p) / p).round();
  }
}

/// A drive the service scans on its own, and when it will next.
class DaemonDrive {
  DaemonDrive.fromJson(Json j)
    : path = j.str('path'),
      present = j.flag('present'),
      lastScanAt = j.intOrNull('last_scan_at'),
      files = j.integer('files'),
      bytes = j.integer('bytes'),
      nextAt = j.intOrNull('next_at'),
      state = j.str('state'),
      lastMethod = j.strOrNull('last_method'),
      lastHow = j.strOrNull('last_how');

  final String path;
  final bool present;
  final int? lastScanAt;
  final int files, bytes;
  final int? nextAt;

  /// "scanning", "queued", "away", "due", "on-request" or "scheduled".
  final String state;

  /// "walk", "journal", "mft", …, and how the last scan went in words.
  final String? lastMethod, lastHow;
}

/// The service's drive-health round, in brief.
class DaemonHealth {
  DaemonHealth.fromJson(Json j)
    : change = j.integer('change'),
      checkedAt = j.intOrNull('checked_at'),
      nextAt = j.intOrNull('next_at'),
      running = j.flag('running'),
      unread = j.integer('unread'),
      worst = j.str('worst', 'unknown');
  final int change;
  final int? checkedAt, nextAt;
  final bool running;

  /// Notifications not yet marked read.
  final int unread;

  /// The worst verdict across this machine's drives: "ok", "watch", "warn", "fail" or "unknown".
  final String worst;
}

/// What the service is doing (GET /api/daemon). `running` is false for `backdown serve`, which has
/// no queue.
class DaemonSnapshot {
  DaemonSnapshot.fromJson(Json j)
    : running = j.flag('running'),
      version = j.strOrNull('version'),
      change = j.integer('change'),
      now = j.intOrNull('now'),
      pid = j.intOrNull('pid'),
      db = j.strOrNull('db'),
      startedAt = j.intOrNull('started_at'),
      checkedAt = j.intOrNull('checked_at'),
      rescanEverySecs = j.intOrNull('rescan_every_secs'),
      current = j.objOrNull('current') == null ? null : Job.fromJson(j.obj('current')),
      side = j.objOrNull('side') == null ? null : Job.fromJson(j.obj('side')),
      queued = [for (final q in j.list('queued')) Job.fromJson(q)],
      recent = [for (final r in j.list('recent')) Job.fromJson(r)],
      drives = [for (final d in j.list('drives')) DaemonDrive.fromJson(d)],
      health = j.objOrNull('health') == null ? null : DaemonHealth.fromJson(j.obj('health'));

  final bool running;
  final String? version;

  /// Goes up with every change; the long poll waits for it to pass the one it has.
  final int change;

  /// The service's clock, for working out this phone's skew.
  final int? now;
  final int? pid;
  final String? db;
  final int? startedAt, checkedAt, rescanEverySecs;
  final Job? current;

  /// A rescue running alongside the main job.
  final Job? side;
  final List<Job> queued;
  final List<Job> recent;
  final List<DaemonDrive> drives;
  final DaemonHealth? health;

  bool get busy => current != null || side != null;
}
