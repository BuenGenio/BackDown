import 'package:backdown_mobile/api/keys.dart';
import 'package:backdown_mobile/demo/demo_fs.dart';
import 'package:backdown_mobile/router.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import '../test/support/harness.dart';
import '../test/support/shots.dart';

/// The browsing screens, light and dark: `flutter test screenshots/core_shots_test.dart`.
void main() {
  setUpAll(loadFonts);

  for (final dark in [false, true]) {
    final prefs = <String, Object>{'theme_mode': dark ? 'dark' : 'light'};
    final s = dark ? '-dark' : '';

    testWidgets('connect$s', (t) async {
      await pumpBackDown(t, withServer: false, prefs: prefs);
      await shoot(t, 'connect$s');
    });

    testWidgets('drives$s', (t) async {
      await pumpBackDown(t, prefs: prefs);
      await shoot(t, 'drives$s');
    });

    testWidgets('folder$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await goTo(t, c, Routes.folder(keyOfPath('/media/sam/Seagate Expansion')));
      await shoot(t, 'folder$s');
    });

    testWidgets('photos-grid$s', (t) async {
      final c = await pumpBackDown(t, prefs: {...prefs, 'folder_grid': true});
      await goTo(t, c, Routes.folder(keyOfPath('/media/sam/Seagate Expansion/Photos/2015')));
      await shoot(t, 'photos-grid$s');
    });

    testWidgets('file$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await goTo(t, c, Routes.file(DemoFs.instance.byName('IMG_2100.JPG')!.id!));
      await shoot(t, 'file$s');
    });

    testWidgets('search$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await goTo(t, c, Routes.search);
      await t.enterText(find.byType(TextField), 'img_21');
      await t.pump(const Duration(milliseconds: 400));
      await t.pumpAndSettle();
      await shoot(t, 'search$s');
    });

    testWidgets('more$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await goTo(t, c, Routes.more);
      await shoot(t, 'more$s');
    });
  }
}
