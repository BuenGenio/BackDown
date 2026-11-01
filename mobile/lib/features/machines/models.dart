import '../../api/json.dart';
import '../../api/keys.dart';
import '../../models/browse.dart';
import '../../models/daemon.dart';

// ---------- other machines (GET /api/machines, remote/ in the Rust)

/// What a machine said about itself when it last answered (`hello`).
class MachineInfo {
  MachineInfo.fromJson(Json j)
    : version = j.strOrNull('version'),
      host = j.strOrNull('host'),
      db = j.strOrNull('db'),
      os = j.strOrNull('os'),
      seen = j.intOrNull('seen'),
      skew = j.integer('skew');

  final String? version, host, db, os;

  /// When it last answered.
  final int? seen;

  /// Its clock minus the service's, in seconds, as of then.
  final int skew;
}

/// An indexed drive on another machine, as its last sync left it here.
class MachineRoot {
  MachineRoot.fromJson(Json j)
    : id = j.integer('id'),
      key = j.str('key'),
      name = j.str('name'),
      path = j.str('path'),
      lastScan = j.intOrNull('last_scan'),
      scannedAt = j.intOrNull('scanned_at'),
      files = j.integer('files'),
      bytes = j.integer('bytes'),
      seenAt = j.intOrNull('seen_at'),
      cached = j.integer('cached'),
      hashed = j.integer('hashed'),
      counts = j.flag('counts');

  final int id;
  final String key, name, path;
  final int? lastScan, scannedAt;
  final int files, bytes;

  /// When that machine last confirmed its files.
  final int? seenAt;

  /// Its files cached here, and how many of them have a SHA-256.
  final int cached, hashed;

  /// Its copies count as proof (confirmed within the proof window).
  final bool counts;

  String get title => name.isEmpty ? path : name;
}

/// Another machine this service reaches over ssh.
class Machine {
  Machine.fromJson(Json j)
    : id = j.integer('id'),
      name = j.str('name'),
      url = j.str('url'),
      db = j.strOrNull('db'),
      createdAt = j.intOrNull('created_at'),
      lastTry = j.intOrNull('last_try'),
      lastSync = j.intOrNull('last_sync'),
      lastError = j.strOrNull('last_error'),
      info = MachineInfo.fromJson(j.obj('info')),
      job = j.objOrNull('job') == null ? null : Job.fromJson(j.obj('job')),
      roots = [for (final r in j.list('roots')) MachineRoot.fromJson(r)];

  final int id;
  final String name, url;

  /// Its index there, when it isn't the default.
  final String? db;
  final int? createdAt, lastTry, lastSync;
  final String? lastError;
  final MachineInfo info;

  /// A sync of it, queued or running.
  final Job? job;
  final List<MachineRoot> roots;

  bool get busy => job != null && (job!.state == 'queued' || job!.state == 'running');

  /// The last try failed, and nothing has worked since.
  bool get unreachable => lastError != null && (lastSync == null || (lastTry ?? 0) > lastSync!);

  int get files => roots.fold(0, (a, r) => a + r.files);
  int get bytes => roots.fold(0, (a, r) => a + r.bytes);
}

class MachineSettings {
  MachineSettings.fromJson(Json j) : proofDays = j.integer('proof_days', 7), syncHours = j.integer('sync_hours', 6);
  final int proofDays, syncHours;
}

/// GET /api/machines: the machines, how copies there count, and whether this service can reach
/// them itself.
class MachinesInfo {
  MachinesInfo.fromJson(Json j)
    : machines = [for (final m in j.list('machines')) Machine.fromJson(m)],
      settings = MachineSettings.fromJson(j.obj('settings')),
      allowed = j.flagOrNull('allowed') ?? true,
      whyNot = j.strOrNull('why_not'),
      daemon = j.flag('daemon');

  final List<Machine> machines;
  final MachineSettings settings;

  /// The licence includes reaching other machines.
  final bool allowed;

  /// Why this service can't reach machines itself (the machine-wide service has no ssh keys).
  final String? whyNot;

  /// A BackDown service (not `backdown serve`) runs the syncs.
  final bool daemon;

  Machine? byId(int id) => machines.where((m) => m.id == id).firstOrNull;
}

/// A file or folder in a listing of another machine: live (its own index's answer, with folder
/// sizes) or as last synced (folders without sizes).
class MachineEntry {
  MachineEntry.fromJson(Json j)
    : kind = j.str('kind', 'file'),
      name = j.str('name'),
      key = j.str('key'),
      id = j.intOrNull('id'),
      size = j.integer('size'),
      files = j.intOrNull('files'),
      mtime = j.integer('mtime'),
      sha256 = j.strOrNull('sha256'),
      copies = j.integer('copies');

  final String kind, name, key;

  /// The file's id in that machine's index (what its details are asked by).
  final int? id;
  final int size;
  final int? files;
  final int mtime;
  final String? sha256;

  /// Other files there with the same content.
  final int copies;

  bool get isDir => kind == 'dir';
}

/// GET /api/machine-ls?m=&dir=: a folder there, or (without `dir`) its drives.
class MachineListing {
  MachineListing.fromJson(Json j)
    : live = j.flag('live'),
      whyNotLive = j.strOrNull('why_not_live'),
      syncedAt = j.intOrNull('synced_at'),
      scannedAt = j.intOrNull('scanned_at'),
      truncated = j.flag('truncated'),
      name = j.str('name'),
      path = j.str('path'),
      key = j.str('key'),
      files = j.intOrNull('files'),
      bytes = j.intOrNull('bytes'),
      crumbs = [for (final c in j.list('crumbs')) Crumb.fromJson(c)],
      entries = [for (final e in j.list('entries')) MachineEntry.fromJson(e)],
      roots = j['roots'] is List ? [for (final r in j.list('roots')) MachineRoot.fromJson(r)] : null;

  /// Asked of the machine just now (else: as last synced here).
  final bool live;
  final String? whyNotLive;
  final int? syncedAt, scannedAt;

  /// Only the first 10,000 entries.
  final bool truncated;
  final String name, path, key;
  final int? files, bytes;
  final List<Crumb> crumbs;
  final List<MachineEntry> entries;

  /// Its drives, when no folder was asked for.
  final List<MachineRoot>? roots;
}

/// A copy of a file here or on the same machine.
class CopyRef {
  CopyRef.fromJson(Json j) : id = j.integer('id'), path = j.str('path'), key = j.str('key'), dirKey = j.str('dir_key');
  final int id;
  final String path, key, dirKey;
}

/// The same content on another machine (from its last sync).
class MachineCopy {
  MachineCopy.fromJson(Json j)
    : machineId = j.integer('machine_id'),
      machine = j.str('machine'),
      rid = j.integer('rid'),
      path = j.str('path'),
      key = j.str('key'),
      dirKey = j.str('dir_key'),
      confirmedAt = j.intOrNull('confirmed_at'),
      counts = j.flag('counts'),
      whyNot = j.strOrNull('why_not');

  final int machineId;
  final String machine;

  /// Its id in that machine's index.
  final int rid;
  final String path, key, dirKey;
  final int? confirmedAt;
  final bool counts;
  final String? whyNot;
}

/// GET /api/machine-file?m=&id=: a file on another machine, and where else its content is. No
/// bytes come from there.
class MachineFile {
  MachineFile.fromJson(Json j)
    : live = j.flag('live'),
      whyNotLive = j.strOrNull('why_not_live'),
      id = j.integer('id'),
      name = j.str('name'),
      path = j.str('path'),
      key = j.str('key'),
      dirKey = j.str('dir_key'),
      size = j.integer('size'),
      mtime = j.integer('mtime'),
      sha256 = j.strOrNull('sha256'),
      hashedAt = j.intOrNull('hashed_at'),
      error = j.strOrNull('error'),
      versions = [for (final v in j.list('versions')) FileVersion.fromJson(v)],
      there = [for (final c in j.list('copies')) CopyRef.fromJson(c)],
      local = [for (final c in j.list('local_copies')) CopyRef.fromJson(c)],
      others = [for (final c in j.list('remote_copies')) MachineCopy.fromJson(c)];

  final bool live;
  final String? whyNotLive;
  final int id;
  final String name, path, key, dirKey;
  final int size, mtime;
  final String? sha256;

  /// When that machine hashed it for BackDown here (rather than for itself).
  final int? hashedAt;
  final String? error;
  final List<FileVersion> versions;

  /// Other copies on the same machine, here, and on other machines.
  final List<CopyRef> there, local;
  final List<MachineCopy> others;
}

// ---------- the swarm (GET /api/swarm, swarm.rs in the Rust)

/// Room on a file system.
class Space {
  Space.fromJson(Json j) : total = j.integer('total'), free = j.integer('free');
  final int total, free;
  int get used => (total - free).clamp(0, total);
  double get fraction => total == 0 ? 0 : used / total;

  static Space? of(Json j, String k) {
    final s = j.objOrNull(k);
    if (s == null) return null;
    final v = Space.fromJson(s);
    return v.total > 0 ? v : null;
  }
}

/// What a drive is: the disk (model, serial) and the file system (label, UUID), as the swarm
/// names drives wherever they're plugged in.
class DriveIdentity {
  DriveIdentity.fromJson(Json j)
    : model = j.obj('disk').strOrNull('model'),
      serial = j.obj('disk').strOrNull('serial'),
      label = j.obj('volume').strOrNull('label'),
      device = j.obj('volume').strOrNull('device'),
      present = j.flagOrNull('present');

  final String? model, serial, label, device;

  /// Connected now (for a where-is hit's drive).
  final bool? present;

  /// "Hercules (WD Elements 2620 s/n WX…)", as swarm.js's driveWords says it.
  String get words {
    final m = model == null || model!.isEmpty ? '' : '$model${serial == null || serial!.isEmpty ? '' : ' s/n $serial'}';
    final l = label ?? '';
    if (l.isNotEmpty && m.isNotEmpty) return '$l ($m)';
    if (l.isNotEmpty) return l;
    if (m.isNotEmpty) return m;
    return device ?? 'a drive';
  }

  /// Its label, else its model: "Hercules".
  String get short {
    for (final s in [label, model, device]) {
      if (s != null && s.isNotEmpty) return s;
    }
    return 'a drive';
  }

  static DriveIdentity? of(Json j, String k) {
    final d = j.objOrNull(k);
    return d == null ? null : DriveIdentity.fromJson(d);
  }
}

/// An indexed drive on a volume.
class IndexedOn {
  IndexedOn.fromJson(Json j)
    : id = j.integer('id'),
      path = j.str('path'),
      key = j.str('key'),
      name = j.str('name'),
      files = j.integer('files'),
      bytes = j.integer('bytes');
  final int id;
  final String path, key, name;
  final int files, bytes;
  String get title => name.isEmpty ? path : name;
}

/// A file system on a disk.
class SwarmVolume {
  SwarmVolume.fromJson(Json j)
    : name = j.str('name'),
      label = j.strOrNull('label'),
      partLabel = j.strOrNull('part_label'),
      fsName = j.strOrNull('fs_name') ?? j.strOrNull('fs'),
      size = j.integer('size'),
      state = j.str('state'),
      via = j.strOrNull('via'),
      mounts = j.strs('mounts'),
      space = Space.of(j, 'space'),
      system = j.flag('system'),
      indexed = [for (final i in j.list('indexed')) IndexedOn.fromJson(i)],
      inPool = j.list('pool').isNotEmpty;

  final String name;
  final String? label, partLabel, fsName;
  final int size;

  /// "mounted", "mountable", "encrypted", "system", …
  final String state;

  /// What it's stacked on: an encrypted container, LVM.
  final String? via;
  final List<String> mounts;
  final Space? space;
  final bool system;
  final List<IndexedOn> indexed;

  /// A pool folder is on it.
  final bool inPool;

  String get title {
    for (final s in [label, partLabel]) {
      if (s != null && s.isNotEmpty) return s;
    }
    return name;
  }
}

/// A physical disk and its volumes.
class SwarmDisk {
  SwarmDisk.fromJson(Json j)
    : name = j.str('name'),
      model = j.strOrNull('model'),
      serial = j.strOrNull('serial'),
      size = j.integer('size'),
      transport = j.strOrNull('transport'),
      rotational = j.flagOrNull('rotational'),
      system = j.flag('system'),
      health = j.strOrNull('health'),
      volumes = [for (final v in j.list('volumes')) SwarmVolume.fromJson(v)];

  final String name;
  final String? model, serial;
  final int size;
  final String? transport;
  final bool? rotational;
  final bool system;

  /// Its drive-health verdict where known: "ok", "warning", "failing".
  final String? health;
  final List<SwarmVolume> volumes;
}

/// An indexed drive that isn't on one of the disks listed: a network share (elsewhere), one that
/// isn't connected (away), one whose folder didn't answer (unanswered).
class SwarmRoot {
  SwarmRoot.fromJson(Json j)
    : id = j.integer('id'),
      name = j.str('name'),
      path = j.str('path'),
      key = j.str('key'),
      files = j.integer('files'),
      bytes = j.integer('bytes'),
      scannedAt = j.intOrNull('scanned_at'),
      space = Space.of(j, 'space'),
      drive = DriveIdentity.of(j, 'drive');

  final int id;
  final String name, path, key;
  final int files, bytes;
  final int? scannedAt;
  final Space? space;

  /// The drive it was last scanned on.
  final DriveIdentity? drive;

  String get title => name.isEmpty ? path : name;
}

/// A pool folder: a folder on one machine's drive that's part of the swarm's one filed archive.
class PoolFolder {
  PoolFolder.fromJson(Json j)
    : id = j.integer('id'),
      path = j.str('path'),
      reserve = j.integer('reserve'),
      takes = j.strs('takes'),
      present = j.flag('present'),
      answering = j.flagOrNull('answering') ?? true,
      space = Space.of(j, 'space'),
      room = j.integer('room'),
      indexed = j.flag('indexed'),
      files = j.integer('files'),
      bytes = j.integer('bytes'),
      drive = DriveIdentity.of(j, 'drive');

  final int id;
  final String path;

  /// Room kept free on its drive.
  final int reserve;

  /// The top-level places it takes (empty: anything).
  final List<String> takes;
  final bool present, answering;
  final Space? space;

  /// Room left to fill: its drive's free space less the reserve.
  final int room;
  final bool indexed;
  final int files, bytes;
  final DriveIdentity? drive;
}

class PoolTotals {
  PoolTotals.fromJson(Json j)
    : areas = j.integer('areas'),
      files = j.integer('files'),
      bytes = j.integer('bytes'),
      room = j.integer('room');
  PoolTotals.zero() : areas = 0, files = 0, bytes = 0, room = 0;
  PoolTotals.sum(Iterable<PoolTotals> all)
    : areas = all.fold(0, (a, t) => a + t.areas),
      files = all.fold(0, (a, t) => a + t.files),
      bytes = all.fold(0, (a, t) => a + t.bytes),
      room = all.fold(0, (a, t) => a + t.room);
  final int areas, files, bytes, room;
}

/// A machine's storage in numbers.
class StorageTotals {
  StorageTotals.fromJson(Json j)
    : disks = j.integer('disks'),
      capacity = j.integer('capacity'),
      free = j.integer('free'),
      files = j.integer('files'),
      bytes = j.integer('bytes'),
      pool = j.objOrNull('pool') == null ? PoolTotals.zero() : PoolTotals.fromJson(j.obj('pool'));
  final int disks, capacity, free, files, bytes;
  final PoolTotals pool;
}

/// One machine of the swarm: this one (probed now) or another (as last kept, or asked just now).
class SwarmMachine {
  SwarmMachine.fromJson(Json j)
    : id = j.intOrNull('id'),
      name = j.str('name'),
      here = j.flag('here'),
      url = j.strOrNull('url'),
      version = j.strOrNull('version'),
      state = j.str('state', 'never'),
      error = j.strOrNull('error'),
      okAt = j.intOrNull('ok_at'),
      lastSync = j.intOrNull('last_sync'),
      asked = j.flagOrNull('asked'),
      askError = j.strOrNull('ask_error'),
      disks = [for (final d in j.list('disks')) SwarmDisk.fromJson(d)],
      elsewhere = [for (final r in j.list('elsewhere')) SwarmRoot.fromJson(r)],
      away = [for (final r in j.list('away')) SwarmRoot.fromJson(r)],
      unanswered = [for (final r in j.list('unanswered')) SwarmRoot.fromJson(r)],
      notes = j.strs('notes'),
      pool = [for (final a in j.list('pool')) PoolFolder.fromJson(a)],
      totals = StorageTotals.fromJson(j.obj('totals'));

  /// Null for this machine.
  final int? id;
  final String name;
  final bool here;
  final String? url, version;

  /// "here", "ok", "error", "unsupported" (a BackDown there from before the swarm), "never".
  final String state;
  final String? error;

  /// When it last said what drives it has.
  final int? okAt;
  final int? lastSync;

  /// After /api/swarm-machine: whether it was reached just now, and why not.
  final bool? asked;
  final String? askError;
  final List<SwarmDisk> disks;
  final List<SwarmRoot> elsewhere, away, unanswered;
  final List<String> notes;
  final List<PoolFolder> pool;
  final StorageTotals totals;
}

class SwarmTotals {
  SwarmTotals(List<SwarmMachine> ms)
    : machines = ms.length,
      reporting = ms.where((m) => m.disks.isNotEmpty).length,
      disks = ms.fold(0, (a, m) => a + m.totals.disks),
      capacity = ms.fold(0, (a, m) => a + m.totals.capacity),
      free = ms.fold(0, (a, m) => a + m.totals.free),
      files = ms.fold(0, (a, m) => a + m.totals.files),
      bytes = ms.fold(0, (a, m) => a + m.totals.bytes),
      pool = PoolTotals.sum(ms.map((m) => m.totals.pool));
  final int machines, reporting, disks, capacity, free, files, bytes;
  final PoolTotals pool;
}

/// GET /api/swarm: every machine as one.
class SwarmOverview {
  SwarmOverview.fromJson(Json j)
    : here = j.str('here'),
      machines = [for (final m in j.list('machines')) SwarmMachine.fromJson(m)],
      canAsk = j.flag('can_ask'),
      whyNot = j.strOrNull('why_not'),
      daemon = j.flag('daemon');

  final String here;
  final List<SwarmMachine> machines;

  /// This service may ask the other machines itself (over ssh, with its person's keys).
  final bool canAsk;
  final String? whyNot;
  final bool daemon;
}

// ---------- where is it (GET /api/swarm-where?q=)

class WhereHit {
  WhereHit.fromJson(Json j)
    : hit = FindHit.fromJson(j),
      machine = j.str('machine'),
      machineId = j.intOrNull('machine_id'),
      here = j.flag('here'),
      cached = j.flag('cached'),
      place = DriveIdentity.of(j, 'place');

  final FindHit hit;
  final String machine;
  final int? machineId;
  final bool here;

  /// Found in what was last synced of that machine (it couldn't be asked).
  final bool cached;

  /// The drive it's on there.
  final DriveIdentity? place;

  /// The drive it's on, in brief: its label or model ("not connected" when it isn't).
  String get drive {
    final p = place;
    if (p != null) return p.present == false ? '${p.short} (not connected)' : p.short;
    return baseName(hit.drive);
  }

  /// The folder it's in there.
  String get folder {
    final path = hit.path;
    final i = path.lastIndexOf(RegExp(r'[/\\]'));
    return i <= 0 ? path : path.substring(0, i);
  }
}

/// How one machine was searched.
class WhereMachine {
  WhereMachine.fromJson(Json j)
    : name = j.str('name'),
      id = j.intOrNull('id'),
      here = j.flag('here'),
      how = j.str('how'),
      total = j.integer('total'),
      complete = j.flagOrNull('complete') ?? true,
      error = j.strOrNull('error'),
      syncedAt = j.intOrNull('synced_at');

  final String name;
  final int? id;
  final bool here;

  /// "here", "live", "cached" (it couldn't be asked) or "none" (it wasn't searched).
  final String how;
  final int total;
  final bool complete;
  final String? error;
  final int? syncedAt;
}

class WhereResult {
  WhereResult.fromJson(Json j)
    : q = j.str('q'),
      hits = [for (final h in j.list('hits')) WhereHit.fromJson(h)],
      machines = [for (final m in j.list('machines')) WhereMachine.fromJson(m)];
  final String q;
  final List<WhereHit> hits;
  final List<WhereMachine> machines;
}

// ---------- the pool's plans (GET /api/pool-plans, /api/pool-plan?id=)

class PoolPlanBrief {
  PoolPlanBrief.fromJson(Json j)
    : id = j.integer('id'),
      createdAt = j.integer('created_at'),
      drive = j.str('drive'),
      root = j.str('root'),
      state = j.str('state'),
      newBytes = j.integer('new_bytes'),
      unknownBytes = j.integer('unknown_bytes'),
      unplacedBytes = j.integer('unplaced_bytes');
  final int id, createdAt;
  final String drive, root, state;
  final int newBytes, unknownBytes, unplacedBytes;
  int get toCopy => newBytes + unknownBytes;
}

/// Where a pool folder is: on which machine (this one: no id), which folder there.
class AreaRef {
  AreaRef.fromJson(Json j)
    : machine = j.str('machine'),
      machineId = j.intOrNull('machine_id'),
      area = j.integer('area'),
      path = j.str('path');
  final String machine;
  final int? machineId;
  final int area;
  final String path;
  String get label => '$machine: $path';
}

/// What a thing on the drive is and where it's filed (plan.rs's item).
class PlanItem {
  PlanItem.fromJson(Json j)
    : what = j.str('what'),
      kind = j.str('kind'),
      source = j.str('source'),
      dest = j.str('dest'),
      files = j.integer('files'),
      bytes = j.integer('bytes');
  final String what, kind, source, dest;
  final int files, bytes;
}

/// One thing on the drive, and where it goes in the pool.
class PoolThing {
  PoolThing.fromJson(Json j)
    : item = PlanItem.fromJson(j.obj('item')),
      inPoolBytes = j.integer('in_pool_bytes'),
      inPoolAt = j.strOrNull('in_pool_at'),
      twiceBytes = j.integer('twice_bytes'),
      newBytes = j.integer('new_bytes'),
      unknownBytes = j.integer('unknown_bytes'),
      leftBytes = j.integer('left_bytes'),
      to = j.objOrNull('to') == null ? null : AreaRef.fromJson(j.obj('to')),
      whyTo = j.str('why_to'),
      skip = j.flag('skip');
  final PlanItem item;
  final int inPoolBytes;
  final String? inPoolAt;
  final int twiceBytes, newBytes, unknownBytes, leftBytes;
  final AreaRef? to;
  final String whyTo;
  final bool skip;

  int get need => skip ? 0 : newBytes + unknownBytes;
}

/// A pool folder as the plan uses it: what's assigned to it, and its room.
class AreaUse {
  AreaUse.fromJson(Json j)
    : at = j.objOrNull('at') == null ? null : AreaRef.fromJson(j.obj('at')),
      drive = j.str('drive'),
      takes = j.strs('takes'),
      room = j.integer('room'),
      assignedFiles = j.integer('assigned_files'),
      assignedBytes = j.integer('assigned_bytes'),
      indexed = j.flag('indexed'),
      present = j.flag('present');
  final AreaRef? at;
  final String drive;
  final List<String> takes;
  final int room, assignedFiles, assignedBytes;
  final bool indexed, present;
}

/// A drive's plan into the pool: what's in it already, what's new, and where each thing goes.
class PoolPlan {
  PoolPlan.fromJson(Json j)
    : id = j.integer('id'),
      createdAt = j.integer('created_at'),
      machine = j.str('machine'),
      root = j.str('root'),
      drive = j.str('drive'),
      things = [for (final t in j.list('things')) PoolThing.fromJson(t)],
      areas = [for (final a in j.list('areas')) AreaUse.fromJson(a)],
      files = j.integer('files'),
      bytes = j.integer('bytes'),
      inPoolBytes = j.integer('in_pool_bytes'),
      twiceBytes = j.integer('twice_bytes'),
      unknownBytes = j.integer('unknown_bytes'),
      leftBytes = j.integer('left_bytes'),
      copyBytes = j.intOrNull('copy_bytes') ?? (j.integer('new_bytes') + j.integer('unknown_bytes')),
      unplacedBytes = j.integer('unplaced_bytes'),
      notes = j.strs('notes'),
      state = j.str('state');

  final int id, createdAt;
  final String machine, root, drive;
  final List<PoolThing> things;
  final List<AreaUse> areas;
  final int files, bytes, inPoolBytes, twiceBytes, unknownBytes, leftBytes, copyBytes, unplacedBytes;
  final List<String> notes;

  /// "new", "sending", "partial" or "done".
  final String state;
}
