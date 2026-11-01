import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../../core/format.dart';
import '../../core/kinds.dart';
import '../../core/widgets.dart';
import '../../providers.dart';
import '../../theme.dart';
import 'common.dart';
import 'grid_table.dart';
import 'highlight.dart';
import 'models.dart';
import 'routes.dart';
import 'text_view.dart';

typedef TableAt = ({int id, String table, int offset});

const _pageRows = 200;

final sqliteRowsProvider = FutureProvider.autoDispose.family<SqliteInfo, TableAt>(
  (ref, at) => fetchParsed(requireApi(ref), 'sqlite', {
    'id': at.id,
    'table': at.table,
    'offset': at.offset,
  }, SqliteInfo.fromJson),
);

/// A SQLite database, opened read-only by the service: its tables and views with their rows; a
/// table opens beside the list on a wide screen, or on its own.
class SqliteView extends StatefulWidget {
  const SqliteView({super.key, required this.id, required this.info});
  final int id;
  final SqliteInfo info;

  @override
  State<SqliteView> createState() => _SqliteViewState();
}

class _SqliteViewState extends State<SqliteView> {
  String? _open;

  @override
  Widget build(BuildContext context) {
    final tables = widget.info.tables;
    if (tables.isEmpty) {
      return const EmptyState(
        icon: Icons.dns_outlined,
        title: 'Empty database',
        message: 'This SQLite file has no tables.',
      );
    }
    return Column(
      children: [
        ViewerBar(meta: 'SQLite · ${plural(tables.length, 'table')} · opened read-only'),
        Expanded(
          child: LayoutBuilder(
            builder: (context, box) {
              final wide = box.maxWidth >= 900;
              final open = _open ?? tables.first.name;
              final list = ListView.builder(
                itemCount: tables.length,
                itemBuilder: (c, i) {
                  final t = tables[i];
                  return ListTile(
                    selected: wide && t.name == open,
                    selectedTileColor: context.bd.selection,
                    leading: Icon(
                      t.kind == 'view' ? Icons.visibility_outlined : Icons.table_chart_outlined,
                      color: FileKind.db.color,
                    ),
                    title: Text(t.name, maxLines: 1, overflow: TextOverflow.ellipsis),
                    subtitle: Text(
                      [if (t.kind != 'table') t.kind, if (t.rows != null) plural(t.rows!, 'row')].join(' · '),
                    ),
                    trailing: wide ? null : const Icon(Icons.chevron_right),
                    onTap: () =>
                        wide ? setState(() => _open = t.name) : context.push(sqliteTableLocation(widget.id, t.name)),
                  );
                },
              );
              if (!wide) return Readable(child: list);
              return Row(
                children: [
                  SizedBox(width: 300, child: list),
                  const VerticalDivider(width: 1),
                  Expanded(
                    child: SqliteTable(key: ValueKey(open), id: widget.id, table: open),
                  ),
                ],
              );
            },
          ),
        ),
      ],
    );
  }
}

/// One table's rows, 200 at a time, and the statement that made it.
class SqliteTable extends ConsumerStatefulWidget {
  const SqliteTable({super.key, required this.id, required this.table, this.named = true});
  final int id;
  final String table;

  /// Say the table's name in its bar (beside the list; a screen of its own has it as its title).
  final bool named;

  @override
  ConsumerState<SqliteTable> createState() => _SqliteTableState();
}

class _SqliteTableState extends ConsumerState<SqliteTable> {
  int _offset = 0;
  SqliteInfo? _last;

  TableAt get _at => (id: widget.id, table: widget.table, offset: _offset);

  @override
  Widget build(BuildContext context) {
    final page = ref.watch(sqliteRowsProvider(_at));
    final d = page.value ?? _last;
    if (page.hasValue) _last = page.value;
    if (d == null) {
      return AsyncBody(
        value: page,
        onRetry: () => ref.invalidate(sqliteRowsProvider(_at)),
        data: (_) => const SizedBox(),
      );
    }
    final meta = d.tables.where((t) => t.name == widget.table).firstOrNull;
    final rows = d.data?.rows ?? const <List<Object?>>[];
    final total = meta?.rows;
    return Column(
      children: [
        ViewerBar(
          meta: [if (widget.named) widget.table, plural(d.data?.columns.length ?? 0, 'column')].join(' · '),
          children: [
            Pager(
              label: rows.isEmpty
                  ? 'no rows here'
                  : '${count(_offset + 1)}–${count(_offset + rows.length)}${total != null ? ' of ${count(total)}' : ''}',
              onPrevious: _offset > 0 ? () => setState(() => _offset = (_offset - _pageRows).clamp(0, 1 << 40)) : null,
              onNext: rows.length >= _pageRows ? () => setState(() => _offset += _pageRows) : null,
            ),
            if (meta?.sql != null)
              IconButton(
                tooltip: 'The statement that made it',
                icon: const Icon(Icons.code),
                onPressed: () => showSql(context, widget.table, meta!.sql!),
              ),
          ],
        ),
        if (page.isLoading) const LinearProgressIndicator(minHeight: 2) else const SizedBox(height: 2),
        Expanded(
          child: GridTable(
            headers: d.data?.columns ?? const [],
            rows: rows,
            nulls: true,
            rowLabel: (r) => '${_offset + r + 1}',
            emptyText: 'No rows in this table.',
          ),
        ),
      ],
    );
  }
}

/// A table as a screen of its own (from the list on a phone).
class SqliteTableScreen extends StatelessWidget {
  const SqliteTableScreen({super.key, required this.id, required this.table});
  final int id;
  final String table;

  @override
  Widget build(BuildContext context) => Scaffold(
    appBar: AppBar(title: Text(table)),
    body: SqliteTable(id: id, table: table, named: false),
  );
}

/// A CREATE statement, highlighted, in a sheet.
Future<void> showSql(BuildContext context, String title, String sql) => showModalBottomSheet<void>(
  context: context,
  showDragHandle: true,
  isScrollControlled: true,
  builder: (c) => SafeArea(
    child: SizedBox(
      height: MediaQuery.sizeOf(c).height * 0.6,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Padding(
            padding: const EdgeInsets.fromLTRB(20, 0, 20, 8),
            child: Text(title, style: c.text.titleMedium),
          ),
          const Divider(),
          Expanded(child: CodeLines(text: Highlighted.of(sql, 'sql'), wrap: true)),
        ],
      ),
    ),
  ),
);
