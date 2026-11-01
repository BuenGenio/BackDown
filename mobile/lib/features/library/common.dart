import 'dart:convert';
import 'dart:io';
import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';
import 'package:path_provider/path_provider.dart';
import 'package:share_plus/share_plus.dart';
import 'package:url_launcher/url_launcher.dart';

import '../../api/api.dart';
import '../../api/json.dart';
import '../../core/actions.dart';
import '../../core/content_image.dart';
import '../../core/kinds.dart';
import '../../core/widgets.dart';
import '../../providers.dart';
import '../../router.dart';
import '../../theme.dart';
import 'data.dart';
import 'models.dart';

/// How wide a column of cards gets on a tablet or desktop; the rest is margin.
const readableWidth = 880.0;

/// Side padding that keeps a list [readableWidth] wide in [width], and 0 on a phone.
double sidePad(double width) => math.max(0, (width - readableWidth) / 2);

/// A list that's [readableWidth] wide at most, centred, built lazily.
class ReadableList extends StatelessWidget {
  const ReadableList({super.key, required this.itemCount, required this.itemBuilder, this.bottom = 32});

  final int itemCount;
  final IndexedWidgetBuilder itemBuilder;
  final double bottom;

  @override
  Widget build(BuildContext context) => LayoutBuilder(
    builder: (c, box) => ListView.builder(
      physics: const AlwaysScrollableScrollPhysics(),
      padding: EdgeInsets.fromLTRB(sidePad(box.maxWidth), 0, sidePad(box.maxWidth), bottom),
      itemCount: itemCount,
      itemBuilder: itemBuilder,
    ),
  );
}

/// A screen's data, or why not: like [AsyncBody], with the paid editions' refusal explained in a
/// card of its own.
class LibraryBody<T> extends StatelessWidget {
  const LibraryBody({super.key, required this.value, required this.data, this.onRetry, this.loading});

  final AsyncValue<T> value;
  final Widget Function(T data) data;
  final VoidCallback? onRetry;
  final Widget? loading;

  @override
  Widget build(BuildContext context) {
    final e = value.error;
    if (!value.hasValue && e is ApiException && e.failure == ApiFailure.needsTier) return LockedPanel(error: e);
    return AsyncBody(value: value, data: data, onRetry: onRetry, loading: loading);
  }
}

/// What a paid edition's view shows instead: what it does, and how to unlock it.
class LockedPanel extends StatelessWidget {
  const LockedPanel({super.key, required this.error});

  final ApiException error;

  @override
  Widget build(BuildContext context) {
    final d = error.detail ?? const {};
    final tier = (d['needs_name'] ?? 'Pro').toString();
    final name = (d['name'] ?? '').toString();
    final pricing = (d['pricing'] ?? 'https://backdown.io/pricing').toString();
    return Center(
      child: SingleChildScrollView(
        padding: const EdgeInsets.all(24),
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 480),
          child: Card(
            child: Padding(
              padding: const EdgeInsets.all(20),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                mainAxisSize: MainAxisSize.min,
                children: [
                  Tag(tier, color: context.colors.primary, icon: Icons.workspace_premium_outlined),
                  const SizedBox(height: 12),
                  Text(name.isEmpty ? 'Part of BackDown $tier' : name, style: context.text.titleLarge),
                  if (d['unlocks'] is String) ...[const SizedBox(height: 8), Text(d['unlocks']! as String)],
                  const SizedBox(height: 8),
                  Text(
                    '${name.isEmpty ? 'This' : name} is part of BackDown $tier. Everything that tells you what you have '
                    'and what\'s duplicated stays free.',
                    style: context.text.bodyMedium?.copyWith(color: context.colors.onSurfaceVariant),
                  ),
                  const SizedBox(height: 8),
                  Text(
                    'Add a licence key on the computer: in Settings there, or with `backdown license add <key>`.',
                    style: context.text.bodySmall?.copyWith(color: context.bd.muted),
                  ),
                  const SizedBox(height: 16),
                  OutlinedButton.icon(
                    onPressed: () => launchUrl(Uri.parse(pricing), mode: LaunchMode.externalApplication),
                    icon: const Icon(Icons.open_in_new, size: 18),
                    label: const Text('See pricing'),
                  ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}

extension LinkColor on BuildContext {
  /// The colour of a path or name that opens something.
  Color get linkColor => colors.secondary;
}

/// "42% exists elsewhere", "under 1% exists elsewhere", "none of it exists elsewhere".
String elsewhereText(Overlap o) {
  if (o.elsewhere <= 0) return 'none of it exists elsewhere';
  final p = (o.share * 100).floor();
  return p == 0 ? 'under 1% exists elsewhere' : '$p% exists elsewhere';
}

String capitalised(String s) => s.isEmpty ? s : '${s[0].toUpperCase()}${s.substring(1)}';

/// A small rounded label, as core's [Pill] draws it, whose text wraps rather than running out of a
/// narrow space (long ones: "under 1% exists elsewhere", "6.1 GB stays only here").
class Tag extends StatelessWidget {
  const Tag(this.text, {super.key, this.color, this.icon});

  final String text;
  final Color? color;
  final IconData? icon;

  @override
  Widget build(BuildContext context) {
    final c = color ?? context.colors.onSurfaceVariant;
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 7, vertical: 2),
      decoration: BoxDecoration(color: c.withValues(alpha: 0.12), borderRadius: BorderRadius.circular(20)),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          if (icon != null) ...[Icon(icon, size: 12, color: c), const SizedBox(width: 3)],
          Flexible(
            child: Text(
              text,
              style: context.text.labelSmall?.copyWith(color: c, fontWeight: FontWeight.w700),
            ),
          ),
        ],
      ),
    );
  }
}

/// A short intro at the top of a view, in the web UI's words.
class Intro extends StatelessWidget {
  const Intro(this.text, {super.key});
  final String text;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.fromLTRB(16, 8, 16, 4),
    child: Text(text, style: context.text.bodyMedium?.copyWith(color: context.colors.onSurfaceVariant)),
  );
}

/// A job or record's state as a pill: done green, partial or stopped amber, failed red, running
/// blue.
class StatePill extends StatelessWidget {
  const StatePill(this.state, {super.key, this.label});
  final String state;
  final String? label;

  @override
  Widget build(BuildContext context) {
    final c = switch (state) {
      'done' || 'ok' => context.bd.good,
      'partial' || 'stopped' || 'interrupted' => context.bd.warn,
      'failed' => context.bd.crit,
      'running' || 'queued' || 'waiting' || 'new' || 'wiping' => context.bd.info,
      _ => context.bd.muted,
    };
    return Tag(label ?? state, color: c);
  }
}

/// A card in a list, with the house margins.
class ListCard extends StatelessWidget {
  const ListCard({super.key, required this.child, this.onTap, this.padding = const EdgeInsets.all(14)});
  final Widget child;
  final VoidCallback? onTap;
  final EdgeInsetsGeometry padding;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.fromLTRB(16, 5, 16, 5),
    child: Card(
      clipBehavior: Clip.antiAlias,
      child: InkWell(
        onTap: onTap,
        child: Padding(padding: padding, child: child),
      ),
    ),
  );
}

/// Muted small text.
class Muted extends StatelessWidget {
  const Muted(this.text, {super.key, this.maxLines, this.color});
  final String text;
  final int? maxLines;
  final Color? color;

  @override
  Widget build(BuildContext context) => Text(
    text,
    maxLines: maxLines,
    overflow: maxLines == null ? null : TextOverflow.ellipsis,
    style: context.text.bodySmall?.copyWith(color: color ?? context.colors.onSurfaceVariant),
  );
}

/// A path to read (monospace, wraps anywhere).
class PathText extends StatelessWidget {
  const PathText(this.path, {super.key, this.maxLines, this.color});
  final String path;
  final int? maxLines;
  final Color? color;

  @override
  Widget build(BuildContext context) => Text(
    // a zero-width space after each separator, so long paths wrap at folders
    path.replaceAllMapped(RegExp(r'[\\/]'), (m) => '${m[0]}​'),
    maxLines: maxLines,
    overflow: maxLines == null ? null : TextOverflow.ellipsis,
    style: context.text.bodySmall?.copyWith(fontFamily: 'monospace', color: color ?? context.colors.onSurfaceVariant),
  );
}

// ---------- opening places

/// The key of the folder holding [key] (the key's bytes up to the last separator), so a name that
/// isn't UTF-8 survives.
String parentKey(String key) {
  final List<int> b;
  try {
    b = base64Url.decode(base64Url.normalize(key));
  } on FormatException {
    return key;
  }
  var i = b.length - 1;
  while (i > 0 && b[i] != 0x2f && b[i] != 0x5c) {
    i--;
  }
  final end = i <= 0 ? 1 : i;
  return base64Url.encode(b.sublist(0, math.min(end, b.length))).replaceAll('=', '');
}

/// Opens a folder by its key, or the folder holding a file (with the file picked out).
Future<void> openKey(BuildContext context, String key, {bool isFile = false}) async {
  if (!isFile) {
    await context.push(Routes.folder(key));
    return;
  }
  await _openResolved(context, {'key': key});
}

/// Opens a path from the index: its folder, or the folder holding it.
Future<void> openPath(BuildContext context, String path) => _openResolved(context, {'path': path});

Future<void> _openResolved(BuildContext context, Map<String, Object?> params) async {
  final api = ProviderScope.containerOf(context, listen: false).read(apiProvider);
  if (api == null) return;
  try {
    final r = asJson(await api.get('resolve', params));
    if (!context.mounted) return;
    final dir = r.strOrNull('dir');
    if (dir == null) return;
    await context.push(Routes.folder(dir, select: r.strOrNull('select')));
  } on ApiException catch (e) {
    if (context.mounted) showSnack(context, e.message);
  }
}

/// Opens a junk subtree where it is: its folder, with it picked out.
Future<void> showInFolder(BuildContext context, String key) => context.push(Routes.folder(parentKey(key), select: key));

Future<void> copyText(BuildContext context, String text, String what) async {
  await Clipboard.setData(ClipboardData(text: text));
  if (context.mounted) showSnack(context, '$what copied');
}

// ---------- documents to share

/// Downloads a document the service makes (the chain of custody, a signed manifest) and opens the
/// share sheet with it. A paid edition's refusal is explained.
Future<void> shareDocument(
  BuildContext context,
  WidgetRef ref,
  String endpoint,
  Map<String, Object?> params,
  String name, {
  String? title,
}) async {
  final api = ref.read(apiProvider);
  if (api == null) return;
  final messenger = ScaffoldMessenger.of(context);
  messenger.showSnackBar(SnackBar(content: Text('Getting $name…'), duration: const Duration(seconds: 30)));
  try {
    final dir = Directory('${(await getTemporaryDirectory()).path}/documents');
    await dir.create(recursive: true);
    final f = await saveDocument(api, endpoint, params, File('${dir.path}/$name'));
    messenger.hideCurrentSnackBar();
    if (f == null) return;
    await SharePlus.instance.share(ShareParams(files: [XFile(f.path)], title: title ?? name));
  } on ApiException catch (e) {
    messenger.hideCurrentSnackBar();
    if (context.mounted) await showActionError(context, e);
  } on Object catch (e) {
    messenger.hideCurrentSnackBar();
    if (context.mounted) showSnack(context, 'Couldn\'t save $name: $e');
  }
}

/// "backdown-custody-2026-11-01.json", as the service names it.
String custodyFileName(DateTime now) => 'backdown-custody-${now.toUtc().toIso8601String().substring(0, 10)}.json';

/// The chain of custody, as an app bar button.
class CustodyButton extends ConsumerWidget {
  const CustodyButton({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) => IconButton(
    tooltip: 'Chain of custody',
    icon: const Icon(Icons.receipt_long_outlined),
    onPressed: () => shareDocument(
      context,
      ref,
      'custody',
      const {},
      custodyFileName(DateTime.now()),
      title: 'BackDown chain of custody',
    ),
  );
}

/// The colour of a recognised kind (ui/app.js KIND_COLOR).
Color kindColor(String kind) => switch (kind) {
  'mail-store' || 'maildir' || 'mdbox' || 'thunderbird' => const Color(0xff3b8fd8),
  'nextcloud' || 'nextcloud-server' => const Color(0xff1f8a8a),
  'timemachine' || 'disk-image' => const Color(0xff55606e),
  'ios-backup' || 'photos' => const Color(0xff8e5bd1),
  'android-backup' => const Color(0xff2e8b57),
  'system' || 'sql-dump' => const Color(0xffb8800e),
  'home-mac' || 'home-linux' || 'home-windows' => const Color(0xffd97a1b),
  'website' || 'music' => const Color(0xffc2458a),
  'git' => const Color(0xffd8432b),
  'videos' => const Color(0xff7a4fd6),
  'export' => const Color(0xff3b6fd8),
  _ => const Color(0xff8a8f98),
};

/// The icon of a recognised kind.
IconData kindIcon(String kind) => switch (kind) {
  'mail-store' || 'maildir' || 'mdbox' || 'thunderbird' => Icons.mail_outline,
  'nextcloud' || 'nextcloud-server' => Icons.cloud_outlined,
  'timemachine' => Icons.history,
  'ios-backup' || 'android-backup' => Icons.phone_iphone,
  'system' => Icons.memory,
  'home-mac' || 'home-linux' || 'home-windows' => Icons.home_outlined,
  'website' => Icons.language,
  'git' => Icons.commit,
  'sql-dump' => Icons.storage_outlined,
  'disk-image' => Icons.album_outlined,
  'photos' => Icons.photo_library_outlined,
  'music' => Icons.library_music_outlined,
  'videos' => Icons.video_library_outlined,
  'export' => Icons.unarchive_outlined,
  'downloads' => Icons.download_outlined,
  'recovery' => Icons.healing_outlined,
  _ => Icons.sell_outlined,
};

/// "2014–2016", from a pair of unix times.
String yearsOf(int? oldest, int? newest) {
  if (oldest == null || newest == null || oldest == 0 || newest == 0) return '';
  final a = DateTime.fromMillisecondsSinceEpoch(oldest * 1000).year;
  final b = DateTime.fromMillisecondsSinceEpoch(newest * 1000).year;
  return a == b ? '$a' : '$a–$b';
}

/// Runs an action the web UI offers, explaining the phone's refusal (see [runAction]).
Future<void> atTheComputer(
  BuildContext context,
  WidgetRef ref,
  String endpoint,
  Map<String, Object?> params,
  String done,
) => runAction(context, ref, (api) => api.post(endpoint, params: params), done: done);

/// A picture of a file small enough to fetch whole, else its kind's icon.
class FileThumb extends ConsumerWidget {
  const FileThumb({super.key, required this.id, required this.name, required this.bytes, this.size = 40});

  final int id;
  final String name;
  final int bytes;
  final double size;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final api = ref.watch(apiProvider);
    final glyph = SizedBox(
      width: size,
      height: size,
      child: Center(
        child: FileGlyph(name: name, size: size * 0.8),
      ),
    );
    if (api == null || id == 0 || !isDecodableImage(name) || bytes > 8 << 20) return glyph;
    final px = (size * MediaQuery.devicePixelRatioOf(context)).round();
    return ClipRRect(
      borderRadius: BorderRadius.circular(6),
      child: Image(
        image: ResizeImage(ContentImage(api, id), width: px, policy: ResizeImagePolicy.fit),
        width: size,
        height: size,
        fit: BoxFit.cover,
        gaplessPlayback: true,
        errorBuilder: (c, e, s) => glyph,
        frameBuilder: (c, child, frame, sync) => frame == null && !sync
            ? Container(width: size, height: size, color: context.colors.surfaceContainerHigh)
            : child,
      ),
    );
  }
}
