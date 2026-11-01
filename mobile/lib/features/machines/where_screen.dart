import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../../core/format.dart';
import '../../core/widgets.dart';
import '../../providers.dart';
import '../../router.dart';
import '../../theme.dart';
import 'models.dart';
import 'providers.dart';
import 'widgets.dart';

/// Where is it: a name looked for on every machine at once (GET /api/swarm-where): this one's
/// index, and each other machine's, asked live over ssh, or its files as last synced when it can't
/// be asked. Each hit says which machine, which drive and where. It's asked when the search is
/// sent, since that asks every machine.
class WhereScreen extends ConsumerStatefulWidget {
  const WhereScreen({super.key, this.q});
  final String? q;

  @override
  ConsumerState<WhereScreen> createState() => _WhereScreenState();
}

class _WhereScreenState extends ConsumerState<WhereScreen> {
  late final _field = TextEditingController(text: widget.q ?? '');
  late String _q = widget.q?.trim() ?? '';

  @override
  void dispose() {
    _field.dispose();
    super.dispose();
  }

  void _send(String v) => setState(() => _q = v.trim());

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        titleSpacing: 8,
        title: TextField(
          controller: _field,
          autofocus: _q.isEmpty,
          textInputAction: TextInputAction.search,
          autocorrect: false,
          decoration: InputDecoration(
            hintText: 'Where is it? A name',
            prefixIcon: const Icon(Icons.travel_explore),
            isDense: true,
            contentPadding: const EdgeInsets.symmetric(vertical: 10),
            suffixIcon: _field.text.isEmpty
                ? null
                : IconButton(
                    tooltip: 'Clear',
                    icon: const Icon(Icons.close),
                    onPressed: () {
                      _field.clear();
                      setState(() => _q = '');
                    },
                  ),
          ),
          onChanged: (_) => setState(() {}),
          onSubmitted: _send,
        ),
        actions: [
          IconButton(tooltip: 'Find', icon: const Icon(Icons.arrow_forward), onPressed: () => _send(_field.text)),
        ],
      ),
      body: _q.isEmpty
          ? const EmptyState(
              icon: Icons.travel_explore,
              title: 'Where is it?',
              message:
                  'A name, looked for on every machine at once: this computer\'s index, and each other machine\'s, '
                  'asked over ssh (or as last synced when it can\'t be reached). Each hit says which machine, which '
                  'drive and where.',
            )
          : _Found(q: _q),
    );
  }
}

class _Found extends ConsumerWidget {
  const _Found({required this.q});
  final String q;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final r = ref.watch(whereProvider(q));
    final skew = ref.watch(skewProvider);
    return AsyncBody(
      value: r,
      onRetry: () => ref.invalidate(whereProvider(q)),
      loading: Center(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            const CircularProgressIndicator(),
            const SizedBox(height: 16),
            Text(
              'Looking on every machine for "$q"…',
              style: context.text.bodyMedium?.copyWith(color: context.bd.muted),
            ),
          ],
        ),
      ),
      data: (f) {
        final notes = [
          for (final m in f.machines)
            if (m.how == 'cached')
              '${m.name} couldn\'t be asked, so its files as synced ${m.syncedAt == null ? 'here' : rel(m.syncedAt, skew: skew)} '
                  'were searched${m.complete ? '' : ' (only some of them, in the time there was)'}.'
            else if (m.how == 'none')
              '${m.name} wasn\'t searched: ${firstLine(m.error ?? '')}',
        ];
        final hits = f.hits;
        final title = hits.isEmpty
            ? 'Nothing named "${f.q}" on any machine'
            : '${hits.length >= 200 ? 'The first 200' : plural(hits.length, 'match', 'matches')} for "${f.q}"';
        final searched = f.machines.map((m) => m.name).join(', ');
        return ListView.builder(
          padding: wideInsets(context),
          itemCount: hits.length + 2,
          itemBuilder: (c, i) {
            if (i == 0) {
              return Padding(
                padding: const EdgeInsets.fromLTRB(16, 14, 16, 6),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(title, style: context.text.titleSmall),
                    if (searched.isNotEmpty)
                      Text('On $searched', style: context.text.bodySmall?.copyWith(color: context.bd.muted)),
                  ],
                ),
              );
            }
            if (i == hits.length + 1) {
              return Column(
                children: [
                  for (final n in notes) NoteCard(icon: Icons.cloud_off_outlined, color: context.bd.warn, text: n),
                  const SizedBox(height: 8),
                ],
              );
            }
            return _HitTile(hit: hits[i - 1]);
          },
        );
      },
    );
  }
}

class _HitTile extends StatelessWidget {
  const _HitTile({required this.hit});
  final WhereHit hit;

  void _open(BuildContext context) {
    final h = hit.hit;
    if (hit.here || hit.machineId == null) {
      if (h.isDir) {
        context.push(Routes.folder(h.key));
      } else if (h.id != null) {
        context.push(Routes.file(h.id!));
      }
      return;
    }
    if (h.isDir) {
      context.push(MachinePaths.ls(hit.machineId!, dir: h.key));
    } else if (h.id != null) {
      context.push(MachinePaths.file(hit.machineId!, h.id!));
    }
  }

  @override
  Widget build(BuildContext context) {
    final h = hit.hit;
    final drive = hit.drive;
    final muted = context.text.bodySmall?.copyWith(color: context.colors.onSurfaceVariant);
    return ListTile(
      leading: FileGlyph(name: h.name, isDir: h.isDir),
      title: Text(h.name, maxLines: 1, overflow: TextOverflow.ellipsis),
      subtitle: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text.rich(
            TextSpan(
              children: [
                TextSpan(
                  text: hit.machine,
                  style: TextStyle(
                    color: hit.here ? context.colors.primary : context.bd.info,
                    fontWeight: FontWeight.w700,
                  ),
                ),
                if (drive.isNotEmpty) TextSpan(text: ' · $drive'),
              ],
            ),
            style: muted,
            maxLines: 1,
            overflow: TextOverflow.ellipsis,
          ),
          Text(hit.folder, style: muted, maxLines: 2, overflow: TextOverflow.ellipsis),
          Text(
            [h.isDir ? 'folder' : bytes(h.size), day(h.mtime), if (hit.cached) 'as synced'].join(' · '),
            style: context.text.bodySmall?.copyWith(color: context.bd.muted),
          ),
        ],
      ),
      isThreeLine: true,
      trailing: const Icon(Icons.chevron_right),
      onTap: () => _open(context),
    );
  }
}
