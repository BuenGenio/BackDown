import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import 'features/app_settings/lock.dart';
import 'features/app_settings/preferences.dart';
import 'router.dart';
import 'theme.dart';

class BackDownApp extends ConsumerWidget {
  const BackDownApp({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final router = ref.watch(routerProvider);
    return MaterialApp.router(
      title: 'BackDown',
      debugShowCheckedModeBanner: false,
      theme: buildTheme(Brightness.light),
      darkTheme: buildTheme(Brightness.dark),
      themeMode: ref.watch(themeModeProvider),
      routerConfig: router,
      builder: (context, child) => LockGate(child: child ?? const SizedBox.shrink()),
    );
  }
}
