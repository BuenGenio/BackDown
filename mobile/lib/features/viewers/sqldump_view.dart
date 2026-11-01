import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../../api/json.dart';
import '../../core/format.dart';
import '../../core/kinds.dart';
import '../../core/widgets.dart';
import '../../providers.dart';
import '../../theme.dart';
import 'common.dart';
import 'models.dart';
import 'routes.dart';
import 'sqlite_view.dart';
import 'text_view.dart';

enum _Sort { name, rows, bytes }

/// A SQL dump as a schema: read whole by the service, a few seconds at a time, and shown as it
/// goes: the header's facts, each table's rows and data size, columns, keys and indexes, and the
/// views, triggers and routines. The dump's own text is one tab away.
class SqlDumpView extends ConsumerStatefulWidget {
  const SqlDumpView({super.key, required this.source, required this.first});
  final ViewSource source;
  final SqlDump first;

  @override
  ConsumerState<SqlDumpView> createState() => _SqlDumpViewState();
}

class _SqlDumpViewState extends ConsumerState<SqlDumpView> {
  late SqlDump _d = widget.first;
  bool _reading = false, _stopped = false, _sql = false;
  String? _error;
  String _q = '';
  _Sort _sort = _Sort.bytes;
  bool _asc = false;

  @override
  void initState() {
    super.initState();
    _pump();
  }

  @override
  void dispose() {
    _stopped = true;
    super.dispose();
  }

  Future<void> _pump() async {
    if (_reading || _d.more == null) return;
    setState(() {
      _reading = true;
      _stopped = false;
      _error = null;
    });
    try {
      while (_d.more != null && !_stopped && mounted) {
        final api = ref.read(apiProvider);
        if (api == null) return;
        final next = await fetchParsed(api, 'sqldump', {'id': widget.source.id, 'more': _d.more}, SqlDump.fromJson);
        if (!mounted) return;
        setState(() => _d = next);
      }
    } on Object catch (e) {
      if (mounted) setState(() => _error = e.toString());
    } finally {
      if (mounted) setState(() => _reading = false);
    }
  }

  List<SqlTable> _tables() {
    final q = _q.toLowerCase();
    final dir = _asc ? 1 : -1;
    final list = [
      for (final t in _d.tables)
        if (q.isEmpty || t.key.toLowerCase().contains(q)) t,
    ];
    int cmp(SqlTable a, SqlTable b) => switch (_sort) {
      _Sort.name => dir * naturalCompare(a.name, b.name),
      _Sort.rows =>
        dir * a.rows.compareTo(b.rows) != 0 ? dir * a.rows.compareTo(b.rows) : naturalCompare(a.name, b.name),
      _Sort.bytes =>
        dir * a.bytes.compareTo(b.bytes) != 0 ? dir * a.bytes.compareTo(b.bytes) : naturalCompare(a.name, b.name),
    };
    return list..sort((a, b) => a.schema.compareTo(b.schema) != 0 ? a.schema.compareTo(b.schema) : cmp(a, b));
  }

  String _totals() {
    final d = _d;
    final rows = d.tables.fold<int>(0, (a, t) => a + t.rows);
    final size = d.tables.fold<int>(0, (a, t) => a + t.bytes);
    return [
      plural(d.tables.length, 'table'),
      plural(rows, 'row'),
      '${bytes(size)} of data',
      for (final (list, what) in [
        (d.views, 'view'),
        (d.triggers, 'trigger'),
        (d.routines, 'routine'),
        (d.events, 'event'),
        (d.sequences, 'sequence'),
      ])
        if (list.isNotEmpty) plural(list.length, what),
    ].join(' · ');
  }

  Widget _progress() {
    final d = _d;
    if (d.complete) {
      return Text(
        'Read to the end${d.elapsedMs > 1500 ? ' in ${(d.elapsedMs / 1000).toStringAsFixed(1)} s' : ''}.',
        style: context.text.bodySmall?.copyWith(color: context.bd.muted),
      );
    }
    final pct = d.size == 0 ? 0 : math.min(100, 100 * d.read ~/ d.size);
    final state = !_reading
        ? 'Paused at'
        : _stopped
        ? 'Stopping at'
        : 'Reading';
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Row(
          children: [
            Expanded(
              child: Text(
                '$state $pct% of ${bytes(d.size)}${d.compression != null ? ' (${d.compression})' : ''}',
                style: context.text.bodySmall,
              ),
            ),
            if (!_reading)
              TextButton(onPressed: _pump, child: const Text('Keep reading'))
            else if (!_stopped)
              TextButton(onPressed: () => setState(() => _stopped = true), child: const Text('Stop')),
          ],
        ),
        Meter(fraction: d.size == 0 ? null : d.read / d.size),
        if (_error != null)
          Padding(
            padding: const EdgeInsets.only(top: 6),
            child: Text(_error!, style: context.text.bodySmall?.copyWith(color: context.bd.crit)),
          ),
      ],
    );
  }

  Widget _schema() {
    final d = _d;
    final tables = _tables();
    final biggest = math.max(1, d.tables.fold<int>(0, (a, t) => math.max(a, t.bytes)));
    final grouped = d.schemas.length > 1;
    final notes = [if (d.cut != null) 'The dump looks cut off: ${d.cut}. What comes before is shown.', ...d.notes];
    final items = <Widget>[
      Padding(
        padding: const EdgeInsets.fromLTRB(12, 12, 12, 0),
        child: Card(
          child: Padding(
            padding: const EdgeInsets.symmetric(vertical: 12),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Padding(
                  padding: const EdgeInsets.fromLTRB(16, 0, 16, 2),
                  child: Text(d.title, style: context.text.titleMedium),
                ),
                Padding(
                  padding: const EdgeInsets.fromLTRB(16, 0, 16, 6),
                  child: Text(
                    _totals(),
                    style: context.text.bodySmall?.copyWith(color: context.colors.onSurfaceVariant),
                  ),
                ),
                for (final (k, v) in d.facts) InfoRow(k, v),
                Padding(padding: const EdgeInsets.fromLTRB(16, 8, 8, 0), child: _progress()),
              ],
            ),
          ),
        ),
      ),
      for (final n in notes) ViewerNote(n),
      Padding(
        padding: const EdgeInsets.fromLTRB(12, 12, 4, 4),
        child: Row(
          children: [
            Expanded(
              child: TextField(
                decoration: const InputDecoration(
                  isDense: true,
                  prefixIcon: Icon(Icons.filter_list),
                  hintText: 'Filter tables',
                ),
                onChanged: (t) => setState(() => _q = t.trim()),
              ),
            ),
            PopupMenuButton<_Sort>(
              tooltip: 'Sort tables',
              icon: const Icon(Icons.sort),
              onSelected: (s) => setState(() {
                if (s == _sort) {
                  _asc = !_asc;
                } else {
                  _sort = s;
                  _asc = s == _Sort.name;
                }
              }),
              itemBuilder: (c) => [
                for (final (s, label) in [(_Sort.name, 'Name'), (_Sort.rows, 'Rows'), (_Sort.bytes, 'Size')])
                  CheckedPopupMenuItem(
                    value: s,
                    checked: s == _sort,
                    child: Text(s == _sort ? '$label ${_asc ? '↑' : '↓'}' : label),
                  ),
              ],
            ),
          ],
        ),
      ),
    ];
    String? group;
    for (final t in tables) {
      if (grouped && t.schema != group) {
        group = t.schema;
        items.add(SectionHeader(group.isEmpty ? '(no schema)' : group));
      }
      items.add(
        ListTile(
          leading: Icon(Icons.table_chart_outlined, color: FileKind.sql.color),
          title: Text(t.name, maxLines: 1, overflow: TextOverflow.ellipsis),
          subtitle: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                [
                  if (t.rows > 0 || t.bytes > 0)
                    '${plural(t.rows, 'row')}${t.uncounted ? '+' : ''} · ${bytes(t.bytes)}'
                  else if (t.defined)
                    'no rows',
                  if (!t.defined) '(data only)',
                ].join(' '),
              ),
              const SizedBox(height: 4),
              Meter(fraction: t.bytes / biggest, height: 3, color: FileKind.sql.color),
            ],
          ),
          trailing: const Icon(Icons.chevron_right),
          onTap: () => context.push(sqlTableLocation(widget.source.id, t.key), extra: _d),
        ),
      );
    }
    if (tables.isEmpty) {
      items.add(
        Padding(
          padding: const EdgeInsets.all(24),
          child: Text(
            _q.isNotEmpty
                ? 'No table matches.'
                : d.complete
                ? 'This dump creates no tables.'
                : 'No tables found yet.',
            style: context.text.bodyMedium?.copyWith(color: context.bd.muted),
            textAlign: TextAlign.center,
          ),
        ),
      );
    }
    if (d.objects > 0 && _q.isEmpty) {
      items.add(
        ListTile(
          leading: Icon(Icons.functions, color: context.bd.info),
          title: const Text('Views, triggers and routines'),
          subtitle: Text(plural(d.objects, 'object')),
          trailing: const Icon(Icons.chevron_right),
          onTap: () => _showObjects(context, d),
        ),
      );
    }
    items.add(const SizedBox(height: 32));
    return Readable(child: ListView(children: items));
  }

  @override
  Widget build(BuildContext context) {
    return Column(
      children: [
        ViewerBar(
          meta: _sql ? 'The dump\'s text' : null,
          children: [
            Padding(
              padding: const EdgeInsets.only(right: 8),
              child: SegmentedButton<bool>(
                showSelectedIcon: false,
                style: const ButtonStyle(visualDensity: VisualDensity.compact),
                segments: const [
                  ButtonSegment(value: false, label: Text('Schema')),
                  ButtonSegment(value: true, label: Text('SQL')),
                ],
                selected: {_sql},
                onSelectionChanged: (v) => setState(() => _sql = v.first),
              ),
            ),
          ],
        ),
        Expanded(
          child: _sql ? _SqlText(source: widget.source, compressed: _d.compression != null) : _schema(),
        ),
      ],
    );
  }
}

/// A compressed dump's text: the single file the archive viewer finds in it.
final _decompressedProvider = FutureProvider.autoDispose.family<String, int>((ref, id) async {
  final a = asJson(await requireApi(ref).get('archive', {'id': id, 'limit': 1, 'quick': 1}));
  final name = a.list('entries').firstOrNull?.strOrNull('name');
  if (name == null) throw StateError('can\'t decompress this file');
  return name;
});

class _SqlText extends ConsumerWidget {
  const _SqlText({required this.source, required this.compressed});
  final ViewSource source;
  final bool compressed;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    if (!compressed) return TextView(source: source);
    return AsyncBody(
      value: ref.watch(_decompressedProvider(source.id)),
      onRetry: () => ref.invalidate(_decompressedProvider(source.id)),
      data: (name) => TextView(
        source: ViewSource.inner(InnerFile.entry(id: source.id, name: name)),
      ),
    );
  }
}

void _showObjects(BuildContext context, SqlDump d) {
  Widget block(BuildContext c, String title, List<SqlObject> list, String Function(SqlObject) extra) => Column(
    crossAxisAlignment: CrossAxisAlignment.start,
    children: [
      SectionHeader(title, trailing: Text('${list.length}', style: c.text.labelMedium)),
      for (final o in list)
        ListTile(
          dense: true,
          title: Text(o.name),
          subtitle: Text([if (o.schema.isNotEmpty) o.schema, extra(o)].where((s) => s.isNotEmpty).join(' · ')),
        ),
    ],
  );
  showModalBottomSheet<void>(
    context: context,
    showDragHandle: true,
    isScrollControlled: true,
    builder: (c) => SafeArea(
      child: ConstrainedBox(
        constraints: BoxConstraints(maxHeight: MediaQuery.sizeOf(c).height * 0.75),
        child: ListView(
          shrinkWrap: true,
          padding: const EdgeInsets.only(bottom: 16),
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(20, 0, 20, 0),
              child: Text('Views, triggers and routines', style: c.text.titleMedium),
            ),
            if (d.views.isNotEmpty) block(c, 'Views', d.views, (o) => o.kind == 'view' ? '' : o.kind),
            if (d.triggers.isNotEmpty) block(c, 'Triggers', d.triggers, (o) => o.on == null ? '' : 'on ${o.on}'),
            if (d.routines.isNotEmpty) block(c, 'Routines', d.routines, (o) => o.kind),
            if (d.events.isNotEmpty) block(c, 'Events', d.events, (o) => ''),
            if (d.sequences.isNotEmpty) block(c, 'Sequences', d.sequences, (o) => ''),
            if (d.other.isNotEmpty) block(c, 'Other objects', d.other, (o) => o.kind),
          ],
        ),
      ),
    ),
  );
}

/// One table of a dump: its columns, keys, indexes and what refers to it.
class SqlTableScreen extends ConsumerWidget {
  const SqlTableScreen({super.key, required this.id, required this.tableKey, this.dump});
  final int id;
  final String tableKey;

  /// The dump as read so far (handed over by the schema view); read again when opened by a link.
  final SqlDump? dump;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final d = dump;
    if (d == null) {
      final again = ref.watch(_dumpAgainProvider(id));
      return Scaffold(
        appBar: AppBar(title: Text(tableKey)),
        body: AsyncBody(
          value: again,
          onRetry: () => ref.invalidate(_dumpAgainProvider(id)),
          data: (d) => _detail(context, d),
        ),
      );
    }
    return Scaffold(
      appBar: AppBar(title: Text(_table(d)?.name ?? tableKey)),
      body: _detail(context, d),
    );
  }

  SqlTable? _table(SqlDump d) => d.tables.where((t) => t.key == tableKey).firstOrNull;

  Widget _detail(BuildContext context, SqlDump d) {
    final t = _table(d);
    if (t == null) {
      return const EmptyState(
        icon: Icons.table_chart_outlined,
        title: 'Not read that far yet',
        message: 'Open the dump again and let it read further.',
      );
    }
    final fkCols = {
      for (final k in t.foreignKeys)
        for (final c in k.columns) c: k.ref,
    };
    final idxCols = {for (final i in t.indexes) ...i.$2};
    final refs = [
      for (final o in d.tables)
        for (final k in o.foreignKeys)
          if (k.ref == t.key) (o, k),
    ];
    final facts = [
      if (t.rows > 0 || t.bytes > 0)
        '${plural(t.rows, 'row')}${t.uncounted ? ' (and some from INSERT … SELECT, not counted)' : ''}'
      else
        'no rows in the dump',
      if (t.bytes > 0)
        '${bytes(t.bytes)} of data in ${plural(t.statements, t.copy ? 'statement' : 'INSERT statement')}',
      if (t.copy) 'COPY data',
      ?t.engine,
      ?t.charset,
    ];
    final mono = context.text.bodySmall?.copyWith(fontFamily: 'monospace');
    void open(String key) => context.push(sqlTableLocation(id, key), extra: d);
    return Readable(
      child: ListView(
        padding: const EdgeInsets.only(bottom: 32),
        children: [
          Padding(
            padding: const EdgeInsets.fromLTRB(16, 16, 16, 4),
            child: Text.rich(
              TextSpan(
                children: [
                  TextSpan(text: t.name, style: context.text.titleLarge),
                  if (t.schema.isNotEmpty)
                    TextSpan(
                      text: '  ${t.schema}',
                      style: context.text.bodyMedium?.copyWith(color: context.bd.muted),
                    ),
                ],
              ),
            ),
          ),
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 16),
            child: Text(
              facts.join(' · '),
              style: context.text.bodySmall?.copyWith(color: context.colors.onSurfaceVariant),
            ),
          ),
          if (t.comment != null && t.comment!.isNotEmpty)
            Padding(padding: const EdgeInsets.fromLTRB(16, 8, 16, 0), child: Text(t.comment!)),
          if (t.note != null && t.note!.isNotEmpty) ViewerNote(t.note!),
          SectionHeader('Columns', trailing: Text('${t.columns.length}', style: context.text.labelMedium)),
          if (t.columns.isEmpty)
            Padding(
              padding: const EdgeInsets.symmetric(horizontal: 16),
              child: Text(
                t.defined ? 'No columns listed.' : 'The dump holds data for this table but no CREATE TABLE for it.',
                style: context.text.bodyMedium?.copyWith(color: context.bd.muted),
              ),
            ),
          for (final c in t.columns)
            ListTile(
              dense: true,
              title: Wrap(
                spacing: 6,
                runSpacing: 4,
                crossAxisAlignment: WrapCrossAlignment.center,
                children: [
                  Text(c.name, style: const TextStyle(fontWeight: FontWeight.w700)),
                  if (c.pk) Pill('PK', color: context.bd.warn),
                  if (fkCols.containsKey(c.name)) Pill('FK', color: context.bd.info),
                  if (!c.pk && idxCols.contains(c.name)) const Pill('IDX'),
                  if (c.auto) Pill('auto', color: context.bd.good),
                ],
              ),
              subtitle: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(
                    [
                      c.type,
                      c.nullable ? 'null' : 'not null',
                      if (c.dflt != null) 'default ${c.dflt}',
                      ...c.extra,
                    ].join(' · '),
                    style: mono,
                  ),
                  if (c.comment != null && c.comment!.isNotEmpty) Text(c.comment!),
                ],
              ),
            ),
          if (t.primaryKey.isNotEmpty) ...[
            const SectionHeader('Primary key'),
            Padding(
              padding: const EdgeInsets.symmetric(horizontal: 16),
              child: Text(t.primaryKey.join(', '), style: mono),
            ),
          ],
          if (t.indexes.isNotEmpty) ...[
            SectionHeader('Indexes', trailing: Text('${t.indexes.length}', style: context.text.labelMedium)),
            for (final (name, cols, kind) in t.indexes)
              ListTile(
                dense: true,
                leading: Pill(switch (kind) {
                  'fulltext' => 'full-text',
                  final k => k,
                }),
                title: Text(name ?? '(unnamed)'),
                subtitle: Text('(${cols.join(', ')})', style: mono),
              ),
          ],
          if (t.foreignKeys.isNotEmpty) ...[
            const SectionHeader('Foreign keys'),
            for (final k in t.foreignKeys)
              ListTile(
                dense: true,
                title: Text('${k.columns.join(', ')} → ${k.ref}', style: mono),
                subtitle: Text(
                  [
                    if (k.refColumns.isNotEmpty) '(${k.refColumns.join(', ')})',
                    if (k.onDelete != null) 'on delete ${k.onDelete}',
                    if (k.onUpdate != null) 'on update ${k.onUpdate}',
                    ?k.name,
                  ].join(' · '),
                ),
                trailing: d.tables.any((x) => x.key == k.ref) ? const Icon(Icons.chevron_right) : null,
                onTap: d.tables.any((x) => x.key == k.ref) ? () => open(k.ref) : null,
              ),
          ],
          if (refs.isNotEmpty) ...[
            const SectionHeader('Referenced by'),
            for (final (o, k) in refs)
              ListTile(
                dense: true,
                title: Text(o.key),
                subtitle: Text('(${k.columns.join(', ')})', style: mono),
                trailing: const Icon(Icons.chevron_right),
                onTap: () => open(o.key),
              ),
          ],
          if (t.checks > 0)
            Padding(
              padding: const EdgeInsets.fromLTRB(16, 12, 16, 0),
              child: Text('${plural(t.checks, 'CHECK constraint')}.', style: context.text.bodySmall),
            ),
          if (t.sql != null)
            Padding(
              padding: const EdgeInsets.fromLTRB(8, 16, 8, 0),
              child: Align(
                alignment: Alignment.centerLeft,
                child: TextButton.icon(
                  onPressed: () => showSql(context, t.name, t.sql!),
                  icon: const Icon(Icons.code),
                  label: const Text('The CREATE statement'),
                ),
              ),
            ),
        ],
      ),
    );
  }
}

/// A dump read again from its start, as far as one pass goes (for a table opened by a link).
final _dumpAgainProvider = FutureProvider.autoDispose.family<SqlDump, int>(
  (ref, id) => fetchParsed(requireApi(ref), 'sqldump', {'id': id}, SqlDump.fromJson),
);
