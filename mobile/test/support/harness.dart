import 'dart:async';

import 'package:backdown_mobile/api/api.dart';
import 'package:backdown_mobile/api/demo_api.dart';
import 'package:backdown_mobile/app.dart';
import 'package:backdown_mobile/providers.dart';
import 'package:backdown_mobile/router.dart';
import 'package:backdown_mobile/servers/server.dart';
import 'package:backdown_mobile/servers/server_store.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_riverpod/misc.dart' show Override;
import 'package:flutter_test/flutter_test.dart';
import 'package:go_router/go_router.dart';
import 'package:shared_preferences/shared_preferences.dart';

const phone = Size(412, 892);

/// Pumps the whole app against the demo service (or [api]), with preferences and tokens in memory.
/// Returns the provider container, to read the router or the API from.
Future<ProviderContainer> pumpBackDown(
  WidgetTester tester, {
  String? location,
  Size size = phone,
  BackDownApi? api,
  bool withServer = true,
  bool canChange = false,
  Map<String, Object> prefs = const {},
  List<Override> overrides = const [],
}) async {
  tester.view
    ..physicalSize = size * 2
    ..devicePixelRatio = 2;
  addTearDown(tester.view.reset);
  SharedPreferences.setMockInitialValues(prefs);
  final p = await SharedPreferences.getInstance();
  final store = await ServerStore.open(p, MemoryTokenVault());
  if (withServer) {
    await store.add(
      ServerEntry(id: 'demo', name: 'Studio', url: Uri.parse('http://demo.backdown.invalid:8787'), demo: true),
      null,
    );
    await store.select('demo');
  }
  final client = api ?? DemoBackDownApi(canChange: canChange);
  addTearDown(client.close);
  await tester.pumpWidget(
    ProviderScope(
      retry: (_, _) => null,
      overrides: [
        prefsProvider.overrideWithValue(p),
        serverStoreProvider.overrideWithValue(store),
        if (withServer) apiProvider.overrideWithValue(client),
        ...overrides,
      ],
      child: const BackDownApp(),
    ),
  );
  await tester.pumpAndSettle();
  final container = ProviderScope.containerOf(tester.element(find.byType(BackDownApp)));
  if (location != null) await goTo(tester, container, location);
  return container;
}

GoRouter routerOf(ProviderContainer c) => c.read(routerProvider);

/// Navigates like a tap would (go for tabs, push for the rest) and lets the screen load.
Future<void> goTo(WidgetTester tester, ProviderContainer c, String location, {Object? extra}) async {
  final router = routerOf(c);
  final tab = [Routes.drives, Routes.search, Routes.activity, Routes.health, Routes.more].contains(location);
  if (tab) {
    router.go(location, extra: extra);
  } else {
    // from the tab it belongs to, so the bar shows that tab selected
    final home = [Routes.more, Routes.health, Routes.activity, Routes.search].where(location.startsWith).firstOrNull;
    if (home != null) {
      router.go(home);
      await tester.pumpAndSettle();
    }
    unawaited(router.push<void>(location, extra: extra));
  }
  await tester.pumpAndSettle();
}
