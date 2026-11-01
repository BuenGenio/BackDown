import 'package:backdown_mobile/router.dart';
import 'package:backdown_mobile/features/library/routes.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import '../test/support/harness.dart';
import '../test/support/shots.dart';

/// What's here, duplicates, the tidying history and This BackDown, light and dark:
/// `flutter test screenshots/library_shots_test.dart`.
void main() {
  setUpAll(loadFonts);

  const wide = Size(1280, 860);

  for (final dark in [false, true]) {
    final prefs = <String, Object>{'theme_mode': dark ? 'dark' : 'light'};
    final s = dark ? '-dark' : '';

    Future<void> shot(WidgetTester t, String slug, String route, {Size size = phone}) async {
      final c = await pumpBackDown(t, prefs: prefs, size: size);
      await goTo(t, c, route);
      await shoot(t, '$slug$s');
    }

    testWidgets('entities$s', (t) => shot(t, 'entities', Routes.entities));
    testWidgets('entities-wide$s', (t) => shot(t, 'entities-wide', Routes.entities, size: wide));

    testWidgets('entity$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await goTo(t, c, Routes.entities);
      await t.tap(find.text('sam@example.org: 7 messages'));
      await t.pumpAndSettle();
      await shoot(t, 'entity$s');
    });

    testWidgets('dupes$s', (t) => shot(t, 'dupes', Routes.dupes));
    testWidgets('dupes-wide$s', (t) => shot(t, 'dupes-wide', Routes.dupes, size: wide));

    testWidgets('dupes-open$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await goTo(t, c, Routes.dupes);
      await t.tap(find.text('IMG_2108.JPG').first);
      await t.pumpAndSettle();
      await t.drag(find.byType(Scrollable).first, const Offset(0, -420));
      await t.pumpAndSettle();
      await shoot(t, 'dupes-open$s');
    });

    testWidgets('dupes-filter$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await goTo(t, c, Routes.dupes);
      await t.tap(find.byTooltip('Filter'));
      await t.pumpAndSettle();
      await shoot(t, 'dupes-filter$s');
    });

    testWidgets('dupes-across$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await goTo(t, c, Routes.dupes);
      await t.scrollUntilVisible(
        find.text('HERE ONCE, AND AGAIN ON ANOTHER MACHINE'),
        600,
        scrollable: find.byType(Scrollable).first,
      );
      await t.drag(find.byType(Scrollable).first, const Offset(0, -300));
      await t.pumpAndSettle();
      await shoot(t, 'dupes-across$s');
    });

    testWidgets('plans$s', (t) => shot(t, 'plans', Routes.plans));
    testWidgets('plan$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await goTo(t, c, Routes.plan(1));
      await t.tap(find.text('Dry run'));
      await t.pumpAndSettle();
      await shoot(t, 'plan$s');
    });
    testWidgets('free-up$s', (t) => shot(t, 'free-up', Routes.freeUp));
    testWidgets('rescues$s', (t) => shot(t, 'rescues', Routes.rescues));
    testWidgets('mirrors$s', (t) => shot(t, 'mirrors', Routes.mirrors));
    testWidgets('manifests$s', (t) => shot(t, 'manifests', Routes.manifests));
    testWidgets('server$s', (t) => shot(t, 'server', Routes.serverInfo));
    testWidgets('server-wide$s', (t) => shot(t, 'server-wide', Routes.serverInfo, size: wide));

    testWidgets('server-features$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs);
      await goTo(t, c, Routes.serverInfo);
      await t.scrollUntilVisible(find.text('Full recognition and provenance'), 500);
      await t.pumpAndSettle();
      await shoot(t, 'server-features$s');
    });

    testWidgets('report$s', (t) => shot(t, 'report', reportRoute));
  }
}
