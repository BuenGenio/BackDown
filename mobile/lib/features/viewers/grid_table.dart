import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../../core/widgets.dart';
import '../../theme.dart';

/// A table of cells that scrolls both ways and builds only the rows on screen: CSV files,
/// spreadsheet pages and database tables. Columns are as wide as their content (sampled), within
/// limits; a tap on a cell shows all of it.
class GridTable extends StatelessWidget {
  const GridTable({
    super.key,
    required this.headers,
    required this.rows,
    this.rowLabel,
    this.emptyText = 'No rows',
    this.nulls = false,
  });

  final List<String> headers;
  final List<List<Object?>> rows;

  /// A row's number in the gutter (a spreadsheet's or a page's row), or no gutter.
  final String Function(int row)? rowLabel;
  final String emptyText;

  /// Show missing values as NULL (a database's), rather than as empty cells (a sheet's).
  final bool nulls;

  static const _rowHeight = 34.0;
  static const _charWidth = 7.4;

  static String text(Object? v) => switch (v) {
    null => '',
    bool b => b ? 'TRUE' : 'FALSE',
    double d when d == d.roundToDouble() && d.abs() < 1e15 => d.toInt().toString(),
    double d => double.parse(d.toStringAsPrecision(12)).toString(),
    _ => v.toString(),
  };

  List<double> _widths(int cols) {
    final w = List<double>.filled(cols, 0);
    for (var c = 0; c < cols; c++) {
      var longest = c < headers.length ? headers[c].length : 1;
      for (var r = 0; r < rows.length && r < 200; r++) {
        final row = rows[r];
        if (c < row.length) longest = math.max(longest, text(row[c]).length);
      }
      w[c] = (longest * _charWidth + 24).clamp(56, 280).toDouble();
    }
    return w;
  }

  @override
  Widget build(BuildContext context) {
    final cols = rows.fold<int>(headers.length, (a, r) => math.max(a, r.length));
    final widths = _widths(cols);
    final gutter = rowLabel == null
        ? 0.0
        : math.max(40.0, (rowLabel!(math.max(0, rows.length - 1)).length * _charWidth + 20));
    final total = gutter + widths.fold<double>(0, (a, b) => a + b);
    final line = context.colors.outlineVariant;
    final head = context.text.labelMedium?.copyWith(fontWeight: FontWeight.w700);
    final body = context.text.bodySmall?.copyWith(color: context.colors.onSurface);
    final muted = context.text.bodySmall?.copyWith(color: context.bd.muted);

    Widget cell(String s, double w, {TextStyle? style, bool right = false, Color? bg, VoidCallback? onTap}) {
      final box = Container(
        width: w,
        height: _rowHeight,
        alignment: right ? Alignment.centerRight : Alignment.centerLeft,
        padding: const EdgeInsets.symmetric(horizontal: 8),
        decoration: BoxDecoration(
          color: bg,
          border: Border(
            right: BorderSide(color: line),
            bottom: BorderSide(color: line),
          ),
        ),
        child: Text(s, maxLines: 1, overflow: TextOverflow.ellipsis, style: style),
      );
      return onTap == null ? box : InkWell(onTap: onTap, child: box);
    }

    final header = Container(
      color: context.colors.surfaceContainer,
      child: Row(
        children: [
          if (rowLabel != null) cell('', gutter, bg: context.colors.surfaceContainer),
          for (var c = 0; c < cols; c++)
            cell(c < headers.length ? headers[c] : '', widths[c], style: head, bg: context.colors.surfaceContainer),
        ],
      ),
    );

    return LayoutBuilder(
      builder: (context, box) {
        final width = math.max(total, box.maxWidth);
        return Scrollbar(
          child: SingleChildScrollView(
            scrollDirection: Axis.horizontal,
            child: SizedBox(
              width: width,
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  SizedBox(width: width, child: header),
                  Expanded(
                    child: rows.isEmpty
                        ? Padding(
                            padding: const EdgeInsets.all(16),
                            child: Text(emptyText, style: muted),
                          )
                        : ListView.builder(
                            itemExtent: _rowHeight,
                            itemCount: rows.length,
                            itemBuilder: (c, r) {
                              final row = rows[r];
                              return Row(
                                children: [
                                  if (rowLabel != null)
                                    cell(
                                      rowLabel!(r),
                                      gutter,
                                      style: muted,
                                      right: true,
                                      bg: context.colors.surfaceContainerLow,
                                    ),
                                  for (var i = 0; i < cols; i++)
                                    () {
                                      final v = i < row.length ? row[i] : null;
                                      final s = text(v);
                                      final isNull = nulls && v == null && i < row.length;
                                      return cell(
                                        isNull ? 'NULL' : s,
                                        widths[i],
                                        style: isNull ? muted?.copyWith(fontStyle: FontStyle.italic) : body,
                                        right: v is num,
                                        onTap: s.isEmpty
                                            ? null
                                            : () => _showCell(context, i < headers.length ? headers[i] : '', s),
                                      );
                                    }(),
                                ],
                              );
                            },
                          ),
                  ),
                ],
              ),
            ),
          ),
        );
      },
    );
  }

  static Future<void> _showCell(BuildContext context, String column, String value) => showModalBottomSheet<void>(
    context: context,
    showDragHandle: true,
    isScrollControlled: true,
    builder: (c) => SafeArea(
      child: ConstrainedBox(
        constraints: BoxConstraints(maxHeight: MediaQuery.sizeOf(c).height * 0.7),
        child: ListView(
          shrinkWrap: true,
          padding: const EdgeInsets.fromLTRB(20, 0, 20, 20),
          children: [
            Row(
              children: [
                Expanded(child: Text(column.isEmpty ? 'Cell' : column, style: c.text.titleMedium)),
                IconButton(
                  tooltip: 'Copy',
                  icon: const Icon(Icons.copy),
                  onPressed: () async {
                    await Clipboard.setData(ClipboardData(text: value));
                    if (c.mounted) showSnack(c, 'Copied');
                  },
                ),
              ],
            ),
            const SizedBox(height: 8),
            SelectableText(value, style: c.text.bodyMedium),
          ],
        ),
      ),
    ),
  );
}

/// Spreadsheet column letters: 0 → A, 25 → Z, 26 → AA.
String columnName(int i) {
  var s = '';
  for (var n = i + 1; n > 0; n = (n - 1) ~/ 26) {
    s = String.fromCharCode(65 + (n - 1) % 26) + s;
  }
  return s;
}

/// Rows "1–200 of 1,234" and buttons to the page before and after.
class Pager extends StatelessWidget {
  const Pager({super.key, required this.label, this.onPrevious, this.onNext});
  final String label;
  final VoidCallback? onPrevious, onNext;

  @override
  Widget build(BuildContext context) => Row(
    mainAxisSize: MainAxisSize.min,
    children: [
      Text(label, style: context.text.bodySmall?.copyWith(color: context.colors.onSurfaceVariant)),
      IconButton(
        tooltip: 'Previous',
        visualDensity: VisualDensity.compact,
        onPressed: onPrevious,
        icon: const Icon(Icons.chevron_left),
      ),
      IconButton(
        tooltip: 'Next',
        visualDensity: VisualDensity.compact,
        onPressed: onNext,
        icon: const Icon(Icons.chevron_right),
      ),
    ],
  );
}
