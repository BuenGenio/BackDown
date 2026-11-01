import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/content_image.dart';
import '../../core/format.dart';
import '../../core/kinds.dart';
import '../../core/widgets.dart';
import '../../models/browse.dart';
import '../../providers.dart';
import '../../theme.dart';

/// Pictures this small are shown as thumbnails (the service has no thumbnails: the file itself is
/// fetched, so big ones are left as icons).
const _thumbLimit = 8 << 20;

bool hasThumb(Entry e) => !e.isDir && e.id != null && isDecodableImage(e.name) && e.size <= _thumbLimit;

/// A file's picture, or its kind's icon.
class EntryThumb extends ConsumerWidget {
  const EntryThumb({super.key, required this.entry, this.size = 40, this.fit = BoxFit.cover, this.radius = 6});

  final Entry entry;
  final double size;
  final BoxFit fit;
  final double radius;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final api = ref.watch(apiProvider);
    if (api == null || !hasThumb(entry)) {
      return SizedBox(
        width: size,
        height: size,
        child: Center(
          child: FileGlyph(name: entry.name, isDir: entry.isDir, size: size * 0.8),
        ),
      );
    }
    final px = (size * MediaQuery.devicePixelRatioOf(context)).round();
    return ClipRRect(
      borderRadius: BorderRadius.circular(radius),
      child: Image(
        image: ResizeImage(ContentImage(api, entry.id!), width: px, policy: ResizeImagePolicy.fit),
        width: size,
        height: size,
        fit: fit,
        gaplessPlayback: true,
        errorBuilder: (c, e, s) => Center(
          child: FileGlyph(name: entry.name, size: size * 0.8),
        ),
        frameBuilder: (c, child, frame, sync) => frame == null && !sync
            ? Container(width: size, height: size, color: context.colors.surfaceContainerHigh)
            : child,
      ),
    );
  }
}

/// The marks the web UI puts on an entry: identical copies, new at this scan, changed or deleted
/// later.
List<Widget> entryBadges(BuildContext context, Entry e) => [
  if (e.copies > 0) Pill('×${e.copies + 1}', color: context.bd.info),
  if (e.newHere) Pill('new', color: context.bd.good),
  if (e.later == 'deleted') Pill('deleted later', color: context.bd.crit),
  if (e.later == 'changed') Pill('changed later', color: context.bd.warn),
];

String entrySubtitle(Entry e) {
  if (e.isDir) {
    final n = e.files;
    return [if (n != null) plural(n, 'file'), bytes(e.size), if (e.mtime > 0) day(e.mtime)].join(' · ');
  }
  return '${bytes(e.size)} · ${day(e.mtime)}';
}

class EntryTile extends StatelessWidget {
  const EntryTile({super.key, required this.entry, this.onTap, this.onLongPress, this.share, this.selected = false});

  final Entry entry;
  final VoidCallback? onTap;
  final VoidCallback? onLongPress;

  /// This entry's size against the largest in the folder, drawn as a thin bar (when sorted by size).
  final double? share;
  final bool selected;

  @override
  Widget build(BuildContext context) {
    final badges = entryBadges(context, entry);
    return ListTile(
      selected: selected,
      selectedTileColor: context.bd.selection,
      leading: EntryThumb(entry: entry),
      title: Text(entry.name, maxLines: 1, overflow: TextOverflow.ellipsis),
      subtitle: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Flexible(child: Text(entrySubtitle(entry), maxLines: 1, overflow: TextOverflow.ellipsis)),
              if (badges.isNotEmpty) ...[
                const SizedBox(width: 6),
                ...badges.expand((b) => [b, const SizedBox(width: 4)]),
              ],
            ],
          ),
          if (share != null) ...[
            const SizedBox(height: 4),
            Meter(fraction: share, height: 3, color: context.bd.folder.withValues(alpha: 0.7)),
          ],
        ],
      ),
      trailing: entry.isDir ? const Icon(Icons.chevron_right) : null,
      onTap: onTap,
      onLongPress: onLongPress,
    );
  }
}

class EntryCard extends StatelessWidget {
  const EntryCard({super.key, required this.entry, this.onTap, this.onLongPress});

  final Entry entry;
  final VoidCallback? onTap;
  final VoidCallback? onLongPress;

  @override
  Widget build(BuildContext context) {
    return InkWell(
      borderRadius: BorderRadius.circular(10),
      onTap: onTap,
      onLongPress: onLongPress,
      child: Padding(
        padding: const EdgeInsets.all(6),
        child: Column(
          children: [
            Expanded(
              child: LayoutBuilder(
                builder: (c, box) => EntryThumb(entry: entry, size: box.biggest.shortestSide, radius: 8),
              ),
            ),
            const SizedBox(height: 6),
            Text(
              entry.name,
              maxLines: 2,
              overflow: TextOverflow.ellipsis,
              textAlign: TextAlign.center,
              style: context.text.bodySmall,
            ),
            Text(
              entry.isDir ? bytes(entry.size) : bytes(entry.size),
              style: context.text.labelSmall?.copyWith(color: context.bd.muted),
            ),
          ],
        ),
      ),
    );
  }
}
