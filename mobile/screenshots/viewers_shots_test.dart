import 'package:backdown_mobile/api/keys.dart';
import 'package:backdown_mobile/demo/demo_fs.dart';
import 'package:backdown_mobile/features/viewers/common.dart';
import 'package:backdown_mobile/features/viewers/routes.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import '../test/support/harness.dart';
import '../test/support/shots.dart';
import '../test/viewers/support.dart';

/// The viewers, light and dark: `flutter test screenshots/viewers_shots_test.dart`.
void main() {
  setUpAll(loadFonts);

  const box = '/media/sam/Seagate Expansion/Mail/mail.example.org';
  const wide = Size(1280, 800);

  for (final dark in [false, true]) {
    final prefs = <String, Object>{'theme_mode': dark ? 'dark' : 'light'};
    final s = dark ? '-dark' : '';

    testWidgets('viewer-photo$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await openInViewer(t, c, 'IMG_2102.JPG', withSiblings: true);
      await shoot(t, 'viewer-photo$s');
    });

    testWidgets('viewer-code$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await openInViewer(t, c, 'sort.py');
      await shoot(t, 'viewer-code$s');
    });

    testWidgets('viewer-markdown$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await openInViewer(t, c, 'README.md');
      await shoot(t, 'viewer-markdown$s');
    });

    testWidgets('viewer-csv$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      final zip = demoFile('site-backup.zip');
      await goTo(t, c, innerLocation(InnerFile.entry(id: zip.id, name: 'site/data/orders-2018.csv', size: 520)));
      await shoot(t, 'viewer-csv$s');
    });

    testWidgets('viewer-pdf$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await openInViewer(t, c, 'invoice-2019-03.pdf', withSiblings: true);
      await shoot(t, 'viewer-pdf$s');
    });

    testWidgets('viewer-mail$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      final cur = DemoFs.instance.byKey[keyOfPath('$box/cur')]!;
      await openInViewer(t, c, cur.children![4].name);
      await shoot(t, 'viewer-mail$s');
    });

    testWidgets('viewer-mbox$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await openInViewer(t, c, 'archive-2012.mbox');
      await shoot(t, 'viewer-mbox$s');
    });

    testWidgets('viewer-thread$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await openInViewer(t, c, 'archive-2012.mbox');
      await t.tap(find.text('Photos from the lake'));
      await t.pumpAndSettle();
      await t.tap(find.text('Expand all'));
      await t.pumpAndSettle();
      await shoot(t, 'viewer-thread$s');
    });

    testWidgets('viewer-maildir$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await goTo(t, c, mailboxLocation(keyOfPath(box)));
      await shoot(t, 'viewer-maildir$s');
    });

    testWidgets('viewer-archive$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await openInViewer(t, c, 'site-backup.zip');
      await t.tap(find.text('site'));
      await t.pumpAndSettle();
      await shoot(t, 'viewer-archive$s');
    });

    testWidgets('viewer-disk$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await openInViewer(t, c, 'old-laptop.img');
      await shoot(t, 'viewer-disk$s');
    });

    testWidgets('viewer-fs$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await goTo(t, c, fsLocation(demoFile('old-laptop.img').id, '2', 'thinkpad'));
      await t.tap(find.text('home'));
      await t.pumpAndSettle();
      await t.tap(find.text('sam'));
      await t.pumpAndSettle();
      await shoot(t, 'viewer-fs$s');
    });

    testWidgets('viewer-fs-deleted$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await goTo(t, c, fsLocation(demoFile('old-laptop.img').id, '1', 'EFI System Partition'));
      await t.tap(find.byType(PopupMenuButton<String>));
      await t.pumpAndSettle();
      await t.tap(find.text('Show deleted files'));
      await t.pumpAndSettle();
      await shoot(t, 'viewer-fs-deleted$s');
    });

    testWidgets('viewer-iso$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await openInViewer(t, c, 'ubuntu-24.04-desktop-amd64.iso');
      await shoot(t, 'viewer-iso$s');
    });

    testWidgets('viewer-sqlite$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await openInViewer(t, c, 'contacts.sqlite');
      await shoot(t, 'viewer-sqlite$s');
    });

    testWidgets('viewer-sqlite-table$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await goTo(t, c, sqliteTableLocation(demoFile('contacts.sqlite').id, 'contacts'));
      await shoot(t, 'viewer-sqlite-table$s');
    });

    testWidgets('viewer-sqldump$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await openInViewer(t, c, 'shop-2018.sql');
      await shoot(t, 'viewer-sqldump$s');
    });

    testWidgets('viewer-sqldump-table$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await openInViewer(t, c, 'shop-2018.sql');
      await t.tap(find.text('orders'));
      await t.pumpAndSettle();
      await shoot(t, 'viewer-sqldump-table$s');
    });

    testWidgets('viewer-sheet$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await openInViewer(t, c, 'Taxes 2019.xlsx');
      await t.tap(find.widgetWithText(ChoiceChip, 'Income'));
      await t.pumpAndSettle();
      await shoot(t, 'viewer-sheet$s');
    });

    testWidgets('viewer-slides$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs, api: SlidesDemo());
      await openRef(t, c, deck);
      await shoot(t, 'viewer-slides$s');
    });

    testWidgets('viewer-fallback$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await openInViewer(t, c, 'Info.plist');
      await shoot(t, 'viewer-fallback$s');
    });

    testWidgets('viewer-media$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await openInViewer(t, c, '03 Hammers.mp3', withSiblings: true);
      await shoot(t, 'viewer-media$s');
    });

    testWidgets('viewer-mbox-wide$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs, size: wide);
      await openInViewer(t, c, 'archive-2012.mbox');
      await shoot(t, 'viewer-mbox-wide$s');
    });

    testWidgets('viewer-sqlite-wide$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs, size: wide);
      await openInViewer(t, c, 'contacts.sqlite');
      await shoot(t, 'viewer-sqlite-wide$s');
    });

    testWidgets('viewer-photo-wide$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs, size: wide);
      await openInViewer(t, c, 'IMG_2102.JPG', withSiblings: true);
      await shoot(t, 'viewer-photo-wide$s');
    });
  }
}
