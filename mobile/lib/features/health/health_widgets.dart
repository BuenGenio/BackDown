import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../../core/format.dart';
import '../../core/widgets.dart';
import '../../providers.dart';
import '../../theme.dart';
import '../activity/job_text.dart' show ago;
import 'health_model.dart';
import 'routes.dart';

/// A verdict's colour: green healthy, amber warning, red failing, grey when there's no data.
Color verdictColor(BuildContext context, String verdict) => switch (verdict) {
  'ok' => context.bd.good,
  'warning' => context.bd.warn,
  'failing' => context.bd.crit,
  'info' => context.bd.info,
  _ => context.bd.muted,
};

/// "healthy", "warning", "failing", "no health data", in the verdict's colour.
class VerdictPill extends StatelessWidget {
  const VerdictPill(this.verdict, {super.key});
  final String verdict;

  @override
  Widget build(BuildContext context) => Pill(
    verdictWord(verdict),
    color: verdictColor(context, verdict),
    icon: switch (verdict) {
      'ok' => Icons.check_circle_outline,
      'warning' => Icons.error_outline,
      'failing' => Icons.dangerous_outlined,
      _ => Icons.help_outline,
    },
  );
}

/// The kind of drive, by how it's attached and whether it spins.
IconData driveIcon(HealthReading h) {
  if (h.transport == 'usb') return Icons.usb;
  if (h.protocol == 'nvme' || h.transport == 'nvme') return Icons.memory;
  if (h.rotational == false) return Icons.sd_storage_outlined;
  return Icons.storage;
}

/// What a drive is, in a line: its device (or "not connected"), how it's attached, its size.
String driveLine(DriveHealth d, {int skew = 0}) {
  final h = d.health;
  return [
    if (d.machine != null) 'on ${d.machine}' else if (d.present) h.device else 'not connected',
    if (h.transport != null && h.transport!.isNotEmpty) h.transport!.toUpperCase(),
    if (h.rotational == true) 'spinning disk' else if (h.rotational == false) 'solid state',
    if (h.size != null) bytes(h.size),
  ].join(' · ');
}

/// A drive in a list: its name, verdict and why; tap for everything it says.
class DriveHealthTile extends ConsumerWidget {
  const DriveHealthTile({super.key, required this.drive});
  final DriveHealth drive;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final skew = ref.watch(skewProvider);
    final d = drive;
    final stale = d.machine != null && d.current == false;
    final away = d.machine == null && !d.present;
    final dim = stale || away;
    final why = verdictRank(d.verdict) > 1 ? d.why : (d.verdict == 'unknown' ? d.health.note : null);
    final trendKey = _mainTrend(d);
    return ListTile(
      leading: Icon(driveIcon(d.health), color: dim ? context.bd.muted : verdictColor(context, d.verdict), size: 28),
      title: Text(d.label, maxLines: 1, overflow: TextOverflow.ellipsis),
      subtitle: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(driveLine(d, skew: skew), maxLines: 1, overflow: TextOverflow.ellipsis),
          if (why != null && why.isNotEmpty)
            Text(
              _cap(why),
              maxLines: 2,
              overflow: TextOverflow.ellipsis,
              style: TextStyle(color: d.verdict == 'unknown' ? null : verdictColor(context, d.verdict)),
            ),
          if (stale)
            Text(
              'Not read there lately: taken out, or its BackDown isn\'t running',
              style: TextStyle(color: context.bd.muted),
            ),
          const SizedBox(height: 4),
          Wrap(
            spacing: 8,
            runSpacing: 4,
            crossAxisAlignment: WrapCrossAlignment.center,
            children: [
              VerdictPill(d.verdict),
              Text(
                d.machine != null
                    ? 'read there ${ago(d.health.readAt, skew: skew)}'
                    : 'read ${ago(d.health.readAt, skew: skew)}',
                style: context.text.bodySmall?.copyWith(color: context.bd.muted),
              ),
            ],
          ),
        ],
      ),
      trailing: trendKey == null
          ? const Icon(Icons.chevron_right)
          : SizedBox(
              width: 56,
              height: 24,
              child: Sparkline(points: d.series, field: trendKey, color: _trendColor(context, d, trendKey)),
            ),
      onTap: () => context.push(healthDrivePath(d.key)),
    );
  }

  /// The series worth a glance in a list: bad sectors when there are any, else temperature.
  static String? _mainTrend(DriveHealth d) {
    bool has(String k) => d.series.where((p) => p.value(k) != null).length >= 2;
    if (has('bad') && d.series.any((p) => (p.bad ?? 0) > 0)) return 'bad';
    if (has('temp')) return 'temp';
    return null;
  }

  static Color _trendColor(BuildContext context, DriveHealth d, String key) {
    final pts = d.series.where((p) => p.value(key) != null).toList();
    final worse = key != 'temp' && pts.length > 1 && pts.last.value(key)! > pts.first.value(key)!;
    return worse ? context.bd.warn : context.bd.info;
  }
}

String _cap(String s) => s.isEmpty ? s : s[0].toUpperCase() + s.substring(1);

/// A small line of one series of readings; gaps (unknown values) are left out (health.js spark).
class Sparkline extends StatelessWidget {
  const Sparkline({super.key, required this.points, required this.field, required this.color});
  final List<HealthPoint> points;
  final String field;
  final Color color;

  @override
  Widget build(BuildContext context) => CustomPaint(
    painter: _SparkPainter([
      for (final p in points)
        if (p.value(field) case final v?) (p.at.toDouble(), v.toDouble()),
    ], color),
  );
}

class _SparkPainter extends CustomPainter {
  _SparkPainter(this.pts, this.color);
  final List<(double, double)> pts;
  final Color color;

  @override
  void paint(Canvas canvas, Size size) {
    if (pts.length < 2) return;
    final xs = pts.map((p) => p.$1), ys = pts.map((p) => p.$2);
    final x0 = xs.reduce(math.min), x1 = xs.reduce(math.max);
    final lo = ys.reduce(math.min), hi = ys.reduce(math.max);
    Offset at((double, double) p) => Offset(
      2 + (x1 > x0 ? (p.$1 - x0) / (x1 - x0) : 0.5) * (size.width - 4),
      size.height - 3 - (hi > lo ? (p.$2 - lo) / (hi - lo) : 0.5) * (size.height - 6),
    );
    final path = Path()..moveTo(at(pts.first).dx, at(pts.first).dy);
    for (final p in pts.skip(1)) {
      path.lineTo(at(p).dx, at(p).dy);
    }
    canvas.drawPath(
      path,
      Paint()
        ..color = color
        ..style = PaintingStyle.stroke
        ..strokeWidth = 1.6
        ..strokeJoin = StrokeJoin.round,
    );
    canvas.drawCircle(at(pts.last), 2.6, Paint()..color = color);
  }

  @override
  bool shouldRepaint(_SparkPainter old) => old.pts != pts || old.color != color;
}

/// A notification from the service: its level as a dot, title and text, when, and what to do.
class NotificationTile extends ConsumerWidget {
  const NotificationTile({super.key, required this.note, this.onRead, this.onDetails});
  final BdNotification note;
  final VoidCallback? onRead;
  final VoidCallback? onDetails;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final skew = ref.watch(skewProvider);
    final n = note;
    final color = n.level == 'info' ? context.bd.info : verdictColor(context, n.level);
    return InkWell(
      onTap: onDetails,
      child: Padding(
        padding: const EdgeInsets.fromLTRB(16, 10, 8, 10),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Padding(
              padding: const EdgeInsets.only(top: 6, right: 14),
              child: n.read ? StatusDot(color.withValues(alpha: 0.45)) : StatusDot(color, size: 11),
            ),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(
                    n.title,
                    style: context.text.titleSmall?.copyWith(fontWeight: n.read ? FontWeight.w500 : FontWeight.w700),
                  ),
                  if (n.body.isNotEmpty) ...[
                    const SizedBox(height: 2),
                    Text(n.body, style: context.text.bodyMedium?.copyWith(color: context.colors.onSurfaceVariant)),
                  ],
                  const SizedBox(height: 4),
                  Text(
                    [ago(n.at, skew: skew), if (!n.shown) 'listed only'].join(' · '),
                    style: context.text.bodySmall?.copyWith(color: context.bd.muted),
                  ),
                ],
              ),
            ),
            // unread: a way to mark it read (a tap still opens what it's about); read: where a tap leads
            if (!n.read && onRead != null)
              IconButton(tooltip: 'Mark as read', icon: const Icon(Icons.done), onPressed: onRead)
            else if (onDetails != null)
              Padding(
                padding: const EdgeInsets.fromLTRB(12, 2, 8, 0),
                child: Icon(Icons.chevron_right, color: context.bd.muted),
              )
            else
              const SizedBox(width: 8),
          ],
        ),
      ),
    );
  }
}

/// A list's content kept to a readable width on a tablet or desktop.
class Readable extends StatelessWidget {
  const Readable({super.key, required this.child, this.maxWidth = 760});
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
