import 'package:backdown_mobile/api/keys.dart';
import 'package:backdown_mobile/demo/demo_fs.dart';
import 'package:backdown_mobile/features/files/file_screen.dart';
import 'package:backdown_mobile/features/files/folder_screen.dart';
import 'package:backdown_mobile/router.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'support/harness.dart';

void main() {
  testWidgets('without a computer, the app starts at Connect', (tester) async {
    await pumpBackDown(tester, withServer: false);
    expect(find.text('Paste the link'), findsOneWidget);
    expect(find.text('Try the demo'), findsOneWidget);
  });

  testWidgets('the demo button adds the demo and shows its drives', (tester) async {
    await pumpBackDown(tester, withServer: false);
    await tester.tap(find.text('Try the demo'));
    await tester.pumpAndSettle();
    expect(find.text('Seagate Expansion'), findsOneWidget);
  });

  testWidgets('Drives lists every drive, the running job and the totals', (tester) async {
    await pumpBackDown(tester);
    expect(find.text('Seagate Expansion'), findsOneWidget);
    expect(find.text('WD Passport'), findsOneWidget);
    expect(find.text('sam'), findsOneWidget);
    expect(find.text('Hash new and changed files'), findsOneWidget);
    expect(find.text('Not connected'), findsOneWidget); // the WD is away
    expect(find.text('Files'), findsOneWidget);
  });

  testWidgets('a drive opens as a folder, and folders open inside it', (tester) async {
    await pumpBackDown(tester);
    await tester.tap(find.text('Seagate Expansion'));
    await tester.pumpAndSettle();
    expect(find.byType(FolderScreen), findsOneWidget);
    expect(find.text('Photos'), findsOneWidget);
    await tester.tap(find.text('Photos'));
    await tester.pumpAndSettle();
    expect(find.text('2015'), findsOneWidget);
    // the breadcrumbs lead back, the drive by its name
    expect(find.text('Seagate Expansion'), findsOneWidget);
  });

  testWidgets('duplicates show how many copies there are', (tester) async {
    final c = await pumpBackDown(tester);
    await goTo(tester, c, Routes.folder(keyOfPath('/media/sam/Seagate Expansion/Photos/2014')));
    expect(find.text('IMG_2100.JPG'), findsOneWidget);
    expect(find.text('×2'), findsWidgets);
  });

  testWidgets('a file\'s details show its copies here and on other machines', (tester) async {
    final c = await pumpBackDown(tester);
    final photo = DemoFs.instance.byName('IMG_2100.JPG')!;
    await goTo(tester, c, Routes.file(photo.id!));
    expect(find.byType(FileScreen), findsOneWidget);
    expect(find.text('The same content elsewhere'.toUpperCase()), findsOneWidget);
    expect(find.text('On nextcloud'), findsOneWidget);
    expect(find.textContaining('WD Passport'), findsWidgets);
  });

  testWidgets('long-press on a file offers details, share and copy path', (tester) async {
    final c = await pumpBackDown(tester);
    await goTo(tester, c, Routes.folder(keyOfPath('/media/sam/Seagate Expansion/Documents')));
    await tester.longPress(find.text('notes.md'));
    await tester.pumpAndSettle();
    expect(find.text('Details'), findsOneWidget);
    expect(find.text('Share'), findsOneWidget);
    expect(find.text('Copy path'), findsOneWidget);
  });

  testWidgets('an earlier scan shows with a way back to now', (tester) async {
    final c = await pumpBackDown(tester);
    final seagate = DemoFs.instance.roots.first;
    await goTo(tester, c, Routes.folder(seagate.node.key, scan: seagate.scans.first.id));
    expect(find.text('Back to now'), findsOneWidget);
  });

  testWidgets('search finds files across drives and says where they are', (tester) async {
    final c = await pumpBackDown(tester);
    await goTo(tester, c, Routes.search);
    await tester.enterText(find.byType(TextField), 'invoice');
    await tester.pump(const Duration(milliseconds: 400));
    await tester.pumpAndSettle();
    expect(find.textContaining('invoice-2019-0'), findsWidgets);
    expect(find.textContaining('Seagate Expansion › Documents/Invoices'), findsWidgets);
  });

  testWidgets('a rescan from a phone is allowed and says so', (tester) async {
    await pumpBackDown(tester);
    await tester.tap(find.byTooltip('Rescan').at(2));
    await tester.pumpAndSettle();
    expect(find.text('Rescan of sam queued'), findsOneWidget);
  });
}
