import 'dart:convert';
import 'dart:typed_data';

import 'package:backdown_mobile/features/viewers/choose.dart';
import 'package:backdown_mobile/features/viewers/common.dart';
import 'package:backdown_mobile/features/viewers/grid_table.dart';
import 'package:backdown_mobile/features/viewers/highlight.dart';
import 'package:backdown_mobile/features/viewers/mail_view.dart';
import 'package:backdown_mobile/features/viewers/models.dart';
import 'package:backdown_mobile/features/viewers/text_view.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  test('CSV: quotes, doubled quotes, CRLF and a guessed separator', () {
    final rows = parseCsv('a;b;c\r\n1;"x; y";"say ""hi"""\r\n2;;\n', null);
    expect(rows, [
      ['a', 'b', 'c'],
      ['1', 'x; y', 'say "hi"'],
      ['2', '', ''],
    ]);
    expect(parseCsv('a\tb\n1\t2', '\t').last, ['1', '2']);
  });

  test('names sort as people read them', () {
    final names = ['IMG_10.JPG', 'img_9.jpg', 'IMG_100.JPG', 'a'];
    names.sort(naturalCompare);
    expect(names, ['a', 'img_9.jpg', 'IMG_10.JPG', 'IMG_100.JPG']);
  });

  test('text is cut into numbered rows, long lines into several', () {
    final long = 'x' * (Highlighted.rowMax * 2 + 5);
    final h = Highlighted.of('one\r\ntwo\n$long\n', null);
    expect(h.lines, 3);
    expect(h.rows, 5);
    expect([for (var r = 0; r < h.rows; r++) h.rowLine[r]], [1, 2, 3, 0, 0]);
    expect(h.rowText(0), 'one'); // without the \r
    expect(h.row(1, dark: false).toPlainText(), 'two');
  });

  test('code gets colours, and its text survives them whole', () {
    const src = 'def f(x):\n    return "hi"  # done\n';
    final h = Highlighted.of(src, languageOf('sort.py'));
    expect(languageName(languageOf('sort.py')), 'Python');
    expect(h.scopes.length, greaterThan(1));
    final shown = [for (var r = 0; r < h.rows; r++) h.row(r, dark: true).toPlainText()].join('\n');
    expect(shown, src.trimRight());
  });

  test('looksText, SQLite headers and hex', () {
    expect(looksText(Uint8List.fromList(utf8.encode('héllo'))), isTrue);
    // a sample that ends inside a character is still text
    expect(looksText(Uint8List.sublistView(Uint8List.fromList(utf8.encode('Привет')), 0, 7)), isTrue);
    expect(looksText(Uint8List.fromList([0x68, 0xff, 0x69])), isFalse);
    expect(looksText(Uint8List.fromList([0x50, 0x4b, 0x03, 0x04, 0, 0])), isFalse);
    expect(isSqliteHeader(Uint8List.fromList(utf8.encode('SQLite format 3\u0000 rest'))), isTrue);
    expect(hexDump(Uint8List.fromList(utf8.encode('AB'))).trim(), startsWith('00000000  41 42'));
  });

  test('spreadsheet column letters', () {
    expect(
      [
        for (final i in [0, 25, 26, 51, 52, 701, 702]) columnName(i),
      ],
      ['A', 'Z', 'AA', 'AZ', 'BA', 'ZZ', 'AAA'],
    );
    expect(GridTable.text(3.0), '3');
    expect(GridTable.text(0.1 + 0.2), '0.3');
    expect(GridTable.text(true), 'TRUE');
  });

  test('mail names and subjects', () {
    expect(senderName('"Ann Example" <ann@example.org>'), 'Ann Example');
    expect(senderName('ann@example.org'), 'ann@example.org');
    expect(bareSubject('Re: AW: Fwd: Plans'), 'Plans');
  });

  test('which files are dumps and text by name', () {
    expect(isSqlDump('shop.sql.gz'), isTrue);
    expect(isSqlDump('notes.txt'), isFalse);
    expect(isTextByName('Makefile'), isTrue);
    expect(isTextByName('photo.jpg'), isFalse);
  });

  test('an archive listing makes up its folders and sums them', () {
    final a = ArchiveListing.fromJson({
      'format': 'tar.gz',
      'entries': [
        {'name': 'x/', 'dir': true, 'size': 0},
        {'name': 'x/a.txt', 'size': 10},
        {'name': 'x/y/b.txt', 'size': 5},
        {'name': 'c.txt', 'size': 1},
      ],
      'total': null,
      'counted': 4,
      'total_size': 16,
      'complete': false,
    });
    expect(a.count, 4);
    expect(a.root.dirs['x']!.count, 2);
    expect(a.root.dirs['x']!.size, 15);
    expect(a.root.dirs['x']!.dirs['y']!.files.single.$2, 'b.txt');
    expect(a.root.files.single.$2, 'c.txt');
  });
}
