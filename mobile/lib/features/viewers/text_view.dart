import 'dart:convert';
import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/format.dart';
import '../../core/kinds.dart';
import '../../core/widgets.dart';
import '../../providers.dart';
import '../../theme.dart';
import 'common.dart';
import 'grid_table.dart';
import 'highlight.dart';

/// The most of a text file read for the viewer.
const textMax = 2 << 20;

/// Whether long lines wrap (a choice kept while the app runs, for every text file).
class WrapLines extends Notifier<bool> {
  @override
  bool build() => true;
  void toggle() => state = !state;
}

final wrapLinesProvider = NotifierProvider<WrapLines, bool>(WrapLines.new);

/// A text file as read: its rows with colours, or (CSV and TSV) its cells.
class LoadedText {
  LoadedText._(this.text, this.table, this.shown, this.total, this.lang);

  final Highlighted? text;
  final List<List<String>>? table;

  /// Bytes read, of [total] (null when not known).
  final int shown;
  final int? total;
  final String? lang;

  bool get truncated => total != null ? total! > shown : shown >= textMax;
  bool get capped => table != null && table!.length > csvMaxRows;

  static LoadedText decode(Uint8List b, String name, int? total) {
    final src = utf8.decode(b, allowMalformed: true);
    final x = extensionOf(name);
    if (x == 'csv' || x == 'tsv') {
      return LoadedText._(null, parseCsv(src, x == 'tsv' ? '\t' : null), b.length, total, null);
    }
    final lang = languageOf(name);
    return LoadedText._(Highlighted.of(src, lang), null, b.length, total, lang);
  }
}

const csvMaxRows = 5000;

/// CSV or TSV cells (the separator guessed from the first line when not given), up to
/// [csvMaxRows] rows and one more to tell there are more, as the web UI reads them.
List<List<String>> parseCsv(String src, String? sep) {
  final nl = src.indexOf('\n');
  final first = nl < 0 ? src : src.substring(0, nl);
  final s =
      sep ??
      [
        for (final c in [',', ';', '\t']) (c, first.split(c).length),
      ].reduce((a, b) => b.$2 > a.$2 ? b : a).$1;
  final sepCode = s.codeUnitAt(0);
  final rows = <List<String>>[];
  var row = <String>[];
  final cell = StringBuffer();
  var quoted = false;
  for (var i = 0; i < src.length && rows.length <= csvMaxRows; i++) {
    final c = src.codeUnitAt(i);
    if (quoted) {
      if (c == 34 && i + 1 < src.length && src.codeUnitAt(i + 1) == 34) {
        cell.writeCharCode(34);
        i++;
      } else if (c == 34) {
        quoted = false;
      } else {
        cell.writeCharCode(c);
      }
    } else if (c == 34) {
      quoted = true;
    } else if (c == sepCode) {
      row.add(cell.toString());
      cell.clear();
    } else if (c == 10) {
      var t = cell.toString();
      if (t.endsWith('\r')) t = t.substring(0, t.length - 1);
      row.add(t);
      rows.add(row);
      row = <String>[];
      cell.clear();
    } else {
      cell.writeCharCode(c);
    }
  }
  if (cell.isNotEmpty || row.isNotEmpty) {
    row.add(cell.toString());
    rows.add(row);
  }
  return rows;
}

// a plain function, so the work sent to another isolate carries only the bytes
Future<LoadedText> _decodeOff(Uint8List b, String name, int? total) =>
    offThread(b.length, () => LoadedText.decode(b, name, total));

final loadedTextProvider = FutureProvider.autoDispose.family<LoadedText, ViewSource>((ref, src) async {
  final b = await src.bytes(requireApi(ref), textMax);
  return _decodeOff(b, src.name, src.size);
});

/// A text or code file: the first 2 MB, with syntax colours, line numbers and a wrap toggle; CSV
/// and TSV as a table.
class TextView extends ConsumerWidget {
  const TextView({super.key, required this.source});
  final ViewSource source;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final loaded = ref.watch(loadedTextProvider(source));
    return AsyncBody(
      value: loaded,
      onRetry: () => ref.invalidate(loadedTextProvider(source)),
      data: (t) => t.table != null ? _CsvTable(t: t) : _Code(t: t),
    );
  }
}

String _shownNote(LoadedText t) => t.total == null
    ? 'Showing the first ${bytes(t.shown)}.'
    : 'Showing the first ${bytes(t.shown)} of ${bytes(t.total)}.';

class _CsvTable extends StatelessWidget {
  const _CsvTable({required this.t});
  final LoadedText t;

  @override
  Widget build(BuildContext context) {
    final all = t.table!;
    final head = all.isEmpty ? <String>[] : all.first;
    final body = all.length <= 1 ? <List<String>>[] : all.sublist(1, math.min(all.length, csvMaxRows + 1));
    return Column(
      children: [
        ViewerBar(meta: '${plural(body.length, 'row')} × ${plural(head.length, 'column')}'),
        if (t.truncated || t.capped) ViewerNote('Showing the first ${count(body.length)} rows.'),
        const SizedBox(height: 4),
        Expanded(
          child: GridTable(headers: head, rows: body, rowLabel: (r) => '${r + 1}'),
        ),
      ],
    );
  }
}

class _Code extends ConsumerWidget {
  const _Code({required this.t});
  final LoadedText t;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final h = t.text!;
    final wrap = ref.watch(wrapLinesProvider);
    final lang = languageName(t.lang);
    return Column(
      children: [
        ViewerBar(
          meta: [plural(h.lines, 'line'), ?lang].join(' · '),
          children: [
            IconButton(
              tooltip: wrap ? 'Don\'t wrap long lines' : 'Wrap long lines',
              isSelected: wrap,
              icon: const Icon(Icons.wrap_text),
              onPressed: () => ref.read(wrapLinesProvider.notifier).toggle(),
            ),
            IconButton(
              tooltip: 'Copy the text',
              icon: const Icon(Icons.copy_all_outlined),
              onPressed: () async {
                await Clipboard.setData(ClipboardData(text: h.text));
                if (context.mounted) showSnack(context, t.truncated ? 'Copied what\'s shown' : 'Copied');
              },
            ),
          ],
        ),
        if (t.truncated) ViewerNote(_shownNote(t)),
        Expanded(
          child: h.text.isEmpty
              ? const EmptyState(icon: Icons.notes, title: 'Empty file')
              : CodeLines(text: h, wrap: wrap),
        ),
      ],
    );
  }
}

/// Rows of text with line numbers, built as they scroll into view.
class CodeLines extends StatelessWidget {
  const CodeLines({
    super.key,
    required this.text,
    required this.wrap,
    this.padding = const EdgeInsets.only(top: 6, bottom: 24),
  });
  final Highlighted text;
  final bool wrap;
  final EdgeInsets padding;

  static TextStyle style(BuildContext context) =>
      TextStyle(fontFamily: 'monospace', fontSize: 12.5, height: 1.45, color: context.colors.onSurface);

  @override
  Widget build(BuildContext context) {
    final mono = style(context);
    final dark = Theme.of(context).brightness == Brightness.dark;
    final digits = math.max(2, '${text.lines}'.length);
    final tp = TextPainter(
      text: TextSpan(text: 'M' * 10, style: mono),
      textDirection: TextDirection.ltr,
      textScaler: MediaQuery.textScalerOf(context),
    )..layout();
    final charW = tp.width / 10;
    final lineH = tp.height;
    tp.dispose();
    final gutter = digits * charW + 20;
    final numStyle = mono.copyWith(color: context.bd.muted);

    Widget row(BuildContext c, int r) {
      final n = text.rowLine[r];
      return Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Container(
            width: gutter,
            padding: const EdgeInsets.only(right: 10),
            alignment: Alignment.topRight,
            child: Text(n == 0 ? '' : '$n', style: numStyle, maxLines: 1),
          ),
          if (wrap)
            Expanded(
              child: Text.rich(text.row(r, dark: dark), style: mono, softWrap: true),
            )
          else
            Text.rich(text.row(r, dark: dark), style: mono, softWrap: false, maxLines: 1),
        ],
      );
    }

    if (wrap) {
      return SelectionArea(
        child: Scrollbar(
          child: ListView.builder(padding: padding.copyWith(right: 12), itemCount: text.rows, itemBuilder: row),
        ),
      );
    }
    var longest = 0;
    for (var r = 0; r < text.rows; r++) {
      longest = math.max(longest, text.rowEnd[r] - text.rowStart[r]);
    }
    return LayoutBuilder(
      builder: (context, box) {
        final width = math.max(box.maxWidth, gutter + longest * charW * 1.02 + 24);
        return SelectionArea(
          child: Scrollbar(
            child: SingleChildScrollView(
              scrollDirection: Axis.horizontal,
              child: SizedBox(
                width: width,
                child: ListView.builder(padding: padding, itemExtent: lineH, itemCount: text.rows, itemBuilder: row),
              ),
            ),
          ),
        );
      },
    );
  }
}
