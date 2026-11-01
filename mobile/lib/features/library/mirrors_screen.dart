import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../api/keys.dart';
import '../../core/actions.dart';
import '../../core/format.dart';
import '../../core/widgets.dart';
import '../../providers.dart';
import '../../theme.dart';
import 'common.dart';
import 'data.dart';
import 'models.dart';

/// Mirrors: folders kept copied to another drive, when each was last synced and verified.
class MirrorsScreen extends ConsumerWidget {
  const MirrorsScreen({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final mirrors = ref.watch(mirrorsProvider);
    final daemon = ref.watch(daemonProvider).value;
    final busy = {
      for (final j in [?daemon?.current, ...?daemon?.queued])
        if (j.kind == 'mirror-sync' || j.kind == 'mirror-verify') ?j.path,
    };
    return Scaffold(
      appBar: AppBar(title: const Text('Mirrors')),
      body: RefreshIndicator(
        onRefresh: () => ref.refresh(mirrorsProvider.future).catchError((_) => <Mirror>[]),
        child: LibraryBody(
          value: mirrors,
          onRetry: () => ref.invalidate(mirrorsProvider),
          data: (list) {
            final rows = <Widget Function()>[
              () => const LookOnlyNote(what: 'Mirrors'),
              () => const Intro(
                'A mirror keeps a folder copied to another drive: new and changed files are copied after each scan, '
                'old copies are kept, and now and then a sample is read back to check the mirror still matches.',
              ),
              if (list.isEmpty)
                () => const Padding(
                  padding: EdgeInsets.all(24),
                  child: EmptyState(
                    icon: Icons.sync_alt,
                    title: 'No mirrors',
                    message: 'Add one on the computer: `backdown mirror add /a/folder /another/drive/folder`.',
                  ),
                ),
              for (final m in list) () => MirrorCard(mirror: m, busy: busy.contains('mirror ${m.id}')),
            ];
            return ReadableList(itemCount: rows.length, itemBuilder: (c, i) => rows[i]());
          },
        ),
      ),
    );
  }
}

class MirrorCard extends ConsumerWidget {
  const MirrorCard({super.key, required this.mirror, this.busy = false});
  final Mirror mirror;
  final bool busy;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final m = mirror;
    final skew = ref.watch(skewProvider);
    final failed = m.lastVerifyOk == false;
    return ListCard(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Icon(Icons.sync_alt, size: 20, color: failed ? context.bd.crit : context.bd.good),
              const SizedBox(width: 8),
              Expanded(
                child: Wrap(
                  crossAxisAlignment: WrapCrossAlignment.center,
                  spacing: 4,
                  children: [
                    Text(baseName(m.source), style: context.text.titleSmall),
                    Icon(Icons.arrow_forward, size: 16, color: context.bd.muted),
                    Text(baseName(m.target), style: context.text.titleSmall),
                  ],
                ),
              ),
              if (busy)
                Tag('syncing', color: context.bd.info)
              else
                Tag(
                  m.connected ? 'both drives here' : 'a drive isn\'t connected',
                  color: m.connected ? context.bd.good : context.bd.muted,
                ),
            ],
          ),
          const SizedBox(height: 8),
          _Place(label: 'From', path: m.source),
          _Place(label: 'To', path: m.target),
          const SizedBox(height: 8),
          Muted(
            'synced ${m.lastSync == null ? 'not yet' : rel(m.lastSync, skew: skew)} · verified '
            '${m.lastVerify == null ? 'not yet' : rel(m.lastVerify, skew: skew)} · checked every '
            '${plural(m.verifyEveryDays, 'day')}',
          ),
          if (m.lastSyncSummary != null) ...[
            const SizedBox(height: 6),
            Text(m.lastSyncSummary!, style: context.text.bodySmall),
          ],
          const SizedBox(height: 6),
          Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Icon(
                failed ? Icons.error_outline : (m.lastVerifyOk == true ? Icons.verified_outlined : Icons.schedule),
                size: 16,
                color: failed ? context.bd.crit : (m.lastVerifyOk == true ? context.bd.good : context.bd.muted),
              ),
              const SizedBox(width: 6),
              Expanded(
                child: Text(
                  m.lastVerifySummary ?? 'not verified yet',
                  style: context.text.bodySmall?.copyWith(color: failed ? context.bd.crit : null),
                ),
              ),
            ],
          ),
        ],
      ),
    );
  }
}

class _Place extends StatelessWidget {
  const _Place({required this.label, required this.path});
  final String label, path;

  @override
  Widget build(BuildContext context) => InkWell(
    onTap: () => openPath(context, path),
    child: Padding(
      padding: const EdgeInsets.symmetric(vertical: 2),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          SizedBox(width: 44, child: Muted(label)),
          Expanded(child: PathText(path, color: context.linkColor)),
        ],
      ),
    ),
  );
}

/// Manifests: signed lists of a drive's files, each as the document anyone can check.
class ManifestsScreen extends ConsumerWidget {
  const ManifestsScreen({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final manifests = ref.watch(manifestsProvider);
    final skew = ref.watch(skewProvider);
    return Scaffold(
      appBar: AppBar(title: const Text('Manifests')),
      body: RefreshIndicator(
        onRefresh: () => ref.refresh(manifestsProvider.future).catchError((_) => <Manifest>[]),
        child: LibraryBody(
          value: manifests,
          onRetry: () => ref.invalidate(manifestsProvider),
          data: (list) {
            final rows = <Widget Function()>[
              () => const Intro(
                'A manifest is a signed list of every file on a drive with its hash, as one scan saw it: '
                '`backdown manifest verify` checks it, anywhere, later. Share one to keep it with the drive\'s records.',
              ),
              if (list.isEmpty)
                () => const Padding(
                  padding: EdgeInsets.all(24),
                  child: EmptyState(
                    icon: Icons.verified_outlined,
                    title: 'No manifests yet',
                    message:
                        'Make one on the computer with `backdown manifest create`, or let the service sign one '
                        'after each scan (Attestations, in its Settings).',
                  ),
                ),
              if (list.isNotEmpty) () => SectionHeader('Signed manifests', trailing: Text('${list.length}')),
              for (final m in list) () => _ManifestTile(manifest: m, skew: skew),
            ];
            return ReadableList(itemCount: rows.length, itemBuilder: (c, i) => rows[i]());
          },
        ),
      ),
    );
  }
}

class _ManifestTile extends ConsumerWidget {
  const _ManifestTile({required this.manifest, required this.skew});
  final Manifest manifest;
  final int skew;

  String get _fileName {
    final name = baseName(manifest.drive);
    return 'manifest-${name.isEmpty || name == '/' ? 'drive' : name}-scan${manifest.scanId}-${manifest.id}.json';
  }

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final m = manifest;
    return ListCard(
      padding: const EdgeInsets.fromLTRB(14, 12, 6, 12),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Icon(Icons.verified_outlined, color: context.bd.good),
          const SizedBox(width: 12),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(baseName(m.drive), style: context.text.titleSmall),
                PathText(m.drive, maxLines: 1),
                const SizedBox(height: 4),
                Muted('${when(m.createdAt)} · ${rel(m.createdAt, skew: skew)}'),
                Muted('scan ${m.scanId} · ${plural(m.files, 'file')} · ${bytes(m.bytes)} · ${count(m.hashed)} hashed'),
                const SizedBox(height: 2),
                Text(
                  'key ${m.fingerprint}',
                  style: context.text.bodySmall?.copyWith(fontFamily: 'monospace', color: context.bd.muted),
                ),
              ],
            ),
          ),
          IconButton(
            tooltip: 'Share the signed document',
            icon: const Icon(Icons.share_outlined),
            onPressed: () =>
                shareDocument(context, ref, 'manifest', {'id': m.id}, _fileName, title: 'Manifest of ${m.drive}'),
          ),
        ],
      ),
    );
  }
}
