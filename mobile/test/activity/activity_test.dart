import 'package:backdown_mobile/api/demo_api.dart';
import 'package:backdown_mobile/demo/demo_activity.dart';
import 'package:backdown_mobile/features/activity/activity_screen.dart';
import 'package:backdown_mobile/features/activity/job_sheet.dart';
import 'package:backdown_mobile/features/files/folder_screen.dart';
import 'package:backdown_mobile/router.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import '../support/harness.dart';

/// The running job's title (in its card, with the stage under it).
final _hashing = find.text('Hash new and changed files');

Future<void> _scrollTo(WidgetTester t, Finder f) async {
  await t.scrollUntilVisible(f, 300, scrollable: find.byType(Scrollable).first);
  await t.pumpAndSettle();
}

void main() {
  testWidgets('Activity shows the running job, the queue, the drives and what ran', (t) async {
    await pumpBackDown(t, location: Routes.activity);
    expect(find.byType(ActivityScreen), findsOneWidget);
    expect(_hashing, findsOneWidget);
    expect(find.text('full SHA-256'), findsOneWidget);
    expect(find.text('42%'), findsOneWidget);
    expect(find.text('21 of 52 files · 53.8 GB of 128.2 GB'), findsOneWidget);
    expect(find.textContaining('about 29 min left'), findsOneWidget);
    expect(find.text('1. Recognise what\'s on the drives'), findsOneWidget);
    expect(find.text('after hashing'), findsOneWidget);
    expect(find.text('Seagate Expansion'), findsOneWidget);
    expect(find.text('Connected'), findsWidgets);
    expect(find.text('Not connected'), findsOneWidget);
    // the away drive isn't given the health of the drive its empty mount point is on
    expect(find.text('warning'), findsOneWidget);
    await _scrollTo(t, find.text('Sync the machine nextcloud'));
    expect(find.textContaining('Connection timed out'), findsOneWidget);
    await _scrollTo(t, find.textContaining('BackDown 1.0.0-beta.4'));
    expect(find.textContaining('rescans each drive every 1 d'), findsOneWidget);
    expect(find.textContaining('/home/sam/.local/share/backdown/index.db', findRichText: true), findsOneWidget);
  });

  testWidgets('it keeps up with the service: the long poll brings the job along', (t) async {
    final api = DemoBackDownApi();
    await pumpBackDown(t, api: api, location: Routes.activity);
    expect(find.text('42%'), findsOneWidget);
    api.tick();
    await t.pumpAndSettle();
    expect(find.text('43%'), findsOneWidget);
  });

  testWidgets('a job opens in full', (t) async {
    await pumpBackDown(t, location: Routes.activity);
    await t.tap(_hashing);
    await t.pumpAndSettle();
    expect(find.byType(JobSheet), findsOneWidget);
    expect(find.text('Hashing (full SHA-256)'), findsOneWidget);
    expect(find.text('/media/sam/Seagate Expansion'), findsOneWidget);
    expect(find.text('Running'), findsOneWidget);
    expect(find.widgetWithText(FilledButton, 'Open the drive'), findsNothing); // hashing isn't a scan
  });

  testWidgets('a finished scan opens its drive', (t) async {
    await pumpBackDown(t, location: Routes.activity);
    await _scrollTo(t, find.text('Scan Seagate Expansion'));
    await t.tap(find.text('Scan Seagate Expansion'));
    await t.pumpAndSettle();
    expect(find.text('57 files, 128.2 GB: 3 new, 1 changed'), findsWidgets);
    expect(find.text('Took'), findsOneWidget);
    await t.tap(find.text('Open the drive'));
    await t.pumpAndSettle();
    expect(find.byType(FolderScreen), findsOneWidget);
  });

  testWidgets('from a phone, Stop needs someone at the computer', (t) async {
    await pumpBackDown(t, location: Routes.activity);
    expect(find.textContaining('Changes other than rescans are made on the computer'), findsOneWidget);
    await t.tap(find.text('Stop'));
    await t.pumpAndSettle();
    expect(find.text('Needs someone at the computer'), findsOneWidget);
  });

  testWidgets('at the computer, Stop stops it and the next job starts', (t) async {
    await pumpBackDown(t, location: Routes.activity, canChange: true);
    expect(find.textContaining('Changes other than rescans'), findsNothing);
    await t.tap(find.text('Stop'));
    await t.pumpAndSettle();
    expect(find.text('Stopping: what\'s done so far is kept'), findsOneWidget);
    expect(find.text('Recognise what\'s on the drives'), findsOneWidget); // running now
    expect(find.text('Nothing waiting.'), findsOneWidget);
  });

  testWidgets('at the computer, a waiting job can be taken off the queue', (t) async {
    await pumpBackDown(t, location: Routes.activity, canChange: true);
    await t.tap(find.byTooltip('Take it off the queue'));
    await t.pumpAndSettle();
    expect(find.text('Taken off the queue'), findsOneWidget);
    expect(find.text('Nothing waiting.'), findsOneWidget);
  });

  testWidgets('a rescan from a phone is queued', (t) async {
    await pumpBackDown(t, location: Routes.activity);
    await _scrollTo(t, find.text('sam'));
    await t.tap(find.byTooltip('Rescan now').last);
    await t.pumpAndSettle();
    expect(find.text('Rescan of sam queued'), findsOneWidget);
    expect(find.text('Waiting its turn'), findsOneWidget);
    // back up to the queue: it's waiting there
    await t.scrollUntilVisible(find.text('2. Scan sam'), -300, scrollable: find.byType(Scrollable).first);
  });

  testWidgets('a folder can be indexed (at the computer)', (t) async {
    await pumpBackDown(t, location: Routes.activity, canChange: true);
    await t.tap(find.byIcon(Icons.more_vert));
    await t.pumpAndSettle();
    await t.tap(find.text('Index a folder…'));
    await t.pumpAndSettle();
    await t.enterText(find.byType(TextField), '/media/sam/Photos');
    await t.tap(find.text('Index it'));
    await t.pumpAndSettle();
    expect(find.text('Indexing: it shows up in Drives when it\'s done'), findsOneWidget);
    expect(find.text('2. Scan Photos'), findsOneWidget);
  });

  testWidgets('the side lane shows beside the main job', (t) async {
    final api = DemoBackDownApi();
    demoStartSideJob(api);
    await pumpBackDown(t, api: api, location: Routes.activity);
    expect(find.text('Sync the machine nextcloud'), findsOneWidget);
    expect(find.text('nextcloud: 1,204 files new or changed there'), findsOneWidget);
    expect(find.textContaining('runs beside scans and hashing'), findsOneWidget);
  });

  testWidgets('`backdown serve` says nothing rescans on its own', (t) async {
    final api = DemoBackDownApi();
    demoServeOnly(api);
    await pumpBackDown(t, api: api, location: Routes.activity);
    expect(find.text('BackDown isn\'t running in the background'), findsOneWidget);
    expect(find.text('Seagate Expansion'), findsOneWidget);
    expect(find.byTooltip('Rescan now'), findsNothing);
    // it's asked again every half minute: let that wait run out with the app gone
    await t.pumpWidget(const SizedBox());
    await t.pump(const Duration(seconds: 31));
  });

  testWidgets('on a wide screen the list keeps a readable width', (t) async {
    await pumpBackDown(t, size: const Size(1280, 800), location: Routes.activity);
    expect(t.getSize(find.byType(Card).first).width, lessThanOrEqualTo(760));
  });
}
