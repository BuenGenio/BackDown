import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../api/json.dart';
import '../../core/actions.dart';
import '../../core/format.dart';
import '../../core/widgets.dart';
import '../../models/daemon.dart';
import '../../providers.dart';
import '../../theme.dart';
import 'common.dart';
import 'data.dart';
import 'models.dart';

/// Rescues: copies of failing drives, files found on formatted cards, lost partitions, files
/// copied out of a damaged file system, and what each found.
class RescuesScreen extends ConsumerWidget {
  const RescuesScreen({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final rescues = ref.watch(rescuesProvider);
    final daemon = ref.watch(daemonProvider).value;
    final live = <String, Job>{
      for (final j in [?daemon?.side, ?daemon?.current, ...?daemon?.queued])
        if (j.kind == 'rescue' && j.path != null) j.path!: j,
    };
    return Scaffold(
      appBar: AppBar(title: const Text('Rescues')),
      body: RefreshIndicator(
        onRefresh: () => ref.refresh(rescuesProvider.future).catchError((_) => <Rescue>[]),
        child: LibraryBody(
          value: rescues,
          onRetry: () => ref.invalidate(rescuesProvider),
          data: (list) {
            final rows = <Widget Function()>[
              () => const LookOnlyNote(what: 'Rescues'),
              () => const Intro(
                'Get files back from a drive or memory card that\'s failing, was formatted, or lost its files. '
                'BackDown only reads it: whatever it rescues goes to another drive.',
              ),
              if (list.isEmpty)
                () => const Padding(
                  padding: EdgeInsets.all(24),
                  child: EmptyState(
                    icon: Icons.healing_outlined,
                    title: 'No rescues yet',
                    message: 'Start one on the computer, in Rescue: pick the drive or card, and what happened to it.',
                  ),
                ),
              if (list.isNotEmpty) () => SectionHeader('Rescues', trailing: Text('${list.length}')),
              for (final r in list)
                () {
                  final j = live['rescue ${r.id}'];
                  return RescueCard(rescue: r, job: j, running: j != null && identical(j, daemon?.side));
                },
            ];
            return ReadableList(itemCount: rows.length, itemBuilder: (c, i) => rows[i]());
          },
        ),
      ),
    );
  }
}

const _titles = {'image': 'Copy', 'carve': 'Find files', 'parts': 'Look for partitions', 'files': 'Copy files out'};

const _groups = [
  ('photos', 'Photos'),
  ('videos', 'Videos'),
  ('documents', 'Documents'),
  ('audio', 'Music and audio'),
  ('archives', 'Archives'),
  ('other', 'Other'),
];

class RescueCard extends ConsumerWidget {
  const RescueCard({super.key, required this.rescue, this.job, this.running = false});

  final Rescue rescue;
  final Job? job;
  final bool running;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final r = rescue;
    final res = r.result;
    final skew = ref.watch(skewProvider);
    final j = job;
    final state = j != null
        ? (running ? 'running' : 'waiting')
        : r.state == 'running' || r.state == 'new'
        ? 'interrupted'
        : r.state;
    final body = <Widget>[];
    void gap([double h = 8]) => body.add(SizedBox(height: h));

    if (j != null && running) {
      final c = j.counts;
      gap();
      body.add(Meter(fraction: c == null || c.totalBytes == 0 ? null : c.bytes / c.totalBytes, color: context.bd.info));
      gap(6);
      body.add(Muted(j.progress.isEmpty ? 'starting…' : j.progress));
    }
    switch (r.mode) {
      case 'image' when res.integer('size') > 0:
        gap();
        body.add(_MapBar(bins: res['bins']));
        gap(6);
        body.add(
          Text(
            [
              '${bytes(res.integer('rescued'))} of ${bytes(res.integer('size'))} copied',
              if (res.integer('bad') > 0)
                '${bytes(res.integer('bad'))} unreadable in ${plural(res.integer('bad_areas'), 'place')}',
              if (res.integer('todo') > 0) '${bytes(res.integer('todo'))} still to try',
            ].join(' · '),
            style: context.text.bodySmall,
          ),
        );
        if (j == null && r.dest != null) {
          gap();
          body.add(_Open(label: 'Show the copy', onTap: () => openPath(context, r.dest!)));
        }
      case 'carve' when res['by_group'] is Map:
        final g = res.obj('by_group');
        final found = [
          for (final (k, l) in _groups)
            if (g.integer(k) > 0) '${count(g.integer(k))} ${l.toLowerCase()}',
        ];
        gap();
        body.add(
          Text(
            found.isEmpty
                ? 'Nothing found yet.'
                : '${found.join(' · ')} · ${bytes(res.integer('found_bytes'))}'
                      '${res.integer('incomplete') > 0 ? ' (${count(res.integer('incomplete'))} cut off or damaged: those come back partly)' : ''}',
            style: context.text.bodyMedium,
          ),
        );
        if (res.integer('unreadable') > 0) {
          gap(4);
          body.add(
            Muted(
              '${bytes(res.integer('unreadable'))} of the drive couldn\'t be read: copying it first ("It\'s failing") '
              'gives those parts more tries.',
              color: context.bd.warn,
            ),
          );
        }
        if (j == null && r.dest != null && res.integer('found') > 0) {
          gap();
          body.add(_Open(label: 'Open the recovered files', onTap: () => openPath(context, r.dest!)));
        }
      case 'parts' when res['found'] is List:
        body.addAll(_parts(context, res));
      case 'files':
        body.addAll(_files(context, ref, r, j));
      default:
        break;
    }
    if (r.state == 'failed' && j == null) {
      gap();
      body.add(Text(r.summary ?? 'It stopped with an error.', style: TextStyle(color: context.bd.crit)));
    } else if (r.summary != null && j == null && r.mode != 'parts' && r.mode != 'files') {
      gap(6);
      body.add(Muted(r.summary!));
    }

    return ListCard(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Icon(Icons.healing_outlined, size: 20, color: context.bd.muted),
              const SizedBox(width: 8),
              Expanded(child: Text(_titles[r.mode] ?? r.mode, style: context.text.titleSmall)),
              StatePill(state),
            ],
          ),
          const SizedBox(height: 6),
          PathText(r.source, color: context.colors.onSurface),
          if (r.mode != 'parts' && r.dest != null) ...[
            const SizedBox(height: 2),
            Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Padding(
                  padding: const EdgeInsets.only(top: 1, right: 4),
                  child: Icon(Icons.subdirectory_arrow_right, size: 14, color: context.bd.muted),
                ),
                Expanded(child: PathText(r.dest!)),
              ],
            ),
          ],
          const SizedBox(height: 4),
          Muted('${when(r.createdAt)} · ${rel(r.createdAt, skew: skew)}'),
          ...body,
        ],
      ),
    );
  }

  List<Widget> _parts(BuildContext context, Json res) {
    final table = res.obj('table');
    final found = res.list('found');
    final lost = found.where((f) => !f.flag('in_table')).length;
    final scheme = table.str('scheme', '?');
    final entries = table.list('entries').length;
    final sector = res.integer('sector', 512);
    return [
      const SizedBox(height: 8),
      Text(
        'Partition table: ${scheme == 'none' ? 'none' : scheme.toUpperCase()}${entries > 0 ? ', listing $entries' : ''}. '
        '${found.isEmpty ? 'No file systems found.' : 'Found ${plural(found.length, 'file system')}${lost > 0 ? ', $lost of them not in the table' : ''}.'}',
        style: context.text.bodyMedium,
      ),
      for (final p in table.strs('problems'))
        Padding(
          padding: const EdgeInsets.only(top: 4),
          child: Muted(p, color: context.bd.warn),
        ),
      if (res.integer('unreadable') > 0)
        Padding(
          padding: const EdgeInsets.only(top: 4),
          child: Muted('${bytes(res.integer('unreadable'))} couldn\'t be read, and was searched as if it were zeros.'),
        ),
      const SizedBox(height: 6),
      for (final f in found)
        Container(
          margin: const EdgeInsets.only(top: 6),
          padding: const EdgeInsets.all(10),
          decoration: BoxDecoration(color: context.colors.surfaceContainer, borderRadius: BorderRadius.circular(10)),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  Expanded(
                    child: Text(
                      [f.str('fs'), if (f.str('label').isNotEmpty) '"${f.str('label')}"'].join(' '),
                      style: context.text.titleSmall,
                    ),
                  ),
                  Text(f.intOrNull('size') == null ? '?' : bytes(f.integer('size')), style: context.text.labelLarge),
                ],
              ),
              const SizedBox(height: 2),
              Muted(
                [
                  'starts at ${bytes(f.integer('start'))} (sector ${count(f.integer('start') ~/ sector)})',
                  if (f.str('via') != 'boot sector' && f.str('via') != 'superblock') 'found by its ${f.str('via')}',
                  if (f.flag('nested')) 'inside another file system',
                ].join(' · '),
              ),
              const SizedBox(height: 6),
              Wrap(
                spacing: 6,
                runSpacing: 4,
                children: [
                  f.flag('in_table')
                      ? Tag('in the table', color: context.bd.good)
                      : Tag('not in the table: lost', color: context.bd.warn),
                  if (f.flag('truncated')) Tag('runs past the end', color: context.bd.warn),
                ],
              ),
            ],
          ),
        ),
    ];
  }

  List<Widget> _files(BuildContext context, WidgetRef ref, Rescue r, Job? j) {
    final res = r.result, o = r.options;
    final items = o['items'] is List ? o['items']! as List : const <Object?>[];
    final everything = items.length == 1 && (items.first?.toString() ?? '').isEmpty;
    final out = <Widget>[
      const SizedBox(height: 8),
      Muted(
        'From the ${o.str('what', 'file system')} at ${bytes(o.integer('start'))}: '
        '${everything ? 'everything' : '${plural(items.length, 'item')} chosen'}${o.flag('with_deleted') ? ', deleted files included' : ''}.',
      ),
    ];
    if (res.intOrNull('files') != null) {
      final n = res.integer('files');
      out.add(const SizedBox(height: 6));
      out.add(
        Text(
          [
            '${plural(n, 'file')} copied and checked · ${bytes(res.integer('bytes'))}',
            if (res.integer('deleted') > 0) '${count(res.integer('deleted'))} of them deleted ones',
            if (res.integer('partly') > 0)
              '${count(res.integer('partly'))} partly overwritten (those parts come back empty)',
            if (res.integer('renamed') > 0)
              '${count(res.integer('renamed'))} saved under a new name, not over what was there',
            if (res.integer('failed') > 0) '${count(res.integer('failed'))} couldn\'t be copied',
          ].join(' · '),
          style: context.text.bodyMedium,
        ),
      );
      for (final p in res.strs('problems').take(20)) {
        out.add(Muted(p, color: context.bd.warn));
      }
      if (j == null && r.dest != null && n > 0) {
        out.add(const SizedBox(height: 8));
        out.add(_Open(label: 'Open the copies', onTap: () => openPath(context, r.dest!)));
      }
    }
    return out;
  }
}

class _Open extends StatelessWidget {
  const _Open({required this.label, required this.onTap});
  final String label;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) => Align(
    alignment: Alignment.centerLeft,
    child: OutlinedButton.icon(
      onPressed: onTap,
      icon: const Icon(Icons.folder_open_outlined, size: 18),
      label: Text(label),
    ),
  );
}

/// The copy as its map sees it: a strip of stretches of the drive, green copied, red unreadable,
/// grey still to try.
class _MapBar extends StatelessWidget {
  const _MapBar({required this.bins});
  final Object? bins;

  @override
  Widget build(BuildContext context) {
    final list = [
      if (bins is List)
        for (final b in bins as List)
          if (b is List && b.length >= 3) [for (final x in b) x is num ? x.toDouble() : 0.0],
    ];
    if (list.isEmpty) return const SizedBox.shrink();
    return ClipRRect(
      borderRadius: BorderRadius.circular(4),
      child: SizedBox(
        height: 14,
        width: double.infinity,
        child: CustomPaint(
          painter: _MapPainter(
            list,
            good: context.bd.good,
            bad: context.bd.crit,
            todo: context.colors.surfaceContainerHighest,
          ),
        ),
      ),
    );
  }
}

class _MapPainter extends CustomPainter {
  _MapPainter(this.bins, {required this.good, required this.bad, required this.todo});
  final List<List<double>> bins;
  final Color good, bad, todo;

  @override
  void paint(Canvas canvas, Size size) {
    final w = size.width / bins.length;
    for (final (i, b) in bins.indexed) {
      final n = b[0] + b[1] + b[2];
      final x = i * w;
      final g = n == 0 ? 0.0 : b[0] / n, r = n == 0 ? 0.0 : b[1] / n;
      canvas.drawRect(Rect.fromLTWH(x, 0, w + 0.5, size.height), Paint()..color = todo);
      canvas.drawRect(Rect.fromLTWH(x, size.height * (1 - g), w + 0.5, size.height * g), Paint()..color = good);
      if (r > 0) {
        canvas.drawRect(
          Rect.fromLTWH(x, size.height * (1 - g - r), w + 0.5, size.height * r.clamp(0.25, 1)),
          Paint()..color = bad,
        );
      }
    }
  }

  @override
  bool shouldRepaint(_MapPainter old) => old.bins != bins || old.good != good;
}
