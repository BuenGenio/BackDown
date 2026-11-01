import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../../api/keys.dart';
import '../../core/format.dart';
import '../../core/widgets.dart';
import '../../models/browse.dart';
import '../../theme.dart';
import 'providers.dart';
import 'widgets.dart';

/// A name searched on another machine, by its own index, asked live over ssh (so it's asked when
/// the search is sent, not at every letter). With [dir], under that folder there.
class MachineFindScreen extends ConsumerStatefulWidget {
  const MachineFindScreen({super.key, required this.machineId, this.dir});
  final int machineId;
  final String? dir;

  @override
  ConsumerState<MachineFindScreen> createState() => _MachineFindScreenState();
}

class _MachineFindScreenState extends ConsumerState<MachineFindScreen> {
  final _field = TextEditingController();
  String _q = '';
  late String? _dir = widget.dir;

  @override
  void dispose() {
    _field.dispose();
    super.dispose();
  }

  void _send(String v) => setState(() => _q = v.trim());

  @override
  Widget build(BuildContext context) {
    final name = ref.watch(machinesProvider).value?.byId(widget.machineId)?.name ?? 'that machine';
    return Scaffold(
      appBar: AppBar(
        titleSpacing: 8,
        title: TextField(
          controller: _field,
          autofocus: true,
          textInputAction: TextInputAction.search,
          autocorrect: false,
          decoration: InputDecoration(
            hintText: _dir == null ? 'Search on $name' : 'Search in ${baseName(pathOfKey(_dir!))} on $name',
            prefixIcon: const Icon(Icons.search),
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
      ),
      body: Column(
        children: [
          if (_dir != null)
            Align(
              alignment: Alignment.centerLeft,
              child: Padding(
                padding: const EdgeInsets.fromLTRB(12, 6, 12, 6),
                child: InputChip(
                  avatar: const Icon(Icons.folder_outlined, size: 18),
                  label: Text('In ${baseName(pathOfKey(_dir!))}'),
                  onDeleted: () => setState(() => _dir = null),
                ),
              ),
            ),
          if (_dir != null) const Divider(height: 1),
          Expanded(
            child: _q.isEmpty
                ? EmptyState(
                    icon: Icons.manage_search,
                    title: 'Find it on $name',
                    message:
                        'By name, in $name\'s own index, asked over ssh when you search. Every machine at once is '
                        'Where is it?, in the swarm.',
                    action: OutlinedButton.icon(
                      onPressed: () =>
                          context.push(MachinePaths.where(_field.text.trim().isEmpty ? null : _field.text)),
                      icon: const Icon(Icons.travel_explore),
                      label: const Text('Where is it?'),
                    ),
                  )
                : _Results(query: (m: widget.machineId, q: _q, dir: _dir), name: name),
          ),
        ],
      ),
    );
  }
}

class _Results extends ConsumerWidget {
  const _Results({required this.query, required this.name});
  final MachineFindQuery query;
  final String name;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final r = ref.watch(machineFindProvider(query));
    return AsyncBody(
      value: r,
      onRetry: () => ref.invalidate(machineFindProvider(query)),
      loading: Center(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            const CircularProgressIndicator(),
            const SizedBox(height: 16),
            Text('Asking $name…', style: context.text.bodyMedium?.copyWith(color: context.bd.muted)),
          ],
        ),
      ),
      data: (res) {
        if (res.hits.isEmpty) {
          return EmptyState(icon: Icons.search_off, title: 'Nothing called "${query.q}" on $name');
        }
        return ListView.builder(
          padding: wideInsets(context),
          itemCount: res.hits.length + 1,
          itemBuilder: (c, i) {
            if (i == res.hits.length) {
              return Padding(
                padding: const EdgeInsets.all(16),
                child: Text(
                  res.total > res.hits.length
                      ? 'The best ${count(res.hits.length)} of ${count(res.total)} matches on $name'
                      : '${plural(res.total, 'match', 'matches')} on $name',
                  textAlign: TextAlign.center,
                  style: context.text.bodySmall?.copyWith(color: context.bd.muted),
                ),
              );
            }
            return _HitTile(hit: res.hits[i], machineId: query.m);
          },
        );
      },
    );
  }
}

class _HitTile extends StatelessWidget {
  const _HitTile({required this.hit, required this.machineId});
  final FindHit hit;
  final int machineId;

  @override
  Widget build(BuildContext context) {
    final h = hit;
    final where = h.folder.isEmpty ? h.drive : '${baseName(h.drive)} › ${h.folder}';
    return Opacity(
      opacity: h.gone ? 0.6 : 1,
      child: ListTile(
        leading: FileGlyph(name: h.name, isDir: h.isDir),
        title: Text(h.name, maxLines: 1, overflow: TextOverflow.ellipsis),
        subtitle: Text(
          [
            where,
            if (h.gone) 'deleted there' else if (!h.isDir) '${bytes(h.size)} · ${day(h.mtime)}' else 'folder',
          ].join('\n'),
          maxLines: 2,
          overflow: TextOverflow.ellipsis,
        ),
        isThreeLine: true,
        onTap: () {
          if (h.isDir) {
            context.push(MachinePaths.ls(machineId, dir: h.key));
          } else if (h.id != null) {
            context.push(MachinePaths.file(machineId, h.id!));
          }
        },
      ),
    );
  }
}
