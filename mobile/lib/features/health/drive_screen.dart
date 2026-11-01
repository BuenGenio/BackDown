import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../../core/format.dart';
import '../../core/widgets.dart';
import '../../providers.dart';
import '../../router.dart';
import '../../theme.dart';
import '../activity/job_text.dart' show ago;
import 'health_model.dart';
import 'health_providers.dart';
import 'health_screen.dart' show checkHealthNow;
import 'health_widgets.dart';

/// Everything a drive says about itself: its verdict and why, every reading, how they moved since
/// the first one, and what got worse when (health.js showDetails).
class DriveScreen extends ConsumerWidget {
  const DriveScreen({super.key, required this.driveKey});

  /// [DriveHealth.key]: `sn:…`, a device, or `@machine:id`.
  final String driveKey;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final drive = ref.watch(driveHealthProvider(driveKey));
    return Scaffold(
      appBar: AppBar(title: Text(drive.value?.label ?? 'Drive health')),
      body: RefreshIndicator(
        onRefresh: () async {
          ref
            ..invalidate(healthProvider)
            ..invalidate(machinesHealthProvider);
          await ref.refresh(driveHealthProvider(driveKey).future).catchError((Object _) => null);
        },
        child: AsyncBody(
          value: drive,
          onRetry: () => ref.invalidate(driveHealthProvider(driveKey)),
          data: (d) => d == null
              ? const EmptyState(
                  icon: Icons.search_off,
                  title: 'Not known any more',
                  message: 'BackDown has no readings of this drive now.',
                )
              : _DriveBody(drive: d),
        ),
      ),
    );
  }
}

/// The readings a drive can give, as the web UI names them (health.js COUNTERS).
List<(String, String)> driveReadings(HealthReading h) {
  String n(int v) => count(v);
  return [
    if (h.temperature case final v?) ('Temperature', '$v °C'),
    if (h.powerOnHours case final v?) ('Powered on', '${n(v)} h (${(v / 8766).toStringAsFixed(1)} years)'),
    if (h.reallocated case final v?) ('Reallocated sectors', n(v)),
    if (h.pending case final v?) ('Pending sectors', n(v)),
    if (h.uncorrectable case final v?) ('Uncorrectable sectors', n(v)),
    if (h.badSectors case final v?) ('Bad sectors (reallocated + pending)', n(v)),
    if (h.crcErrors case final v?) ('Cable (CRC) errors', n(v)),
    if (h.percentageUsed case final v?) ('Write endurance used', '$v%'),
    if (h.availableSpare case final v?)
      (
        'Spare blocks left',
        '$v%${h.spareThreshold != null && h.spareThreshold! > 0 ? ' (its limit: ${h.spareThreshold}%)' : ''}',
      ),
    if (h.mediaErrors case final v?) ('Media errors', n(v)),
    if (h.criticalWarning case final v?) ('Critical warnings', v == 0 ? 'none' : '0x${v.toRadixString(16)}'),
    if (h.unsafeShutdowns case final v?) ('Unsafe shutdowns', n(v)),
    if (h.powerCycles case final v?) ('Power cycles', n(v)),
  ];
}

const _trends = [
  ('temp', 'Temperature', ' °C'),
  ('bad', 'Bad sectors', ''),
  ('wear', 'Wear', '%'),
  ('crc', 'Cable errors', ''),
  ('media', 'Media errors', ''),
];

class _DriveBody extends ConsumerWidget {
  const _DriveBody({required this.drive});
  final DriveHealth drive;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final skew = ref.watch(skewProvider);
    final d = drive;
    final h = d.health;
    final daemon = d.machine == null && (ref.watch(healthProvider).value?.daemon ?? false);
    final roots = ref.watch(rootsProvider).value ?? const [];
    final sub = [
      if (d.machine != null)
        'on ${d.machine}${h.device.isNotEmpty ? ' (${h.device})' : ''}'
      else if (d.present)
        h.device
      else
        'not connected',
      if (h.transport != null && h.transport!.isNotEmpty) h.transport!.toUpperCase(),
      if (h.serial != null) 'S/N ${h.serial}',
      if (h.protocol != null) h.protocol!.toUpperCase(),
    ].join(' · ');
    final trends = [
      for (final (key, name, unit) in _trends)
        if (d.series.where((p) => p.value(key) != null).length >= 2) (key, name, unit),
    ];
    final failing = d.verdict == 'failing';
    final readings = driveReadings(h);
    return ListView(
      physics: const AlwaysScrollableScrollPhysics(),
      children: [
        Readable(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Padding(
                padding: const EdgeInsets.fromLTRB(16, 8, 16, 4),
                child: Wrap(
                  spacing: 10,
                  runSpacing: 6,
                  crossAxisAlignment: WrapCrossAlignment.center,
                  children: [
                    VerdictPill(d.verdict),
                    Text(sub, style: context.text.bodySmall?.copyWith(color: context.colors.onSurfaceVariant)),
                  ],
                ),
              ),
              if (failing) _Callout(drive: d),
              if (d.findings.isNotEmpty)
                Padding(
                  padding: const EdgeInsets.fromLTRB(16, 8, 16, 4),
                  child: Column(
                    children: [for (final f in d.findings) _Line(level: f.level, text: _cap(f.text))],
                  ),
                )
              else if (d.verdict == 'ok')
                Padding(
                  padding: const EdgeInsets.fromLTRB(16, 8, 16, 4),
                  child: _Line(level: 'ok', text: 'Nothing in its health data points to trouble.'),
                ),
              if (h.note != null && h.note!.isNotEmpty)
                Padding(
                  padding: const EdgeInsets.fromLTRB(16, 8, 16, 4),
                  child: Card(
                    child: Padding(
                      padding: const EdgeInsets.all(12),
                      child: Row(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Icon(Icons.info_outline, size: 18, color: context.bd.muted),
                          const SizedBox(width: 10),
                          Expanded(child: Text(_cap(h.note!), style: context.text.bodyMedium)),
                        ],
                      ),
                    ),
                  ),
                ),
              if (readings.isNotEmpty) ...[
                const SectionHeader('Readings'),
                for (final (label, value) in readings) InfoRow(label, value),
              ],
              if (trends.isNotEmpty) ...[
                SectionHeader('Since ${day(d.firstAt ?? d.series.first.at)}'),
                for (final (key, name, unit) in trends) _Trend(points: d.series, field: key, name: name, unit: unit),
              ],
              if (d.trend.isNotEmpty)
                _Note('Since the first reading: ${d.trend.join(', ')}.')
              else if (d.readings > 1)
                const _Note('No change since the first reading.'),
              if (d.changes.isNotEmpty) ...[
                const SectionHeader('What changed'),
                for (final c in d.changes.reversed)
                  Padding(
                    padding: const EdgeInsets.fromLTRB(16, 4, 16, 4),
                    child: _Line(
                      level: c.level,
                      text: _cap(c.text),
                      trailing: ago(c.at, skew: skew),
                    ),
                  ),
              ],
              const SectionHeader('About it'),
              if (h.model != null) InfoRow('Model', h.model!),
              if (h.serial != null) InfoRow('Serial number', h.serial!, mono: true, copyable: true),
              if (h.wwn != null) InfoRow('WWN', h.wwn!, mono: true, copyable: true),
              if (h.device.isNotEmpty) InfoRow(d.present || d.machine != null ? 'Device' : 'Was', h.device, mono: true),
              if (h.size != null) InfoRow('Size', bytes(h.size)),
              if (h.rotational != null) InfoRow('Kind', h.rotational! ? 'Spinning disk' : 'Solid state'),
              if (h.reportedAs != null) InfoRow('Its own check', h.reportedAs!),
              if (h.failingAttributes.isNotEmpty)
                InfoRow('Failing now', h.failingAttributes.join(', '), valueColor: context.bd.crit),
              if (h.mounts.isNotEmpty)
                _Mounts(
                  mounts: h.mounts,
                  // another machine's mount points aren't this computer's drives
                  rootPaths: d.machine == null ? {for (final r in roots) r.path: r.key} : const {},
                ),
              Padding(
                padding: const EdgeInsets.fromLTRB(16, 16, 16, 24),
                child: Row(
                  crossAxisAlignment: CrossAxisAlignment.center,
                  children: [
                    Expanded(
                      child: Text(
                        [
                          'Read ${d.machine != null ? 'on ${d.machine} ' : ''}${ago(h.readAt, skew: skew)} from '
                              '${h.source.isEmpty ? 'nothing' : h.source}',
                          if (d.readings > 0)
                            '${plural(d.readings, 'reading')} kept${d.machine != null ? ' there' : ''}',
                          if (d.machine != null) 'fetched ${ago(d.fetchedAt, skew: skew)}, with its last sync',
                        ].join(' · '),
                        style: context.text.bodySmall?.copyWith(color: context.bd.muted),
                      ),
                    ),
                    if (daemon && d.present)
                      TextButton(onPressed: () => checkHealthNow(context, ref), child: const Text('Check now')),
                  ],
                ),
              ),
            ],
          ),
        ),
      ],
    );
  }
}

String _cap(String s) => s.isEmpty ? s : s[0].toUpperCase() + s.substring(1);

/// A finding or a change: its level as a dot, the words, and when.
class _Line extends StatelessWidget {
  const _Line({required this.level, required this.text, this.trailing});
  final String level, text;
  final String? trailing;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.symmetric(vertical: 4),
    child: Row(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Padding(padding: const EdgeInsets.only(top: 6, right: 10), child: StatusDot(verdictColor(context, level))),
        Expanded(child: Text(text, style: context.text.bodyMedium)),
        if (trailing != null)
          Padding(
            padding: const EdgeInsets.only(left: 8),
            child: Text(trailing!, style: context.text.bodySmall?.copyWith(color: context.bd.muted)),
          ),
      ],
    ),
  );
}

class _Note extends StatelessWidget {
  const _Note(this.text);
  final String text;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.fromLTRB(16, 8, 16, 4),
    child: Text(text, style: context.text.bodySmall?.copyWith(color: context.bd.muted)),
  );
}

/// One series as a line, with its range.
class _Trend extends StatelessWidget {
  const _Trend({required this.points, required this.field, required this.name, required this.unit});
  final List<HealthPoint> points;
  final String field, name, unit;

  @override
  Widget build(BuildContext context) {
    final vs = [for (final p in points) ?p.value(field)];
    final lo = vs.reduce((a, b) => a < b ? a : b), hi = vs.reduce((a, b) => a > b ? a : b);
    final worse = field != 'temp' && vs.last > vs.first;
    final color = worse ? context.bd.warn : context.bd.info;
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 6, 16, 6),
      child: Row(
        children: [
          SizedBox(width: 104, child: Text(name, style: context.text.bodyMedium)),
          Expanded(
            child: SizedBox(
              height: 36,
              child: Sparkline(points: points, field: field, color: color),
            ),
          ),
          const SizedBox(width: 10),
          SizedBox(
            width: 82,
            child: Text(
              lo == hi ? '$lo$unit' : '$lo$unit – $hi$unit',
              style: context.text.bodySmall?.copyWith(color: worse ? color : context.bd.muted),
              textAlign: TextAlign.end,
            ),
          ),
        ],
      ),
    );
  }
}

/// Where its volumes were mounted; an indexed drive opens in Drives.
class _Mounts extends StatelessWidget {
  const _Mounts({required this.mounts, required this.rootPaths});
  final List<String> mounts;
  final Map<String, String> rootPaths;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 7),
    child: Row(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        SizedBox(
          width: 112,
          child: Text('Mounted at', style: context.text.bodyMedium?.copyWith(color: context.colors.onSurfaceVariant)),
        ),
        Expanded(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              for (final m in mounts)
                if (rootPaths[m] case final key?)
                  InkWell(
                    onTap: () => context.push(Routes.folder(key)),
                    child: Padding(
                      padding: const EdgeInsets.only(bottom: 4),
                      child: Row(
                        mainAxisSize: MainAxisSize.min,
                        children: [
                          Flexible(
                            child: Text(m, style: context.text.bodyMedium?.copyWith(color: context.colors.primary)),
                          ),
                          const SizedBox(width: 4),
                          Icon(Icons.folder_open, size: 16, color: context.colors.primary),
                        ],
                      ),
                    ),
                  )
                else
                  Padding(
                    padding: const EdgeInsets.only(bottom: 4),
                    child: Text(m, style: context.text.bodyMedium),
                  ),
            ],
          ),
        ),
      ],
    ),
  );
}

/// What to do about a failing drive.
class _Callout extends StatelessWidget {
  const _Callout({required this.drive});
  final DriveHealth drive;

  @override
  Widget build(BuildContext context) {
    final crit = context.bd.crit;
    final where = drive.machine != null
        ? 'It\'s on ${drive.machine}: copy it there, with BackDown\'s Rescue on that machine.'
        : drive.present
        ? 'On the computer, BackDown\'s Rescue copies it once, good areas first, and never writes to it.'
        : 'Connect it, then copy it from Rescue on the computer.';
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 8, 16, 4),
      child: Container(
        padding: const EdgeInsets.all(14),
        decoration: BoxDecoration(
          color: crit.withValues(alpha: 0.08),
          borderRadius: BorderRadius.circular(14),
          border: Border.all(color: crit.withValues(alpha: 0.5)),
        ),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(
              'Copy what matters off this drive now.',
              style: context.text.titleSmall?.copyWith(color: crit, fontWeight: FontWeight.w700),
            ),
            const SizedBox(height: 4),
            const Text('A failing drive can stop reading at any moment, and every read wears it further.'),
            const SizedBox(height: 4),
            Text(where, style: context.text.bodySmall?.copyWith(color: context.colors.onSurfaceVariant)),
          ],
        ),
      ),
    );
  }
}
