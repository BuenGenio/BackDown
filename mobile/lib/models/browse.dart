import '../api/json.dart';

/// An indexed drive or folder (GET /api/roots).
class Root {
  Root.fromJson(Json j)
    : id = j.integer('id'),
      name = j.str('name'),
      path = j.str('path'),
      key = j.str('key'),
      lastScan = j.intOrNull('last_scan'),
      scannedAt = j.intOrNull('scanned_at'),
      files = j.integer('files'),
      bytes = j.integer('bytes');

  final int id;
  final String name;
  final String path;
  final String key;
  final int? lastScan;
  final int? scannedAt;
  final int files;
  final int bytes;
}

/// Totals over the whole index (GET /api/index-status).
class IndexStatus {
  IndexStatus.fromJson(Json j)
    : files = j.integer('files'),
      bytes = j.integer('bytes'),
      hashed = j.integer('hashed'),
      sampled = j.integer('sampled'),
      scans = j.integer('scans'),
      unreadable = j.integer('unreadable'),
      versions = j.integer('versions');

  final int files, bytes, hashed, sampled, scans, unreadable, versions;
}

class Crumb {
  Crumb.fromJson(Json j) : key = j.str('key'), name = j.str('name');
  final String key;
  final String name;
}

/// A file or folder in a listing.
class Entry {
  Entry.fromJson(Json j)
    : kind = j.str('kind', 'file'),
      name = j.str('name'),
      key = j.str('key'),
      id = j.intOrNull('id'),
      size = j.integer('size'),
      files = j.intOrNull('files'),
      mtime = j.integer('mtime'),
      sha256 = j.strOrNull('sha256'),
      copies = j.integer('copies'),
      versions = j.integer('versions', 1),
      newHere = j.flag('new_here'),
      later = j.str('later', 'kept');

  /// "dir" or "file".
  final String kind;
  final String name;
  final String key;

  /// The file's index id (what its bytes and details are asked by); null for folders.
  final int? id;

  /// Folders: the bytes of everything below.
  final int size;

  /// Folders: how many files are below.
  final int? files;

  /// Folders: the newest file below.
  final int mtime;
  final String? sha256;

  /// Other current files with identical content.
  final int copies;
  final int versions;

  /// Appeared or changed at the scan being viewed.
  final bool newHere;

  /// What happened after the scan being viewed: "kept", "changed" or "deleted".
  final String later;

  bool get isDir => kind == 'dir';
  bool get hidden => name.startsWith('.');
}

/// A folder at one scan (GET /api/ls?dir=KEY[&scan=N]).
class Listing {
  Listing.fromJson(Json j)
    : rootId = j.integer('root_id'),
      scan = j.integer('scan'),
      latestScan = j.integer('latest_scan'),
      firstScan = j.integer('first_scan'),
      scannedAt = j.integer('scanned_at'),
      name = j.str('name'),
      path = j.str('path'),
      key = j.str('key'),
      crumbs = [for (final c in j.list('crumbs')) Crumb.fromJson(c)],
      files = j.integer('files'),
      bytes = j.integer('bytes'),
      entries = [for (final e in j.list('entries')) Entry.fromJson(e)],
      needsReindex = j.flag('needs_reindex');

  final int rootId, scan, latestScan, firstScan, scannedAt;
  final String name, path, key;
  final List<Crumb> crumbs;
  final int files, bytes;
  final List<Entry> entries;
  final bool needsReindex;

  bool get isPast => scan != latestScan;
}

/// One version of a path, as scans saw it.
class FileVersion {
  FileVersion.fromJson(Json j)
    : id = j.integer('id'),
      size = j.integer('size'),
      mtime = j.integer('mtime'),
      sha256 = j.strOrNull('sha256'),
      firstScan = j.integer('first_scan'),
      lastScan = j.integer('last_scan'),
      from = j.integer('from'),
      to = j.integer('to'),
      current = j.flag('current'),
      readable = j.flag('readable');

  final int id, size, mtime, firstScan, lastScan, from, to;
  final String? sha256;
  final bool current;

  /// Its bytes can still be read from somewhere.
  final bool readable;
}

/// Another file here with the same content.
class LocalCopy {
  LocalCopy.fromJson(Json j)
    : id = j.integer('id'),
      path = j.str('path'),
      key = j.str('key'),
      dirKey = j.str('dir_key');
  final int id;
  final String path, key, dirKey;
}

/// The same content on another machine (from its last sync).
class RemoteCopy {
  RemoteCopy.fromJson(Json j)
    : machineId = j.integer('machine_id'),
      rid = j.integer('rid'),
      machine = j.str('machine'),
      path = j.str('path'),
      confirmedAt = j.intOrNull('confirmed_at'),
      counts = j.flag('counts'),
      whyNot = j.strOrNull('why_not');
  final int machineId;

  /// The file's id in that machine's index (its details: /more/machines/ID/file/RID).
  final int rid;
  final String machine, path;
  final int? confirmedAt;

  /// Counts as a verified copy: hashed there, readable, confirmed recently.
  final bool counts;
  final String? whyNot;
}

/// A file's details (GET /api/file?id=N).
class FileInfo {
  FileInfo.fromJson(Json j)
    : id = j.integer('id'),
      name = j.str('name'),
      path = j.str('path'),
      key = j.str('key'),
      dirKey = j.str('dir_key'),
      size = j.integer('size'),
      mtime = j.integer('mtime'),
      sha256 = j.strOrNull('sha256'),
      error = j.strOrNull('error'),
      current = j.flag('current'),
      versions = [for (final v in j.list('versions')) FileVersion.fromJson(v)],
      copies = [for (final c in j.list('copies')) LocalCopy.fromJson(c)],
      remoteCopies = [for (final c in j.list('remote_copies')) RemoteCopy.fromJson(c)],
      contentFrom = j.strOrNull('content_from'),
      proofDays = j.intOrNull('proof_days');

  final int id;
  final String name, path, key, dirKey;
  final int size, mtime;
  final String? sha256;

  /// Why it couldn't be read, when the index noted that.
  final String? error;

  /// Still there at the drive's latest scan.
  final bool current;
  final List<FileVersion> versions;
  final List<LocalCopy> copies;
  final List<RemoteCopy> remoteCopies;

  /// Where the bytes would come from if this exact file is gone ("" when it's this file).
  final String? contentFrom;
  final int? proofDays;
}

/// A match from the ranked name search (GET /api/find).
class FindHit {
  FindHit.fromJson(Json j)
    : kind = j.str('kind', 'file'),
      id = j.intOrNull('id'),
      name = j.str('name'),
      path = j.str('path'),
      key = j.str('key'),
      dirKey = j.str('dir_key'),
      size = j.integer('size'),
      mtime = j.integer('mtime'),
      root = j.integer('root'),
      drive = j.str('drive'),
      folder = j.str('folder'),
      rank = j.integer('rank'),
      gone = j.flag('gone'),
      seen = j.intOrNull('seen'),
      scan = j.intOrNull('scan');

  final String kind;
  final int? id;
  final String name, path, key, dirKey;
  final int size, mtime, root;

  /// The drive's name, and the folder as a path inside it ("" at its top).
  final String drive, folder;

  /// 4 the name itself, 3 starts with it, 2 a word in it starts with it, 1 has it somewhere.
  final int rank;

  /// Not there any more: last seen at [scan], which finished at [seen].
  final bool gone;
  final int? seen, scan;

  bool get isDir => kind == 'dir';
}

class FindResult {
  FindResult.fromJson(Json j)
    : hits = [for (final h in j.list('hits')) FindHit.fromJson(h)],
      total = j.integer('total'),
      gone = j.integer('gone'),
      ms = j.integer('ms');
  final List<FindHit> hits;
  final int total, gone, ms;
}

/// A finished scan of a drive (GET /api/timeline?root=N).
class ScanPoint {
  ScanPoint.fromJson(Json j)
    : id = j.integer('id'),
      finishedAt = j.integer('finished_at'),
      files = j.integer('files'),
      bytes = j.integer('bytes');
  final int id, finishedAt, files, bytes;
}
