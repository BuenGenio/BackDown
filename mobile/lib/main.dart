import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'app.dart';
import 'background/background.dart';
import 'providers.dart';
import 'servers/server_store.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  final prefs = await SharedPreferences.getInstance();
  final store = await ServerStore.open(prefs, SecureTokenVault());
  await initBackground(prefs);
  runApp(
    ProviderScope(
      // a failed request is shown with a "Try again" button; nothing retries behind the person's back
      retry: (_, _) => null,
      overrides: [prefsProvider.overrideWithValue(prefs), serverStoreProvider.overrideWithValue(store)],
      child: const BackDownApp(),
    ),
  );
}
