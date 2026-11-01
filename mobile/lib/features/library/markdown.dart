import 'package:flutter/material.dart';

import '../../theme.dart';

/// The report's Markdown (summary.rs), shown as a phone shows a document: headings, paragraphs,
/// rules, and each table row as a small block of its own (a table as wide as a desktop's doesn't
/// fit), with **bold**, `code`, _emphasis_ and <br>. Not a general Markdown renderer.
class ReportView extends StatelessWidget {
  const ReportView({super.key, required this.markdown, this.padding = const EdgeInsets.fromLTRB(16, 8, 16, 32)});

  final String markdown;
  final EdgeInsetsGeometry padding;

  @override
  Widget build(BuildContext context) {
    final blocks = parseReport(markdown);
    return ListView.builder(padding: padding, itemCount: blocks.length, itemBuilder: (c, i) => _block(c, blocks[i]));
  }

  Widget _block(BuildContext context, MdBlock b) {
    final t = context.text;
    switch (b) {
      case MdHeading(:final level, :final text):
        final style = switch (level) {
          1 => t.headlineSmall?.copyWith(fontWeight: FontWeight.w800),
          2 => t.titleLarge?.copyWith(fontWeight: FontWeight.w700),
          _ => t.titleMedium?.copyWith(fontWeight: FontWeight.w700),
        };
        return Padding(
          padding: EdgeInsets.only(top: level == 1 ? 4 : 20, bottom: 6),
          child: Text.rich(inline(context, text), style: style),
        );
      case MdRule():
        return const Padding(padding: EdgeInsets.symmetric(vertical: 12), child: Divider());
      case MdParagraph(:final text):
        return Padding(
          padding: const EdgeInsets.only(bottom: 10),
          child: Text.rich(inline(context, text), style: t.bodyMedium),
        );
      case MdTable(:final header, :final rows):
        return Padding(
          padding: const EdgeInsets.only(bottom: 10),
          child: Column(
            children: [
              for (final r in rows)
                Container(
                  width: double.infinity,
                  margin: const EdgeInsets.only(bottom: 6),
                  padding: const EdgeInsets.all(10),
                  decoration: BoxDecoration(
                    color: context.colors.surfaceContainerLowest,
                    borderRadius: BorderRadius.circular(10),
                    border: Border.all(color: context.colors.outlineVariant),
                  ),
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      if (r.isNotEmpty)
                        Text.rich(inline(context, r.first), style: t.titleSmall?.copyWith(fontWeight: FontWeight.w700)),
                      for (var i = 1; i < r.length; i++)
                        if (r[i].trim().isNotEmpty)
                          Padding(
                            padding: const EdgeInsets.only(top: 3),
                            child: Row(
                              crossAxisAlignment: CrossAxisAlignment.start,
                              children: [
                                SizedBox(
                                  width: 96,
                                  child: Text(
                                    i < header.length ? header[i] : '',
                                    style: t.bodySmall?.copyWith(color: context.colors.onSurfaceVariant),
                                  ),
                                ),
                                Expanded(child: Text.rich(inline(context, r[i]), style: t.bodySmall)),
                              ],
                            ),
                          ),
                    ],
                  ),
                ),
            ],
          ),
        );
    }
  }
}

sealed class MdBlock {
  const MdBlock();
}

class MdHeading extends MdBlock {
  const MdHeading(this.level, this.text);
  final int level;
  final String text;
}

class MdParagraph extends MdBlock {
  const MdParagraph(this.text);
  final String text;
}

class MdRule extends MdBlock {
  const MdRule();
}

class MdTable extends MdBlock {
  const MdTable(this.header, this.rows);
  final List<String> header;
  final List<List<String>> rows;
}

/// A table row's cells (`\|` is a bar inside a cell).
List<String> _cells(String line) {
  var s = line.trim();
  if (s.startsWith('|')) s = s.substring(1);
  if (s.endsWith('|') && !s.endsWith(r'\|')) s = s.substring(0, s.length - 1);
  final out = <String>[];
  final cur = StringBuffer();
  for (var i = 0; i < s.length; i++) {
    if (s[i] == r'\' && i + 1 < s.length && s[i + 1] == '|') {
      cur.write('|');
      i++;
    } else if (s[i] == '|') {
      out.add(cur.toString().trim());
      cur.clear();
    } else {
      cur.write(s[i]);
    }
  }
  out.add(cur.toString().trim());
  return out;
}

List<MdBlock> parseReport(String md) {
  final lines = md.replaceAll('\r\n', '\n').split('\n');
  final out = <MdBlock>[];
  final para = <String>[];
  void flush() {
    if (para.isNotEmpty) out.add(MdParagraph(para.join(' ')));
    para.clear();
  }

  for (var i = 0; i < lines.length; i++) {
    final l = lines[i].trimRight();
    final h = RegExp(r'^(#{1,6})\s+(.*)$').firstMatch(l);
    if (h != null) {
      flush();
      out.add(MdHeading(h[1]!.length, h[2]!));
    } else if (RegExp(r'^\s*(-{3,}|\*{3,})\s*$').hasMatch(l)) {
      flush();
      out.add(const MdRule());
    } else if (l.trimLeft().startsWith('|')) {
      flush();
      final header = _cells(l);
      final rows = <List<String>>[];
      var j = i + 1;
      if (j < lines.length && RegExp(r'^\s*\|?[\s:|-]+\|?\s*$').hasMatch(lines[j])) j++;
      while (j < lines.length && lines[j].trimLeft().startsWith('|')) {
        rows.add(_cells(lines[j]));
        j++;
      }
      out.add(MdTable(header, rows));
      i = j - 1;
    } else if (l.trim().isEmpty) {
      flush();
    } else {
      para.add(l.trim());
    }
  }
  flush();
  return out;
}

final _inline = RegExp(r'\*\*(.+?)\*\*|`([^`]+)`|<br\s*/?>|(?<![\w])_(.+?)_(?![\w])');

/// **bold**, `code`, _emphasis_ and <br> in a line.
TextSpan inline(BuildContext context, String text) {
  final spans = <InlineSpan>[];
  var at = 0;
  for (final m in _inline.allMatches(text)) {
    if (m.start > at) spans.add(TextSpan(text: text.substring(at, m.start)));
    if (m[1] != null) {
      spans.add(
        TextSpan(
          text: m[1],
          style: const TextStyle(fontWeight: FontWeight.w700),
        ),
      );
    } else if (m[2] != null) {
      spans.add(
        TextSpan(
          text: m[2]!.replaceAllMapped(RegExp(r'[\\/]'), (x) => '${x[0]}​'),
          style: TextStyle(
            fontFamily: 'monospace',
            fontSize: (context.text.bodySmall?.fontSize ?? 12) * 0.95,
            backgroundColor: context.colors.surfaceContainerHigh,
          ),
        ),
      );
    } else if (m[3] != null) {
      spans.add(
        TextSpan(
          text: m[3],
          style: const TextStyle(fontStyle: FontStyle.italic),
        ),
      );
    } else {
      spans.add(const TextSpan(text: '\n'));
    }
    at = m.end;
  }
  if (at < text.length) spans.add(TextSpan(text: text.substring(at)));
  return TextSpan(children: spans);
}
