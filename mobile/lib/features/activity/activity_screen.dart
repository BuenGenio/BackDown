import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../../api/json.dart';
import '../../core/actions.dart';
import '../../core/format.dart';
import '../../core/widgets.dart';
import '../../models/browse.dart';
import '../../models/daemon.dart';
import '../../providers.dart';
import '../../router.dart';
import '../../theme.dart';
import '../health/health_model.dart';
import '../health/health_providers.dart';
import '../health/health_widgets.dart';
import 'activity_data.dart';
import 'job_sheet.dart';
import 'job_text.dart';

/// The Activity tab: what the service is doing, what's next, when each drive is scanned, what ran
/// lately (ui/app.js renderActivity). Kept live by the long poll; times are the service's.
class ActivityScreen extends ConsumerWidget {
  const ActivityScreen({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final daemon = ref.watch(daemonProvider);
    final running = daemon.value?.running ?? false;
    return Scaffold(
      appBar: AppBar(
        title: const Text('Activity'),
        actions: [if (running) _Menu(db: daemon.value?.db ?? '')],
      ),
      body: RefreshIndicator(
        onRefresh: () async {
          ref
            ..invalidate(rootsProvider)
            ..invalidate(mirrorsProvider)
            ..invalidate(healthProvider)
            ..invalidate(daemonProvider);
        },
        child: AsyncBody(
          value: daemon,
          onRetry: () => ref.invalidate(daemonProvider),
          data: (s) => s.running ? _Live(snapshot: s) : const _NoService(),
        ),
      ),
    );
  }
}

enum _MenuItem { rescanAll, indexFolder }

class _Menu extends ConsumerWidget {
  const _Menu({required this.db});
  final String db;

  @override
  Widget build(BuildContext context, WidgetRef ref) => PopupMenuButton<_MenuItem>(
    tooltip: 'More actions',
    onSelected: (v) => switch (v) {
      _MenuItem.rescanAll => runAction(
        context,
        ref,
        (api) => api.post('rescan'),
        done: 'Rescans of every drive queued',
      ),
      _MenuItem.indexFolder => indexFolder(context, ref, windows: db.contains(r'\')),
    },
    itemBuilder: (c) => const [
      PopupMenuItem(
        value: _MenuItem.rescanAll,
        child: ListTile(leading: Icon(Icons.refresh), title: Text('Rescan every drive')),
      ),
      PopupMenuItem(
        value: _MenuItem.indexFolder,
        child: ListTile(leading: Icon(Icons.create_new_folder_outlined), title: Text('Index a folder…')),
      ),
    ],
  );
}

/// Asks for a folder on the computer and queues its first scan (POST /api/index; at the computer
/// only). BackDown only reads it.
Future<void> indexFolder(BuildContext context, WidgetRef ref, {bool windows = false}) async {
  final path = await showDialog<String>(
    context: context,
    builder: (c) => _IndexDialog(windows: windows),
  );
  if (path == null || path.trim().isEmpty || !context.mounted) return;
  final r = await runAction(
    context,
    ref,
    (api) => api.post('index', params: {'path': path.trim()}),
    done: 'Indexing: it shows up in Drives when it\'s done',
  );
  if (r != null) ref.invalidate(rootsProvider);
}

class _IndexDialog extends StatefulWidget {
  const _IndexDialog({required this.windows});
  final bool windows;

  @override
  State<_IndexDialog> createState() => _IndexDialogState();
}

class _IndexDialogState extends State<_IndexDialog> {
  final _path = TextEditingController();

  @override
  void dispose() {
    _path.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => AlertDialog(
    title: const Text('Index a folder'),
    content: Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        const Text('A folder or drive on the computer. BackDown only reads it.'),
        const SizedBox(height: 12),
        TextField(
          controller: _path,
          autofocus: true,
          autocorrect: false,
          enableSuggestions: false,
          decoration: InputDecoration(hintText: widget.windows ? r'D:\Photos' : '/media/me/Photos'),
          onSubmitted: (v) => Navigator.pop(context, v),
        ),
      ],
    ),
    actions: [
      TextButton(onPressed: () => Navigator.pop(context), child: const Text('Cancel')),
      FilledButton(onPressed: () => Navigator.pop(context, _path.text), child: const Text('Index it')),
    ],
  );
}

/// Everything the service is doing, top to bottom; built lazily, since the queue and the list of
/// recent jobs can be long.
class _Live extends ConsumerWidget {
  const _Live({required this.snapshot});
  final DaemonSnapshot snapshot;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final s = snapshot;
    final namer = JobNamer.watch(ref, s);
    final roots = ref.watch(rootsProvider).value;
    final health = ref.watch(healthProvider).value;
    final mirrors = ref.watch(mirrorsProvider).value ?? const [];
    final items = <Widget Function()>[
      () => const LookOnlyNote(what: 'Changes other than rescans'),
      if (roots != null && roots.isEmpty) () => _Welcome(windows: (s.db ?? '').contains(r'\')),
      if (s.current case final j?) () => _JobCard(job: j, namer: namer) else () => _IdleCard(snapshot: s, namer: namer),
      if (s.side case final j?) () => _JobCard(job: j, namer: namer, side: true),
      () => SectionHeader('Up next', trailing: s.queued.isEmpty ? null : Text('${s.queued.length}')),
      if (s.queued.isEmpty) () => const _Muted('Nothing waiting.'),
      for (final (i, j) in s.queued.indexed) () => _QueuedTile(job: j, index: i, namer: namer),
      () => const SectionHeader('Drives and schedule'),
      if (s.drives.isEmpty) () => const _Muted('No drives indexed yet.'),
      for (final d in s.drives)
        () => ScheduleTile(
          drive: d,
          root: namer.rootOf(d.path),
          name: namer.driveName(d.path),
          health: health?.ofPath(d.path),
          live: true,
        ),
      if (mirrors.isNotEmpty) ...[
        () => const SectionHeader('Mirrors'),
        for (final m in mirrors) () => _MirrorTile(mirror: m, namer: namer, health: health),
      ],
      () => SectionHeader('Recent', trailing: s.recent.isEmpty ? null : Text('${s.recent.length}')),
      if (s.recent.isEmpty) () => const _Muted('Nothing has run since BackDown started.'),
      for (final j in s.recent) () => _RecentTile(job: j, namer: namer),
      () => _Footer(snapshot: s),
    ];
    return ListView.builder(
      physics: const AlwaysScrollableScrollPhysics(),
      padding: const EdgeInsets.only(bottom: 24),
      itemCount: items.length,
      itemBuilder: (c, i) => Readable(child: items[i]()),
    );
  }
}

/// `backdown serve`: nothing runs in the background, so nothing rescans on its own.
class _NoService extends ConsumerWidget {
  const _NoService();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final roots = ref.watch(rootsProvider).value ?? const <Root>[];
    final health = ref.watch(healthProvider).value;
    final items = <Widget Function()>[
      () => Padding(
        padding: const EdgeInsets.fromLTRB(16, 8, 16, 4),
        child: Card(
          child: Padding(
            padding: const EdgeInsets.all(16),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Row(
                  children: [
                    Icon(Icons.pause_circle_outline, color: context.bd.muted),
                    const SizedBox(width: 10),
                    Expanded(child: Text('BackDown isn\'t running in the background', style: context.text.titleMedium)),
                  ],
                ),
                const SizedBox(height: 8),
                const Text(
                  'This computer\'s BackDown was started with `backdown serve`, so nothing rescans the drives on '
                  'its own. To keep them up to date, run BackDown as a service there: `backdown service install`.',
                ),
              ],
            ),
          ),
        ),
      ),
      () => const SectionHeader('Drives'),
      if (roots.isEmpty) () => const _Muted('No drives indexed yet: `backdown index /media/my-drive` on the computer.'),
      for (final r in roots)
        () => ScheduleTile(
          drive: DaemonDrive.fromJson({
            'path': r.path,
            'last_scan_at': r.scannedAt,
            'files': r.files,
            'bytes': r.bytes,
            'state': 'on-request',
          }),
          root: r,
          name: r.name.isEmpty ? r.path : r.name,
          health: health?.ofPath(r.path),
          live: false,
        ),
    ];
    return ListView.builder(
      physics: const AlwaysScrollableScrollPhysics(),
      padding: const EdgeInsets.only(bottom: 24),
      itemCount: items.length,
      itemBuilder: (c, i) => Readable(child: items[i]()),
    );
  }
}

class _Muted extends StatelessWidget {
  const _Muted(this.text);
  final String text;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.fromLTRB(16, 4, 16, 8),
    child: Text(text, style: context.text.bodyMedium?.copyWith(color: context.bd.muted)),
  );
}

/// Nothing indexed yet: where to start.
class _Welcome extends ConsumerWidget {
  const _Welcome({required this.windows});
  final bool windows;

  @override
  Widget build(BuildContext context, WidgetRef ref) => Padding(
    padding: const EdgeInsets.fromLTRB(16, 8, 16, 4),
    child: Card(
      color: context.colors.primaryContainer.withValues(alpha: 0.5),
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text('Welcome to BackDown', style: context.text.titleMedium),
            const SizedBox(height: 6),
            const Text(
              'Nothing is indexed yet. Choose a drive or folder on the computer for BackDown to read: it shows up '
              'in Drives when its scan is done.',
            ),
            const SizedBox(height: 10),
            FilledButton.tonalIcon(
              icon: const Icon(Icons.create_new_folder_outlined),
              label: const Text('Index a folder…'),
              onPressed: () => indexFolder(context, ref, windows: windows),
            ),
          ],
        ),
      ),
    ),
  );
}

/// A dot for something running. It doesn't pulse: the bar and the numbers move instead.
class _LiveDot extends StatelessWidget {
  const _LiveDot();

  @override
  Widget build(BuildContext context) => Container(
    width: 12,
    height: 12,
    decoration: BoxDecoration(
      color: context.bd.info,
      shape: BoxShape.circle,
      boxShadow: [BoxShadow(color: context.bd.info.withValues(alpha: 0.35), blurRadius: 0, spreadRadius: 4)],
    ),
  );
}

/// The running job (or, [side], the rescue or sync running beside it): how far, how long left.
class _JobCard extends ConsumerWidget {
  const _JobCard({required this.job, required this.namer, this.side = false});
  final Job job;
  final JobNamer namer;
  final bool side;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final skew = ref.watch(skewProvider);
    final j = job;
    final p = j.fraction;
    final stage = jobStage(j);
    final eta = jobEta(j, nowSeconds(skew));
    final detail = side ? (j.progress.isEmpty ? 'starting…' : j.progress) : jobDetail(j);
    final foot = [
      'Started ${ago(j.startedAt, skew: skew)}',
      if (side) 'runs beside scans and hashing' else if (j.reason.isNotEmpty) jobReason(j.reason),
      ?eta,
    ].join(' · ');
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 8, 16, 4),
      child: Card(
        clipBehavior: Clip.antiAlias,
        child: InkWell(
          onTap: () => showJobSheet(context, j),
          child: Padding(
            padding: const EdgeInsets.fromLTRB(16, 14, 12, 10),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Row(
                  children: [
                    const _LiveDot(),
                    const SizedBox(width: 12),
                    Expanded(
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Text(namer.title(j), style: context.text.titleMedium),
                          if (stage.isNotEmpty)
                            Text(stage, style: context.text.bodySmall?.copyWith(color: context.bd.muted)),
                        ],
                      ),
                    ),
                    if (p != null)
                      Text(percent(p), style: context.text.titleMedium?.copyWith(fontWeight: FontWeight.w700)),
                  ],
                ),
                const SizedBox(height: 12),
                JobMeter(fraction: p),
                const SizedBox(height: 10),
                Text(detail, style: context.text.bodyMedium),
                const SizedBox(height: 2),
                Row(
                  children: [
                    Expanded(
                      child: Text(foot, style: context.text.bodySmall?.copyWith(color: context.bd.muted)),
                    ),
                    if (j.stoppable)
                      TextButton.icon(
                        icon: const Icon(Icons.stop_circle_outlined, size: 18),
                        label: const Text('Stop'),
                        onPressed: () => stopJob(context, ref, j),
                      ),
                  ],
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// Nothing running: what's next.
class _IdleCard extends ConsumerWidget {
  const _IdleCard({required this.snapshot, required this.namer});
  final DaemonSnapshot snapshot;
  final JobNamer namer;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final skew = ref.watch(skewProvider);
    final s = snapshot;
    final due = [
      for (final d in s.drives)
        if (d.present && d.nextAt != null) d,
    ]..sort((a, b) => a.nextAt!.compareTo(b.nextAt!));
    final next = due.firstOrNull;
    final text = next != null
        ? 'Next: a scan of ${namer.driveName(next.path)} ${rel(next.nextAt, skew: skew)}.'
        : s.rescanEverySecs != null
        ? 'Nothing is due: drives that are away are scanned when they come back.'
        : 'Drives are scanned when you ask: the rescan button, or `backdown index` on the computer.';
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 8, 16, 4),
      child: Card(
        child: Padding(
          padding: const EdgeInsets.all(16),
          child: Row(
            children: [
              Icon(Icons.check_circle_outline, color: context.bd.good, size: 28),
              const SizedBox(width: 14),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text('Up to date', style: context.text.titleMedium),
                    const SizedBox(height: 2),
                    Text(text, style: context.text.bodyMedium?.copyWith(color: context.colors.onSurfaceVariant)),
                  ],
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _QueuedTile extends ConsumerWidget {
  const _QueuedTile({required this.job, required this.index, required this.namer});
  final Job job;
  final int index;
  final JobNamer namer;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final skew = ref.watch(skewProvider);
    final j = job;
    return ListTile(
      leading: Icon(jobIcon(j.kind)),
      title: Text('${index + 1}. ${namer.title(j)}', maxLines: 2, overflow: TextOverflow.ellipsis),
      subtitle: Padding(
        padding: const EdgeInsets.only(top: 4),
        child: Wrap(
          spacing: 8,
          runSpacing: 4,
          crossAxisAlignment: WrapCrossAlignment.center,
          children: [
            if (j.reason.isNotEmpty) Pill(jobReason(j.reason)),
            Text('queued ${ago(j.queuedAt, skew: skew)}'),
          ],
        ),
      ),
      trailing: IconButton(
        tooltip: 'Take it off the queue',
        icon: const Icon(Icons.close),
        onPressed: () => stopJob(context, ref, j),
      ),
      onTap: () => showJobSheet(context, j),
    );
  }
}

class _RecentTile extends ConsumerWidget {
  const _RecentTile({required this.job, required this.namer});
  final Job job;
  final JobNamer namer;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final skew = ref.watch(skewProvider);
    final j = job;
    final (icon, color) = switch (j.state) {
      'failed' => (Icons.error_outline, context.bd.crit),
      'stopped' => (Icons.stop_circle_outlined, context.bd.warn),
      _ => (Icons.check_circle_outline, context.bd.good),
    };
    final said = j.failed ? (j.error ?? 'failed') : j.summary;
    final took = jobTook(j);
    return ListTile(
      leading: Icon(icon, color: color),
      title: Text(namer.title(j), maxLines: 2, overflow: TextOverflow.ellipsis),
      subtitle: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          if (said.isNotEmpty)
            Text(
              said,
              maxLines: 2,
              overflow: TextOverflow.ellipsis,
              style: j.failed ? TextStyle(color: context.bd.crit) : null,
            ),
          Text(
            [
              ago(j.finishedAt, skew: skew),
              if (took != null) 'took ${dur(took)}',
              if (j.reason.isNotEmpty) jobReason(j.reason),
            ].join(' · '),
            style: context.text.bodySmall?.copyWith(color: context.bd.muted),
          ),
        ],
      ),
      onTap: () => showJobSheet(context, j),
    );
  }
}

/// An indexed drive: whether it's here, when it was last scanned and how, when it will be next,
/// and a rescan (ui/app.js drivesTable).
class ScheduleTile extends ConsumerWidget {
  const ScheduleTile({
    super.key,
    required this.drive,
    required this.root,
    required this.name,
    required this.health,
    required this.live,
  });

  final DaemonDrive drive;
  final Root? root;
  final String name;
  final DriveHealth? health;

  /// The service schedules scans (false for `backdown serve`).
  final bool live;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final skew = ref.watch(skewProvider);
    final x = drive;
    final (String label, Color? color) = switch (x.state) {
      'scanning' => ('Scanning now', context.bd.info),
      'queued' => ('Waiting its turn', context.bd.info),
      'away' => ('Not connected', context.bd.muted),
      'due' => ('Due now', context.bd.warn),
      'on-request' => ('On request', null),
      'scheduled' => ('Connected', context.bd.good),
      _ => (x.state, null),
    };
    final next = switch (x.state) {
      'away' => 'when it\'s plugged back in',
      'on-request' => null,
      'scanning' => 'now',
      'queued' => 'next in line',
      _ => x.nextAt == null ? null : rel(x.nextAt, skew: skew),
    };
    final away = x.state == 'away';
    final busy = x.state == 'scanning' || x.state == 'queued';
    final r = root;
    return ListTile(
      leading: Icon(
        away ? Icons.usb_off_outlined : Icons.storage,
        color: away ? context.bd.muted : context.bd.folder,
        size: 28,
      ),
      title: Text(name, maxLines: 1, overflow: TextOverflow.ellipsis),
      subtitle: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Padding(
            padding: const EdgeInsets.symmetric(vertical: 3),
            child: Wrap(
              spacing: 6,
              runSpacing: 4,
              children: [
                if (live) Pill(label, color: color),
                if (health != null) VerdictPill(health!.verdict),
              ],
            ),
          ),
          Text(
            [
              'Last scan ${x.lastScanAt != null ? ago(x.lastScanAt, skew: skew) : 'never finished'}',
              if (live && next != null) 'next $next',
            ].join(' · '),
          ),
          if (x.lastHow != null && x.lastHow!.isNotEmpty)
            Text(
              x.lastHow!,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: context.text.bodySmall?.copyWith(color: context.bd.muted),
            ),
          Text(
            '${plural(x.files, 'file')} · ${bytes(x.bytes)}',
            style: context.text.bodySmall?.copyWith(color: context.bd.muted),
          ),
        ],
      ),
      trailing: live && r != null && x.present
          ? IconButton(
              tooltip: 'Rescan now',
              icon: const Icon(Icons.refresh),
              onPressed: busy ? null : () => rescanDrive(context, ref, r.id, name),
            )
          : null,
      onTap: r == null ? null : () => context.push(Routes.folder(r.key)),
    );
  }
}

class _MirrorTile extends ConsumerWidget {
  const _MirrorTile({required this.mirror, required this.namer, required this.health});
  final MirrorInfo mirror;
  final JobNamer namer;
  final HealthOverview? health;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final skew = ref.watch(skewProvider);
    final m = mirror;
    final bad = m.lastVerifyOk == false;
    final dying = health?.ofPath(m.target);
    final warn = dying != null && verdictRank(dying.verdict) > 1;
    return ListTile(
      leading: Icon(Icons.sync_alt, color: bad ? context.bd.crit : context.bd.good),
      title: Text('${namer.driveName(m.source)} → ${m.target}', maxLines: 2, overflow: TextOverflow.ellipsis),
      subtitle: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(m.lastVerifySummary ?? 'not verified yet', maxLines: 2, overflow: TextOverflow.ellipsis),
          if (warn)
            Text(
              'This mirror drive is dying (${dying.label}: ${verdictWord(dying.verdict)}): add another mirror on a '
              'healthy drive.',
              style: TextStyle(color: verdictColor(context, dying.verdict)),
            ),
          const SizedBox(height: 4),
          Wrap(
            spacing: 8,
            runSpacing: 4,
            crossAxisAlignment: WrapCrossAlignment.center,
            children: [
              Pill(
                m.connected ? 'both drives here' : 'a drive isn\'t connected',
                color: m.connected ? context.bd.good : null,
              ),
              Text(
                'synced ${ago(m.lastSync, skew: skew)} · verified ${ago(m.lastVerify, skew: skew)}',
                style: context.text.bodySmall?.copyWith(color: context.bd.muted),
              ),
            ],
          ),
        ],
      ),
      onTap: () => context.push(Routes.mirrors),
    );
  }
}

/// The service itself: its version, since when, how often it rescans, the index it keeps.
class _Footer extends ConsumerWidget {
  const _Footer({required this.snapshot});
  final DaemonSnapshot snapshot;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final skew = ref.watch(skewProvider);
    final s = snapshot;
    final ways = _scanWays(ref.watch(scanWaysProvider).value);
    final style = context.text.bodySmall?.copyWith(color: context.bd.muted);
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 24, 16, 8),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            [
              'BackDown ${s.version ?? ''}'.trim(),
              'running since ${ago(s.startedAt, skew: skew)}',
              s.rescanEverySecs != null ? 'rescans each drive every ${dur(s.rescanEverySecs!)}' : 'rescans on request',
              'drives checked ${ago(s.checkedAt, skew: skew)}',
            ].join(' · '),
            style: style,
          ),
          if (s.db != null) ...[
            const SizedBox(height: 4),
            Text.rich(
              TextSpan(
                text: 'Index ',
                children: [
                  TextSpan(
                    text: s.db,
                    style: const TextStyle(fontFamily: 'monospace'),
                  ),
                ],
              ),
              style: style,
            ),
          ],
          if (ways != null) ...[const SizedBox(height: 4), Text(ways, style: style)],
        ],
      ),
    );
  }

  /// How rescans are made, from the scan settings (ui/app.js scanWays).
  static String? _scanWays(Json? a) {
    final s = a?.objOrNull('settings');
    if (s == null) return null;
    final safety = [
      if (s.integer('walk_every_scans') > 0) 'every ${s.integer('walk_every_scans')} scans',
      if (s.integer('walk_every_days') > 0) 'every ${s.integer('walk_every_days')} days',
    ].join(' or ');
    if (!s.flag('fast')) return 'Every scan walks every folder (the change log and file table are turned off).';
    return 'Rescans use the drive\'s change log or file table when they can'
        '${safety.isEmpty ? '' : ', and walk every folder $safety to be sure'}.';
  }
}
