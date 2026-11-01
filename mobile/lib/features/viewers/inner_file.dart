import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/format.dart';
import '../../core/kinds.dart';
import '../../providers.dart';
import '../../theme.dart';
import 'choose.dart';
import 'common.dart';

/// A file from inside a container (an archive's entry, a file in a disk image, an attachment) in
/// the viewer that suits it, with Share and Open in another app.
class InnerFileScreen extends ConsumerWidget {
  const InnerFileScreen({super.key, required this.file, this.note});
  final InnerFile file;

  /// Said above it (a deleted file's chances).
  final String? note;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final api = ref.watch(apiProvider);
    final fits = file.streams || (file.size ?? 0) <= entryMax;
    return Scaffold(
      appBar: AppBar(
        title: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(file.base, maxLines: 1, overflow: TextOverflow.ellipsis),
            Text(
              [
                FileKind.of(file.base).label,
                if (file.size != null) bytes(file.size),
                file.endpoint == 'attachment' ? 'attachment' : 'inside a file',
              ].join(' · '),
              style: context.text.bodySmall?.copyWith(color: context.colors.onSurfaceVariant),
            ),
          ],
        ),
        actions: [
          if (api != null && fits) ...[
            IconButton(
              tooltip: 'Share',
              icon: const Icon(Icons.share_outlined),
              onPressed: () => shareInner(context, api, file),
            ),
            IconButton(
              tooltip: 'Open in another app',
              icon: const Icon(Icons.open_in_new),
              onPressed: () => openInnerElsewhere(context, api, file),
            ),
          ],
        ],
      ),
      body: Column(
        children: [
          if (note != null) ViewerNote(note!, icon: Icons.restore_page_outlined, color: context.bd.warn),
          Expanded(child: InnerViewer(file: file)),
        ],
      ),
    );
  }
}
