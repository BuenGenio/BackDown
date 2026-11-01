import 'package:backdown_mobile/api/api.dart';
import 'package:backdown_mobile/api/demo_api.dart';
import 'package:backdown_mobile/features/files/file_screen.dart';
import 'package:backdown_mobile/features/machines/machine_file_screen.dart';
import 'package:backdown_mobile/features/machines/machine_screen.dart';
import 'package:backdown_mobile/features/machines/pool_plans_screen.dart';
import 'package:backdown_mobile/features/machines/providers.dart';
import 'package:backdown_mobile/features/machines/swarm_screen.dart';
import 'package:backdown_mobile/features/machines/where_screen.dart';
import 'package:backdown_mobile/router.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import '../support/harness.dart';

/// A service from before the swarm: no /api/swarm or /api/pool-*.
class _OlderService extends DemoBackDownApi {
  @override
  Future<Object?> get(String endpoint, [Map<String, Object?> params = const {}, Duration? timeout]) {
    if (endpoint.startsWith('swarm') || endpoint.startsWith('pool-')) {
      throw ApiException(ApiFailure.refused, 'no such endpoint', status: 400);
    }
    return super.get(endpoint, params, timeout);
  }
}

Future<void> _scrollTo(WidgetTester tester, Finder f) async {
  await tester.scrollUntilVisible(f, 300, scrollable: find.byType(Scrollable).first);
  await tester.ensureVisible(f);
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('the swarm: every machine with its drives, free space in total, and the pool', (tester) async {
    final c = await pumpBackDown(tester);
    await goTo(tester, c, Routes.more);
    await tester.tap(find.text('Swarm'));
    await tester.pumpAndSettle();
    expect(find.byType(SwarmScreen), findsOneWidget);
    expect(find.textContaining('free of'), findsWidgets);
    expect(find.textContaining('Across 7 drives on 3 machines'), findsOneWidget);
    expect(find.text('The pool'), findsOneWidget);
    expect(find.text('/srv/photos'), findsOneWidget);
    expect(find.text('/mnt/pool'), findsWidgets);
    await _scrollTo(tester, find.text('this machine'));
    expect(find.text('studio'), findsOneWidget);
    await _scrollTo(tester, find.text('Seagate Expansion'));
    expect(find.text('not connected'), findsOneWidget);
    expect(find.textContaining('last on WD Passport (WDC WD20SDRW-11VUUS0'), findsOneWidget);
  });

  testWidgets('each other machine is asked: one couldn\'t be, the other answered', (tester) async {
    final c = await pumpBackDown(tester);
    await goTo(tester, c, Routes.swarm);
    await _scrollTo(tester, find.text('couldn\'t be asked'));
    expect(find.textContaining('Connection timed out'), findsWidgets);
    await _scrollTo(tester, find.text('reached'));
    expect(find.textContaining('drives as of just now'), findsOneWidget);
    await _scrollTo(tester, find.text('warning'));
    expect(find.text('old-backup'), findsWidgets);
  });

  testWidgets('a machine in the swarm opens its page', (tester) async {
    final c = await pumpBackDown(tester);
    await goTo(tester, c, Routes.swarm);
    await _scrollTo(tester, find.byTooltip('Open attic-pi'));
    await tester.tap(find.byTooltip('Open attic-pi'));
    await tester.pumpAndSettle();
    expect(find.byType(MachineScreen), findsOneWidget);
    expect(find.text('Synced 2 hours ago'), findsOneWidget);
  });

  testWidgets('where is it: a name on every machine, and how each was searched', (tester) async {
    final c = await pumpBackDown(tester);
    await goTo(tester, c, Routes.swarm);
    await tester.tap(find.text('Where is it? A name, on every machine'));
    await tester.pumpAndSettle();
    expect(find.byType(WhereScreen), findsOneWidget);
    await tester.enterText(find.byType(TextField), 'IMG_2100');
    await tester.testTextInput.receiveAction(TextInputAction.search);
    await tester.pumpAndSettle();
    expect(find.text('3 matches for "IMG_2100"'), findsOneWidget);
    expect(find.text('/srv/photos/2014'), findsOneWidget);
    expect(find.text('/media/sam/Seagate Expansion/Photos/2014'), findsOneWidget);
    expect(find.textContaining('WD Passport (not connected)'), findsOneWidget);
    expect(find.textContaining('as synced'), findsWidgets);
    expect(find.textContaining('nextcloud couldn\'t be asked, so its files as synced'), findsOneWidget);
  });

  testWidgets('a hit here opens the file here; one on another machine, its details there', (tester) async {
    final c = await pumpBackDown(tester);
    await goTo(tester, c, MachinePaths.where('IMG_2100'));
    await tester.tap(find.text('/media/sam/Seagate Expansion/Photos/2014'));
    await tester.pumpAndSettle();
    expect(find.byType(FileScreen), findsOneWidget);
    routerOf(c).pop();
    await tester.pumpAndSettle();
    await tester.tap(find.text('/srv/photos/2014'));
    await tester.pumpAndSettle();
    expect(find.byType(MachineFileScreen), findsOneWidget);
    expect(find.textContaining('No bytes come from nextcloud'), findsOneWidget);
  });

  testWidgets('where is it finds what\'s only on the machine that answered', (tester) async {
    final c = await pumpBackDown(tester);
    await goTo(tester, c, MachinePaths.where('re:member'));
    expect(find.text('01 re:member.flac'), findsOneWidget);
    expect(find.textContaining('pi-pool'), findsWidgets);
    expect(find.text('/mnt/pool/Media/Music/Ólafur Arnalds/re:member'), findsWidgets);
  });

  testWidgets('the pool\'s plans, read-only: what\'s in the pool already and where the rest goes', (tester) async {
    final c = await pumpBackDown(tester);
    await goTo(tester, c, Routes.swarm);
    await _scrollTo(tester, find.text('Plans into the pool'));
    await tester.tap(find.text('Plans into the pool'));
    await tester.pumpAndSettle();
    expect(find.byType(PoolPlansScreen), findsOneWidget);
    expect(find.text('Seagate Expansion'), findsOneWidget);
    expect(find.text('WD Passport'), findsOneWidget);
    await tester.tap(find.text('Seagate Expansion'));
    await tester.pumpAndSettle();
    expect(find.byType(PoolPlanScreen), findsOneWidget);
    expect(find.text('in the pool already'), findsOneWidget);
    expect(find.text('to copy'), findsOneWidget);
    expect(find.text('not compared yet'), findsOneWidget);
    expect(find.textContaining('nextcloud didn\'t answer'), findsOneWidget);
    await _scrollTo(tester, find.text('Music: Nils Frahm, Spaces'));
    expect(find.text('attic-pi: /mnt/pool'), findsOneWidget);
    await _scrollTo(tester, find.text('Copy it into the pool'));
    expect(find.textContaining('backdown pool send 2'), findsOneWidget);
  });

  testWidgets('a service from before the swarm says to update it', (tester) async {
    final c = await pumpBackDown(tester, api: _OlderService());
    await goTo(tester, c, Routes.swarm);
    expect(find.text('The swarm needs a newer BackDown'), findsOneWidget);
    await goTo(tester, c, MachinePaths.plans);
    expect(find.text('The pool needs a newer BackDown'), findsOneWidget);
  });

  testWidgets('the swarm reads right on a wide screen', (tester) async {
    final c = await pumpBackDown(tester, size: const Size(1280, 800));
    await goTo(tester, c, Routes.swarm);
    expect(find.byType(SwarmScreen), findsOneWidget);
    expect(tester.takeException(), isNull);
    final card = tester.getSize(find.text('The pool'));
    expect(card.width, lessThan(820));
  });
}
