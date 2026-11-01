import 'package:backdown_mobile/api/keys.dart';
import 'package:backdown_mobile/demo/demo_fs.dart';
import 'package:backdown_mobile/features/files/file_screen.dart';
import 'package:backdown_mobile/features/machines/machine_file_screen.dart';
import 'package:backdown_mobile/features/machines/machine_folder_screen.dart';
import 'package:backdown_mobile/features/machines/machine_screen.dart';
import 'package:backdown_mobile/features/machines/machines_screen.dart';
import 'package:backdown_mobile/features/machines/providers.dart';
import 'package:backdown_mobile/router.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import '../support/harness.dart';

/// Scrolls the screen's list until [f] is on screen, whole.
Future<void> see(WidgetTester tester, Finder f, {double by = 300}) async {
  await tester.scrollUntilVisible(f, by, scrollable: find.byType(Scrollable).first);
  await tester.ensureVisible(f);
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('More leads to the other machines, each with how it is and its drives', (tester) async {
    final c = await pumpBackDown(tester);
    await goTo(tester, c, Routes.more);
    await tester.tap(find.text('Other machines'));
    await tester.pumpAndSettle();
    expect(find.byType(MachinesScreen), findsOneWidget);
    expect(find.text('nextcloud'), findsOneWidget);
    expect(find.text('attic-pi'), findsOneWidget);
    expect(find.text('can\'t be reached'), findsOneWidget);
    expect(find.text('synced 2 hours ago'), findsOneWidget);
    expect(find.text('photos'), findsOneWidget);
    expect(find.text('old-backup'), findsOneWidget);
  });

  testWidgets('a machine that can\'t be reached says why, and its drives as last synced', (tester) async {
    final c = await pumpBackDown(tester);
    await goTo(tester, c, Routes.machine(1));
    expect(find.byType(MachineScreen), findsOneWidget);
    expect(find.text('Can\'t be reached'), findsOneWidget);
    expect(find.textContaining('Connection timed out'), findsOneWidget);
    expect(find.text('BackDown there'), findsOneWidget);
    // from a phone, changes are made at the computer
    expect(find.textContaining('are made on the computer BackDown runs on'), findsOneWidget);
    await see(tester, find.text('/srv/photos'));
    await see(tester, find.text('/srv/pool'));
    expect(find.text('counts as proof'), findsWidgets);
    expect(find.text('too long ago to count'), findsNothing);
  });

  testWidgets('copies on a drive confirmed too long ago say they don\'t count', (tester) async {
    final c = await pumpBackDown(tester);
    await goTo(tester, c, Routes.machine(2));
    expect(find.text('Synced 2 hours ago'), findsOneWidget);
    await see(tester, find.text('too long ago to count'));
    await see(tester, find.text('counts as proof'));
  });

  testWidgets('sync now from a phone explains it needs someone at the computer', (tester) async {
    final c = await pumpBackDown(tester);
    await goTo(tester, c, Routes.machine(2));
    await tester.tap(find.text('Sync now'));
    await tester.pumpAndSettle();
    expect(find.text('Needs someone at the computer'), findsOneWidget);
  });

  testWidgets('at the computer, sync now queues a sync and the machine says so', (tester) async {
    final c = await pumpBackDown(tester, canChange: true);
    await goTo(tester, c, Routes.machine(2));
    await tester.tap(find.text('Sync now'));
    await tester.pumpAndSettle();
    expect(find.text('Syncing: see Activity for progress'), findsOneWidget);
    expect(find.text('Sync waiting its turn'), findsOneWidget);
    expect(find.text('Syncing…'), findsOneWidget);
  });

  testWidgets('at the computer, testing a machine says whether it was reached', (tester) async {
    final c = await pumpBackDown(tester, canChange: true);
    await goTo(tester, c, Routes.machine(1));
    await tester.tap(find.text('Test connection'));
    await tester.pumpAndSettle();
    expect(find.text('Can\'t be reached: see why on the page'), findsOneWidget);
  });

  testWidgets('at the computer, how copies count can be changed', (tester) async {
    final c = await pumpBackDown(tester, canChange: true);
    await goTo(tester, c, Routes.machine(2));
    await see(tester, find.text('Change…'));
    await tester.tap(find.text('Change…'));
    await tester.pumpAndSettle();
    await tester.enterText(find.byType(TextFormField).first, '14');
    await tester.tap(find.text('Save'));
    await tester.pumpAndSettle();
    expect(find.text('Saved'), findsOneWidget);
    expect(find.text('Copies count for 14 days'), findsOneWidget);
    // the old drive's copies count now
    await see(tester, find.text('/mnt/old-backup'), by: -300);
    expect(find.text('too long ago to count'), findsNothing);
    expect(find.text('counts as proof'), findsNWidgets(2));
  });

  testWidgets('adding and forgetting a machine are shown as done at the computer', (tester) async {
    final c = await pumpBackDown(tester);
    await goTo(tester, c, Routes.machines);
    await tester.tap(find.byTooltip('Add a machine'));
    await tester.pumpAndSettle();
    expect(find.text('backdown remote add nas ssh://me@nas.example.org'), findsOneWidget);
    Navigator.of(tester.element(find.text('Before it works'))).pop();
    await tester.pumpAndSettle();
    await goTo(tester, c, Routes.machine(1));
    await see(tester, find.text('Forget nextcloud'));
    await tester.tap(find.text('Forget nextcloud'));
    await tester.pumpAndSettle();
    expect(find.text('backdown remote remove nextcloud'), findsOneWidget);
  });

  testWidgets('a machine that can\'t be asked is browsed as last synced, and says why', (tester) async {
    final c = await pumpBackDown(tester);
    await goTo(tester, c, MachinePaths.ls(1, dir: keyOfPath('/srv/photos')));
    expect(find.byType(MachineFolderScreen), findsOneWidget);
    expect(find.textContaining('As synced'), findsOneWidget);
    expect(find.textContaining('folders without sizes'), findsOneWidget);
    expect(find.textContaining('Connection timed out'), findsOneWidget);
    await tester.tap(find.text('2014'));
    await tester.pumpAndSettle();
    expect(find.text('IMG_2100.JPG'), findsOneWidget);
    // the way back up: the drive by its name
    expect(find.text('photos'), findsOneWidget);
  });

  testWidgets('a machine that answers is browsed live, with folder sizes', (tester) async {
    final c = await pumpBackDown(tester);
    await goTo(tester, c, MachinePaths.ls(2, dir: keyOfPath('/mnt/pool')));
    expect(find.textContaining('live from attic-pi over ssh'), findsOneWidget);
    await tester.tap(find.text('Media'));
    await tester.pumpAndSettle();
    expect(find.text('Photos'), findsOneWidget);
    expect(find.text('Music'), findsOneWidget);
    expect(find.textContaining('14 files'), findsOneWidget);
  });

  testWidgets('without a folder, a machine\'s browse shows its drives', (tester) async {
    final c = await pumpBackDown(tester);
    await goTo(tester, c, MachinePaths.ls(2));
    expect(find.text('Drives on attic-pi'), findsOneWidget);
    expect(find.text('pool'), findsOneWidget);
    expect(find.text('old-backup'), findsOneWidget);
  });

  testWidgets('a file on another machine says no bytes come from there, and where its copies are', (tester) async {
    final c = await pumpBackDown(tester);
    final photo = DemoFs.instance.byName('IMG_2100.JPG')!;
    await goTo(tester, c, MachinePaths.file(1, 9000 + photo.id!));
    expect(find.byType(MachineFileScreen), findsOneWidget);
    expect(find.textContaining('No bytes come from nextcloud'), findsOneWidget);
    expect(find.textContaining('as last synced'), findsWidgets);
    // the copy here opens as a file here
    await tester.tap(find.text('Open the copy here'));
    await tester.pumpAndSettle();
    expect(find.byType(FileScreen), findsOneWidget);
    routerOf(c).pop();
    await tester.pumpAndSettle();
    await see(tester, find.text('/media/sam/Seagate Expansion/Photos/2014/IMG_2100.JPG'));
    expect(find.text('ON THIS COMPUTER'), findsOneWidget);
    await see(tester, find.text('/media/sam/WD Passport/Photos backup/2014/IMG_2100.JPG'));
    await tester.tap(find.text('/media/sam/WD Passport/Photos backup/2014/IMG_2100.JPG'));
    await tester.pumpAndSettle();
    expect(find.byType(FileScreen), findsOneWidget);
  });

  testWidgets('a file there opens from its folder, and shows its folder', (tester) async {
    final c = await pumpBackDown(tester);
    await goTo(tester, c, MachinePaths.ls(2, dir: keyOfPath('/mnt/old-backup/laptop-2015/Documents')));
    await tester.tap(find.text('thesis.pdf'));
    await tester.pumpAndSettle();
    expect(find.byType(MachineFileScreen), findsOneWidget);
    expect(find.text('/mnt/old-backup/laptop-2015/Documents/thesis.pdf'), findsOneWidget);
    expect(find.text('No identical file here.'), findsOneWidget);
    await tester.tap(find.text('Show in folder'));
    await tester.pumpAndSettle();
    expect(find.byType(MachineFolderScreen), findsOneWidget);
    expect(find.text('cv-2015.docx'), findsOneWidget);
  });

  testWidgets('search on a machine asks it, and says when it can\'t be asked', (tester) async {
    final c = await pumpBackDown(tester);
    await goTo(tester, c, MachinePaths.find(2));
    await tester.enterText(find.byType(TextField), 'thesis');
    await tester.testTextInput.receiveAction(TextInputAction.search);
    await tester.pumpAndSettle();
    expect(find.text('thesis.pdf'), findsOneWidget);
    await tester.tap(find.text('thesis.pdf'));
    await tester.pumpAndSettle();
    expect(find.byType(MachineFileScreen), findsOneWidget);

    await goTo(tester, c, MachinePaths.find(1));
    await tester.enterText(find.byType(TextField), 'img');
    await tester.testTextInput.receiveAction(TextInputAction.search);
    await tester.pumpAndSettle();
    expect(find.textContaining('Connection timed out'), findsOneWidget);
  });
}
