import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../../core/actions.dart';
import '../../core/file_io.dart';
import '../../core/format.dart';
import '../../core/widgets.dart';
import '../../models/file_ref.dart';
import '../../providers.dart';
import '../../router.dart';
import '../../theme.dart';
import '../viewers/open.dart';
import 'common.dart';
import 'data.dart';
import 'models.dart';

/// Duplicates: what's stored more than once, the groups that free the most first, each opening
/// to where its copies are, and what's here once and again on another machine.
class DupesScreen extends ConsumerStatefulWidget {
  const DupesScreen({super.key});

  @override
  ConsumerState<DupesScreen> createState() => _DupesScreenState();
}

class _DupesScreenState extends ConsumerState<DupesScreen> {
  late DupesQuery _q = DupesQuery(now: nowSeconds());
  final _open = <String>{};
  DupesData? _last;
  DupesQuery? _opened;
  String _find = '';

  void _set(DupesQuery q) => setState(() => _q = q);

  /// How many groups the next "Show more" asks for: about as many again, at most 200 more.
  int get _nextLimit => math.min(5000, _q.limit + math.max(20, math.min(_q.limit, 200)));

  Future<void> _filters() async {
    final q = await showModalBottomSheet<DupesQuery>(
      context: context,
      useRootNavigator: true,
      showDragHandle: true,
      isScrollControlled: true,
      builder: (c) => _FilterSheet(query: _q),
    );
    if (q != null && q != _q) _set(q.copyWith(limit: 20, now: nowSeconds()));
  }

  @override
  Widget build(BuildContext context) {
    final value = ref.watch(dupesProvider(_q));
    // a "Show more" or a new filter keeps what's there on screen while it loads
    final shown = value.value ?? _last;
    if (value.hasValue) _last = value.value;
    if (shown != null && _opened != _q && value.hasValue) {
      // few enough to read at once: open them
      _opened = _q;
      final listed = shown.listed;
      if (listed.length <= 3) _open.addAll(listed.map((g) => g.sha256));
    }
    return Scaffold(
      appBar: AppBar(
        title: const Text('Duplicates'),
        actions: [
          IconButton(
            tooltip: 'Filter',
            onPressed: _filters,
            icon: Badge(
              isLabelVisible: _q.filtering || _q.min != '1M' || _q.hidden,
              smallSize: 8,
              child: const Icon(Icons.tune),
            ),
          ),
        ],
      ),
      body: RefreshIndicator(
        onRefresh: () => ref.refresh(dupesProvider(_q).future).catchError((_) => _last ?? DupesData.fromJson(const {})),
        child: shown == null
            ? LibraryBody(value: value, onRetry: () => ref.invalidate(dupesProvider(_q)), data: (d) => _body(d))
            : Stack(
                children: [
                  _body(shown),
                  if (value.isLoading)
                    const Positioned(top: 0, left: 0, right: 0, child: LinearProgressIndicator(minHeight: 2)),
                ],
              ),
      ),
    );
  }

  Widget _body(DupesData d) {
    final listed = d.listed;
    final needle = _find.trim().toLowerCase();
    final found = needle.isEmpty
        ? listed
        : [
            for (final g in listed)
              if (g.shown.any((f) => f.path.toLowerCase().contains(needle))) g,
          ];
    final across = d.across;
    final hasAcross = across != null && across.files > 0;
    final rows = <Widget Function()>[
      () => _Header(
        data: d,
        query: _q,
        onClear: () => _set(DupesQuery(min: _q.min, now: nowSeconds())),
      ),
      () => _FilterChips(
        query: _q,
        onTap: _filters,
        onMin: (m) => _set(_q.copyWith(min: m, limit: 20)),
      ),
      if (listed.isEmpty) () => _Empty(data: d, query: _q, onHidden: () => _set(_q.copyWith(hidden: true, limit: 20))),
      if (listed.length > 3)
        () => Padding(
          padding: const EdgeInsets.fromLTRB(16, 4, 16, 6),
          child: TextField(
            onChanged: (v) => setState(() => _find = v),
            decoration: const InputDecoration(
              isDense: true,
              hintText: 'Find by name or folder…',
              prefixIcon: Icon(Icons.search),
            ),
          ),
        ),
      if (needle.isNotEmpty)
        () => Padding(
          padding: const EdgeInsets.fromLTRB(16, 0, 16, 4),
          child: Muted('${plural(found.length, 'group')} of ${count(listed.length)} match "${_find.trim()}".'),
        ),
      for (final g in found)
        () => DupeGroupCard(
          group: g,
          open: _open.contains(g.sha256),
          onToggle: () => setState(() => _open.contains(g.sha256) ? _open.remove(g.sha256) : _open.add(g.sha256)),
        ),
      if (d.largest.length < d.groups && listed.isNotEmpty)
        () => Padding(
          padding: const EdgeInsets.fromLTRB(16, 12, 16, 4),
          child: Column(
            children: [
              Muted('The ${count(d.largest.length)} groups that free the most are here.'),
              if (_q.limit < 5000) ...[
                const SizedBox(height: 8),
                OutlinedButton.icon(
                  onPressed: () => _set(_q.copyWith(limit: _nextLimit)),
                  icon: const Icon(Icons.expand_more),
                  label: Text('Show ${count(math.min(_nextLimit, d.groups) - d.largest.length)} more'),
                ),
              ],
            ],
          ),
        ),
      if (hasAcross) ...[
        () => const SectionHeader('Here once, and again on another machine'),
        () => Padding(
          padding: const EdgeInsets.fromLTRB(16, 0, 16, 8),
          child: Muted(
            '${bytes(across.bytes)} in ${plural(across.files, 'file')} here ${across.files == 1 ? 'has' : 'have'} an '
            'identical copy on another machine; for ${bytes(across.provenBytes)} of it that copy counts as proof '
            '(hashed there, and confirmed there recently), so Free up can free it here.',
          ),
        ),
        for (final a in across.largest) () => _AcrossTile(group: a),
      ],
    ];
    return ReadableList(itemCount: rows.length, itemBuilder: (c, i) => rows[i]());
  }
}

class _Header extends StatelessWidget {
  const _Header({required this.data, required this.query, required this.onClear});
  final DupesData data;
  final DupesQuery query;
  final VoidCallback onClear;

  @override
  Widget build(BuildContext context) {
    final d = data;
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 8, 16, 0),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          const LookOnlyNote(what: 'Freeing up copies and other changes'),
          const SizedBox(height: 8),
          Card(
            child: Padding(
              padding: const EdgeInsets.all(16),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Row(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Expanded(
                        child: Text(
                          bytes(d.reclaimable),
                          style: context.text.headlineMedium?.copyWith(
                            fontWeight: FontWeight.w800,
                            color: context.colors.primary,
                          ),
                        ),
                      ),
                      Tooltip(
                        message: 'Copies are compared by their full SHA-256, not by name or size; hardlinks of one file count once',
                        child: Tag('Content verified', icon: Icons.check, color: context.bd.good),
                      ),
                    ],
                  ),
                  const SizedBox(height: 2),
                  Text(
                    'could be freed across ${plural(d.groups, 'group')} of identical files',
                    style: context.text.bodyMedium,
                  ),
                  if (d.narrowed)
                    Padding(
                      padding: const EdgeInsets.only(top: 4),
                      child: Muted('of ${count(d.allGroups)} (${bytes(d.allReclaimable)}) in all'),
                    ),
                  if (d.onlyHidden > 0)
                    Padding(
                      padding: const EdgeInsets.only(top: 4),
                      child: Muted(
                        '${plural(d.onlyHidden, 'group is', 'groups are')} only in hidden folders: show hidden files to '
                        'see ${d.onlyHidden == 1 ? 'it' : 'them'}.',
                      ),
                    ),
                  if (query.filtering) ...[
                    const SizedBox(height: 8),
                    Align(
                      alignment: Alignment.centerLeft,
                      child: TextButton.icon(
                        onPressed: onClear,
                        icon: const Icon(Icons.filter_alt_off_outlined, size: 18),
                        label: const Text('Clear filters'),
                      ),
                    ),
                  ],
                ],
              ),
            ),
          ),
        ],
      ),
    );
  }
}

class _FilterChips extends StatelessWidget {
  const _FilterChips({required this.query, required this.onTap, required this.onMin});
  final DupesQuery query;
  final VoidCallback onTap;
  final ValueChanged<String> onMin;

  @override
  Widget build(BuildContext context) {
    final q = query;
    final minLabel = dupeMins.firstWhere((m) => m.$1 == q.min, orElse: () => dupeMins[2]).$2;
    return SizedBox(
      height: 52,
      child: ListView(
        scrollDirection: Axis.horizontal,
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
        children: [
          PopupMenuButton<String>(
            tooltip: 'Smallest file to count',
            onSelected: onMin,
            itemBuilder: (c) => [
              for (final (v, l) in dupeMins) CheckedPopupMenuItem(value: v, checked: v == q.min, child: Text(l)),
            ],
            child: Chip(
              avatar: const Icon(Icons.straighten, size: 16),
              label: Text(q.min == '1' ? 'Files of any size' : 'Files of $minLabel'),
              visualDensity: VisualDensity.compact,
            ),
          ),
          const SizedBox(width: 8),
          ActionChip(
            avatar: const Icon(Icons.category_outlined, size: 16),
            label: Text(q.type == DupeType.any ? 'Any type' : q.type.label),
            visualDensity: VisualDensity.compact,
            onPressed: onTap,
          ),
          const SizedBox(width: 8),
          ActionChip(
            avatar: const Icon(Icons.event_outlined, size: 16),
            label: Text(q.date.label),
            visualDensity: VisualDensity.compact,
            onPressed: onTap,
          ),
          const SizedBox(width: 8),
          ActionChip(
            avatar: Icon(q.hidden ? Icons.visibility_outlined : Icons.visibility_off_outlined, size: 16),
            label: Text(q.hidden ? 'Hidden files too' : 'Hidden files left out'),
            visualDensity: VisualDensity.compact,
            onPressed: onTap,
          ),
        ],
      ),
    );
  }
}

class _Empty extends StatelessWidget {
  const _Empty({required this.data, required this.query, required this.onHidden});
  final DupesData data;
  final DupesQuery query;
  final VoidCallback onHidden;

  @override
  Widget build(BuildContext context) {
    final d = data;
    if (d.allGroups > 0 || (d.across?.files ?? 0) > 0) {
      return Padding(
        padding: const EdgeInsets.all(24),
        child: EmptyState(
          icon: Icons.filter_alt_off_outlined,
          title: 'No duplicates match',
          message:
              '${plural(d.allGroups, 'group')} of identical files ${d.narrowed ? 'left out by the filters' : 'found'}.',
          action: d.onlyHidden > 0 ? OutlinedButton(onPressed: onHidden, child: const Text('Show hidden files')) : null,
        ),
      );
    }
    return Padding(
      padding: const EdgeInsets.all(24),
      child: EmptyState(
        icon: Icons.content_copy_outlined,
        title: query.min == '1' ? 'No duplicates found yet' : 'No duplicates this size',
        message: 'Only hashed files count: `backdown hash` hashes everything that could be a duplicate.',
      ),
    );
  }
}

/// Where a group's copies are, on one line: what their folders share, and in braces where they
/// part ways (ui/dupes.js placesHtml): /home/me/Projects/workspace-{ask, chart, +35 more}.
({String head, List<String> inner, int more, String tail})? placesOf(DupeGroup g, {int max = 3}) {
  final folders = <String>{for (final f in g.files) f.folder}.toList();
  if (folders.length < 2) return null;
  final split = RegExp(r'[\\/]');
  final sep = folders.first.contains('\\') && !folders.first.contains('/') ? '\\' : '/';
  final ps = [for (final f in folders) f.split(split)];
  final minLen = ps.map((p) => p.length).reduce(math.min);
  var pre = 0, post = 0;
  while (pre < minLen && ps.every((p) => p[pre] == ps[0][pre])) {
    pre++;
  }
  while (post < minLen - pre && ps.every((p) => p[p.length - 1 - post] == ps[0][ps[0].length - 1 - post])) {
    post++;
  }
  final mids = <String>{for (final p in ps) p.sublist(pre, p.length - post).join(sep)}.toList();
  if (mids.any((m) => m.isEmpty)) {
    return (head: folders.first, inner: const [], more: folders.length - 1, tail: '');
  }
  String edge(List<String> names, {bool end = false}) {
    String flip(String s) => end ? s.split('').reversed.join() : s;
    final ns = names.map(flip).toList();
    var i = 0;
    while (ns.every((n) => i < n.length && n[i] == ns[0][i])) {
      i++;
    }
    final e = ns[0].substring(0, i);
    final cut = ['-', '_', '.', ' ', '/', '\\'].map(e.lastIndexOf).reduce(math.max);
    return cut < 0 ? '' : flip(e.substring(0, cut + 1));
  }

  final a = edge(mids);
  final z = edge([for (final m in mids) m.substring(a.length)], end: true);
  final inner = [for (final m in mids) m.substring(a.length, m.length - z.length)]
      .map((s) => s.isEmpty ? '·' : s)
      .toList();
  final parts = ps[0];
  final head = (pre > 0 ? '${parts.take(pre).join(sep)}$sep' : '') + a;
  final tail = z + (post > 0 ? '$sep${parts.sublist(parts.length - post).join(sep)}' : '');
  return (head: head, inner: inner.take(max).toList(), more: math.max(0, inner.length - max), tail: tail);
}

/// One set of identical files: its name, how many copies and what it frees, and, opened, where
/// each copy is.
class DupeGroupCard extends StatelessWidget {
  const DupeGroupCard({super.key, required this.group, required this.open, required this.onToggle});

  final DupeGroup group;
  final bool open;
  final VoidCallback onToggle;

  @override
  Widget build(BuildContext context) {
    final g = group;
    final shown = g.shown;
    final first = shown.firstOrNull ?? g.files.first;
    final places = placesOf(g);
    final dim = context.text.bodySmall?.copyWith(color: context.colors.onSurfaceVariant);
    final where = places == null
        ? Text('in ${first.folder}', style: dim, maxLines: 2, overflow: TextOverflow.ellipsis)
        : places.inner.isEmpty
        ? Text(
            '${places.head} and ${plural(places.more, 'folder')} below it',
            style: dim,
            maxLines: 2,
            overflow: TextOverflow.ellipsis,
          )
        : Text.rich(
            TextSpan(
              children: [
                TextSpan(text: places.head),
                const TextSpan(text: '{'),
                TextSpan(
                  text: places.inner.join(', '),
                  style: TextStyle(color: context.colors.onSurface, fontWeight: FontWeight.w700),
                ),
                if (places.more > 0) TextSpan(text: ', +${count(places.more)} more'),
                const TextSpan(text: '}'),
                TextSpan(text: places.tail),
              ],
            ),
            style: dim,
            maxLines: 2,
            overflow: TextOverflow.ellipsis,
          );
    final others = g.otherNames;
    final notes = [
      if (g.files.length > g.copies) '${count(g.files.length)} names',
      if (g.files.length > shown.length) '${count(g.files.length - shown.length)} not listed',
      if (g.remote.isNotEmpty) '+${count(g.remote.length)} on other machines',
    ];
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 5, 16, 5),
      child: Card(
        clipBehavior: Clip.antiAlias,
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            InkWell(
              onTap: onToggle,
              child: Padding(
                padding: const EdgeInsets.fromLTRB(12, 12, 8, 12),
                child: Row(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    FileThumb(id: first.id, name: first.name, bytes: g.size, size: 44),
                    const SizedBox(width: 12),
                    Expanded(
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Text(g.name, style: context.text.titleSmall, maxLines: 2, overflow: TextOverflow.ellipsis),
                          if (others.isNotEmpty)
                            Muted(
                              'also named ${others.take(2).join(', ')}${others.length > 2 ? '…' : ''}',
                              maxLines: 1,
                            ),
                          const SizedBox(height: 2),
                          where,
                          const SizedBox(height: 6),
                          Wrap(
                            spacing: 6,
                            runSpacing: 4,
                            crossAxisAlignment: WrapCrossAlignment.center,
                            children: [
                              Tag('×${count(g.copies)}', color: context.bd.info),
                              Text('${bytes(g.size)} each', style: context.text.labelMedium),
                              if (notes.isNotEmpty) Muted(notes.join(' · ')),
                            ],
                          ),
                        ],
                      ),
                    ),
                    const SizedBox(width: 8),
                    Column(
                      crossAxisAlignment: CrossAxisAlignment.end,
                      children: [
                        Text(
                          bytes(g.reclaimable),
                          style: context.text.titleSmall?.copyWith(fontWeight: FontWeight.w800),
                        ),
                        Muted('to free'),
                        const SizedBox(height: 4),
                        AnimatedRotation(
                          turns: open ? 0.5 : 0,
                          duration: const Duration(milliseconds: 150),
                          child: Icon(Icons.expand_more, color: context.bd.muted),
                        ),
                      ],
                    ),
                  ],
                ),
              ),
            ),
            if (open) ...[
              const Divider(height: 1),
              for (final f in shown) _CopyTile(group: g, file: f),
              if (g.files.length > shown.length)
                Padding(
                  padding: const EdgeInsets.fromLTRB(16, 6, 16, 6),
                  child: Muted(
                    '${plural(g.files.length - shown.length, 'more copy is', 'more copies are')} here but not listed '
                    '(filtered out or hidden); ${g.files.length - shown.length == 1 ? 'it stays' : 'they stay'}.',
                  ),
                ),
              for (final r in g.remote) RemoteCopyTile(copy: r),
              const SizedBox(height: 6),
            ],
          ],
        ),
      ),
    );
  }
}

class _CopyTile extends ConsumerWidget {
  const _CopyTile({required this.group, required this.file});
  final DupeGroup group;
  final DupeFile file;

  FileRef get _ref => FileRef(id: file.id, name: file.name, size: group.size, mtime: file.mtime);

  Future<void> _menu(BuildContext context, WidgetRef ref) async {
    final api = ref.read(apiProvider);
    await showModalBottomSheet<void>(
      context: context,
      useRootNavigator: true,
      showDragHandle: true,
      builder: (c) {
        void go(VoidCallback f) {
          Navigator.pop(c);
          f();
        }

        return SafeArea(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              ListTile(
                leading: FileThumb(id: file.id, name: file.name, bytes: group.size),
                title: Text(file.name, maxLines: 2, overflow: TextOverflow.ellipsis),
                subtitle: PathText(file.folder, maxLines: 2),
              ),
              const Divider(),
              ListTile(
                leading: const Icon(Icons.visibility_outlined),
                title: const Text('Open'),
                onTap: () => go(() => openFile(context, _ref)),
              ),
              ListTile(
                leading: const Icon(Icons.folder_open_outlined),
                title: const Text('Show in folder'),
                onTap: () => go(() => context.push(Routes.folder(file.dir, select: file.key))),
              ),
              ListTile(
                leading: const Icon(Icons.info_outline),
                title: const Text('Details'),
                subtitle: const Text('Where else it is, its versions and hash'),
                onTap: () => go(() => context.push(Routes.file(file.id))),
              ),
              if (api != null)
                ListTile(
                  leading: const Icon(Icons.share_outlined),
                  title: const Text('Share'),
                  onTap: () => go(() => shareFile(context, api, _ref)),
                ),
              ListTile(
                leading: const Icon(Icons.copy),
                title: const Text('Copy path'),
                onTap: () => go(() => copyText(context, file.path, 'Path')),
              ),
            ],
          ),
        );
      },
    );
  }

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final f = file;
    return ListTile(
      dense: true,
      contentPadding: const EdgeInsets.only(left: 16, right: 4),
      leading: FileThumb(id: f.id, name: f.name, bytes: group.size, size: 28),
      title: Text(f.name, maxLines: 1, overflow: TextOverflow.ellipsis),
      subtitle: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [PathText(f.folder, maxLines: 3), if (f.mtime > 0) Muted('modified ${when(f.mtime)}')],
      ),
      trailing: IconButton(
        tooltip: 'Show in folder',
        icon: const Icon(Icons.folder_open_outlined, size: 20),
        onPressed: () => context.push(Routes.folder(f.dir, select: f.key)),
      ),
      onTap: f.id == 0 ? null : () => openFile(context, _ref),
      onLongPress: () => _menu(context, ref),
    );
  }
}

/// A copy on another machine.
class RemoteCopyTile extends ConsumerWidget {
  const RemoteCopyTile({super.key, required this.copy});
  final RemoteCopy copy;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final c = copy;
    final skew = ref.watch(skewProvider);
    return ListTile(
      dense: true,
      leading: Icon(
        c.counts ? Icons.verified_outlined : Icons.dns_outlined,
        color: c.counts ? context.bd.good : context.bd.muted,
      ),
      title: Text('On ${c.machine}'),
      subtitle: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          PathText(c.path, maxLines: 3),
          Muted(
            [
              if (c.confirmedAt != null) 'confirmed ${rel(c.confirmedAt, skew: skew)}',
              if (c.counts) 'counts as a verified copy' else ?c.whyNot,
            ].join(' · '),
          ),
        ],
      ),
      onTap: () => context.push(Routes.machine(c.machineId)),
    );
  }
}

class _AcrossTile extends StatelessWidget {
  const _AcrossTile({required this.group});
  final AcrossGroup group;

  @override
  Widget build(BuildContext context) {
    final g = group;
    return ListCard(
      padding: EdgeInsets.zero,
      onTap: () => openPath(context, g.path),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          ListTile(
            leading: FileThumb(id: 0, name: leafOf(g.path), bytes: g.size, size: 32),
            title: Text(leafOf(g.path), maxLines: 1, overflow: TextOverflow.ellipsis),
            subtitle: PathText(folderOf(g.path), maxLines: 2),
            trailing: Text(bytes(g.size), style: context.text.labelLarge),
          ),
          for (final r in g.remote) RemoteCopyTile(copy: r),
        ],
      ),
    );
  }
}

class _FilterSheet extends StatefulWidget {
  const _FilterSheet({required this.query});
  final DupesQuery query;

  @override
  State<_FilterSheet> createState() => _FilterSheetState();
}

class _FilterSheetState extends State<_FilterSheet> {
  late DupesQuery _q = widget.query;

  @override
  Widget build(BuildContext context) {
    Widget chips<T>(List<T> values, T current, String Function(T) label, void Function(T) set) => Wrap(
      spacing: 8,
      runSpacing: 8,
      children: [
        for (final v in values)
          ChoiceChip(label: Text(label(v)), selected: v == current, onSelected: (_) => setState(() => set(v))),
      ],
    );
    return SafeArea(
      child: ConstrainedBox(
        constraints: BoxConstraints(maxHeight: MediaQuery.sizeOf(context).height * 0.85),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Flexible(
              child: SingleChildScrollView(
                padding: const EdgeInsets.fromLTRB(20, 0, 20, 8),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    Text('Filter duplicates', style: context.text.titleMedium),
                    const SizedBox(height: 16),
                    Text('Files of', style: context.text.labelLarge),
                    const SizedBox(height: 8),
                    chips(
                      dupeMins,
                      dupeMins.firstWhere((m) => m.$1 == _q.min, orElse: () => dupeMins[2]),
                      (m) => m.$2,
                      (m) => _q = _q.copyWith(min: m.$1),
                    ),
                    const SizedBox(height: 16),
                    Text('Type', style: context.text.labelLarge),
                    const SizedBox(height: 8),
                    chips(DupeType.values, _q.type, (t) => t.label, (t) => _q = _q.copyWith(type: t)),
                    const SizedBox(height: 16),
                    Text('Modified', style: context.text.labelLarge),
                    const SizedBox(height: 8),
                    chips(DupeDate.values, _q.date, (t) => t.label, (t) => _q = _q.copyWith(date: t)),
                    const SizedBox(height: 8),
                    SwitchListTile(
                      contentPadding: EdgeInsets.zero,
                      title: const Text('Hidden files too'),
                      subtitle: const Text('Names starting with a dot, or ending with ~'),
                      value: _q.hidden,
                      onChanged: (v) => setState(() => _q = _q.copyWith(hidden: v)),
                    ),
                  ],
                ),
              ),
            ),
            const Divider(),
            Padding(
              padding: const EdgeInsets.fromLTRB(12, 4, 20, 12),
              child: Row(
                children: [
                  TextButton(
                    onPressed: () => setState(() => _q = DupesQuery(now: _q.now)),
                    child: const Text('Clear filters'),
                  ),
                  const Spacer(),
                  FilledButton(onPressed: () => Navigator.pop(context, _q), child: const Text('Show')),
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }
}
