import '../../api/json.dart';

/// What the viewers read from the service: one class per answer, built with the `Json` helpers so
/// an older or newer service never takes a viewer down.

// ---- mail (peek/mail.rs, mailbox/)

class MailAttachment {
  MailAttachment.fromJson(Json j)
    : part = j.integer('part'),
      name = j.str('name', 'attachment'),
      type = j.strOrNull('type'),
      size = j.integer('size');
  final int part;
  final String name;
  final String? type;
  final int size;
}

/// One message, parsed by the service (GET /api/mail?id=N[&at=OFFSET]).
class MailMessage {
  MailMessage.fromJson(Json j)
    : from = j.str('from'),
      to = j.str('to'),
      cc = j.str('cc'),
      replyTo = j.str('reply_to'),
      subject = j.str('subject'),
      date = j.intOrNull('date'),
      dateText = j.strOrNull('date_text'),
      text = j.str('text'),
      fromHtml = j.flag('from_html'),
      attachments = [for (final a in j.list('attachments')) MailAttachment.fromJson(a)],
      truncated = j.flag('truncated');

  final String from, to, cc, replyTo, subject;
  final int? date;
  final String? dateText;
  final String text;

  /// It only had an HTML version, turned into plain text by the service.
  final bool fromHtml;
  final List<MailAttachment> attachments;
  final bool truncated;
}

/// How far the service has read a mailbox (every /api/threads and /api/thread answer has it).
class MailboxStatus {
  MailboxStatus.fromJson(Json j)
    : kind = j.str('kind', 'mbox'),
      messages = j.integer('messages'),
      read = j.integer('read'),
      size = j.integer('size'),
      done = j.flag('done'),
      capped = j.flag('capped'),
      unreadable = j.integer('unreadable');

  /// "mbox" or "maildir".
  final String kind;
  final int messages;

  /// Bytes (mbox) or files (Maildir) read so far, of [size].
  final int read, size;
  final bool done;

  /// Stopped at the most messages it threads.
  final bool capped;
  final int unreadable;

  bool get isMbox => kind == 'mbox';
}

class ThreadSummary {
  ThreadSummary.fromJson(Json j)
    : key = j.integer('key'),
      subject = j.str('subject'),
      who = j.strs('who'),
      people = j.integer('people'),
      count = j.integer('count', 1),
      first = j.intOrNull('first'),
      last = j.intOrNull('last'),
      unread = j.integer('unread'),
      flags = j.str('flags'),
      hits = j.integer('hits');

  /// Its first message's key: an mbox offset or a Maildir file's index id.
  final int key;
  final String subject;
  final List<String> who;
  final int people, count;
  final int? first, last;
  final int unread;

  /// Maildir flag letters: F flagged, R replied, P forwarded, D draft, T trashed.
  final String flags;
  final int hits;

  String get people4 {
    if (who.isEmpty) return '(unknown sender)';
    final more = people > who.length ? ' +${people - who.length}' : '';
    return '${who.join(', ')}$more';
  }
}

class MailFolder {
  MailFolder.fromJson(Json j)
    : name = j.str('name'),
      key = j.str('key'),
      count = j.integer('count'),
      fresh = j.integer('new'),
      here = j.flag('here');
  final String name, key;
  final int count, fresh;
  final bool here;
}

/// A page of a mailbox's threads (GET /api/threads).
class MailboxPage {
  MailboxPage.fromJson(Json j)
    : status = MailboxStatus.fromJson(j),
      threads = [for (final t in j.list('threads')) ThreadSummary.fromJson(t)],
      total = j.integer('total'),
      offset = j.integer('offset'),
      folders = [for (final f in j.list('folders')) MailFolder.fromJson(f)],
      name = j.strOrNull('name');

  final MailboxStatus status;
  final List<ThreadSummary> threads;
  final int total, offset;

  /// A Maildir's family (inbox, Sent, …), on the first page; empty when it's alone.
  final List<MailFolder> folders;
  final String? name;
}

class ThreadMessage {
  ThreadMessage.fromJson(Json j)
    : key = j.integer('key'),
      from = j.str('from'),
      subject = j.str('subject'),
      date = j.intOrNull('date'),
      size = j.integer('size'),
      depth = j.integer('depth'),
      flags = j.str('flags'),
      unread = j.flag('unread'),
      dup = j.flag('dup'),
      orphan = j.flag('orphan'),
      hit = j.flag('hit');
  final int key;
  final String from, subject;
  final int? date;
  final int size, depth;
  final String flags;
  final bool unread, dup, orphan, hit;
}

/// One conversation (GET /api/thread).
class MailThread {
  MailThread.fromJson(Json j)
    : status = MailboxStatus.fromJson(j),
      thread = ThreadSummary.fromJson(j.obj('thread')),
      who = j.strs('who'),
      messages = [for (final m in j.list('messages')) ThreadMessage.fromJson(m)];
  final MailboxStatus status;
  final ThreadSummary thread;
  final List<String> who;
  final List<ThreadMessage> messages;
}

// ---- archives (peek/archives.rs)

class ArchiveEntry {
  ArchiveEntry.fromJson(Json j)
    : name = j.str('name'),
      size = j.intOrNull('size'),
      mtime = j.intOrNull('mtime'),
      dir = j.flag('dir'),
      encrypted = j.flag('encrypted'),
      link = j.strOrNull('link');

  /// Its whole name in the archive: what /api/entry asks for.
  final String name;
  final int? size, mtime;
  final bool dir, encrypted;
  final String? link;
}

/// A folder of an archive's listing, its implied parents made up, sizes and counts summed.
class ArchiveDir {
  ArchiveDir(this.name);
  final String name;
  final Map<String, ArchiveDir> dirs = {};
  final List<(ArchiveEntry, String)> files = [];
  int size = 0, count = 0;
  int? mtime;
}

class ArchiveListing {
  ArchiveListing.fromJson(Json j)
    : format = j.str('format', 'archive'),
      entries = [for (final e in j.list('entries')) ArchiveEntry.fromJson(e)],
      total = j.intOrNull('total'),
      counted = j.integer('counted'),
      totalSize = j.integer('total_size'),
      complete = j.flag('complete'),
      note = j.strOrNull('note') {
    root = _tree(entries);
  }

  final String format;
  final List<ArchiveEntry> entries;
  final int? total;
  final int counted, totalSize;
  final bool complete;
  final String? note;
  late final ArchiveDir root;

  int get count => total ?? counted;

  static ArchiveDir _tree(List<ArchiveEntry> entries) {
    final root = ArchiveDir('');
    for (final e in entries) {
      final parts = [
        for (final p in e.name.split(RegExp(r'[\\/]')))
          if (p.isNotEmpty && p != '.') p,
      ];
      if (parts.isEmpty) continue;
      var n = root;
      for (final p in e.dir ? parts : parts.sublist(0, parts.length - 1)) {
        n = n.dirs.putIfAbsent(p, () => ArchiveDir(p));
      }
      if (e.dir) {
        n.mtime = e.mtime;
      } else {
        n.files.add((e, parts.last));
      }
    }
    void sum(ArchiveDir n) {
      for (final c in n.dirs.values) {
        sum(c);
        n.size += c.size;
        n.count += c.count;
      }
      for (final (f, _) in n.files) {
        n.size += f.size ?? 0;
        n.count++;
      }
    }

    sum(root);
    return root;
  }
}

// ---- disk images and their filesystems (peek/disk.rs, peek/fsview.rs)

/// A filesystem as the service recognised it (a guess from its headers, or what it read).
class FsKind {
  FsKind.fromJson(Json j)
    : kind = j.str('kind'),
      label = j.strOrNull('label'),
      size = j.intOrNull('size'),
      notes = j.strs('notes');
  final String kind;
  final String? label;
  final int? size;
  final List<String> notes;

  /// The ones whose files BackDown lists itself.
  bool get browsable => RegExp(r'^(FAT(12|16|32)?|exFAT|ext[234]|NTFS)$').hasMatch(kind);

  String get text => label == null || label!.isEmpty ? kind : '$kind “$label”';
}

class Partition {
  Partition.fromJson(Json j)
    : index = j.integer('index'),
      type = j.str('type'),
      code = j.str('code'),
      name = j.strOrNull('name'),
      start = j.integer('start'),
      size = j.integer('size'),
      bootable = j.flag('bootable'),
      fs = j.objOrNull('fs') == null ? null : FsKind.fromJson(j.obj('fs'));
  final int index;
  final String type, code;
  final String? name;
  final int start, size;
  final bool bootable;
  final FsKind? fs;
}

/// A disk image: its format, partition table, filesystems, and an ISO's files (GET /api/disk).
class DiskInfo {
  DiskInfo.fromJson(Json j)
    : format = j.str('format', 'unknown'),
      fileSize = j.integer('file_size'),
      virtualSize = j.intOrNull('virtual_size'),
      table = j.strOrNull('table'),
      partitions = [for (final p in j.list('partitions')) Partition.fromJson(p)],
      fs = j.objOrNull('fs') == null ? null : FsKind.fromJson(j.obj('fs')),
      label = j.strOrNull('label'),
      details = [
        if (j['details'] is List)
          for (final d in j['details'] as List)
            if (d is List && d.length >= 2) (d[0].toString(), d[1].toString()),
      ],
      entries = j['entries'] is List ? [for (final e in j.list('entries')) FsEntry.fromJson(e)] : null,
      truncated = j.flag('truncated'),
      dmgParts = [for (final p in j.list('dmg_parts')) (p.str('name'), p.integer('size'))];

  final String format;
  final int fileSize;
  final int? virtualSize;
  final String? table;
  final List<Partition> partitions;
  final FsKind? fs;
  final String? label;
  final List<(String, String)> details;

  /// An ISO's folder (its root when no path was asked for).
  final List<FsEntry>? entries;
  final bool truncated;
  final List<(String, int)> dmgParts;

  bool get known => format != 'unknown';
  bool get iso => format == 'iso9660';

  static const _names = {
    'iso9660': 'ISO 9660 disc image',
    'raw': 'Disk image',
    'dmg': 'Apple disk image (UDIF)',
    'vmdk': 'VMware virtual disk (VMDK)',
    'qcow2': 'QEMU virtual disk (qcow2)',
    'qcow': 'QEMU virtual disk (qcow)',
    'vdi': 'VirtualBox virtual disk (VDI)',
    'vhd': 'Virtual PC / Hyper-V disk (VHD)',
    'vhdx': 'Hyper-V virtual disk (VHDX)',
  };

  String get title => _names[format] ?? format;
}

class FsDeleted {
  FsDeleted.fromJson(Json j)
    : state = j.str('state'),
      confidence = j.integer('confidence'),
      why = j.str('why'),
      seg = j.str('seg');

  /// "recoverable", "partly overwritten", "overwritten" or "can't be read".
  final String state;
  final int confidence;
  final String why;

  /// Its path segment, to ask for it.
  final String seg;
}

/// A file or folder in a filesystem's listing (or an ISO's).
class FsEntry {
  FsEntry.fromJson(Json j)
    : name = j.str('name'),
      dir = j.flag('dir'),
      size = j.integer('size'),
      mtime = j.intOrNull('mtime'),
      link = j.strOrNull('link'),
      unreadable = j.strOrNull('unreadable'),
      flags = j.strs('flags'),
      deleted = j.objOrNull('deleted') == null ? null : FsDeleted.fromJson(j.obj('deleted'));
  final String name;
  final bool dir;
  final int size;
  final int? mtime;
  final String? link, unreadable;
  final List<String> flags;
  final FsDeleted? deleted;

  /// Its segment in a path to ask for it by.
  String get seg => deleted?.seg ?? name;
}

/// One folder of a filesystem in a disk image (GET /api/fs).
class FsListing {
  FsListing.fromJson(Json j)
    : fs = j.objOrNull('fs') == null ? null : FsKind.fromJson(j.obj('fs')),
      path = j.str('path'),
      entries = [for (final e in j.list('entries')) FsEntry.fromJson(e)],
      truncated = j.flag('truncated'),
      undelete = j.strOrNull('undelete'),
      inDeleted = j.flag('in_deleted'),
      deleted = j['deleted'] is List ? [for (final e in j.list('deleted')) FsEntry.fromJson(e)] : null,
      deletedNote = j.strOrNull('deleted_note');
  final FsKind? fs;
  final String path;
  final List<FsEntry> entries;
  final bool truncated;

  /// Why deleted files can't be listed on this filesystem, when they can't.
  final String? undelete;
  final bool inDeleted;
  final List<FsEntry>? deleted;
  final String? deletedNote;
}

// ---- SQLite (serve.rs sqlite())

class SqliteTable {
  SqliteTable.fromJson(Json j)
    : name = j.str('name'),
      kind = j.str('kind', 'table'),
      sql = j.strOrNull('sql'),
      rows = j.intOrNull('rows');
  final String name, kind;
  final String? sql;
  final int? rows;
}

class SqliteRows {
  SqliteRows.fromJson(Json j)
    : columns = j.strs('columns'),
      rows = [
        if (j['rows'] is List)
          for (final r in j['rows'] as List)
            if (r is List) List<Object?>.of(r),
      ],
      offset = j.integer('offset');
  final List<String> columns;
  final List<List<Object?>> rows;
  final int offset;
}

class SqliteInfo {
  SqliteInfo.fromJson(Json j)
    : tables = [for (final t in j.list('tables')) SqliteTable.fromJson(t)],
      data = j.objOrNull('data') == null ? null : SqliteRows.fromJson(j.obj('data'));
  final List<SqliteTable> tables;
  final SqliteRows? data;
}

// ---- spreadsheets and slides (peek/sheets.rs, peek/slides.rs)

class SheetPage {
  SheetPage.fromJson(Json j)
    : sheets = [for (final s in j.list('sheets')) (s.str('name'), s.flag('hidden'))],
      sheet = j.str('sheet'),
      offset = j.integer('offset'),
      rows = [
        if (j['rows'] is List)
          for (final r in j['rows'] as List)
            if (r is List) List<Object?>.of(r) else <Object?>[],
      ],
      width = j.integer('width'),
      totalRows = j.intOrNull('total_rows'),
      totalCols = j.intOrNull('total_cols'),
      more = j.flag('more'),
      cut = j.flag('cut'),
      early = j.flag('early');
  final List<(String, bool)> sheets;
  final String sheet;
  final int offset;
  final List<List<Object?>> rows;
  final int width;
  final int? totalRows, totalCols;
  final bool more, cut, early;
}

class Slide {
  Slide.fromJson(Json j)
    : n = j.integer('n'),
      title = j.strOrNull('title'),
      paras = [for (final p in j.list('paras')) (p.str('t'), p.integer('lvl'))],
      images = j.strs('images'),
      notes = j.str('notes'),
      cut = j.flag('cut');
  final int n;
  final String? title;
  final List<(String, int)> paras;
  final List<String> images;
  final String notes;
  final bool cut;
}

class SlideDeck {
  SlideDeck.fromJson(Json j)
    : format = j.str('format'),
      count = j.integer('count'),
      slides = [for (final s in j.list('slides')) Slide.fromJson(s)];
  final String format;
  final int count;
  final List<Slide> slides;
}

// ---- SQL dumps (sqldump/)

class SqlColumn {
  SqlColumn.fromJson(Json j)
    : name = j.str('name'),
      type = j.str('type'),
      nullable = j.flag('null'),
      dflt = j.strOrNull('default'),
      pk = j.flag('pk'),
      auto = j.flag('auto'),
      extra = j.strs('extra'),
      comment = j.strOrNull('comment');
  final String name, type;
  final bool nullable;
  final String? dflt;
  final bool pk, auto;
  final List<String> extra;
  final String? comment;
}

class SqlForeignKey {
  SqlForeignKey.fromJson(Json j)
    : name = j.strOrNull('name'),
      columns = j.strs('columns'),
      ref = j.str('ref'),
      refColumns = j.strs('ref_columns'),
      onDelete = j.strOrNull('on_delete'),
      onUpdate = j.strOrNull('on_update');
  final String? name;
  final List<String> columns;
  final String ref;
  final List<String> refColumns;
  final String? onDelete, onUpdate;
}

class SqlTable {
  SqlTable.fromJson(Json j)
    : key = j.str('key'),
      schema = j.str('schema'),
      name = j.str('name'),
      defined = j.flag('defined'),
      columns = [for (final c in j.list('columns')) SqlColumn.fromJson(c)],
      primaryKey = j.strs('primary_key'),
      indexes = [for (final i in j.list('indexes')) (i.strOrNull('name'), i.strs('columns'), i.str('kind', 'index'))],
      foreignKeys = [for (final k in j.list('foreign_keys')) SqlForeignKey.fromJson(k)],
      checks = j.integer('checks'),
      rows = j.integer('rows'),
      uncounted = j.flag('uncounted'),
      bytes = j.integer('bytes'),
      statements = j.integer('statements'),
      copy = j.flag('copy'),
      engine = j.strOrNull('engine'),
      charset = j.strOrNull('charset'),
      comment = j.strOrNull('comment'),
      note = j.strOrNull('note'),
      sql = j.strOrNull('sql');
  final String key, schema, name;

  /// The dump has its CREATE TABLE (not only data for it).
  final bool defined;
  final List<SqlColumn> columns;
  final List<String> primaryKey;
  final List<(String?, List<String>, String)> indexes;
  final List<SqlForeignKey> foreignKeys;
  final int checks, rows;
  final bool uncounted;
  final int bytes, statements;
  final bool copy;
  final String? engine, charset, comment, note, sql;
}

class SqlObject {
  SqlObject.fromJson(Json j)
    : schema = j.str('schema'),
      name = j.str('name'),
      kind = j.str('kind'),
      on = j.strOrNull('on');
  final String schema, name, kind;
  final String? on;
}

/// A SQL dump's schema as far as the service has read it (GET /api/sqldump).
class SqlDump {
  SqlDump.fromJson(Json j)
    : format = j.str('format', 'sql'),
      dialect = j.str('dialect', 'sql'),
      compression = j.strOrNull('compression'),
      facts = [
        if (j['facts'] is List)
          for (final f in j['facts'] as List)
            if (f is List && f.length >= 2) (f[0].toString(), f[1].toString()),
      ],
      schemas = j.strs('schemas'),
      tables = [for (final t in j.list('tables')) SqlTable.fromJson(t)],
      views = _objs(j, 'views'),
      triggers = _objs(j, 'triggers'),
      routines = _objs(j, 'routines'),
      events = _objs(j, 'events'),
      sequences = _objs(j, 'sequences'),
      other = _objs(j, 'other'),
      size = j.integer('size'),
      read = j.integer('read'),
      complete = j.flag('complete'),
      cut = j.strOrNull('cut'),
      notes = j.strs('notes'),
      elapsedMs = j.integer('elapsed_ms'),
      more = j.strOrNull('more');

  static List<SqlObject> _objs(Json j, String k) => [for (final o in j.list(k)) SqlObject.fromJson(o)];

  /// "sql", or what else the file turned out to be ("pg_dump custom format", binary…).
  final String format;
  final String dialect;
  final String? compression;
  final List<(String, String)> facts;
  final List<String> schemas;
  final List<SqlTable> tables;
  final List<SqlObject> views, triggers, routines, events, sequences, other;
  final int size, read;
  final bool complete;

  /// Why the dump looks cut off, when it does.
  final String? cut;
  final List<String> notes;
  final int elapsedMs;

  /// The token to read on with, while there's more.
  final String? more;

  bool get isSql => format == 'sql';
  int get objects => views.length + triggers.length + routines.length + events.length + sequences.length + other.length;

  String get title => switch (dialect) {
    'mysql' => 'MySQL dump',
    'postgres' => 'PostgreSQL dump',
    'sqlite' => 'SQLite dump',
    _ => 'SQL dump',
  };
}
