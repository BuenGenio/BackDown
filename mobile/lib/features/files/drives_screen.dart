import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../../api/api.dart';
import '../../core/actions.dart';
import '../../core/format.dart';
import '../../core/widgets.dart';
import '../../models/browse.dart';
import '../../models/daemon.dart';
import '../../providers.dart';
import '../../router.dart';
import '../../theme.dart';
import '../activity/job_text.dart';
import '../app_settings/server_switcher.dart';

/// Home: what the service is doing, the index in numbers, and every drive it has indexed.
class DrivesScreen extends ConsumerWidget {
  const DrivesScreen({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final roots = ref.watch(rootsProvider);
    return Scaffold(
      appBar: AppBar(
        title: const ServerTitle(),
        actions: [
          IconButton(tooltip: 'Search', icon: const Icon(Icons.search), onPressed: () => context.go(Routes.search)),
        ],
      ),
      body: RefreshIndicator(
        onRefresh: () async {
          ref.invalidate(indexStatusProvider);
          await ref.refresh(rootsProvider.future).catchError((_) => <Root>[]);
        },
        child: AsyncBody(
          value: roots,
          onRetry: () => ref.invalidate(rootsProvider),
          data: (list) => ListView(
            padding: const EdgeInsets.only(bottom: 24),
            children: [
              const _StatusCard(),
              const _Totals(),
              SectionHeader('Drives', trailing: Text('${list.length}', style: context.text.labelMedium)),
              if (list.isEmpty)
                const Padding(
                  padding: EdgeInsets.all(24),
                  child: EmptyState(
                    icon: Icons.storage_outlined,
                    title: 'Nothing indexed yet',
                    message: 'On the computer, add a drive with `backdown index /path/to/drive`.',
                  ),
                ),
              for (final r in list) _DriveTile(root: r),
              const SectionHeader('Explore'),
              const _Shortcuts(),
            ],
          ),
        ),
      ),
    );
  }
}

/// What the service is doing now, in one card (tap for Activity).
class _StatusCard extends ConsumerWidget {
  const _StatusCard();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final d = ref.watch(daemonProvider);
    final skew = ref.watch(skewProvider);
    final Widget body;
    if (d.value case final s?) {
      final job = s.current ?? s.side;
      if (!s.running) {
        body = _line(
          context,
          Icons.pause_circle_outline,
          context.bd.muted,
          'No background service',
          'This is `backdown serve`: it shows the index, and scans run from the command line.',
        );
      } else if (job != null) {
        final f = job.fraction;
        final left = job.secondsLeft(nowSeconds(skew));
        body = Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            _line(
              context,
              jobIcon(job.kind),
              context.bd.info,
              jobTitle(job),
              [
                if (job.progress.isNotEmpty) job.progress,
                if (left != null) 'about ${dur(left)} left',
                if (s.queued.isNotEmpty) '${s.queued.length} more queued',
              ].join(' · '),
            ),
            const SizedBox(height: 10),
            Meter(fraction: f, color: context.bd.info),
          ],
        );
      } else {
        final last = s.recent.firstOrNull;
        body = _line(
          context,
          Icons.check_circle_outline,
          context.bd.good,
          'Idle',
          last == null ? 'Nothing has run yet.' : 'Last: ${jobTitle(last)}, ${rel(last.finishedAt, skew: skew)}',
        );
      }
    } else if (d.hasError) {
      final e = d.error;
      body = _line(
        context,
        Icons.cloud_off_outlined,
        context.bd.crit,
        e is ApiException && e.failure == ApiFailure.offline ? 'Can\'t reach the computer' : 'Not connected',
        e.toString(),
      );
    } else {
      body = const SizedBox(height: 44, child: Center(child: LinearProgressIndicator()));
    }
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 8, 16, 4),
      child: Card(
        clipBehavior: Clip.antiAlias,
        child: InkWell(
          onTap: () => context.go(Routes.activity),
          child: Padding(padding: const EdgeInsets.all(14), child: body),
        ),
      ),
    );
  }

  Widget _line(BuildContext context, IconData icon, Color color, String title, String sub) => Row(
    children: [
      Icon(icon, color: color),
      const SizedBox(width: 12),
      Expanded(
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(title, style: context.text.titleSmall, maxLines: 1, overflow: TextOverflow.ellipsis),
            if (sub.isNotEmpty)
              Text(
                sub,
                style: context.text.bodySmall?.copyWith(color: context.colors.onSurfaceVariant),
                maxLines: 2,
                overflow: TextOverflow.ellipsis,
              ),
          ],
        ),
      ),
      Icon(Icons.chevron_right, color: context.bd.muted),
    ],
  );
}

class _Totals extends ConsumerWidget {
  const _Totals();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final st = ref.watch(indexStatusProvider).value;
    if (st == null) return const SizedBox.shrink();
    final hashed = st.files == 0 ? null : st.hashed / st.files;
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 8, 16, 0),
      child: IntrinsicHeight(
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Expanded(
              child: StatTile(label: 'Files', value: count(st.files), icon: Icons.description_outlined),
            ),
            const SizedBox(width: 10),
            Expanded(
              child: StatTile(label: 'Size', value: bytes(st.bytes), icon: Icons.data_usage),
            ),
            const SizedBox(width: 10),
            Expanded(
              child: StatTile(
                label: 'Hashed',
                value: percent(hashed),
                icon: Icons.tag,
                sub: st.unreadable > 0 ? '${count(st.unreadable)} unreadable' : null,
              ),
            ),
          ],
        ),
      ),
    );
  }
}

class _DriveTile extends ConsumerWidget {
  const _DriveTile({required this.root});
  final Root root;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final skew = ref.watch(skewProvider);
    final DaemonDrive? live = ref.watch(daemonProvider).value?.drives.where((d) => d.path == root.path).firstOrNull;
    final state = live?.state;
    final (label, color) = switch (state) {
      'scanning' => ('Scanning', context.bd.info),
      'queued' => ('Queued', context.bd.info),
      'away' => ('Not connected', context.bd.muted),
      'due' => ('Scan due', context.bd.warn),
      _ => (null, null),
    };
    return ListTile(
      leading: Icon(
        state == 'away' ? Icons.usb_off_outlined : Icons.storage,
        color: state == 'away' ? context.bd.muted : context.bd.folder,
        size: 30,
      ),
      title: Text(root.name.isEmpty ? root.path : root.name, maxLines: 1, overflow: TextOverflow.ellipsis),
      subtitle: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          if (root.path != root.name) Text(root.path, maxLines: 1, overflow: TextOverflow.ellipsis),
          Text(
            '${bytes(root.bytes)} · ${plural(root.files, 'file')} · ${rel(root.scannedAt, skew: skew)}',
            maxLines: 1,
            overflow: TextOverflow.ellipsis,
          ),
          if (label != null) ...[const SizedBox(height: 4), Pill(label, color: color)],
        ],
      ),
      trailing: IconButton(
        tooltip: 'Rescan',
        icon: const Icon(Icons.refresh),
        onPressed: state == 'scanning' || state == 'queued'
            ? null
            : () => runAction(
                context,
                ref,
                (api) => api.post('rescan', params: {'root': root.id}),
                done: 'Rescan of ${root.name} queued',
              ),
      ),
      onTap: () => context.push(Routes.folder(root.key)),
    );
  }
}

class _Shortcuts extends StatelessWidget {
  const _Shortcuts();

  @override
  Widget build(BuildContext context) {
    final items = [
      (Icons.sell_outlined, "What's here", Routes.entities),
      (Icons.content_copy_outlined, 'Duplicates', Routes.dupes),
      (Icons.hub_outlined, 'Swarm', Routes.swarm),
      (Icons.dns_outlined, 'Machines', Routes.machines),
    ];
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 16),
      child: GridView.count(
        crossAxisCount: 2,
        shrinkWrap: true,
        physics: const NeverScrollableScrollPhysics(),
        mainAxisSpacing: 10,
        crossAxisSpacing: 10,
        childAspectRatio: 2.8,
        children: [
          for (final (icon, label, route) in items)
            Card(
              clipBehavior: Clip.antiAlias,
              child: InkWell(
                onTap: () => context.push(route),
                child: Padding(
                  padding: const EdgeInsets.symmetric(horizontal: 14),
                  child: Row(
                    children: [
                      Icon(icon, color: context.colors.primary),
                      const SizedBox(width: 10),
                      Expanded(child: Text(label, style: context.text.titleSmall)),
                    ],
                  ),
                ),
              ),
            ),
        ],
      ),
    );
  }
}
