import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:isolate';
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:open_file/open_file.dart';
import 'package:share_plus/share_plus.dart';

import '../../api/api.dart';
import '../../api/json.dart';
import '../../core/file_io.dart';
import '../../core/format.dart';
import '../../core/widgets.dart';
import '../../models/file_ref.dart';
import '../../theme.dart';

/// Answers bigger than this are decoded on another isolate, so the UI keeps moving.
const _offThreadBytes = 192 << 10;

/// GET an endpoint whose answer can be big (an archive's 10,000 entries, a sheet's 500 rows), and
/// make [parse]'s model from it off the UI thread when it's big. [parse] must be a top-level or
/// static function (it runs on another isolate).
Future<T> fetchParsed<T>(BackDownApi api, String endpoint, Map<String, Object?> params, T Function(Json) parse) async {
  // the demo answers with maps already, and small ones aren't worth a trip
  if (api.isDemo) return parse(asJson(await api.get(endpoint, params)));
  return _decode(await api.bytes(endpoint, params), parse);
}

// Not inside the async function above: a closure made there would carry its whole frame (the
// client and its sockets) to the other isolate, which can't be sent.
Future<T> _decode<T>(Uint8List raw, T Function(Json) parse) {
  T decode() {
    try {
      return parse(asJson(jsonDecode(utf8.decode(raw, allowMalformed: true))));
    } on FormatException {
      throw ApiException(ApiFailure.server, 'The service answered with something that isn\'t JSON.');
    }
  }

  return raw.length < _offThreadBytes ? Future.sync(decode) : Isolate.run(decode);
}

/// Runs [work] on another isolate when [size] says it's worth it (and inline otherwise, which
/// also keeps widget tests, whose clock is fake, from waiting on a real isolate). Make [work] in
/// a plain (not async) function, so it carries only what it needs.
Future<T> offThread<T>(int size, T Function() work, {int threshold = 64 << 10}) =>
    size < threshold ? Future.sync(work) : Isolate.run(work);

/// Where wide screens keep reading text to a comfortable width.
class Readable extends StatelessWidget {
  const Readable({super.key, required this.child, this.maxWidth = 860});
  final Widget child;
  final double maxWidth;

  @override
  Widget build(BuildContext context) => Center(
    child: ConstrainedBox(
      constraints: BoxConstraints(maxWidth: maxWidth),
      child: child,
    ),
  );
}

/// A thin bar under the app bar with what a viewer shows ("120 lines · Python") and its own tools.
class ViewerBar extends StatelessWidget {
  const ViewerBar({super.key, this.meta, this.children = const []});
  final String? meta;
  final List<Widget> children;

  @override
  Widget build(BuildContext context) {
    return Container(
      constraints: const BoxConstraints(minHeight: 44),
      padding: const EdgeInsets.fromLTRB(16, 2, 4, 2),
      decoration: BoxDecoration(
        color: context.colors.surfaceContainerLow,
        border: Border(bottom: BorderSide(color: context.colors.outlineVariant)),
      ),
      child: Row(
        children: [
          Expanded(
            child: Text(
              meta ?? '',
              style: context.text.bodySmall?.copyWith(color: context.colors.onSurfaceVariant),
              maxLines: 2,
              overflow: TextOverflow.ellipsis,
            ),
          ),
          ...children,
        ],
      ),
    );
  }
}

/// A note about what's shown ("Showing the first 2.1 MB of 52 MB."), like the web UI's.
class ViewerNote extends StatelessWidget {
  const ViewerNote(this.text, {super.key, this.icon = Icons.info_outline, this.color, this.action, this.margin});
  final String text;
  final IconData icon;
  final Color? color;
  final Widget? action;
  final EdgeInsetsGeometry? margin;

  @override
  Widget build(BuildContext context) {
    final c = color ?? context.bd.info;
    return Container(
      margin: margin ?? const EdgeInsets.fromLTRB(12, 8, 12, 0),
      padding: EdgeInsets.fromLTRB(12, 8, action == null ? 12 : 4, 8),
      decoration: BoxDecoration(color: c.withValues(alpha: 0.10), borderRadius: BorderRadius.circular(10)),
      child: Row(
        children: [
          Icon(icon, size: 18, color: c),
          const SizedBox(width: 10),
          Expanded(child: Text(text, style: context.text.bodySmall)),
          ?action,
        ],
      ),
    );
  }
}

/// A centred spinner with words, for slow reads (a mailbox, a dump).
class ViewerLoading extends StatelessWidget {
  const ViewerLoading({super.key, this.text});
  final String? text;

  @override
  Widget build(BuildContext context) => Center(
    child: Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        const CircularProgressIndicator(),
        if (text != null) ...[
          const SizedBox(height: 14),
          Text(text!, style: context.text.bodyMedium?.copyWith(color: context.colors.onSurfaceVariant)),
        ],
      ],
    ),
  );
}

/// "1 Nov 2026" style date, or nothing.
String? dayOrNull(int? t) => t == null || t == 0 ? null : day(t);

/// A file from inside a container: an entry of an archive or disk image (GET /api/entry) or a
/// mail attachment (GET /api/attachment).
@immutable
class InnerFile {
  const InnerFile.entry({required this.id, required this.name, this.part, this.size, this.streams = false})
    : endpoint = 'entry',
      at = null,
      attachment = null;

  const InnerFile.attachment({required this.id, required this.name, required int this.attachment, this.at, this.size})
    : endpoint = 'attachment',
      part = null,
      streams = false;

  /// The container's index id.
  final int id;
  final String endpoint;

  /// Its name in the container's listing (a path inside an archive or filesystem), or an
  /// attachment's file name.
  final String name;

  /// The partition of a disk image whose filesystem it's in ("0": the whole image).
  final String? part;

  /// An attachment's number in its message, and that message's offset in an mbox.
  final int? attachment, at;
  final int? size;

  /// The service streams it at any size (a file in a disk image's filesystem); entries of
  /// archives come out whole, up to 64 MB.
  final bool streams;

  String get base {
    final t = name.replaceAll(RegExp(r'[\\/]+$'), '');
    final i = t.lastIndexOf(RegExp(r'[\\/]'));
    return i < 0 ? t : t.substring(i + 1);
  }

  Map<String, Object?> get params =>
      endpoint == 'entry' ? {'id': id, 'name': name, 'part': part} : {'id': id, 'at': at, 'part': attachment};

  /// Its bytes, at most [max].
  Future<Uint8List> bytes(BackDownApi api, [int? max]) =>
      endpoint == 'entry' ? api.bytes('entry', {...params, 'max': max}, max) : api.bytes('attachment', params, max);

  @override
  bool operator ==(Object other) =>
      other is InnerFile &&
      other.id == id &&
      other.endpoint == endpoint &&
      other.name == name &&
      other.part == part &&
      other.attachment == attachment &&
      other.at == at;

  @override
  int get hashCode => Object.hash(id, endpoint, name, part, attachment, at);
}

/// The most of a container's entry the service hands over in one piece.
const entryMax = 64 << 20;

/// Downloads a file from inside a container into the app's cache, with the same progress dialog
/// as a whole file (core/file_io.dart). Null if it failed or was cancelled.
Future<File?> fetchInnerToCache(BuildContext context, BackDownApi api, InnerFile f) {
  final key = '${f.endpoint}-${f.id}-${f.part ?? ''}-${f.at ?? ''}-${f.attachment ?? ''}-${f.name.hashCode}';
  return fetchToCache(
    context,
    api,
    FileRef(id: f.id, name: f.base, size: f.size ?? 0),
    endpoint: f.endpoint,
    params: f.params,
    cacheKey: 'inner/$key',
  );
}

Future<void> shareInner(BuildContext context, BackDownApi api, InnerFile f) async {
  final file = await fetchInnerToCache(context, api, f);
  if (file == null) return;
  await SharePlus.instance.share(ShareParams(files: [XFile(file.path)], title: f.base));
}

Future<void> openInnerElsewhere(BuildContext context, BackDownApi api, InnerFile f) async {
  final file = await fetchInnerToCache(context, api, f);
  if (file == null) return;
  final r = await OpenFile.open(file.path);
  if (r.type != ResultType.done && context.mounted) {
    showSnack(context, r.type == ResultType.noAppToOpen ? 'No app here opens ${f.base}.' : r.message);
  }
}

/// Whether the first bytes of a file look like text (no NULs, valid UTF-8), as the web UI judges.
bool looksText(Uint8List b) {
  final n = b.length < 8192 ? b.length : 8192;
  for (var i = 0; i < n; i++) {
    if (b[i] == 0) return false;
  }
  // a character the sample's end cuts in two isn't held against it: back off to where it starts
  var end = n, i = n - 1;
  while (i >= 0 && n - i <= 3 && (b[i] & 0xC0) == 0x80) {
    i--;
  }
  if (i >= 0) {
    final lead = b[i];
    final len = lead >= 0xF0
        ? 4
        : lead >= 0xE0
        ? 3
        : lead >= 0xC0
        ? 2
        : 1;
    if (i + len > n) end = i;
  }
  try {
    utf8.decode(Uint8List.sublistView(b, 0, end));
    return true;
  } on FormatException {
    return false;
  }
}

bool isSqliteHeader(Uint8List b) =>
    b.length >= 16 && latin1.decode(Uint8List.sublistView(b, 0, 15)) == 'SQLite format 3';

/// The first [n] bytes as a hex dump, 16 to a line.
String hexDump(Uint8List b, [int n = 4096]) {
  final end = b.length < n ? b.length : n;
  final out = StringBuffer();
  for (var o = 0; o < end; o += 16) {
    final row = Uint8List.sublistView(b, o, o + 16 > end ? end : o + 16);
    out
      ..write(o.toRadixString(16).padLeft(8, '0'))
      ..write('  ')
      ..write([for (final x in row) x.toRadixString(16).padLeft(2, '0')].join(' ').padRight(48))
      ..write('  ')
      ..writeln(String.fromCharCodes([for (final x in row) x >= 32 && x < 127 ? x : 46]));
  }
  return out.toString();
}

/// What a viewer reads its bytes from: a whole file (GET /api/content) or a file inside a
/// container (an archive entry, a file in a disk image, an attachment).
@immutable
class ViewSource {
  ViewSource.file(FileRef f) : id = f.id, name = f.name, size = f.size, inner = null;
  ViewSource.inner(InnerFile f) : id = f.id, name = f.base, size = f.size, inner = f;

  final int id;
  final String name;
  final int? size;
  final InnerFile? inner;

  Future<Uint8List> bytes(BackDownApi api, [int? max]) =>
      inner?.bytes(api, max) ?? api.bytes('content', {'id': id}, max);

  /// Where a player streams it from.
  Uri uri(BackDownApi api) => inner == null ? api.uri('content', {'id': id}) : api.uri(inner!.endpoint, inner!.params);

  @override
  bool operator ==(Object other) => other is ViewSource && other.id == id && other.inner == inner;

  @override
  int get hashCode => Object.hash(id, inner);
}

/// What a viewer page knows about its place in the viewer: whether it's the one on screen (a
/// player pauses when it isn't), and how to say it's zoomed in (so a swipe pans the picture
/// instead of turning the page).
class ViewerPage extends InheritedWidget {
  const ViewerPage({super.key, required this.active, required this.onZoom, required super.child});

  final bool active;
  final ValueChanged<bool> onZoom;

  static ViewerPage? of(BuildContext context) => context.dependOnInheritedWidgetOfExactType<ViewerPage>();

  @override
  bool updateShouldNotify(ViewerPage old) => old.active != active;
}

/// Where a listing inside a file is: its root, then each folder down to here; a tap goes back up.
class PathCrumbs extends StatelessWidget {
  const PathCrumbs({super.key, required this.names, required this.onTap});

  /// The root's name first.
  final List<String> names;
  final ValueChanged<int> onTap;

  @override
  Widget build(BuildContext context) {
    return SizedBox(
      height: 40,
      child: ListView.separated(
        scrollDirection: Axis.horizontal,
        reverse: true,
        padding: const EdgeInsets.symmetric(horizontal: 12),
        itemCount: names.length,
        separatorBuilder: (c, i) => Icon(Icons.chevron_left, size: 16, color: context.bd.muted),
        itemBuilder: (c, i) {
          final depth = names.length - 1 - i;
          final last = i == 0;
          return Center(
            child: InkWell(
              borderRadius: BorderRadius.circular(6),
              onTap: last ? null : () => onTap(depth),
              child: Padding(
                padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 4),
                child: Text(
                  names[depth],
                  style: context.text.labelLarge?.copyWith(
                    color: last ? context.colors.onSurface : context.colors.primary,
                    fontWeight: last ? FontWeight.w700 : FontWeight.w500,
                  ),
                ),
              ),
            ),
          );
        },
      ),
    );
  }
}

/// Names compared as people read them: "IMG_9" before "IMG_10", case aside.
int naturalCompare(String a, String b) {
  final x = a.toLowerCase(), y = b.toLowerCase();
  var i = 0, j = 0;
  bool digit(int c) => c >= 48 && c <= 57;
  while (i < x.length && j < y.length) {
    final c = x.codeUnitAt(i), d = y.codeUnitAt(j);
    if (digit(c) && digit(d)) {
      var i2 = i, j2 = j;
      while (i2 < x.length && digit(x.codeUnitAt(i2))) {
        i2++;
      }
      while (j2 < y.length && digit(y.codeUnitAt(j2))) {
        j2++;
      }
      final n = BigInt.parse(x.substring(i, i2)).compareTo(BigInt.parse(y.substring(j, j2)));
      if (n != 0) return n;
      i = i2;
      j = j2;
    } else {
      if (c != d) return c.compareTo(d);
      i++;
      j++;
    }
  }
  return (x.length - i).compareTo(y.length - j);
}

/// How a listing inside a file is sorted.
enum InnerSort {
  name('Name'),
  size('Size'),
  date('Date modified');

  const InnerSort(this.label);
  final String label;
}

/// A sort menu button for listings inside files.
class InnerSortButton extends StatelessWidget {
  const InnerSortButton({super.key, required this.sort, required this.descending, required this.onPick});
  final InnerSort sort;
  final bool descending;
  final ValueChanged<InnerSort> onPick;

  @override
  Widget build(BuildContext context) => PopupMenuButton<InnerSort>(
    tooltip: 'Sort',
    icon: const Icon(Icons.sort),
    onSelected: onPick,
    itemBuilder: (c) => [
      for (final s in InnerSort.values)
        CheckedPopupMenuItem(
          value: s,
          checked: s == sort,
          child: Text(s == sort ? '${s.label} ${descending ? '↑' : '↓'}' : s.label),
        ),
    ],
  );
}
