// The real app, against the BackDown service on this computer, screen by screen: what the person
// would see. Needs a running service; the token is read with `backdown token`, so it never goes on
// a command line. Screenshots go to $LIVE_SHOTS_DIR (keep them out of the repo: they show real
// files).
//
//   LIVE_SHOTS_DIR=/somewhere flutter test integration_test/live_app_test.dart -d linux
import 'dart:io';
import 'dart:ui' as ui;

import 'package:backdown_mobile/app.dart';
import 'package:backdown_mobile/providers.dart';
import 'package:backdown_mobile/router.dart';
import 'package:backdown_mobile/servers/server.dart';
import 'package:backdown_mobile/servers/server_store.dart';
import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:shared_preferences/shared_preferences.dart';

final _out = Platform.environment['LIVE_SHOTS_DIR'] ?? '${Directory.systemTemp.path}/backdown-live-shots';
final _url = Platform.environment['BACKDOWN_URL'] ?? 'http://127.0.0.1:8787';

Future<String> _token() async {
  final r = await Process.run(Platform.environment['BACKDOWN_BIN'] ?? 'backdown', ['token']);
  return r.stdout.toString().trim();
}

/// Pumps frames in real time until [f] finds something (or [timeout]).
Future<void> _until(WidgetTester t, Finder f, {Duration timeout = const Duration(seconds: 30)}) async {
  final end = DateTime.now().add(timeout);
  while (DateTime.now().isBefore(end)) {
    await t.pump(const Duration(milliseconds: 200));
    if (f.evaluate().isNotEmpty) return;
  }
}

/// Lets the screen's requests land: frames for [d] of real time.
Future<void> _settle(WidgetTester t, [Duration d = const Duration(seconds: 3)]) async {
  final end = DateTime.now().add(d);
  while (DateTime.now().isBefore(end)) {
    await t.pump(const Duration(milliseconds: 100));
  }
}

Future<void> _shot(WidgetTester t, String slug) async {
  final view = t.binding.renderViews.first;
  final layer = view.debugLayer! as OffsetLayer;
  final image = await layer.toImage(view.paintBounds);
  final bytes = await image.toByteData(format: ui.ImageByteFormat.png);
  final f = File('$_out/$slug.png')..parent.createSync(recursive: true);
  f.writeAsBytesSync(bytes!.buffer.asUint8List());
}

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  testWidgets('every screen against the real service', (t) async {
    SharedPreferences.setMockInitialValues({});
    final prefs = await SharedPreferences.getInstance();
    final store = await ServerStore.open(prefs, MemoryTokenVault());
    final url = Uri.parse(_url);
    await store.add(ServerEntry(id: 'live', name: 'This computer', url: url), await _token());
    await store.select('live');
    await t.pumpWidget(
      ProviderScope(
        retry: (_, _) => null,
        overrides: [prefsProvider.overrideWithValue(prefs), serverStoreProvider.overrideWithValue(store)],
        child: const BackDownApp(),
      ),
    );
    final router = ProviderScope.containerOf(t.element(find.byType(BackDownApp))).read(routerProvider);
    Future<void> visit(
      String location,
      String slug, {
      Finder? wait,
      Duration settle = const Duration(seconds: 3),
    }) async {
      if ([Routes.drives, Routes.search, Routes.activity, Routes.health, Routes.more].contains(location)) {
        router.go(location);
      } else {
        // ignore: unawaited_futures
        router.push(location);
      }
      if (wait != null) await _until(t, wait, timeout: const Duration(minutes: 2));
      await _settle(t, settle);
      await _shot(t, slug);
    }

    await _until(t, find.text('DRIVES'));
    await _settle(t);
    await _shot(t, '01-drives');
    for (final (route, slug) in [
      (Routes.activity, '02-activity'),
      (Routes.health, '03-health'),
      (Routes.more, '04-more'),
      (Routes.serverInfo, '05-server'),
      (Routes.entities, '06-entities'),
      (Routes.dupes, '07-dupes'),
      (Routes.machines, '08-machines'),
      (Routes.swarm, '09-swarm'),
      (Routes.plans, '10-plans'),
    ]) {
      await visit(route, slug, settle: const Duration(seconds: 6));
      if (router.canPop()) router.pop();
    }
    // a drive, then its biggest folder
    router.go(Routes.drives);
    await _settle(t);
    await t.tap(find.byType(ListTile).first);
    await _until(t, find.byIcon(Icons.sort));
    await _settle(t);
    await _shot(t, '11-folder');
    // search every drive (slow on a big index)
    router.go(Routes.search);
    await _settle(t, const Duration(seconds: 1));
    await t.enterText(find.byType(TextField).first, 'readme');
    await _until(t, find.textContaining('match'), timeout: const Duration(minutes: 3));
    await _settle(t);
    await _shot(t, '12-search');
  }, timeout: const Timeout(Duration(minutes: 15)));
}
