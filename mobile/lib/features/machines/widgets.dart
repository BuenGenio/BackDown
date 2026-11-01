import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../../core/format.dart';
import '../../core/widgets.dart';
import '../../theme.dart';
import 'models.dart';

/// Content this wide at most; on a tablet or desktop the rest is margin.
const readableWidth = 820.0;

/// A list's padding that keeps its content [readableWidth] wide at most, centred.
EdgeInsets wideInsets(BuildContext context, {double top = 0, double bottom = 32, double side = 0}) {
  final w = MediaQuery.sizeOf(context).width;
  final s = math.max(side, (w - readableWidth) / 2);
  return EdgeInsets.fromLTRB(s, top, s, bottom);
}

/// "Can't" for a sentence the service started in lower case.
String capitalised(String s) => s.isEmpty ? s : s[0].toUpperCase() + s.substring(1);

/// The first line of an error, shortened for a list.
String firstLine(String s, [int max = 160]) {
  final l = s.split('\n').first.trim();
  return l.length > max ? '${l.substring(0, max)}…' : l;
}

/// How a machine is, in a word and a colour (machines.js's status()).
(String, Color, IconData) machineStatus(BuildContext context, Machine m, {int skew = 0}) {
  if (m.busy) {
    return (m.job!.state == 'running' ? 'syncing…' : 'sync waiting', context.bd.info, Icons.sync);
  }
  if (m.unreachable) return ('can\'t be reached', context.bd.crit, Icons.cloud_off_outlined);
  if (m.lastSync == null) return ('not synced yet', context.bd.muted, Icons.schedule);
  return ('synced ${rel(m.lastSync, skew: skew)}', context.bd.good, Icons.check_circle_outline);
}

/// A bar of a drive's used space (amber past 85%, red past 95%), with the part kept free
/// ([reserve]) marked at its end.
class SpaceBar extends StatelessWidget {
  const SpaceBar({super.key, required this.total, required this.free, this.reserve = 0, this.height = 8});

  final int total, free, reserve;
  final double height;

  @override
  Widget build(BuildContext context) {
    final used = total <= 0 ? 0.0 : ((total - free) / total).clamp(0.0, 1.0);
    final kept = total <= 0 ? 0.0 : (reserve / total).clamp(0.0, 1.0 - used);
    final color = used > 0.95
        ? context.bd.crit
        : used > 0.85
        ? context.bd.warn
        : context.bd.folder;
    return Semantics(
      label: '${percent(used)} used',
      child: ClipRRect(
        borderRadius: BorderRadius.circular(height),
        child: SizedBox(
          height: height,
          child: LayoutBuilder(
            builder: (c, box) => Stack(
              children: [
                Positioned.fill(child: ColoredBox(color: context.colors.surfaceContainerHighest)),
                Positioned(
                  left: 0,
                  top: 0,
                  bottom: 0,
                  width: box.maxWidth * used,
                  child: ColoredBox(color: color),
                ),
                if (kept > 0)
                  Positioned(
                    right: 0,
                    top: 0,
                    bottom: 0,
                    width: box.maxWidth * kept,
                    child: ColoredBox(color: context.bd.muted.withValues(alpha: 0.35)),
                  ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// A tinted note across a screen: why something can't be done, or how what's shown was got.
class NoteCard extends StatelessWidget {
  const NoteCard({
    super.key,
    required this.icon,
    required this.text,
    this.title,
    this.color,
    this.trailing,
    this.margin,
  });

  final IconData icon;
  final String? title;
  final String text;
  final Color? color;
  final Widget? trailing;
  final EdgeInsetsGeometry? margin;

  @override
  Widget build(BuildContext context) {
    final c = color ?? context.bd.info;
    return Container(
      margin: margin ?? const EdgeInsets.fromLTRB(16, 8, 16, 4),
      padding: const EdgeInsets.fromLTRB(12, 10, 12, 10),
      decoration: BoxDecoration(color: c.withValues(alpha: 0.11), borderRadius: BorderRadius.circular(12)),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Padding(
            padding: const EdgeInsets.only(top: 1),
            child: Icon(icon, size: 20, color: c),
          ),
          const SizedBox(width: 10),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                if (title != null) ...[Text(title!, style: context.text.titleSmall), const SizedBox(height: 2)],
                Text(text, style: context.text.bodySmall?.copyWith(color: context.colors.onSurfaceVariant)),
              ],
            ),
          ),
          ?trailing,
        ],
      ),
    );
  }
}

/// A command to type at the computer, in monospace; tap to copy it.
class CommandLine extends StatelessWidget {
  const CommandLine(this.command, {super.key});
  final String command;

  @override
  Widget build(BuildContext context) {
    return Material(
      color: context.colors.surfaceContainerHigh,
      borderRadius: BorderRadius.circular(10),
      child: InkWell(
        borderRadius: BorderRadius.circular(10),
        onTap: () async {
          await Clipboard.setData(ClipboardData(text: command));
          if (context.mounted) showSnack(context, 'Copied');
        },
        child: Padding(
          padding: const EdgeInsets.fromLTRB(12, 10, 8, 10),
          child: Row(
            children: [
              Expanded(
                child: Text(command, style: context.text.bodySmall?.copyWith(fontFamily: 'monospace')),
              ),
              Icon(Icons.copy, size: 16, color: context.bd.muted),
            ],
          ),
        ),
      ),
    );
  }
}

/// A card with a title row, for a machine, a disk or the pool.
class SectionCard extends StatelessWidget {
  const SectionCard({super.key, required this.children, this.margin, this.padding});

  final List<Widget> children;
  final EdgeInsetsGeometry? margin, padding;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: margin ?? const EdgeInsets.fromLTRB(16, 6, 16, 6),
      child: Card(
        clipBehavior: Clip.antiAlias,
        child: Padding(
          padding: padding ?? const EdgeInsets.fromLTRB(14, 12, 14, 12),
          child: Column(crossAxisAlignment: CrossAxisAlignment.stretch, children: children),
        ),
      ),
    );
  }
}

/// Names compared as people read them: "IMG_9" before "IMG_10", case aside.
int naturalCompare(String a, String b) {
  final re = RegExp(r'(\d+)|(\D+)');
  final x = re.allMatches(a.toLowerCase()).toList(), y = re.allMatches(b.toLowerCase()).toList();
  for (var i = 0; i < math.min(x.length, y.length); i++) {
    final p = x[i].group(0)!, q = y[i].group(0)!;
    final c = x[i].group(1) != null && y[i].group(1) != null
        ? (BigInt.parse(p).compareTo(BigInt.parse(q)))
        : p.compareTo(q);
    if (c != 0) return c;
  }
  return x.length.compareTo(y.length);
}

/// "Add a machine": done at the computer, in its BackDown or a terminal there.
Future<void> showAddMachineHelp(BuildContext context) => showModalBottomSheet<void>(
  context: context,
  showDragHandle: true,
  isScrollControlled: true,
  builder: (c) => SafeArea(
    child: ConstrainedBox(
      constraints: BoxConstraints(maxHeight: MediaQuery.sizeOf(c).height * 0.85, maxWidth: 640),
      child: ListView(
        shrinkWrap: true,
        padding: const EdgeInsets.fromLTRB(20, 0, 20, 24),
        children: [
          Text('Add a machine', style: c.text.titleLarge),
          const SizedBox(height: 8),
          Text(
            'A NAS or another computer with BackDown on it. Its index is read over ssh with the keys of the '
            'person BackDown runs as, so a machine is added at the computer: in its BackDown (Machines, then '
            '"Add a machine"), or in a terminal there:',
            style: c.text.bodyMedium,
          ),
          const SizedBox(height: 12),
          const CommandLine('backdown remote add nas ssh://me@nas.example.org'),
          const SizedBox(height: 16),
          Text('Before it works', style: c.text.titleSmall),
          const SizedBox(height: 6),
          for (final (i, step) in const [
            'Install BackDown on that machine and index its drives there (backdown index /volume1, or '
                'backdown service install).',
            'Make sure ssh from the computer logs in there without a password (ssh-copy-id, or your ssh agent), '
                'and that its host key has been accepted once in a terminal.',
            'Add it. BackDown then syncs it every few hours: what\'s indexed there, and the hashes of files there '
                'that could be copies of files here.',
          ].indexed)
            Padding(
              padding: const EdgeInsets.only(bottom: 8),
              child: Row(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  SizedBox(width: 22, child: Text('${i + 1}.', style: c.text.bodyMedium)),
                  Expanded(child: Text(step, style: c.text.bodyMedium)),
                ],
              ),
            ),
          const SizedBox(height: 4),
          Text(
            'BackDown never asks for a password, never listens on the network for this, and never changes anything '
            'on the other machine.',
            style: c.text.bodySmall?.copyWith(color: c.bd.muted),
          ),
        ],
      ),
    ),
  ),
);

/// "Forget a machine": done at the computer too.
Future<void> showRemoveMachineHelp(BuildContext context, Machine m) => showDialog<void>(
  context: context,
  builder: (c) => AlertDialog(
    icon: const Icon(Icons.computer_outlined),
    title: Text('Forget ${m.name}'),
    content: Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(
          'Machines are forgotten at the computer: in its BackDown (${m.name}, then "Remove…"), or in a terminal '
          'there:',
        ),
        const SizedBox(height: 12),
        CommandLine('backdown remote remove ${m.name}'),
        const SizedBox(height: 12),
        Text(
          'Nothing on ${m.name} is touched; its copies stop counting here, and what was cached about it is dropped.',
          style: c.text.bodySmall?.copyWith(color: c.bd.muted),
        ),
      ],
    ),
    actions: [TextButton(onPressed: () => Navigator.pop(c), child: const Text('OK'))],
  ),
);
