import 'dart:async';

import 'package:backdown_mobile/api/keys.dart';
import 'package:backdown_mobile/demo/demo_fs.dart';
import 'package:backdown_mobile/features/files/file_screen.dart';
import 'package:backdown_mobile/features/files/folder_screen.dart';
import 'package:backdown_mobile/router.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'support/harness.dart';

/// Moving between the tabs and the full-screen screens (a file's details, the viewers), which
/// go_router only allows one way round.
void main() {
  final documents = Routes.folder(keyOfPath('/media/sam/Seagate Expansion/Documents'));

  testWidgets('a folder opens from another tab', (t) async {
    final c = await pumpBackDown(t);
    for (final tab in [Routes.more, Routes.search, Routes.health]) {
      await goTo(t, c, tab);
      unawaited(routerOf(c).push(documents));
      await t.pumpAndSettle();
      expect(find.byType(FolderScreen), findsOneWidget, reason: tab);
    }
  });

  testWidgets('"Show in folder" from a file\'s details opens the folder with the file in it', (t) async {
    final c = await pumpBackDown(t);
    final notes = DemoFs.instance.byName('notes.md')!;
    await goTo(t, c, Routes.file(notes.id!));
    await t.tap(find.text('Show in folder'));
    await t.pumpAndSettle();
    expect(find.byType(FileScreen), findsNothing);
    expect(find.byType(FolderScreen), findsOneWidget);
    final tile = t.widget<ListTile>(find.ancestor(of: find.text('notes.md'), matching: find.byType(ListTile)));
    expect(tile.selected, isTrue);
  });

  testWidgets('a copy on another machine opens that machine\'s file from a file\'s details', (t) async {
    final c = await pumpBackDown(t);
    await goTo(t, c, Routes.file(DemoFs.instance.byName('IMG_2100.JPG')!.id!));
    await t.tap(find.text('On nextcloud'));
    await t.pumpAndSettle();
    expect(find.byType(FileScreen), findsNothing);
    expect(find.textContaining('IMG_2100.JPG'), findsWidgets);
  });
}
