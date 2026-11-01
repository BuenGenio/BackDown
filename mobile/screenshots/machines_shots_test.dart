import 'package:backdown_mobile/api/keys.dart';
import 'package:backdown_mobile/demo/demo_fs.dart';
import 'package:backdown_mobile/features/machines/providers.dart';
import 'package:backdown_mobile/router.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

import '../test/support/harness.dart';
import '../test/support/shots.dart';

/// Other machines and the swarm, light and dark: `flutter test screenshots/machines_shots_test.dart`.
void main() {
  setUpAll(loadFonts);

  /// Opens [route] the way a tap would: from More.
  Future<void> open(WidgetTester t, ProviderContainer c, String route) async {
    await goTo(t, c, Routes.more);
    await goTo(t, c, route);
  }

  Future<void> scroll(WidgetTester t, double by) async {
    await t.drag(find.byType(Scrollable).first, Offset(0, -by));
    await t.pumpAndSettle();
  }

  for (final dark in [false, true]) {
    final prefs = <String, Object>{'theme_mode': dark ? 'dark' : 'light'};
    final s = dark ? '-dark' : '';

    testWidgets('machines$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await open(t, c, Routes.machines);
      await shoot(t, 'machines$s');
    });

    testWidgets('machine-unreachable$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await open(t, c, Routes.machine(1));
      await shoot(t, 'machine-unreachable$s');
      await scroll(t, 700);
      await shoot(t, 'machine-unreachable-2$s');
    });

    testWidgets('machine$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await open(t, c, Routes.machine(2));
      await scroll(t, 420);
      await shoot(t, 'machine$s');
    });

    testWidgets('machine-folder-cached$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await open(t, c, MachinePaths.ls(1, dir: keyOfPath('/srv/photos/2014')));
      await shoot(t, 'machine-folder-cached$s');
    });

    testWidgets('machine-folder-live$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await open(t, c, MachinePaths.ls(2, dir: keyOfPath('/mnt/pool/Media/Photos/Pixel 3')));
      await shoot(t, 'machine-folder-live$s');
    });

    testWidgets('machine-file$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      final photo = DemoFs.instance.byName('IMG_2100.JPG')!;
      await open(t, c, MachinePaths.file(1, 9000 + photo.id!));
      await shoot(t, 'machine-file$s');
    });

    testWidgets('machine-find$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await open(t, c, MachinePaths.find(2));
      await t.enterText(find.byType(TextField), 'img');
      await t.testTextInput.receiveAction(TextInputAction.search);
      await t.pumpAndSettle();
      await shoot(t, 'machine-find$s');
    });

    testWidgets('swarm$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await open(t, c, Routes.swarm);
      await shoot(t, 'swarm$s');
      await scroll(t, 800);
      await shoot(t, 'swarm-2$s');
      await scroll(t, 800);
      await shoot(t, 'swarm-3$s');
      await scroll(t, 800);
      await shoot(t, 'swarm-4$s');
    });

    testWidgets('swarm-wide$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs, size: const Size(1280, 860));
      await open(t, c, Routes.swarm);
      await shoot(t, 'swarm-wide$s');
    });

    testWidgets('where$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await open(t, c, MachinePaths.where('IMG_2100'));
      await shoot(t, 'where$s');
    });

    testWidgets('where-many$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await open(t, c, MachinePaths.where('2015'));
      await shoot(t, 'where-many$s');
    });

    testWidgets('pool-plans$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await open(t, c, MachinePaths.plans);
      await shoot(t, 'pool-plans$s');
    });

    testWidgets('pool-plan$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await open(t, c, MachinePaths.plan(2));
      await shoot(t, 'pool-plan$s');
      await scroll(t, 700);
      await shoot(t, 'pool-plan-2$s');
    });
  }
}
