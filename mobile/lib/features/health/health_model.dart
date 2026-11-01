import '../../api/json.dart';

/// One reading of one physical drive (backdown_core::health::Health): what it says about itself.
class HealthReading {
  HealthReading.fromJson(Json j)
    : id = j.str('id'),
      device = j.str('device'),
      name = j.str('name'),
      model = j.strOrNull('model'),
      serial = j.strOrNull('serial'),
      wwn = j.strOrNull('wwn'),
      transport = j.strOrNull('transport'),
      rotational = j.flagOrNull('rotational'),
      size = j.intOrNull('size'),
      protocol = j.strOrNull('protocol'),
      reported = j.strOrNull('reported'),
      reportedAs = j.strOrNull('reported_as'),
      temperature = j.intOrNull('temperature'),
      powerOnHours = j.intOrNull('power_on_hours'),
      powerCycles = j.intOrNull('power_cycles'),
      reallocated = j.intOrNull('reallocated'),
      pending = j.intOrNull('pending'),
      uncorrectable = j.intOrNull('uncorrectable'),
      badSectors = j.intOrNull('bad_sectors'),
      crcErrors = j.intOrNull('crc_errors'),
      criticalWarning = j.intOrNull('critical_warning'),
      percentageUsed = j.intOrNull('percentage_used'),
      availableSpare = j.intOrNull('available_spare'),
      spareThreshold = j.intOrNull('spare_threshold'),
      mediaErrors = j.intOrNull('media_errors'),
      unsafeShutdowns = j.intOrNull('unsafe_shutdowns'),
      failingAttributes = j.strs('failing_attributes'),
      source = j.str('source'),
      note = j.strOrNull('note'),
      mounts = j.strs('mounts'),
      readAt = j.integer('read_at');

  /// `sn:<serial>`, else `wwn:<wwn>`; empty when the drive has neither.
  final String id;

  /// `/dev/sda`, `/dev/disk4`, `\\.\PHYSICALDRIVE1`; and its short name (`sda`).
  final String device, name;
  final String? model, serial, wwn, transport;
  final bool? rotational;
  final int? size;

  /// `ata`, `nvme` or `scsi`.
  final String? protocol;

  /// The drive's own verdict, and in its own words (PASSED, FAILED!).
  final String? reported, reportedAs;
  final int? temperature, powerOnHours, powerCycles;
  final int? reallocated, pending, uncorrectable, badSectors, crcErrors;
  final int? criticalWarning, percentageUsed, availableSpare, spareThreshold, mediaErrors, unsafeShutdowns;
  final List<String> failingAttributes;

  /// Where the numbers came from: udisks2, smartctl, diskutil, Windows (joined by " + ").
  final String source;

  /// Why some or all of it couldn't be read, and what would read it.
  final String? note;

  /// Mount points of its volumes when it was read: which drive a folder is on.
  final List<String> mounts;
  final int readAt;

  /// A readable name: the model, else the device.
  String get label {
    final m = model?.trim() ?? '';
    if (m.isNotEmpty) return m;
    return device.isNotEmpty ? device : 'A drive';
  }
}

/// Something a verdict rests on ("8 sectors had gone bad…").
class HealthFinding {
  HealthFinding.fromJson(Json j) : level = j.str('level', 'unknown'), code = j.str('code'), text = j.str('text');
  final String level, code, text;
}

/// Something that got worse between two readings.
class HealthChange {
  HealthChange.fromJson(Json j)
    : level = j.str('level', 'warning'),
      code = j.str('code'),
      text = j.str('text'),
      at = j.integer('at');
  final String level, code, text;
  final int at;
}

/// One reading reduced to what a trend line shows.
class HealthPoint {
  HealthPoint.fromJson(Json j)
    : at = j.integer('at'),
      temp = j.intOrNull('temp'),
      bad = j.intOrNull('bad'),
      wear = j.intOrNull('wear'),
      crc = j.intOrNull('crc'),
      media = j.intOrNull('media');
  final int at;
  final int? temp, bad, wear, crc, media;

  int? value(String key) => switch (key) {
    'temp' => temp,
    'bad' => bad,
    'wear' => wear,
    'crc' => crc,
    'media' => media,
    _ => null,
  };
}

/// A drive as people see it (backdown_core::health::Summary): its latest reading, what it means,
/// what changed, and a series for trend lines. Another machine's drives also say which machine,
/// when they were fetched and whether that machine read them lately.
class DriveHealth {
  DriveHealth.fromJson(Json j, {String? machine})
    : id = j.str('id'),
      present = j.flag('present'),
      health = HealthReading.fromJson(j.obj('health')),
      verdict = j.str('verdict', 'unknown'),
      findings = [for (final f in j.list('findings')) HealthFinding.fromJson(f)],
      changes = [for (final c in j.list('changes')) HealthChange.fromJson(c)],
      trend = j.strs('trend'),
      firstAt = j.intOrNull('first_at'),
      readings = j.integer('readings'),
      series = [for (final p in j.list('series')) HealthPoint.fromJson(p)],
      machine = machine ?? j.strOrNull('machine'),
      fetchedAt = j.intOrNull('fetched_at'),
      current = j.flagOrNull('current');

  final String id;

  /// Connected and read just now (this computer's drives only).
  final bool present;
  final HealthReading health;

  /// "ok", "warning", "failing" or "unknown".
  final String verdict;
  final List<HealthFinding> findings;

  /// The latest changes for the worse, newest last.
  final List<HealthChange> changes;

  /// Since the first reading, in a few words ("+8 reallocated", "wear +1%").
  final List<String> trend;
  final int? firstAt;
  final int readings;
  final List<HealthPoint> series;

  /// The other machine it's on, for another machine's drive.
  final String? machine;
  final int? fetchedAt;

  /// Another machine's drive: read there lately (not taken out, its BackDown running).
  final bool? current;

  /// How the app and the service's notifications name it: its id, the device when it has none,
  /// and `@machine:id` for another machine's.
  String get key => machine != null ? '@$machine:$id' : (id.isNotEmpty ? id : health.device);

  String get label => health.label;

  /// The finding that says most about its verdict.
  String? get why {
    for (final f in findings) {
      if (f.level == verdict) return f.text;
    }
    return null;
  }
}

/// How bad a verdict is: failing 3, warning 2, ok 1, unknown 0 (health.js RANK).
int verdictRank(String v) => switch (v) {
  'failing' => 3,
  'warning' => 2,
  'ok' => 1,
  _ => 0,
};

/// A verdict in words, as the web UI says it.
String verdictWord(String v) => switch (v) {
  'ok' => 'healthy',
  'warning' => 'warning',
  'failing' => 'failing',
  _ => 'no health data',
};

/// This computer's drives (GET /api/health).
class HealthOverview {
  HealthOverview.fromJson(Json j)
    : drives = [for (final d in j.list('drives')) DriveHealth.fromJson(d)],
      notes = j.strs('notes'),
      error = j.strOrNull('error'),
      checkedAt = j.intOrNull('checked_at'),
      nextAt = j.intOrNull('next_at'),
      running = j.flag('running'),
      everySecs = j.intOrNull('every_secs'),
      daemon = j.flag('daemon');

  final List<DriveHealth> drives;

  /// Notes about reading the drives at all ("smartctl isn't installed").
  final List<String> notes;
  final String? error;
  final int? checkedAt, nextAt;

  /// Reading the drives right now.
  final bool running;
  final int? everySecs;

  /// The service reads them on its own (`backdown serve` reads them when asked).
  final bool daemon;

  /// The connected drives that need attention.
  List<DriveHealth> get troubled => [
    for (final d in drives)
      if (d.present && verdictRank(d.verdict) > 1) d,
  ];

  /// The worst verdict among the connected drives.
  String get worst {
    var w = 'unknown';
    for (final d in drives) {
      if (d.present && verdictRank(d.verdict) > verdictRank(w)) w = d.verdict;
    }
    return w;
  }

  /// The connected drive [path] is on: the deepest mount point holding it (health.js healthOfPath).
  /// Drives that aren't connected count too, so a drive that's away isn't taken for the one its
  /// empty mount point sits on; then there's no answer.
  DriveHealth? ofPath(String path) {
    DriveHealth? best;
    var len = -1;
    for (final d in drives) {
      for (final m in d.health.mounts) {
        final sep = m.contains(r'\') ? r'\' : '/';
        final inside = path == m || path.startsWith(m.endsWith(sep) ? m : '$m$sep');
        if (inside && (m.length > len || (m.length == len && d.present))) {
          best = d;
          len = m.length;
        }
      }
    }
    return best != null && best.present ? best : null;
  }
}

/// A notification another machine's BackDown made, as fetched with its sync.
class MachineNote {
  MachineNote.fromJson(Json j) : level = j.str('level', 'info'), title = j.str('title'), at = j.integer('at');
  final String level, title;
  final int at;
}

/// Another machine and its drives, as its own BackDown read them (fetched with its last sync).
class MachineHealth {
  MachineHealth.fromJson(Json j)
    : id = j.integer('id'),
      name = j.str('name'),
      url = j.str('url'),
      lastSync = j.intOrNull('last_sync'),
      lastTry = j.intOrNull('last_try'),
      lastError = j.strOrNull('last_error'),
      version = j.strOrNull('version'),
      state = j.obj('health').str('state', 'never'),
      error = j.obj('health').strOrNull('error'),
      note = j.obj('health').strOrNull('note'),
      worst = j.str('worst', 'unknown'),
      drives = [for (final d in j.list('drives')) DriveHealth.fromJson(d, machine: j.str('name'))],
      notes = [for (final n in j.list('notes')) MachineNote.fromJson(n)];

  final int id;
  final String name, url;
  final int? lastSync, lastTry;
  final String? lastError, version;

  /// How fetching its drive health went: "ok", "never", "unsupported", "failed"…, and why not.
  final String state;
  final String? error, note;
  final String worst;
  final List<DriveHealth> drives;
  final List<MachineNote> notes;
}

/// The other machines' drives (GET /api/health-machines).
class MachinesHealth {
  MachinesHealth.fromJson(Json j)
    : machines = [for (final m in j.list('machines')) MachineHealth.fromJson(m)],
      worst = j.str('worst', 'unknown'),
      staleDays = j.integer('stale_days', 7),
      whyNot = j.strOrNull('why_not');

  const MachinesHealth.none() : machines = const [], worst = 'unknown', staleDays = 7, whyNot = null;

  final List<MachineHealth> machines;
  final String worst;
  final int staleDays;

  /// Why this service doesn't sync machines (the machine-wide service).
  final String? whyNot;
}

/// A notification the service recorded (backdown_core::alerts::Stored).
class BdNotification {
  BdNotification.fromJson(Json j)
    : id = j.integer('id'),
      at = j.integer('at'),
      kind = j.str('kind'),
      level = j.str('level', 'info'),
      title = j.str('title'),
      body = j.str('body'),
      drive = j.strOrNull('drive'),
      shown = j.flag('shown'),
      read = j.flag('read');

  final int id, at;

  /// "health", "drift" or "job".
  final String kind;

  /// "info", "warning" or "failing".
  final String level;
  final String title, body;

  /// The drive's health id, when it's about a drive (`@machine:id` for another machine's).
  final String? drive;

  /// It went to the computer's desktop.
  final bool shown;
  final bool read;
}

/// GET /api/notifications.
class Notifications {
  Notifications.fromJson(Json j)
    : notes = [for (final n in j.list('notes')) BdNotification.fromJson(n)],
      unread = j.integer('unread');
  final List<BdNotification> notes;
  final int unread;
}

/// Which news the service shows on the computer's desktop (GET /api/notify-settings).
class NotifySettings {
  NotifySettings.fromJson(Json j)
    : desktop = j.flagOrNull('desktop'),
      health = j.flagOrNull('health'),
      drift = j.flagOrNull('drift'),
      jobs = j.flagOrNull('jobs'),
      available = j.flag('available'),
      why = j.strOrNull('why');

  final bool? desktop, health, drift, jobs;

  /// The computer can show notifications at all.
  final bool available;

  /// Why it can't.
  final String? why;

  bool? operator [](String key) => switch (key) {
    'desktop' => desktop,
    'health' => health,
    'drift' => drift,
    'jobs' => jobs,
    _ => null,
  };
}
