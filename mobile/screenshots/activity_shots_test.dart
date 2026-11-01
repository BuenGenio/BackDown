import 'package:backdown_mobile/api/demo_api.dart';
import 'package:backdown_mobile/background/platform.dart';
import 'package:backdown_mobile/demo/demo_activity.dart';
import 'package:backdown_mobile/features/health/routes.dart';
import 'package:backdown_mobile/router.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import '../test/background/fake_platform.dart';
import '../test/support/harness.dart';
import '../test/support/shots.dart';

/// Activity, Health, a drive's health, a job and the phone's notification settings, light and
/// dark: `flutter test screenshots/activity_shots_test.dart`.
void main() {
  setUpAll(loadFonts);

  Future<void> scrollDown(WidgetTester t, double by) async {
    await t.drag(find.byType(Scrollable).first, Offset(0, -by));
    await t.pumpAndSettle();
  }

  for (final dark in [false, true]) {
    final prefs = <String, Object>{'theme_mode': dark ? 'dark' : 'light'};
    final s = dark ? '-dark' : '';

    testWidgets('activity$s', (t) async {
      await pumpBackDown(t, prefs: prefs, location: Routes.activity);
      await shoot(t, 'activity$s');
    });

    testWidgets('activity-recent$s', (t) async {
      await pumpBackDown(t, prefs: prefs, location: Routes.activity);
      await scrollDown(t, 1400);
      await shoot(t, 'activity-recent$s');
    });

    testWidgets('job$s', (t) async {
      await pumpBackDown(t, prefs: prefs, location: Routes.activity);
      await t.tap(find.text('Hash new and changed files', findRichText: true));
      await t.pumpAndSettle();
      await shoot(t, 'job$s');
    });

    testWidgets('health$s', (t) async {
      await pumpBackDown(t, prefs: prefs, location: Routes.health);
      await shoot(t, 'health$s');
    });

    testWidgets('health-more$s', (t) async {
      await pumpBackDown(t, prefs: prefs, location: Routes.health);
      await scrollDown(t, 1300);
      await shoot(t, 'health-more$s');
    });

    testWidgets('drive$s', (t) async {
      final c = await pumpBackDown(t, prefs: prefs, location: Routes.health);
      await goTo(t, c, healthDrivePath('sn:ZFL0A7Q2'));
      await shoot(t, 'drive$s');
    });

    testWidgets('notify-settings$s', (t) async {
      final c = await pumpBackDown(
        t,
        prefs: {...prefs, 'bg_every': 60},
        overrides: [backgroundPlatformProvider.overrideWithValue(FakeBackground())],
      );
      await goTo(t, c, Routes.appSettings);
      await shoot(t, 'notify-settings$s');
    });
  }

  testWidgets('activity-side', (t) async {
    final api = DemoBackDownApi();
    demoStartSideJob(api);
    await pumpBackDown(t, api: api, location: Routes.activity);
    await shoot(t, 'activity-side');
  });

  testWidgets('activity-serve', (t) async {
    final api = DemoBackDownApi();
    demoServeOnly(api);
    await pumpBackDown(t, api: api, location: Routes.activity);
    await shoot(t, 'activity-serve');
    // `backdown serve` is asked again every half minute: let that wait run out with the app gone
    await t.pumpWidget(const SizedBox());
    await t.pump(const Duration(seconds: 31));
  });

  testWidgets('activity-wide', (t) async {
    await pumpBackDown(t, size: const Size(1280, 800), location: Routes.activity);
    await shoot(t, 'activity-wide');
  });

  testWidgets('health-wide-dark', (t) async {
    await pumpBackDown(t, size: const Size(1280, 800), prefs: {'theme_mode': 'dark'}, location: Routes.health);
    await shoot(t, 'health-wide-dark');
  });

  testWidgets('drive-other-machine', (t) async {
    final c = await pumpBackDown(t, location: Routes.health);
    await goTo(t, c, healthDrivePath('@nextcloud:sn:Y8K2T1DEMO'));
    await shoot(t, 'drive-other-machine');
  });
}
