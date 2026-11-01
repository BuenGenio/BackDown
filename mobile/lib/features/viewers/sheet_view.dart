import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/format.dart';
import '../../providers.dart';
import '../../theme.dart';
import 'common.dart';
import 'fallback.dart';
import 'grid_table.dart';
import 'models.dart';

typedef SheetAt = ({int id, String? sheet, int offset});

const _rowsPerPage = 500;

final sheetProvider = FutureProvider.autoDispose.family<SheetPage, SheetAt>(
  (ref, at) => fetchParsed(requireApi(ref), 'sheet', {
    'id': at.id,
    'sheet': at.sheet,
    'offset': at.offset,
    'limit': _rowsPerPage,
  }, SheetPage.fromJson),
);

/// A spreadsheet (xlsx, xls, ods…), read by the service a page of 500 rows at a time: its sheets
/// as tabs, column letters and row numbers as in the spreadsheet.
class SheetView extends ConsumerStatefulWidget {
  const SheetView({super.key, required this.source});
  final ViewSource source;

  @override
  ConsumerState<SheetView> createState() => _SheetViewState();
}

class _SheetViewState extends ConsumerState<SheetView> {
  String? _sheet;
  int _offset = 0;
  SheetPage? _last;

  SheetAt get _at => (id: widget.source.id, sheet: _sheet, offset: _offset);

  @override
  Widget build(BuildContext context) {
    final page = ref.watch(sheetProvider(_at));
    // keep the last page on screen while the next one loads
    final d = page.value ?? _last;
    if (page.hasValue) _last = page.value;
    if (d == null) {
      if (page.hasError) return viewerError(widget.source, page.error!, () => ref.invalidate(sheetProvider(_at)));
      return const Center(child: CircularProgressIndicator());
    }
    final cols = d.width < 1 ? 1 : d.width;
    final n = d.rows.length;
    final total = d.totalRows;
    return Column(
      children: [
        ViewerBar(
          meta: [
            plural(d.sheets.length, 'sheet'),
            if (total != null) '${plural(total, 'row')} × ${plural(d.totalCols ?? cols, 'column')}',
          ].join(' · '),
          children: [
            Pager(
              label: n == 0
                  ? 'no rows here'
                  : '${count(d.offset + 1)}–${count(d.offset + n)}${total != null ? ' of ${count(total)}' : ''}',
              onPrevious: d.offset > 0
                  ? () => setState(() => _offset = (_offset - _rowsPerPage).clamp(0, 1 << 40))
                  : null,
              onNext: d.more ? () => setState(() => _offset += _rowsPerPage) : null,
            ),
          ],
        ),
        if (d.sheets.length > 1)
          SizedBox(
            height: 48,
            child: ListView.separated(
              scrollDirection: Axis.horizontal,
              padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
              itemCount: d.sheets.length,
              separatorBuilder: (c, i) => const SizedBox(width: 6),
              itemBuilder: (c, i) {
                final (name, hidden) = d.sheets[i];
                return ChoiceChip(
                  label: Text(name, style: hidden ? const TextStyle(fontStyle: FontStyle.italic) : null),
                  avatar: hidden ? Icon(Icons.visibility_off_outlined, size: 16, color: context.bd.muted) : null,
                  tooltip: hidden ? 'Hidden sheet' : null,
                  selected: name == d.sheet,
                  onSelected: (_) => setState(() {
                    _sheet = name;
                    _offset = 0;
                  }),
                );
              },
            ),
          ),
        if (d.cut) ViewerNote('Showing the first ${plural(cols, 'column')}.'),
        if (d.early) const ViewerNote('A very large sheet: stopped reading early.'),
        if (page.isLoading) const LinearProgressIndicator(minHeight: 2) else const SizedBox(height: 2),
        Expanded(
          child: GridTable(
            headers: [for (var c = 0; c < cols; c++) columnName(c)],
            rows: d.rows,
            rowLabel: (r) => '${d.offset + r + 1}',
            emptyText: 'This sheet has no rows here.',
          ),
        ),
      ],
    );
  }
}
