/// What the library screens read from the service: what's recognised, duplicates, plans, freeing
/// up, rescues, mirrors, manifests, and the service's editions and updates. Plain classes, so the
/// big answers (/api/entities, /api/dupes, /api/plan) can be read in another isolate and sent back.
library;

import '../../api/json.dart';

// ---------- what's here (/api/entities, /api/overlaps)

/// A regenerable subtree: caches, thumbnails, previews, build output, trash.
class Junk {
  Junk.fromJson(Json j)
    : path = j.str('path'),
      key = j.str('key'),
      why = j.str('why'),
      files = j.integer('files'),
      bytes = j.integer('bytes');

  final String path, key, why;
  final int files, bytes;
}

/// A list of junk subtrees, kept short: their totals, the reasons, and the biggest few.
class JunkSummary {
  JunkSummary(List<Junk> all, {int keep = 30})
    : count = all.length,
      bytes = all.fold(0, (a, j) => a + j.bytes),
      files = all.fold(0, (a, j) => a + j.files),
      whys = _whys(all),
      biggest = ([...all]..sort((a, b) => b.bytes.compareTo(a.bytes))).take(keep).toList();

  const JunkSummary.none() : count = 0, bytes = 0, files = 0, whys = const [], biggest = const [];

  final int count, bytes, files;

  /// Why they're regenerable, the most bytes first ("Python bytecode cache", "npm packages…").
  final List<String> whys;
  final List<Junk> biggest;

  bool get isEmpty => count == 0;

  static List<String> _whys(List<Junk> all) {
    final by = <String, int>{};
    for (final j in all) {
      by[j.why] = (by[j.why] ?? 0) + j.bytes;
    }
    return (by.entries.toList()..sort((a, b) => b.value.compareTo(a.value))).map((e) => e.key).toList();
  }
}

/// A recognised thing: a mail store, a phone backup, a photo library, a repository, a dump…
class Entity {
  Entity.fromJson(Json j, this.rootPath)
    : kind = j.str('kind'),
      label = j.str('label'),
      title = j.str('title'),
      path = j.str('path'),
      key = j.str('key'),
      isFile = j.flag('is_file'),
      confidence = j.number('confidence') ?? 1,
      files = j.integer('files'),
      bytes = j.integer('bytes'),
      oldest = j.intOrNull('oldest'),
      newest = j.intOrNull('newest'),
      facts = [
        if (j['facts'] is List)
          for (final f in j['facts'] as List)
            if (f is List && f.length >= 2) (f[0].toString(), f[1].toString()),
      ],
      provenance = j.strs('provenance'),
      provenanceLocked = j.flag('provenance_locked'),
      junk = JunkSummary([for (final x in j.list('junk')) Junk.fromJson(x)]),
      inside = j.strOrNull('inside'),
      lowValue = j.flag('low_value') {
    search = [label, title, path, ...provenance, for (final f in facts) f.$2].join(' ').toLowerCase();
  }

  final String kind, label, title, path, key;
  final bool isFile;

  /// 0..1: how sure the rule is.
  final double confidence;
  final int files, bytes;

  /// Modification times of the files inside (not when they were copied here).
  final int? oldest, newest;

  /// The facts behind the title, in order ("Mailboxes" → "142").
  final List<(String, String)> facts;

  /// Where it came from, as sentences. Empty on Community, where it's part of Pro.
  final List<String> provenance;
  final bool provenanceLocked;
  final JunkSummary junk;

  /// The key of the recognised thing it sits in, if any.
  final String? inside;

  /// Probably not worth keeping as a unit (a downloads pile, recovered fragments).
  final bool lowValue;

  /// The drive it's on.
  final String rootPath;

  /// Everything the filter looks through, lower case.
  late final String search;

  /// Its path below its drive ("." for the drive itself).
  String get relPath {
    if (!path.startsWith(rootPath)) return path;
    final r = path.substring(rootPath.length).replaceFirst(RegExp(r'^[\\/]+'), '');
    return r.isEmpty ? '.' : r;
  }
}

/// What's recognised on one drive, as of one scan.
class DriveEntities {
  DriveEntities.fromJson(Json j)
    : rootId = j.integer('root_id'),
      root = j.str('root'),
      scan = j.integer('scan'),
      computedAt = j.integer('computed_at'),
      fresh = j.flagOrNull('fresh') ?? true,
      readDrive = j.flagOrNull('read_drive') ?? true,
      entities = [for (final e in j.list('entities')) Entity.fromJson(e, j.str('root'))],
      junk = JunkSummary([for (final x in j.list('junk')) Junk.fromJson(x)]);

  final int rootId;
  final String root;
  final int scan, computedAt;

  /// Recognised at the drive's latest scan (else it's been scanned again since).
  final bool fresh;

  /// The drive was connected, so its files could be read for provenance.
  final bool readDrive;
  final List<Entity> entities;

  /// Junk that isn't inside any recognised thing.
  final JunkSummary junk;

  /// Every regenerable byte on the drive, in recognised things or not.
  int get junkBytes => junk.bytes + entities.fold(0, (a, e) => a + e.junk.bytes);
}

/// GET /api/entities.
class EntitiesData {
  EntitiesData.fromJson(Json j)
    : daemon = j.flag('daemon'),
      drives = [for (final r in j.list('roots')) DriveEntities.fromJson(r)];

  /// There's a background service (it can recognise again).
  final bool daemon;
  final List<DriveEntities> drives;

  Iterable<Entity> get all => drives.expand((d) => d.entities);
}

/// The kinds BackDown recognises, in the order they're listed (recognise/mod.rs KINDS).
const entityKinds = [
  'mail-store',
  'maildir',
  'mdbox',
  'thunderbird',
  'nextcloud',
  'nextcloud-server',
  'timemachine',
  'ios-backup',
  'android-backup',
  'system',
  'home-mac',
  'home-linux',
  'home-windows',
  'website',
  'git',
  'sql-dump',
  'disk-image',
  'photos',
  'music',
  'videos',
  'export',
  'downloads',
  'recovery',
];

/// One of the overlaps: how much of a drive, top-level folder or recognised thing exists elsewhere.
class Overlap {
  Overlap.fromJson(Json j)
    : level = j.obj('unit').str('level'),
      label = j.obj('unit').str('label'),
      path = j.obj('unit').str('path'),
      key = j.obj('unit').str('key'),
      bytes = j.obj('unit').integer('bytes'),
      elsewhere = j.integer('elsewhere'),
      elsewhereFiles = j.integer('elsewhere_files'),
      containers = [
        for (final c in j.list('containers'))
          OverlapPlace(label: c.str('label'), path: c.str('path'), key: c.str('key'), bytes: c.integer('bytes')),
      ];

  /// "drive", "folder" or "entity".
  final String level;
  final String label, path, key;
  final int bytes, elsewhere, elsewhereFiles;

  /// Where those copies are, most first.
  final List<OverlapPlace> containers;

  double get share => bytes == 0 ? 0 : elsewhere / bytes;
}

class OverlapPlace {
  const OverlapPlace({required this.label, required this.path, required this.key, required this.bytes});
  final String label, path, key;
  final int bytes;
}

// ---------- duplicates (/api/dupes)

/// A copy on another machine, as last synced.
class RemoteCopy {
  RemoteCopy.fromJson(Json j)
    : machineId = j.integer('machine_id'),
      machine = j.str('machine'),
      path = j.str('path'),
      key = j.str('key'),
      dirKey = j.str('dir_key'),
      confirmedAt = j.intOrNull('confirmed_at'),
      counts = j.flag('counts'),
      whyNot = j.strOrNull('why_not');

  final int machineId;
  final String machine, path, key, dirKey;
  final int? confirmedAt;

  /// It counts as a verified copy: hashed there, readable, confirmed within the proof window.
  final bool counts;
  final String? whyNot;
}

/// One path of a group of identical files.
class DupeFile {
  DupeFile.fromJson(Json j)
    : path = j.str('path'),
      key = j.str('key'),
      dir = j.str('dir'),
      id = j.integer('id'),
      mtime = j.integer('mtime'),
      copy = j.integer('copy'),
      listed = j.flagOrNull('listed') ?? true;

  final String path, key;

  /// Its folder's key.
  final String dir;
  final int id, mtime;

  /// Which of the group's distinct copies it is: hardlinks of one file share the number.
  final int copy;

  /// It passes the filter (a group is listed when one of its paths does).
  final bool listed;

  String get name => leafOf(path);
  String get folder => folderOf(path);
}

/// A set of identical files.
class DupeGroup {
  DupeGroup.fromJson(Json j)
    : sha256 = j.str('sha256'),
      size = j.integer('size'),
      copies = j.integer('copies'),
      reclaimable = j.integer('reclaimable'),
      files = j.list('files').isEmpty
          ? [
              for (final (i, p) in j.strs('paths').indexed)
                DupeFile.fromJson({'path': p, 'key': '', 'dir': '', 'id': 0, 'mtime': 0, 'copy': i}),
            ]
          : [for (final f in j.list('files')) DupeFile.fromJson(f)],
      remote = [for (final r in j.list('remote')) RemoteCopy.fromJson(r)];

  final String sha256;

  /// Bytes each.
  final int size;

  /// Distinct copies (hardlinks of one file count once).
  final int copies;
  final int reclaimable;
  final List<DupeFile> files;

  /// The same content on other machines.
  final List<RemoteCopy> remote;

  /// The copies the filter lets through.
  List<DupeFile> get shown => [
    for (final f in files)
      if (f.listed) f,
  ];

  /// Its name: the first listed copy's.
  String get name => (shown.firstOrNull ?? files.firstOrNull)?.name ?? '';

  /// Other names its copies go by.
  List<String> get otherNames {
    final seen = <String>{name};
    return [
      for (final f in shown)
        if (seen.add(f.name)) f.name,
    ];
  }
}

/// Content that's here once and again on another machine.
class AcrossGroup {
  AcrossGroup.fromJson(Json j)
    : sha256 = j.str('sha256'),
      size = j.integer('size'),
      path = j.str('path'),
      remote = [for (final r in j.list('remote')) RemoteCopy.fromJson(r)];
  final String sha256;
  final int size;
  final String path;
  final List<RemoteCopy> remote;
}

class Across {
  Across.fromJson(Json j)
    : files = j.integer('files'),
      bytes = j.integer('bytes'),
      provenFiles = j.integer('proven_files'),
      provenBytes = j.integer('proven_bytes'),
      largest = [for (final g in j.list('largest')) AcrossGroup.fromJson(g)];
  final int files, bytes, provenFiles, provenBytes;
  final List<AcrossGroup> largest;
}

/// GET /api/dupes.
class DupesData {
  DupesData.fromJson(Json j)
    : groups = j.integer('groups'),
      reclaimable = j.integer('reclaimable'),
      largest = [for (final g in j.list('largest')) DupeGroup.fromJson(g)],
      allGroups = j.objOrNull('unfiltered')?.integer('groups') ?? j.integer('groups'),
      allReclaimable = j.objOrNull('unfiltered')?.integer('reclaimable') ?? j.integer('reclaimable'),
      onlyHidden = j.integer('only_hidden'),
      across = j.objOrNull('across') == null ? null : Across.fromJson(j.obj('across'));

  /// Groups (and bytes to free) that match the filter.
  final int groups, reclaimable;

  /// The groups that free the most, most first.
  final List<DupeGroup> largest;

  /// The same, the filter aside.
  final int allGroups, allReclaimable;

  /// Groups left out only because every path of theirs is hidden.
  final int onlyHidden;
  final Across? across;

  bool get narrowed => allGroups != groups;

  /// The groups with a copy the filter lets through.
  List<DupeGroup> get listed => [
    for (final g in largest)
      if (g.files.any((f) => f.listed)) g,
  ];
}

// ---------- plans (/api/plans, /api/plan, /api/plan-dry, /api/dedups)

class PlanSummary {
  PlanSummary.fromJson(Json j)
    : id = j.integer('id'),
      state = j.str('state'),
      createdAt = j.integer('created_at'),
      target = j.str('target'),
      copyBytes = j.integer('copy_bytes'),
      copyFiles = j.integer('copy_files'),
      items = j.integer('items'),
      drives = [for (final d in j.list('drives')) (drive: d.str('drive'), covered: d.flag('covered'))];

  final int id;

  /// "new", "running", "done" or "partial".
  final String state;
  final int createdAt;
  final String target;
  final int copyBytes, copyFiles, items;
  final List<({String drive, bool covered})> drives;
}

/// One thing a plan files.
class PlanItem {
  PlanItem.fromJson(Json j)
    : what = j.str('what'),
      kind = j.str('kind'),
      source = j.str('source'),
      sourceKey = j.str('source_key'),
      drive = j.str('drive'),
      isFile = j.flag('is_file'),
      dest = j.str('dest'),
      files = j.integer('files'),
      bytes = j.integer('bytes'),
      action = j.str('action'),
      why = j.str('why'),
      merge = j.strs('merge');

  final String what, kind, source, sourceKey, drive;
  final bool isFile;

  /// Below the archive.
  final String dest;
  final int files, bytes;

  /// "copy", "copy-unique", "merge" or "skip".
  final String action;
  final String why;

  /// For a merge: the other snapshots of the mailbox.
  final List<String> merge;
}

/// What a plan leaves on each drive once it's applied.
class DriveOutcome {
  DriveOutcome.fromJson(Json j)
    : drive = j.str('drive'),
      path = j.str('path'),
      bytes = j.integer('bytes'),
      copy = j.integer('copy'),
      already = j.integer('already'),
      notCopied = j.integer('not_copied'),
      covered = j.flag('covered');
  final String drive, path;
  final int bytes, copy, already, notCopied;

  /// Every byte will have a copy in the archive.
  final bool covered;
}

/// GET /api/plan?id=.
class PlanDetail {
  PlanDetail.fromJson(Json j)
    : id = j.integer('id'),
      state = j.str('state'),
      target = j.obj('plan').str('target'),
      createdAt = j.obj('plan').integer('created_at'),
      schema = j.obj('plan').str('schema'),
      items = [for (final i in j.obj('plan').list('items')) PlanItem.fromJson(i)],
      drives = [for (final d in j.obj('plan').list('drives')) DriveOutcome.fromJson(d)],
      copyFiles = j.obj('plan').integer('copy_files'),
      copyBytes = j.obj('plan').integer('copy_bytes'),
      journal = {
        if (j['journal'] is List)
          for (final r in j['journal'] as List)
            if (r is List && r.length >= 3) r[0].toString(): (n: _n(r[1]), bytes: _n(r[2])),
      };

  final int id;
  final String state, target;
  final int createdAt;
  final String schema;
  final List<PlanItem> items;
  final List<DriveOutcome> drives;
  final int copyFiles, copyBytes;

  /// What's been done, by state: "copied", "merged", "already-there", "duplicate", "conflict",
  /// "failed".
  final Map<String, ({int n, int bytes})> journal;

  bool get leavesOutCaches => schema.contains('caches left out');
}

/// GET /api/plan-dry: what applying a plan would do now.
class DryRun {
  DryRun.fromJson(Json j)
    : wouldCopy = j.integer('would_copy'),
      wouldCopyBytes = j.integer('would_copy_bytes'),
      wouldMerge = j.integer('would_merge'),
      resumed = j.integer('resumed'),
      duplicates = j.integer('duplicates'),
      conflicts = j.integer('conflicts');
  final int wouldCopy, wouldCopyBytes, wouldMerge, resumed, duplicates, conflicts;
}

/// A run of "Tidy the archive" (GET /api/dedups).
class DedupRun {
  DedupRun.fromJson(Json j)
    : id = j.integer('id'),
      createdAt = j.integer('created_at'),
      folder = j.str('folder'),
      link = j.str('link'),
      state = j.str('state'),
      summary = j.strOrNull('summary'),
      linked = j.integer('linked');
  final int id, createdAt;
  final String folder, link, state;
  final String? summary;
  final int linked;
}

// ---------- freeing up, rescues, mirrors, manifests

/// A wipe on record (GET /api/wipes).
class Wipe {
  Wipe.fromJson(Json j)
    : id = j.integer('id'),
      createdAt = j.integer('created_at'),
      paths = j.strs('paths'),
      moveTo = j.strOrNull('move_to'),
      state = j.str('state'),
      summary = j.strOrNull('summary');
  final int id, createdAt;
  final List<String> paths;
  final String? moveTo;

  /// "new", "running", "done", "partial" or "failed".
  final String state;
  final String? summary;
}

/// A rescue on record (GET /api/rescues).
class Rescue {
  Rescue.fromJson(Json j)
    : id = j.integer('id'),
      createdAt = j.integer('created_at'),
      updatedAt = j.integer('updated_at'),
      mode = j.str('mode'),
      source = j.str('source'),
      dest = j.strOrNull('dest'),
      options = j.obj('options'),
      state = j.str('state'),
      summary = j.strOrNull('summary'),
      result = j.obj('result');
  final int id, createdAt, updatedAt;

  /// "image" (copy a drive), "carve" (find files), "parts" (look for partitions), "files" (copy
  /// files out of a file system).
  final String mode;
  final String source;
  final String? dest;
  final Json options;

  /// "new", "running", "done", "stopped" or "failed".
  final String state;
  final String? summary;
  final Json result;
}

/// A folder kept copied to another drive (GET /api/mirrors).
class Mirror {
  Mirror.fromJson(Json j)
    : id = j.integer('id'),
      source = j.str('source'),
      target = j.str('target'),
      createdAt = j.integer('created_at'),
      verifyEveryDays = j.integer('verify_every_days'),
      lastSync = j.intOrNull('last_sync'),
      lastSyncSummary = j.strOrNull('last_sync_summary'),
      lastVerify = j.intOrNull('last_verify'),
      lastVerifySummary = j.strOrNull('last_verify_summary'),
      lastVerifyOk = j.flagOrNull('last_verify_ok'),
      connected = j.flag('connected');
  final int id;
  final String source, target;
  final int createdAt, verifyEveryDays;
  final int? lastSync;
  final String? lastSyncSummary;
  final int? lastVerify;
  final String? lastVerifySummary;
  final bool? lastVerifyOk;

  /// Both drives are here now.
  final bool connected;
}

/// A signed manifest of a drive (GET /api/manifests).
class Manifest {
  Manifest.fromJson(Json j)
    : id = j.integer('id'),
      rootId = j.integer('root_id'),
      scanId = j.integer('scan_id'),
      createdAt = j.integer('created_at'),
      merkle = j.str('merkle'),
      drive = j.str('drive'),
      files = j.integer('files'),
      bytes = j.integer('bytes'),
      hashed = j.integer('hashed'),
      fingerprint = j.str('fingerprint');
  final int id, rootId, scanId, createdAt;
  final String merkle, drive;
  final int files, bytes, hashed;

  /// The signing key's fingerprint.
  final String fingerprint;
}

// ---------- this BackDown (/api/features, /api/update, /api/scan-settings)

class Licence {
  Licence.fromJson(Json j)
    : id = j.str('id'),
      product = j.str('product'),
      kind = j.str('kind'),
      seats = j.integer('seats', 1),
      email = j.str('email'),
      name = j.strOrNull('name'),
      issued = j.str('issued'),
      expires = j.strOrNull('expires');
  final String id, product, kind;
  final int seats;
  final String email;
  final String? name;
  final String issued;
  final String? expires;
}

class Feature {
  Feature.fromJson(Json j)
    : key = j.str('key'),
      name = j.str('name'),
      unlocks = j.str('unlocks'),
      tier = j.str('tier', 'community'),
      tierName = j.str('tier_name', 'Community'),
      ready = j.flagOrNull('ready') ?? true,
      allowed = j.flagOrNull('allowed') ?? true;
  final String key, name, unlocks, tier, tierName;
  final bool ready, allowed;
}

/// GET /api/features: the edition, the preview policy, the licence, and every feature.
class Features {
  Features.fromJson(Json j)
    : version = j.str('version'),
      tier = j.str('tier', 'community'),
      tierName = j.str('tier_name', 'Community'),
      preview = j.flag('preview'),
      summary = j.str('summary'),
      licence = j.objOrNull('licence') == null ? null : Licence.fromJson(j.obj('licence')),
      licenceProblem = j.strOrNull('licence_problem'),
      licenceFile = j.str('licence_file'),
      pricing = j.str('pricing', 'https://backdown.io/pricing'),
      features = [for (final f in j.list('features')) Feature.fromJson(f)];
  final String version, tier, tierName;

  /// A preview release: everything is unlocked.
  final bool preview;
  final String summary;
  final Licence? licence;
  final String? licenceProblem;
  final String licenceFile, pricing;
  final List<Feature> features;
}

/// GET /api/update.
class UpdateInfo {
  UpdateInfo.fromJson(Json j)
    : canInstall = j.flag('can_install'),
      whyNot = j.strOrNull('why_not'),
      checkDaily = j.flag('enabled'),
      installState = j.obj('install').str('state'),
      installMessage = j.obj('install').str('message'),
      current = j.obj('status').str('current'),
      channel = j.obj('status').str('channel'),
      checkedAt = j.obj('status').intOrNull('checked_at'),
      newer = j.obj('status').flag('newer'),
      error = j.obj('status').strOrNull('error'),
      blockedUntil = j.obj('status').intOrNull('blocked_until'),
      latestVersion = j.obj('status').objOrNull('latest')?.strOrNull('version'),
      notesUrl = j.obj('status').objOrNull('latest')?.strOrNull('notes_url'),
      publishedAt = j.obj('status').objOrNull('latest')?.strOrNull('published_at');

  final bool canInstall;
  final String? whyNot;

  /// It looks for updates once a day.
  final bool checkDaily;
  final String installState, installMessage;
  final String current;

  /// "stable" or "preview".
  final String channel;
  final int? checkedAt;
  final bool newer;
  final String? error;
  final int? blockedUntil;
  final String? latestVersion, notesUrl, publishedAt;
}

int _n(Object? v) => v is num ? v.toInt() : int.tryParse('$v') ?? 0;

// ---------- paths

final _sep = RegExp(r'[\\/]');

/// The last name in a path.
String leafOf(String p) {
  final t = p.replaceFirst(RegExp(r'[\\/]+$'), '');
  final i = t.lastIndexOf(_sep);
  return i < 0 ? t : t.substring(i + 1);
}

/// The folder a path is in.
String folderOf(String p) {
  final t = p.replaceFirst(RegExp(r'[\\/]+$'), '');
  final i = t.lastIndexOf(_sep);
  if (i < 0) return p;
  return i == 0 ? t.substring(0, 1) : t.substring(0, i);
}
