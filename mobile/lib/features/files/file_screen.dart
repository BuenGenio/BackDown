import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../../api/keys.dart';
import '../../core/content_image.dart';
import '../../core/file_io.dart';
import '../../core/format.dart';
import '../../core/kinds.dart';
import '../../core/widgets.dart';
import '../../models/browse.dart';
import '../../models/file_ref.dart';
import '../../providers.dart';
import '../../router.dart';
import '../../theme.dart';
import '../viewers/open.dart';

/// A file's details: where it is, the same content elsewhere (here and on other machines), its
/// versions, and open, share or "open in".
class FileScreen extends ConsumerWidget {
  const FileScreen({super.key, required this.id});
  final int id;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final info = ref.watch(fileInfoProvider(id));
    final skew = ref.watch(skewProvider);
    return Scaffold(
      appBar: AppBar(title: Text(info.value?.name ?? 'File')),
      body: AsyncBody(
        value: info,
        onRetry: () => ref.invalidate(fileInfoProvider(id)),
        data: (f) {
          final api = ref.watch(apiProvider);
          final file = FileRef(id: f.id, name: f.name, size: f.size, mtime: f.mtime);
          final readable = f.versions.where((v) => v.current).firstOrNull?.readable ?? f.current;
          return ListView(
            padding: const EdgeInsets.only(bottom: 32),
            children: [
              _Preview(info: f),
              Padding(
                padding: const EdgeInsets.fromLTRB(16, 4, 16, 0),
                child: Wrap(
                  spacing: 8,
                  runSpacing: 8,
                  children: [
                    FilledButton.icon(
                      onPressed: readable ? () => openFile(context, file) : null,
                      icon: const Icon(Icons.visibility_outlined),
                      label: const Text('Open'),
                    ),
                    if (api != null) ...[
                      OutlinedButton.icon(
                        onPressed: readable ? () => shareFile(context, api, file) : null,
                        icon: const Icon(Icons.share_outlined),
                        label: const Text('Share'),
                      ),
                      OutlinedButton.icon(
                        onPressed: readable ? () => openInAnotherApp(context, api, file) : null,
                        icon: const Icon(Icons.open_in_new),
                        label: const Text('Open in…'),
                      ),
                    ],
                    OutlinedButton.icon(
                      onPressed: () => context.push(Routes.folder(f.dirKey, select: f.key)),
                      icon: const Icon(Icons.folder_open_outlined),
                      label: const Text('Show in folder'),
                    ),
                  ],
                ),
              ),
              const SectionHeader('Details'),
              InfoRow('Where', f.path, copyable: true),
              InfoRow('Size', '${bytes(f.size)} (${count(f.size)} bytes)'),
              InfoRow('Modified', when(f.mtime)),
              InfoRow('Kind', FileKind.of(f.name).label),
              InfoRow('SHA-256', f.sha256 ?? 'not hashed yet', mono: f.sha256 != null, copyable: f.sha256 != null),
              if (!f.current) InfoRow('Now', 'Not on the drive any more', valueColor: context.bd.warn),
              if (f.error != null) InfoRow('Couldn\'t read', f.error!, valueColor: context.bd.crit),
              if (f.contentFrom != null && f.contentFrom!.isNotEmpty) InfoRow('Read from', f.contentFrom!),
              if (f.copies.isNotEmpty || f.remoteCopies.isNotEmpty) ...[
                SectionHeader(
                  'The same content elsewhere',
                  trailing: Pill('${f.copies.length + f.remoteCopies.length}', color: context.bd.info),
                ),
                for (final c in f.copies)
                  ListTile(
                    dense: true,
                    leading: Icon(Icons.content_copy_outlined, color: context.bd.info),
                    title: Text(baseName(c.path)),
                    subtitle: Text(c.path, maxLines: 2, overflow: TextOverflow.ellipsis),
                    onTap: () => context.push(Routes.file(c.id)),
                  ),
                for (final c in f.remoteCopies)
                  ListTile(
                    dense: true,
                    leading: Icon(
                      c.counts ? Icons.verified_outlined : Icons.dns_outlined,
                      color: c.counts ? context.bd.good : context.bd.muted,
                    ),
                    title: Text('On ${c.machine}'),
                    subtitle: Text(
                      [
                        c.path,
                        if (c.confirmedAt != null) 'confirmed ${rel(c.confirmedAt, skew: skew)}',
                        if (c.counts) 'counts as a verified copy' else ?c.whyNot,
                      ].join('\n'),
                    ),
                    isThreeLine: true,
                    onTap: () => context.push('${Routes.machine(c.machineId)}/file/${c.rid}'),
                  ),
              ],
              if (f.versions.length > 1 || f.versions.any((v) => !v.current)) ...[
                SectionHeader('Versions', trailing: Text('${f.versions.length}', style: context.text.labelMedium)),
                for (final v in f.versions.reversed) _VersionTile(v: v, skew: skew),
              ],
            ],
          );
        },
      ),
    );
  }
}

class _Preview extends ConsumerWidget {
  const _Preview({required this.info});
  final FileInfo info;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final api = ref.watch(apiProvider);
    final pic = api != null && isDecodableImage(info.name) && info.size <= (25 << 20);
    return Padding(
      padding: const EdgeInsets.all(16),
      child: Row(
        children: [
          Container(
            width: 84,
            height: 84,
            decoration: BoxDecoration(
              color: context.colors.surfaceContainerHigh,
              borderRadius: BorderRadius.circular(12),
            ),
            clipBehavior: Clip.antiAlias,
            child: pic
                ? Image(
                    image: ResizeImage(ContentImage(api, info.id), width: 256, policy: ResizeImagePolicy.fit),
                    fit: BoxFit.cover,
                    errorBuilder: (c, e, s) => Center(child: FileGlyph(name: info.name, size: 44)),
                  )
                : Center(child: FileGlyph(name: info.name, size: 44)),
          ),
          const SizedBox(width: 16),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                SelectableText(info.name, style: context.text.titleMedium),
                const SizedBox(height: 4),
                Text(
                  '${bytes(info.size)} · ${day(info.mtime)}',
                  style: context.text.bodyMedium?.copyWith(color: context.colors.onSurfaceVariant),
                ),
              ],
            ),
          ),
        ],
      ),
    );
  }
}

class _VersionTile extends StatelessWidget {
  const _VersionTile({required this.v, required this.skew});
  final FileVersion v;
  final int skew;

  @override
  Widget build(BuildContext context) {
    return ListTile(
      dense: true,
      leading: Icon(
        v.current ? Icons.radio_button_checked : Icons.history,
        color: v.current ? context.colors.primary : context.bd.muted,
      ),
      title: Text('${bytes(v.size)} · modified ${day(v.mtime)}'),
      subtitle: Text(
        [
          'seen ${day(v.from)} – ${v.current ? 'now' : day(v.to)}',
          if (v.sha256 != null) shortHash(v.sha256),
          if (!v.readable) 'its bytes aren\'t anywhere now',
        ].join(' · '),
      ),
    );
  }
}
