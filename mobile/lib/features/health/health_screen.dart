import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../../api/api.dart';
import '../../core/actions.dart';
import '../../core/format.dart';
import '../../core/widgets.dart';
import '../../providers.dart';
import '../../router.dart';
import '../../theme.dart';
import '../activity/job_text.dart' show ago;
import 'health_model.dart';
import 'health_providers.dart';
import 'health_widgets.dart';
import 'routes.dart';

/// The Health tab: what each of this computer's drives says about itself, the other machines'
/// drives as their own BackDown read them, the service's notifications, and which of them pop up
/// on the computer (ui/health.js). Nothing here touches a drive: the service reads them every six
/// hours, when one is plugged in, and when asked.
class HealthScreen extends ConsumerStatefulWidget {
  const HealthScreen({super.key});

  @override
  ConsumerState<HealthScreen> createState() => _HealthScreenState();
}

class _HealthScreenState extends ConsumerState<HealthScreen> {
  bool _allNotes = false;

  Future<void> _refresh() async {
    ref
      ..invalidate(notificationsProvider)
      ..invalidate(machinesHealthProvider)
      ..invalidate(notifySettingsProvider);
    await ref.refresh(healthProvider.future).catchError((Object _) => HealthOverview.fromJson(const {}));
  }

  @override
  Widget build(BuildContext context) {
    final health = ref.watch(healthProvider);
    final daemon = health.value?.daemon ?? false;
    return Scaffold(
      appBar: AppBar(
        title: const Text('Health'),
        actions: [
          if (daemon) _CheckNowButton(running: health.value?.running ?? false),
          IconButton(
            tooltip: 'Notifications on this phone',
            icon: const Icon(Icons.notifications_outlined),
            onPressed: () => context.push(Routes.appSettings),
          ),
        ],
      ),
      body: RefreshIndicator(
        onRefresh: _refresh,
        child: AsyncBody(value: health, onRetry: () => ref.invalidate(healthProvider), data: (h) => _body(context, h)),
      ),
    );
  }

  Widget _body(BuildContext context, HealthOverview h) {
    final notes = ref.watch(notificationsProvider);
    final machines = ref.watch(machinesHealthProvider);
    final server = ref.watch(currentServerProvider);
    final unread = notes.value?.unread ?? 0;
    final items = <Widget Function()>[
      () => const LookOnlyNote(),
      () => _StatusCard(overview: h),
      for (final d in h.drives)
        if (d.present && d.verdict == 'failing') () => _FailingCallout(drive: d),
      if (unread > 0) ..._notesSection(context, notes, h, machines.value),
      () => SectionHeader(
        'Drives on ${server?.name ?? 'the computer'}',
        trailing: Text('${h.drives.length}', style: context.text.labelMedium),
      ),
      if (h.drives.isEmpty)
        () => _Muted(
          h.daemon
              ? 'No drives read yet: BackDown reads them shortly after it starts, and every six hours.'
              : 'No drives found to read here.',
        ),
      for (final d in h.drives) () => DriveHealthTile(drive: d),
      for (final n in h.notes) () => _Muted(n),
      if (h.error != null) () => _Muted('Reading them failed: ${h.error}', color: context.bd.crit),
      if (unread == 0) ..._notesSection(context, notes, h, machines.value),
      ..._machinesSection(context, machines),
      () => const _DesktopSettings(),
      () => const SizedBox(height: 32),
    ];
    return ListView.builder(
      physics: const AlwaysScrollableScrollPhysics(),
      itemCount: items.length,
      itemBuilder: (c, i) => Readable(child: items[i]()),
    );
  }

  List<Widget Function()> _notesSection(
    BuildContext context,
    AsyncValue<Notifications> value,
    HealthOverview h,
    MachinesHealth? machines,
  ) {
    final n = value.value;
    final header = SectionHeader(
      'Notifications',
      trailing: n == null || n.unread == 0
          ? null
          : Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                Pill('${n.unread} unread', color: context.colors.primary),
                const SizedBox(width: 4),
                TextButton(onPressed: () => _markRead('all'), child: const Text('Mark all read')),
              ],
            ),
    );
    if (n == null) {
      return [
        () => header,
        if (value.hasError) () => _Muted(value.error.toString()) else () => const _Muted('Loading…'),
      ];
    }
    if (n.notes.isEmpty) {
      return [
        () => header,
        () => const _Muted(
          'Nothing to tell: BackDown notifies when a drive gets worse, a mirror stops matching its source, or a '
          'scheduled job fails.',
        ),
      ];
    }
    final few = math.max(5, n.unread);
    final shown = _allNotes ? n.notes : n.notes.take(few).toList();
    bool known(String key) =>
        h.drives.any((d) => d.key == key || d.id == key) ||
        (machines?.machines.any((m) => m.drives.any((d) => d.key == key)) ?? false);
    return [
      () => header,
      for (final note in shown)
        () => NotificationTile(
          note: note,
          onRead: () => _markRead('${note.id}'),
          onDetails: note.drive != null && known(note.drive!)
              ? () => context.push(healthDrivePath(note.drive!))
              : note.kind == 'job'
              ? () => context.go(Routes.activity)
              : null,
        ),
      if (n.notes.length > shown.length)
        () => Padding(
          padding: const EdgeInsets.symmetric(horizontal: 8),
          child: Align(
            alignment: Alignment.centerLeft,
            child: TextButton(
              onPressed: () => setState(() => _allNotes = true),
              child: Text('Show ${n.notes.length - shown.length} older'),
            ),
          ),
        ),
    ];
  }

  Future<void> _markRead(String which) async {
    final r = await runAction(context, ref, (api) => api.post('notifications', params: {'read': which}));
    if (r != null) ref.invalidate(notificationsProvider);
  }

  List<Widget Function()> _machinesSection(BuildContext context, AsyncValue<MachinesHealth> value) {
    const header = SectionHeader('Other machines\' drives');
    final err = value.error;
    if (err is ApiException && err.failure == ApiFailure.needsTier) {
      // locked (Team): said only to someone who has added machines
      if ((ref.watch(machineCountProvider).value ?? 0) == 0) return const [];
      final unlocks = err.detail?['unlocks'];
      return [
        () => header,
        () => _Muted([err.message, if (unlocks is String) unlocks].join(' ')),
      ];
    }
    if (err != null) return [() => header, () => _Muted(err.toString())];
    final m = value.value;
    if (m == null || m.machines.isEmpty) return const [];
    return [
      () => header,
      () => const _Muted('As each machine\'s own BackDown read them, fetched when it\'s synced.'),
      if (m.whyNot != null) () => _Muted(_capital(m.whyNot!)),
      for (final machine in m.machines) ...[
        () => _MachineHeader(machine: machine),
        for (final d in machine.drives) () => DriveHealthTile(drive: d),
        if (machine.note != null) () => _Muted(_capital(machine.note!)),
        if (machine.notes.isNotEmpty) () => _MachineNotes(machine: machine),
      ],
    ];
  }
}

String _capital(String s) => s.isEmpty ? s : '${s[0].toUpperCase()}${s.substring(1)}${s.endsWith('.') ? '' : '.'}';

class _Muted extends StatelessWidget {
  const _Muted(this.text, {this.color});
  final String text;
  final Color? color;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.fromLTRB(16, 4, 16, 8),
    child: Text(text, style: context.text.bodySmall?.copyWith(color: color ?? context.bd.muted)),
  );
}

/// "Check now": the service reads every drive again (at the computer only).
class _CheckNowButton extends ConsumerWidget {
  const _CheckNowButton({required this.running});
  final bool running;

  @override
  Widget build(BuildContext context, WidgetRef ref) => IconButton(
    tooltip: running ? 'Reading drive health…' : 'Check now',
    icon: const Icon(Icons.refresh),
    onPressed: running ? null : () => checkHealthNow(context, ref),
  );
}

/// Asks the service to read every drive's health now.
Future<void> checkHealthNow(BuildContext context, WidgetRef ref) async {
  final r = await runAction(context, ref, (api) => api.post('health-check'), done: 'Reading every drive\'s health now');
  if (r != null) ref.invalidate(healthProvider);
}

/// How the drives are, in a line, and when they were read.
class _StatusCard extends ConsumerWidget {
  const _StatusCard({required this.overview});
  final HealthOverview overview;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final skew = ref.watch(skewProvider);
    final h = overview;
    final troubled = h.troubled;
    final failing = troubled.where((d) => d.verdict == 'failing').toList();
    final present = h.drives.where((d) => d.present).toList();
    final worst = h.worst;
    final (IconData icon, Color color, String title) = h.running
        ? (Icons.sync, context.bd.info, 'Reading drive health…')
        : failing.isNotEmpty
        ? (Icons.dangerous_outlined, context.bd.crit, '${failing.first.label} is failing')
        : troubled.isNotEmpty
        ? (
            Icons.error_outline,
            context.bd.warn,
            '${troubled.length} drive${troubled.length == 1 ? ' needs' : 's need'} watching',
          )
        : present.any((d) => d.verdict == 'ok')
        ? (Icons.verified_outlined, context.bd.good, 'The connected drives look healthy')
        : (Icons.help_outline, context.bd.muted, 'No health data from the connected drives');
    final sub = h.daemon
        ? [
            if (h.checkedAt != null) 'Read ${ago(h.checkedAt, skew: skew)}',
            if (h.nextAt != null && !h.running) 'next ${rel(h.nextAt, skew: skew)}',
          ].join(' · ')
        : 'Read just now. This is `backdown serve`: drives are read when asked, and nothing is kept between '
              'readings unless BackDown runs as a service.';
    final more = failing.length > 1
        ? '${failing.length - 1} more failing.'
        : troubled.isNotEmpty && failing.isEmpty
        ? 'Make sure what\'s on ${troubled.length == 1 ? 'it' : 'them'} has another copy.'
        : null;
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 8, 16, 4),
      child: Card(
        child: Padding(
          padding: const EdgeInsets.all(14),
          child: Row(
            children: [
              Icon(icon, color: color, size: 30),
              const SizedBox(width: 14),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(title, style: context.text.titleMedium),
                    if (sub.isNotEmpty)
                      Text(sub, style: context.text.bodySmall?.copyWith(color: context.colors.onSurfaceVariant)),
                    if (more != null)
                      Padding(
                        padding: const EdgeInsets.only(top: 4),
                        child: Text(more, style: context.text.bodySmall?.copyWith(color: verdictColor(context, worst))),
                      ),
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

/// A connected drive that's failing: what to do about it, now.
class _FailingCallout extends StatelessWidget {
  const _FailingCallout({required this.drive});
  final DriveHealth drive;

  @override
  Widget build(BuildContext context) {
    final why = drive.why ?? 'its own check says it is failing';
    final crit = context.bd.crit;
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 8, 16, 4),
      child: Card(
        color: crit.withValues(alpha: 0.08),
        shape: RoundedRectangleBorder(
          borderRadius: BorderRadius.circular(14),
          side: BorderSide(color: crit.withValues(alpha: 0.5)),
        ),
        child: Padding(
          padding: const EdgeInsets.all(14),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  Icon(Icons.warning_amber_rounded, color: crit),
                  const SizedBox(width: 10),
                  Expanded(
                    child: Text(
                      '${drive.label} is failing',
                      style: context.text.titleMedium?.copyWith(color: crit, fontWeight: FontWeight.w700),
                    ),
                  ),
                ],
              ),
              const SizedBox(height: 6),
              Text(
                '${drive.health.device.isEmpty ? '' : '${drive.health.device}: '}$why. What\'s on it may soon be '
                'unreadable: copy it now, while it still reads, to another drive. On the computer, BackDown\'s '
                'Rescue copies it once, good areas first, and never writes to it.',
              ),
              Align(
                alignment: Alignment.centerRight,
                child: TextButton(
                  onPressed: () => context.push(healthDrivePath(drive.key)),
                  child: const Text('Details'),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _MachineHeader extends ConsumerWidget {
  const _MachineHeader({required this.machine});
  final MachineHealth machine;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final skew = ref.watch(skewProvider);
    final m = machine;
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 12, 16, 0),
      child: Row(
        children: [
          Icon(Icons.dns_outlined, size: 20, color: context.colors.onSurfaceVariant),
          const SizedBox(width: 10),
          Text(m.name, style: context.text.titleSmall?.copyWith(fontWeight: FontWeight.w700)),
          if (verdictRank(m.worst) > 1) ...[const SizedBox(width: 8), VerdictPill(m.worst)],
          const SizedBox(width: 8),
          Expanded(
            child: Text(
              m.lastSync != null ? 'synced ${ago(m.lastSync, skew: skew)}' : 'not synced yet',
              style: context.text.bodySmall?.copyWith(color: context.bd.muted),
              textAlign: TextAlign.end,
              overflow: TextOverflow.ellipsis,
            ),
          ),
        ],
      ),
    );
  }
}

/// Another machine's own latest notifications (a few).
class _MachineNotes extends ConsumerWidget {
  const _MachineNotes({required this.machine});
  final MachineHealth machine;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final skew = ref.watch(skewProvider);
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 0, 16, 8),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text('Its own latest notifications:', style: context.text.bodySmall?.copyWith(color: context.bd.muted)),
          for (final n in machine.notes.take(3))
            Padding(
              padding: const EdgeInsets.only(top: 4),
              child: Row(
                children: [
                  StatusDot(n.level == 'info' ? context.bd.info : verdictColor(context, n.level), size: 7),
                  const SizedBox(width: 8),
                  Expanded(child: Text(n.title, style: context.text.bodySmall)),
                  Text(
                    ago(n.at, skew: skew),
                    style: context.text.bodySmall?.copyWith(color: context.bd.muted),
                  ),
                ],
              ),
            ),
        ],
      ),
    );
  }
}

/// Which news also pops up on the computer's desktop, and a test (GET/POST /api/notify-settings,
/// POST /api/notify-test). The phone's own notifications are in the app's settings.
class _DesktopSettings extends ConsumerWidget {
  const _DesktopSettings();

  static const _rows = [
    ('desktop', 'Show notifications on the desktop'),
    ('health', 'When a drive\'s health gets worse'),
    ('drift', 'When a mirror no longer matches what it copies'),
    ('jobs', 'When a scheduled job fails'),
  ];

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final value = ref.watch(notifySettingsProvider);
    final s = value.value;
    Future<void> set(String key, bool on) async {
      final r = await runAction(
        context,
        ref,
        (api) => api.post('notify-settings', params: {key: on ? 1 : 0}),
        done: 'Saved',
      );
      if (r != null) ref.invalidate(notifySettingsProvider);
    }

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        const SectionHeader('Notifications on the computer'),
        if (s == null)
          _Muted(value.hasError ? value.error.toString() : 'Loading…')
        else ...[
          const _Muted('Health lists everything; these choose what also pops up on the computer\'s desktop.'),
          if (s.why != null) _Muted(_capital(s.why!)),
          for (final (key, label) in _rows)
            if (s[key] case final on?) SwitchListTile(title: Text(label), value: on, onChanged: (v) => set(key, v)),
          if (s.available)
            Padding(
              padding: const EdgeInsets.fromLTRB(16, 4, 16, 4),
              child: OutlinedButton.icon(
                icon: const Icon(Icons.notifications_active_outlined),
                label: const Text('Send a test notification'),
                onPressed: () => runAction(
                  context,
                  ref,
                  (api) => api.post('notify-test'),
                  done: 'Sent: it should be on the computer\'s desktop now',
                ),
              ),
            ),
        ],
        ListTile(
          leading: const Icon(Icons.phone_android),
          title: const Text('Notifications on this phone'),
          subtitle: const Text('Failed and finished jobs, and drive health news, checked in the background'),
          trailing: const Icon(Icons.chevron_right),
          onTap: () => context.push(Routes.appSettings),
        ),
      ],
    );
  }
}
