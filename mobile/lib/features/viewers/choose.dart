import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../api/api.dart';
import '../../api/json.dart';
import '../../core/kinds.dart';
import '../../models/file_ref.dart';
import '../../providers.dart';
import 'archive_view.dart';
import 'common.dart';
import 'disk_view.dart';
import 'fallback.dart';
import 'highlight.dart';
import 'image_view.dart';
import 'mail_view.dart';
import 'mailbox_view.dart';
import 'media_view.dart';
import 'models.dart';
import 'pdf_view.dart';
import 'sheet_view.dart';
import 'slides_view.dart';
import 'sqldump_view.dart';
import 'sqlite_view.dart';
import 'text_view.dart';

// containers the service parses (peek/), as the web UI's viewers.js has them
const _sheets = {'xlsx', 'xlsm', 'xlsb', 'xls', 'xltx', 'xltm', 'ods'};
const _decks = {'pptx', 'pptm', 'ppsx', 'odp'};
const _archives = {
  'zip', 'jar', 'apk', 'epub', 'war', 'cbz', 'whl', 'tar', 'gz', 'tgz', 'bz2', 'tbz', 'tbz2', 'xz', 'txz', //
  'zst', 'tzst',
};
const _disks = {'iso', 'img', 'dd', 'raw', 'bin', 'dmg', 'vmdk', 'vdi', 'qcow', 'qcow2', 'vhd', 'vhdx'};

/// Files shown as text by their name alone.
final _texty = {
  ...'txt md markdown rst log nfo csv tsv eml mbox env gitignore gitattributes editorconfig lock'.split(' '),
  ...'js mjs cjs jsx ts tsx py rs go c h cc cpp hpp java kt swift rb php pl lua r sh bash zsh ps1 css scss less html '
          'htm xml svg json yaml yml toml ini cfg conf sql diff patch makefile dockerfile cs dart scala vb m mm asm '
          'gradle vue svelte'
      .split(' '),
};

bool isTextByName(String name) => _texty.contains(extensionOf(name));

bool isSqlDump(String name) =>
    RegExp(r'\.(sql|mysql|pgsql|psql|dump)(\.(gz|bz2|xz|zst))?$', caseSensitive: false).hasMatch(name);

/// The most of an unknown file shown as text when it turns out to be text.
const _textGuessMax = 8 << 20;

enum ViewKind {
  image,
  text,
  pdf,
  video,
  audio,
  mail,
  mailbox,
  archive,
  disk,
  sqlite,
  sqldump,
  sheet,
  slides,
  fallback,
  error,
}

/// Which viewer a file gets, with what deciding it already read (a dump's first pass, a disk
/// image's layout, a database's tables), so it isn't asked for twice.
class Choice {
  const Choice(this.kind, [this.data]);
  final ViewKind kind;
  final Object? data;
}

/// Why a file has no viewer here.
class NoPreview {
  const NoPreview(this.title, [this.message, this.head]);
  final String title;
  final String? message;
  final Uint8List? head;
}

/// Chooses as the web UI does (viewers.js render): mail by what's inside when the name says
/// nothing, then sheets, decks, mail, dumps, archives and disk images (each only if the service
/// recognises it), pictures, PDFs, audio and video, databases, and text by name or by its first
/// bytes; anything else gets the fallback.
Future<Choice> chooseViewer(BackDownApi api, ViewSource s) async {
  var x = extensionOf(s.name);
  final k = FileKind.of(s.name);
  if (k == FileKind.file) {
    // no telling extension: maybe mail (Thunderbird's "Inbox", a Maildir message's long name)
    try {
      final kind = asJson(await api.get('mail-kind', {'id': s.id})).strOrNull('kind');
      if (kind == 'mbox') x = 'mbox';
      if (kind == 'message') x = 'eml';
    } on Object {
      // not mail, or the service can't say: carry on by name
    }
  }
  if (_sheets.contains(x)) return const Choice(ViewKind.sheet);
  if (_decks.contains(x)) return const Choice(ViewKind.slides);
  if (x == 'eml') return const Choice(ViewKind.mail);
  if (x == 'mbox' || x == 'mbx') return const Choice(ViewKind.mailbox);
  if (isSqlDump(s.name)) {
    try {
      final d = await fetchParsed(api, 'sqldump', {'id': s.id}, SqlDump.fromJson);
      if (d.isSql) return Choice(ViewKind.sqldump, d);
    } on ApiException {
      // not a dump after all (or unreadable): the next viewers say
    }
  }
  if (_archives.contains(x)) return const Choice(ViewKind.archive);
  if (_disks.contains(x)) {
    try {
      final d = await fetchParsed(api, 'disk', {'id': s.id}, DiskInfo.fromJson);
      if (d.known) return Choice(ViewKind.disk, d);
    } on ApiException {
      // not a disk image the service knows: carry on
    }
  }
  if (k == FileKind.image) {
    if (isDecodableImage(s.name)) return const Choice(ViewKind.image);
    return Choice(
      ViewKind.fallback,
      NoPreview(
        'No preview for ${x.toUpperCase()} pictures here',
        'The phone can\'t show this format itself; another app on it may.',
      ),
    );
  }
  if (x == 'pdf') return const Choice(ViewKind.pdf);
  if (k == FileKind.doc) {
    return const Choice(
      ViewKind.fallback,
      NoPreview('No preview for documents here', 'Open it in another app on this phone, such as a word processor.'),
    );
  }
  if (k == FileKind.audio) return const Choice(ViewKind.audio);
  if (k == FileKind.video) return const Choice(ViewKind.video);
  if (k == FileKind.db) {
    final info = await _sqlite(api, s.id);
    if (info != null) return Choice(ViewKind.sqlite, info);
  }
  if (isTextByName(s.name)) return const Choice(ViewKind.text);
  final Uint8List head;
  try {
    head = await s.bytes(api, 65536);
  } on ApiException catch (e) {
    return Choice(ViewKind.error, e);
  }
  if (k != FileKind.db && isSqliteHeader(head)) {
    final info = await _sqlite(api, s.id);
    if (info != null) return Choice(ViewKind.sqlite, info);
  }
  if ((s.size ?? 0) < _textGuessMax && head.isNotEmpty && looksText(head)) return const Choice(ViewKind.text);
  if (head.isEmpty) return const Choice(ViewKind.fallback, NoPreview('Empty file', 'There\'s nothing in it to show.'));
  return Choice(
    ViewKind.fallback,
    NoPreview('No preview for this kind of file', 'Another app on this phone may open it.', head),
  );
}

Future<SqliteInfo?> _sqlite(BackDownApi api, int id) async {
  try {
    return await fetchParsed(api, 'sqlite', {'id': id}, SqliteInfo.fromJson);
  } on ApiException {
    return null;
  }
}

final choiceProvider = FutureProvider.autoDispose.family<Choice, ViewSource>(
  (ref, s) => chooseViewer(requireApi(ref), s),
);

/// A file from a drive in the viewer that suits it.
class FileViewer extends ConsumerWidget {
  const FileViewer({super.key, required this.file});
  final FileRef file;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final s = ViewSource.file(file);
    final choice = ref.watch(choiceProvider(s));
    return switch (choice) {
      AsyncValue(:final value?) => _viewer(s, value),
      AsyncValue(:final error?) => viewerError(s, error, () => ref.invalidate(choiceProvider(s))),
      _ => const Center(child: CircularProgressIndicator()),
    };
  }

  Widget _viewer(ViewSource s, Choice c) => switch (c.kind) {
    ViewKind.image => ImageView(source: s),
    ViewKind.text => TextView(source: s),
    ViewKind.pdf => PdfView(source: s),
    ViewKind.video => MediaView(source: s, video: true),
    ViewKind.audio => MediaView(source: s, video: false),
    ViewKind.mail => MailView(at: (s.id, null)),
    ViewKind.mailbox => MailboxView(source: MailboxSource.mbox(s.id)),
    ViewKind.archive => ArchiveView(source: s),
    ViewKind.disk => DiskView(id: s.id, name: s.name, disk: c.data! as DiskInfo),
    ViewKind.sqlite => SqliteView(id: s.id, info: c.data! as SqliteInfo),
    ViewKind.sqldump => SqlDumpView(source: s, first: c.data! as SqlDump),
    ViewKind.sheet => SheetView(source: s),
    ViewKind.slides => SlidesView(source: s),
    ViewKind.fallback => FallbackView(
      source: s,
      title: (c.data! as NoPreview).title,
      message: (c.data! as NoPreview).message,
      head: (c.data! as NoPreview).head,
    ),
    ViewKind.error => FallbackView.error(s, c.data!),
  };
}

/// A preview of a file inside a container, by its kind: pictures, audio and video, PDFs, text and
/// code (by name or by its first bytes); anything else gets the fallback, with its first bytes.
class InnerViewer extends ConsumerWidget {
  const InnerViewer({super.key, required this.file});
  final InnerFile file;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final s = ViewSource.inner(file);
    final k = FileKind.of(file.base);
    final big = (file.size ?? 0) > entryMax;
    if (big && !file.streams) {
      return FallbackView(
        source: s,
        title: 'Too large to open from inside the container',
        message: 'BackDown hands over files of up to 64 MB from inside archives.',
        canOpen: false,
      );
    }
    if (k == FileKind.image) {
      if (isDecodableImage(file.base)) return ImageView(source: s);
      return FallbackView(source: s, title: 'No preview for ${extensionOf(file.base).toUpperCase()} pictures here');
    }
    if (k == FileKind.audio || k == FileKind.video) return MediaView(source: s, video: k == FileKind.video);
    if (extensionOf(file.base) == 'pdf') return PdfView(source: s);
    if (isTextByName(file.base) || languageOf(file.base) != null) return TextView(source: s);
    final head = ref.watch(_innerHeadProvider(file));
    return switch (head) {
      AsyncValue(:final value?) =>
        looksText(value) && value.isNotEmpty
            ? TextView(source: s)
            : FallbackView(
                source: s,
                title: value.isEmpty ? 'Empty file' : 'No preview for this kind of file',
                message: value.isEmpty ? null : 'Another app on this phone may open it.',
                head: value,
              ),
      AsyncValue(:final error?) => viewerError(s, error, () => ref.invalidate(_innerHeadProvider(file))),
      _ => const Center(child: CircularProgressIndicator()),
    };
  }
}

final _innerHeadProvider = FutureProvider.autoDispose.family<Uint8List, InnerFile>(
  (ref, f) => f.bytes(requireApi(ref), 65536),
);
