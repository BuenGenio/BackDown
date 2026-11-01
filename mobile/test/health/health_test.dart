import 'package:backdown_mobile/api/api.dart';
import 'package:backdown_mobile/api/demo_api.dart';
import 'package:backdown_mobile/features/health/drive_screen.dart';
import 'package:backdown_mobile/features/health/health_model.dart';
import 'package:backdown_mobile/features/health/health_screen.dart';
import 'package:backdown_mobile/features/health/routes.dart';
import 'package:backdown_mobile/router.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import '../support/harness.dart';

Future<void> _scrollTo(WidgetTester t, Finder f) async {
  await t.scrollUntilVisible(f, 300, scrollable: find.byType(Scrollable).first);
  await t.pumpAndSettle();
}

/// The demo, but GET /api/health-machines is a paid-edition refusal.
class _Locked extends DemoBackDownApi {
  @override
  Future<Object?> get(String endpoint, [Map<String, Object?> params = const {}, Duration? timeout]) {
    if (endpoint == 'health-machines') {
      throw ApiException(ApiFailure.needsTier, 'Other machines\' drive health is part of BackDown Team.', status: 402);
    }
    // and no other machines at all
    if (endpoint == 'machines') return Future.value({'machines': <Object?>[], 'allowed': true, 'daemon': true});
    return super.get(endpoint, params, timeout);
  }
}

void main() {
  testWidgets('Health shows how the drives are, the news first, and each drive', (t) async {
    await pumpBackDown(t, location: Routes.health);
    expect(find.byType(HealthScreen), findsOneWidget);
    expect(find.text('1 drive needs watching'), findsOneWidget);
    expect(find.text('2 unread'), findsOneWidget);
    expect(find.text('ST2000DM008-2FR102 is getting worse'), findsOneWidget);
    await _scrollTo(t, find.text('8 sectors had gone bad and have been replaced by spares (reallocated)'));
    await _scrollTo(t, find.text('Samsung SSD 980 1TB'));
    await _scrollTo(t, find.text('Generic- SD/MMC'));
    expect(find.text('no health data'), findsOneWidget);
    await _scrollTo(t, find.text('WDC WD20SDRW-11VUUS0'));
    expect(find.textContaining('not connected'), findsWidgets);
  });

  testWidgets('other machines\' drives, as their own BackDown read them', (t) async {
    await pumpBackDown(t, location: Routes.health);
    await _scrollTo(t, find.text('synced yesterday'));
    expect(find.text('nextcloud'), findsOneWidget);
    await _scrollTo(t, find.text('TOSHIBA MQ04UBF100'));
    expect(find.textContaining('Not read there lately'), findsOneWidget);
    expect(find.text('TOSHIBA MQ04UBF100 needs watching'), findsOneWidget);
  });

  testWidgets('a locked edition with no machines says nothing about them', (t) async {
    await pumpBackDown(t, api: _Locked(), location: Routes.health);
    await _scrollTo(t, find.text('Notifications on the computer'.toUpperCase()));
    expect(find.text('Other machines\' drives'.toUpperCase()), findsNothing);
  });

  testWidgets('a drive opens with every reading and what changed', (t) async {
    await pumpBackDown(t, location: Routes.health);
    await _scrollTo(t, find.text('ST2000DM008-2FR102'));
    await t.tap(find.text('ST2000DM008-2FR102'));
    await t.pumpAndSettle();
    expect(find.byType(DriveScreen), findsOneWidget);
    expect(find.text('Reallocated sectors'), findsOneWidget);
    expect(find.text('31,244 h (3.6 years)'), findsOneWidget);
    expect(find.text('0 – 8'), findsOneWidget); // bad sectors' range
    expect(find.text('Since the first reading: +8 reallocated, +1404 h on.'), findsOneWidget);
    await _scrollTo(t, find.text('6 more sectors have gone bad and been replaced by spares (now 8)'));
    await _scrollTo(t, find.text('/media/sam/Seagate Expansion'));
    expect(find.text('ZFL0A7Q2'), findsOneWidget);
  });

  testWidgets('a notification about a drive leads to it', (t) async {
    await pumpBackDown(t, location: Routes.health);
    await t.tap(find.text('ST2000DM008-2FR102 needs watching'));
    await t.pumpAndSettle();
    expect(find.byType(DriveScreen), findsOneWidget);
  });

  testWidgets('an unknown drive says so', (t) async {
    final c = await pumpBackDown(t, location: Routes.health);
    await goTo(t, c, healthDrivePath('sn:GONE'));
    expect(find.text('Not known any more'), findsOneWidget);
  });

  testWidgets('another machine\'s drive opens too', (t) async {
    final c = await pumpBackDown(t, location: Routes.health);
    await goTo(t, c, healthDrivePath('@nextcloud:sn:Y8K2T1DEMO'));
    expect(find.text('TOSHIBA MQ04UBF100'), findsWidgets);
    expect(find.textContaining('on nextcloud (/dev/sdc)'), findsOneWidget);
    expect(find.text('Pending sectors'), findsOneWidget);
  });

  testWidgets('from a phone, marking read needs someone at the computer', (t) async {
    await pumpBackDown(t, location: Routes.health);
    await t.tap(find.byTooltip('Mark as read').first);
    await t.pumpAndSettle();
    expect(find.text('Needs someone at the computer'), findsOneWidget);
  });

  testWidgets('at the computer, marking all read clears the badge', (t) async {
    await pumpBackDown(t, location: Routes.health, canChange: true);
    expect(find.text('2'), findsOneWidget); // the tab's badge
    await t.tap(find.text('Mark all read'));
    await t.pumpAndSettle();
    expect(find.text('2 unread'), findsNothing);
    expect(find.byTooltip('Mark as read'), findsNothing);
    expect(find.text('2'), findsNothing);
  });

  testWidgets('at the computer, Check now reads the drives again', (t) async {
    await pumpBackDown(t, location: Routes.health, canChange: true);
    await t.tap(find.byTooltip('Check now'));
    await t.pumpAndSettle();
    expect(find.text('Reading every drive\'s health now'), findsOneWidget);
    expect(find.textContaining('Read just now'), findsOneWidget);
  });

  testWidgets('the computer\'s notification settings change at the computer', (t) async {
    await pumpBackDown(t, location: Routes.health, canChange: true);
    final desktop = find.widgetWithText(SwitchListTile, 'Show notifications on the desktop');
    await _scrollTo(t, desktop);
    expect(t.widget<SwitchListTile>(desktop).value, isTrue);
    await t.tap(desktop);
    await t.pumpAndSettle();
    expect(t.widget<SwitchListTile>(desktop).value, isFalse);
    expect(find.text('Send a test notification'), findsOneWidget);
  });

  test('a folder is on the connected drive with the deepest mount point holding it', () {
    Map<String, Object?> drive(String id, bool present, List<String> mounts) => {
      'id': id,
      'present': present,
      'verdict': 'ok',
      'health': {'id': id, 'device': '/dev/$id', 'mounts': mounts},
    };
    final h = HealthOverview.fromJson({
      'drives': [
        drive('root', true, ['/', '/home']),
        drive('usb', true, ['/media/sam/Seagate']),
        drive('away', false, ['/media/sam/WD']),
      ],
    });
    expect(h.ofPath('/home/sam/Documents')?.id, 'root');
    expect(h.ofPath('/media/sam/Seagate/Photos')?.id, 'usb');
    expect(h.ofPath('/media/sam/Seagate2')?.id, 'root');
    // away: not the drive its empty mount point is on
    expect(h.ofPath('/media/sam/WD'), isNull);
    expect(h.worst, 'ok');
  });
}
