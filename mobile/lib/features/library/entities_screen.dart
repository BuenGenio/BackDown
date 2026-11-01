import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../../api/keys.dart';
import '../../core/actions.dart';
import '../../core/format.dart';
import '../../core/widgets.dart';
import '../../models/browse.dart';
import '../../providers.dart';
import '../../theme.dart';
import 'common.dart';
import 'data.dart';
import 'models.dart';
import 'routes.dart';

/// What's here: the things recognised on every drive (mail stores, phone backups, photo
/// libraries, repositories, dumps…), by kind, with where each came from and how much of it exists
/// elsewhere.
class EntitiesScreen extends ConsumerStatefulWidget {
  const EntitiesScreen({super.key});

  @override
  ConsumerState<EntitiesScreen> createState() => _EntitiesScreenState();
}

class _EntitiesScreenState extends ConsumerState<EntitiesScreen> {
  String? _kind;
  String? _drive;
  String _q = '';
  final _search = TextEditingController();

  @override
  void dispose() {
    _search.dispose();
    super.dispose();
  }

  bool _matches(Entity e) =>
      (_kind == null || e.kind == _kind) &&
      (_drive == null || e.rootPath == _drive) &&
      (_q.isEmpty || e.search.contains(_q));

  @override
  Widget build(BuildContext context) {
    final data = ref.watch(entitiesProvider);
    final daemon = data.value?.daemon ?? false;
    return Scaffold(
      appBar: AppBar(
        title: const Text('What\'s here'),
        actions: [
          IconButton(
            tooltip: 'Report',
            icon: const Icon(Icons.summarize_outlined),
            onPressed: () => context.push(reportRoute),
          ),
          if (daemon)
            PopupMenuButton<String>(
              onSelected: (v) => runAction(
                context,
                ref,
                (api) => api.post('recognise'),
                done: 'Recognising: it shows in Activity, and here when it\'s done',
              ),
              itemBuilder: (c) => const [PopupMenuItem(value: 'again', child: Text('Recognise again'))],
            ),
        ],
      ),
      body: RefreshIndicator(
        onRefresh: () async {
          ref.invalidate(overlapsProvider);
          await ref.refresh(entitiesProvider.future).catchError((_) => EntitiesData.fromJson(const {}));
        },
        child: LibraryBody(
          value: data,
          onRetry: () => ref.invalidate(entitiesProvider),
          data: (d) => _body(context, d),
        ),
      ),
    );
  }

  Widget _body(BuildContext context, EntitiesData d) {
    if (d.drives.isEmpty || d.all.isEmpty) {
      return EmptyState(
        icon: Icons.sell_outlined,
        title: 'Nothing recognised yet',
        message: d.daemon
            ? 'BackDown recognises what\'s on a drive after it hashes it. Start it now with "Recognise again".'
            : 'Run `backdown entities` on the computer, then come back.',
      );
    }
    final overlaps = ref.watch(overlapsProvider).value ?? const <String, Overlap>{};
    final names = {for (final r in ref.watch(rootsProvider).value ?? const <Root>[]) r.path: r.name};
    String driveName(String path) => names[path] ?? baseName(path);

    // the kinds there are, in BackDown's order, then the filtered things in each
    final all = d.all.toList();
    final kinds = <String, ({String label, int n, int bytes})>{};
    for (final e in all) {
      final k = kinds[e.kind];
      kinds[e.kind] = (label: e.label, n: (k?.n ?? 0) + 1, bytes: (k?.bytes ?? 0) + e.bytes);
    }
    int order(String k) {
      final i = entityKinds.indexOf(k);
      return i < 0 ? entityKinds.length : i;
    }

    final kindOrder = kinds.keys.toList()..sort((a, b) => order(a).compareTo(order(b)));
    final shown = all.where(_matches).toList();
    final byKind = <String, List<Entity>>{};
    for (final e in shown) {
      (byKind[e.kind] ??= []).add(e);
    }
    for (final l in byKind.values) {
      l.sort((a, b) => b.bytes.compareTo(a.bytes));
    }

    // one flat list of rows, so a long one builds lazily
    final rows = <Widget Function()>[
      () => _Filters(
        search: _search,
        onSearch: (v) => setState(() => _q = v.trim().toLowerCase()),
        kinds: [(null, 'Everything', all.length), for (final k in kindOrder) (k, kinds[k]!.label, kinds[k]!.n)],
        kind: _kind,
        onKind: (k) => setState(() => _kind = k),
      ),
      if (d.drives.length > 1 || d.drives.any((r) => !r.fresh || !r.readDrive || r.junkBytes > 0))
        () => SectionHeader('Drives', trailing: Text('${d.drives.length}', style: context.text.labelMedium)),
      for (final r in d.drives)
        if (d.drives.length > 1 || !r.fresh || !r.readDrive || r.junkBytes > 0)
          () => _DriveRow(
            drive: r,
            name: driveName(r.root),
            overlap: overlaps['${keyOfPath(r.root)}:drive'] ?? _driveOverlap(overlaps, r.root),
            selected: _drive == r.root,
            onTap: d.drives.length < 2 ? null : () => setState(() => _drive = _drive == r.root ? null : r.root),
          ),
      if (shown.isEmpty)
        () => const Padding(
          padding: EdgeInsets.all(32),
          child: EmptyState(icon: Icons.filter_alt_off_outlined, title: 'Nothing matches the filter'),
        ),
      for (final k in kindOrder)
        if (byKind[k] case final list?) ...[
          () => SectionHeader(
            kinds[k]!.label,
            trailing: Text(
              '${count(list.length)} · ${bytes(list.fold<int>(0, (a, e) => a + e.bytes))}',
              style: context.text.labelMedium?.copyWith(color: context.bd.muted),
            ),
          ),
          for (final e in list)
            () => EntityCard(
              entity: e,
              overlap: overlaps['${e.key}:entity'],
              drive: d.drives.length > 1 ? driveName(e.rootPath) : null,
              onTap: () => showEntity(
                context,
                e,
                overlaps['${e.key}:entity'],
                all,
                d.drives.where((r) => r.root == e.rootPath).firstOrNull?.computedAt,
              ),
            ),
        ],
    ];
    return ReadableList(itemCount: rows.length, itemBuilder: (c, i) => rows[i]());
  }

  /// The drive's overlap found by its path, when its key isn't the one this app would make.
  Overlap? _driveOverlap(Map<String, Overlap> all, String path) {
    for (final o in all.values) {
      if (o.level == 'drive' && o.path == path) return o;
    }
    return null;
  }
}

class _Filters extends StatelessWidget {
  const _Filters({
    required this.search,
    required this.onSearch,
    required this.kinds,
    required this.kind,
    required this.onKind,
  });

  final TextEditingController search;
  final ValueChanged<String> onSearch;
  final List<(String?, String, int)> kinds;
  final String? kind;
  final ValueChanged<String?> onKind;

  @override
  Widget build(BuildContext context) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: const EdgeInsets.fromLTRB(16, 8, 16, 4),
          child: TextField(
            controller: search,
            onChanged: onSearch,
            textInputAction: TextInputAction.search,
            decoration: InputDecoration(
              isDense: true,
              hintText: 'Filter by name, path or origin',
              prefixIcon: const Icon(Icons.filter_list),
              suffixIcon: ListenableBuilder(
                listenable: search,
                builder: (c, _) => search.text.isEmpty
                    ? const SizedBox.shrink()
                    : IconButton(
                        tooltip: 'Clear',
                        icon: const Icon(Icons.close),
                        onPressed: () {
                          search.clear();
                          onSearch('');
                        },
                      ),
              ),
            ),
          ),
        ),
        // a few dozen kinds at most: all built, so any can be scrolled to
        SingleChildScrollView(
          scrollDirection: Axis.horizontal,
          padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 6),
          child: Row(
            children: [
              for (final (i, (k, label, n)) in kinds.indexed) ...[
                if (i > 0) const SizedBox(width: 8),
                FilterChip(
                  label: Text('$label  ${count(n)}'),
                  selected: kind == k,
                  showCheckmark: false,
                  visualDensity: VisualDensity.compact,
                  onSelected: (_) => onKind(kind == k ? null : k),
                ),
              ],
            ],
          ),
        ),
      ],
    );
  }
}

class _DriveRow extends StatelessWidget {
  const _DriveRow({required this.drive, required this.name, this.overlap, this.selected = false, this.onTap});

  final DriveEntities drive;
  final String name;
  final Overlap? overlap;
  final bool selected;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    final o = overlap;
    final junk = drive.junkBytes;
    return ListTile(
      selected: selected,
      selectedTileColor: context.bd.selection.withValues(alpha: 0.5),
      leading: Icon(Icons.storage, color: context.bd.folder),
      title: Text(name, maxLines: 1, overflow: TextOverflow.ellipsis),
      subtitle: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            [plural(drive.entities.length, 'thing'), if (drive.root != name) drive.root].join(' · '),
            maxLines: 1,
            overflow: TextOverflow.ellipsis,
          ),
          if (o != null || junk > 0 || !drive.fresh || !drive.readDrive) ...[
            const SizedBox(height: 4),
            Wrap(
              spacing: 6,
              runSpacing: 4,
              children: [
                if (o != null)
                  Tag(elsewhereText(o), color: o.bytes > 0 && o.elsewhere >= o.bytes ? context.bd.good : null),
                if (junk > 0) Tag('${bytes(junk)} regenerable'),
                if (!drive.fresh) Tag('from an earlier scan', color: context.bd.warn),
                if (!drive.readDrive) Tag('drive wasn\'t connected', color: context.bd.muted),
              ],
            ),
          ],
        ],
      ),
      trailing: onTap == null ? null : Icon(selected ? Icons.filter_alt : Icons.filter_alt_outlined, size: 20),
      onTap: onTap,
    );
  }
}

/// One recognised thing, as a card in the list.
class EntityCard extends StatelessWidget {
  const EntityCard({super.key, required this.entity, this.overlap, this.drive, this.onTap});

  final Entity entity;
  final Overlap? overlap;

  /// The drive's name, when there are several.
  final String? drive;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    final e = entity;
    final color = kindColor(e.kind);
    final meta = [
      e.label,
      yearsOf(e.oldest, e.newest),
      if (!e.isFile) plural(e.files, 'file'),
      if (e.confidence < 0.8) '${(e.confidence * 100).round()}% sure',
    ].where((s) => s.isNotEmpty).join(' · ');
    final o = overlap;
    return Opacity(
      opacity: e.lowValue ? 0.75 : 1,
      child: ListCard(
        onTap: onTap,
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            CircleAvatar(
              radius: 18,
              backgroundColor: color.withValues(alpha: 0.14),
              child: Icon(kindIcon(e.kind), size: 20, color: color),
            ),
            const SizedBox(width: 12),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Row(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Expanded(
                        child: Text(
                          e.title.isEmpty ? e.label : e.title,
                          style: context.text.titleSmall,
                          maxLines: 2,
                          overflow: TextOverflow.ellipsis,
                        ),
                      ),
                      const SizedBox(width: 8),
                      Text(bytes(e.bytes), style: context.text.labelLarge?.copyWith(fontWeight: FontWeight.w700)),
                    ],
                  ),
                  const SizedBox(height: 2),
                  Muted(meta, maxLines: 2),
                  const SizedBox(height: 4),
                  InkWell(
                    borderRadius: BorderRadius.circular(4),
                    onTap: () => openKey(context, e.key, isFile: e.isFile),
                    child: Padding(
                      padding: const EdgeInsets.symmetric(vertical: 2),
                      child: PathText(
                        drive == null ? e.relPath : '$drive › ${e.relPath}',
                        maxLines: 2,
                        color: context.linkColor,
                      ),
                    ),
                  ),
                  if (e.provenance.isNotEmpty) ...[
                    const SizedBox(height: 6),
                    _Line(icon: Icons.route_outlined, text: e.provenance.first, maxLines: 3),
                  ] else if (e.provenanceLocked) ...[
                    const SizedBox(height: 6),
                    const _Line(icon: Icons.lock_outline, text: 'Where it came from is part of BackDown Pro.'),
                  ],
                  if (o != null && o.bytes > 0 && o.elsewhere > 0) ...[
                    const SizedBox(height: 8),
                    Meter(fraction: o.share, height: 4, color: context.bd.good),
                    const SizedBox(height: 4),
                    Muted(
                      '${capitalised(elsewhereText(o))}'
                      '${o.containers.isEmpty ? '' : ', mostly in ${o.containers.first.label}'}',
                      maxLines: 2,
                    ),
                  ] else if (o != null && o.bytes > 0) ...[
                    const SizedBox(height: 6),
                    const _Line(icon: Icons.error_outline, text: 'None of it exists elsewhere.'),
                  ],
                  if (!e.junk.isEmpty) ...[
                    const SizedBox(height: 6),
                    _Line(
                      icon: Icons.cleaning_services_outlined,
                      text: '${bytes(e.junk.bytes)} regenerable: ${e.junk.whys.take(3).join(', ')}',
                      maxLines: 2,
                    ),
                  ],
                  if (e.lowValue) ...[
                    const SizedBox(height: 6),
                    const _Line(icon: Icons.info_outline, text: 'Probably not worth keeping as a unit.'),
                  ],
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }
}

class _Line extends StatelessWidget {
  const _Line({required this.icon, required this.text, this.maxLines});
  final IconData icon;
  final String text;
  final int? maxLines;

  @override
  Widget build(BuildContext context) => Row(
    crossAxisAlignment: CrossAxisAlignment.start,
    children: [
      Padding(
        padding: const EdgeInsets.only(top: 1),
        child: Icon(icon, size: 14, color: context.bd.muted),
      ),
      const SizedBox(width: 6),
      Expanded(child: Muted(text, maxLines: maxLines)),
    ],
  );
}

/// Everything about one recognised thing, in a sheet: open it, its facts, where it came from,
/// where else it is, and its regenerable parts.
Future<void> showEntity(BuildContext context, Entity e, Overlap? overlap, List<Entity> all, [int? recognisedAt]) {
  return showModalBottomSheet<void>(
    context: context,
    useRootNavigator: true,
    isScrollControlled: true,
    showDragHandle: true,
    useSafeArea: true,
    builder: (c) => DraggableScrollableSheet(
      expand: false,
      initialChildSize: 0.75,
      minChildSize: 0.4,
      maxChildSize: 0.95,
      builder: (c, scroll) => _EntitySheet(
        entity: e,
        overlap: overlap,
        all: all,
        scroll: scroll,
        recognisedAt: recognisedAt,
        outer: context,
      ),
    ),
  );
}

class _EntitySheet extends ConsumerWidget {
  const _EntitySheet({
    required this.entity,
    required this.overlap,
    required this.all,
    required this.scroll,
    this.recognisedAt,
    required this.outer,
  });

  /// The screen's context, to open things from once the sheet is closed.
  final BuildContext outer;

  final Entity entity;
  final Overlap? overlap;
  final List<Entity> all;
  final ScrollController scroll;
  final int? recognisedAt;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final e = entity;
    final color = kindColor(e.kind);
    final parent = e.inside == null ? null : all.where((x) => x.key == e.inside).firstOrNull;
    final within = all.where((x) => x.inside == e.key).toList();
    final o = overlap;
    final skew = ref.watch(skewProvider);
    void close() => Navigator.of(context).pop();
    return ListView(
      controller: scroll,
      padding: const EdgeInsets.only(bottom: 32),
      children: [
        ListTile(
          leading: CircleAvatar(
            backgroundColor: color.withValues(alpha: 0.14),
            child: Icon(kindIcon(e.kind), color: color),
          ),
          title: Text(e.title.isEmpty ? e.label : e.title, style: context.text.titleMedium),
          subtitle: Text(e.label),
        ),
        Padding(
          padding: const EdgeInsets.fromLTRB(16, 0, 16, 4),
          child: Wrap(
            spacing: 8,
            runSpacing: 8,
            children: [
              FilledButton.icon(
                onPressed: () {
                  close();
                  openKey(outer, e.key, isFile: e.isFile);
                },
                icon: const Icon(Icons.folder_open_outlined),
                label: Text(e.isFile ? 'Show in folder' : 'Open'),
              ),
              OutlinedButton.icon(
                onPressed: () => copyText(context, e.path, 'Path'),
                icon: const Icon(Icons.copy, size: 18),
                label: const Text('Copy path'),
              ),
            ],
          ),
        ),
        const SectionHeader('Details'),
        InfoRow('Where', e.path, copyable: true),
        InfoRow('Size', e.isFile ? bytes(e.bytes) : '${bytes(e.bytes)} in ${plural(e.files, 'file')}'),
        if (yearsOf(e.oldest, e.newest).isNotEmpty)
          InfoRow('Dates', '${day(e.oldest)} – ${day(e.newest)} (when its files were last changed)'),
        InfoRow('Sure', '${(e.confidence * 100).round()}%'),
        for (final (k, v) in e.facts)
          if (k != 'Size') InfoRow(k, v),
        if (parent != null) InfoRow('Inside', '${parent.label}: ${parent.title}'),
        if (e.lowValue) const InfoRow('Note', 'Probably not worth keeping as a unit.'),
        const SectionHeader('Where it came from'),
        if (e.provenance.isNotEmpty)
          for (final p in e.provenance)
            ListTile(dense: true, leading: const Icon(Icons.route_outlined, size: 20), title: Text(p))
        else
          Padding(
            padding: const EdgeInsets.fromLTRB(16, 4, 16, 4),
            child: Muted(
              e.provenanceLocked
                  ? 'Where it came from is part of BackDown Pro: the host, domain, device or person, read from backup '
                        'scripts, plists, git remotes and dump headers.'
                  : 'Nothing on the drive says where it came from.',
            ),
          ),
        if (o != null && o.bytes > 0) ...[
          SectionHeader('Exists elsewhere', trailing: Text(percent(o.share), style: context.text.labelMedium)),
          Padding(
            padding: const EdgeInsets.fromLTRB(16, 2, 16, 6),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Meter(fraction: o.share, color: context.bd.good),
                const SizedBox(height: 6),
                Muted(
                  '${bytes(o.elsewhere)} of ${bytes(o.bytes)} (${plural(o.elsewhereFiles, 'file')}) has an identical '
                  'copy outside it.',
                ),
              ],
            ),
          ),
          for (final c in o.containers)
            ListTile(
              dense: true,
              leading: Icon(
                c.label.startsWith('the machine') ? Icons.dns_outlined : Icons.content_copy_outlined,
                color: context.bd.info,
              ),
              title: Text(c.label, maxLines: 2, overflow: TextOverflow.ellipsis),
              subtitle: PathText(c.path, maxLines: 2),
              trailing: Text(bytes(c.bytes), style: context.text.labelMedium),
              onTap: c.label.startsWith('the machine') || c.key.isEmpty
                  ? null
                  : () {
                      close();
                      openKey(outer, c.key);
                    },
            ),
        ],
        if (within.isNotEmpty) ...[
          SectionHeader('Inside it', trailing: Text('${within.length}', style: context.text.labelMedium)),
          for (final x in within.take(30))
            ListTile(
              dense: true,
              leading: Icon(kindIcon(x.kind), color: kindColor(x.kind), size: 20),
              title: Text(x.title.isEmpty ? x.label : x.title, maxLines: 1, overflow: TextOverflow.ellipsis),
              subtitle: Text('${x.label} · ${bytes(x.bytes)}'),
              onTap: () {
                close();
                openKey(outer, x.key, isFile: x.isFile);
              },
            ),
        ],
        if (!e.junk.isEmpty) ...[
          SectionHeader('Regenerable', trailing: Text(bytes(e.junk.bytes), style: context.text.labelMedium)),
          Padding(
            padding: const EdgeInsets.fromLTRB(16, 0, 16, 6),
            child: Muted(
              'Caches, previews and build output the programs that made them can make again: '
              '${plural(e.junk.count, 'place')}, ${plural(e.junk.files, 'file')}.',
            ),
          ),
          for (final j in e.junk.biggest)
            ListTile(
              dense: true,
              leading: Icon(Icons.cleaning_services_outlined, color: context.bd.muted, size: 20),
              title: Text(j.why),
              subtitle: PathText(
                j.path.startsWith(e.path)
                    ? j.path.substring(e.path.length).replaceFirst(RegExp(r'^[\\/]'), '')
                    : j.path,
                maxLines: 2,
              ),
              trailing: Text(bytes(j.bytes), style: context.text.labelMedium),
              onTap: () {
                close();
                showInFolder(outer, j.key);
              },
            ),
          if (e.junk.count > e.junk.biggest.length)
            Padding(
              padding: const EdgeInsets.fromLTRB(16, 4, 16, 0),
              child: Muted('And ${count(e.junk.count - e.junk.biggest.length)} smaller ones.'),
            ),
        ],
        if (recognisedAt != null && recognisedAt! > 0)
          Padding(
            padding: const EdgeInsets.fromLTRB(16, 16, 16, 0),
            child: Muted('Recognised ${rel(recognisedAt, skew: skew)}.'),
          ),
      ],
    );
  }
}
