import 'package:backdown_mobile/background/background.dart';
import 'package:backdown_mobile/background/check.dart';
import 'package:backdown_mobile/background/notices.dart';
import 'package:backdown_mobile/features/health/health_screen.dart';
import 'package:backdown_mobile/router.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:shared_preferences/shared_preferences.dart';

import '../support/harness.dart';
import 'fake_platform.dart';

void main() {
  Future<ProviderContainer> settings(
    WidgetTester t,
    FakeBackground phone, {
    Map<String, Object> prefs = const {},
  }) async {
    final c = await pumpBackDown(t, prefs: prefs, overrides: [backgroundPlatformProvider.overrideWithValue(phone)]);
    await goTo(t, c, Routes.appSettings);
    return c;
  }

  testWidgets('turning checks on asks for the permission, then schedules them', (t) async {
    final phone = FakeBackground();
    await settings(t, phone);
    expect(find.text('Off: nothing is checked in the background.'), findsOneWidget);
    expect(find.text('A job failed'), findsNothing);
    await t.tap(find.text('1 hour'));
    await t.pumpAndSettle();
    expect(phone.asked, 1);
    expect(phone.schedules, [60]);
    expect(find.textContaining('Checks every hour, as far as the phone allows'), findsOneWidget);
    expect(find.text('A job failed'), findsOneWidget);
    expect((await SharedPreferences.getInstance()).getInt('bg_every'), 60);

    await t.tap(find.text('15 min'));
    await t.pumpAndSettle();
    expect(phone.asked, 1); // already given
    expect(phone.schedules, [60, 15]);

    await t.tap(find.text('Off'));
    await t.pumpAndSettle();
    expect(phone.schedules, [60, 15, 0]);
    expect(find.text('A job failed'), findsNothing);
  });

  testWidgets('without the permission it stays off and says why', (t) async {
    final phone = FakeBackground(grant: false);
    await settings(t, phone);
    await t.tap(find.text('6 hours'));
    await t.pumpAndSettle();
    expect(phone.schedules, isEmpty);
    expect(find.textContaining('notifications are off for it in the phone\'s settings'), findsOneWidget);
    expect(find.text('Off: nothing is checked in the background.'), findsOneWidget);
  });

  testWidgets('what to tell is chosen kind by kind, and a test shows one', (t) async {
    final phone = FakeBackground();
    await settings(t, phone, prefs: {'bg_every': 15});
    final reach = find.widgetWithText(SwitchListTile, 'A computer can\'t be reached');
    expect(t.widget<SwitchListTile>(reach).value, isFalse);
    await t.tap(reach);
    await t.pumpAndSettle();
    expect(t.widget<SwitchListTile>(reach).value, isTrue);
    expect((await SharedPreferences.getInstance()).getBool('bg_unreachable'), isTrue);
    await t.ensureVisible(find.text('Send a test notification'));
    await t.pumpAndSettle();
    await t.tap(find.text('Send a test notification'));
    await t.pumpAndSettle();
    expect(phone.tests, 1);
  });

  testWidgets('notifications turned off on the phone are pointed out', (t) async {
    await settings(t, FakeBackground(notificationsAllowed: false), prefs: {'bg_every': 60});
    expect(find.text('Notifications are off for BackDown in the phone\'s settings.'), findsOneWidget);
  });

  testWidgets('on iOS it says iOS decides', (t) async {
    await settings(t, FakeBackground(ios: true), prefs: {'bg_every': 60});
    expect(find.textContaining('iOS decides when background checks run'), findsOneWidget);
  });

  testWidgets('where nothing runs in the background, it says so', (t) async {
    final c = await pumpBackDown(t);
    await goTo(t, c, Routes.appSettings);
    expect(find.text('Not on this device'), findsOneWidget);
  });

  testWidgets('a tap on a notification opens its tab', (t) async {
    final c = await pumpBackDown(t);
    final follow = Provider<int>((ref) {
      attachNotificationTaps(ref, ref.read(routerProvider));
      return 0;
    });
    c.read(follow);
    notificationTaps.value = const NoticeTap('demo', Routes.health);
    await t.pumpAndSettle();
    expect(find.byType(HealthScreen), findsOneWidget);
    expect(notificationTaps.value, isNull);
  });
}
