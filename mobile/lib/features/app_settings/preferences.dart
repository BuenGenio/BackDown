import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../providers.dart';

/// Light, dark, or as the phone is.
class ThemeModeNotifier extends Notifier<ThemeMode> {
  static const _key = 'theme_mode';

  @override
  ThemeMode build() => switch (ref.watch(prefsProvider).getString(_key)) {
    'light' => ThemeMode.light,
    'dark' => ThemeMode.dark,
    _ => ThemeMode.system,
  };

  Future<void> set(ThemeMode mode) async {
    state = mode;
    await ref.read(prefsProvider).setString(_key, mode.name);
  }
}

final themeModeProvider = NotifierProvider<ThemeModeNotifier, ThemeMode>(ThemeModeNotifier.new);

/// App Lock: the phone's fingerprint, face or screen lock before BackDown shows anything, since a
/// paired phone can read every file on the drives it's connected to.
class AppLockNotifier extends Notifier<bool> {
  static const _key = 'app_lock';

  @override
  bool build() => ref.watch(prefsProvider).getBool(_key) ?? false;

  Future<void> set(bool on) async {
    state = on;
    await ref.read(prefsProvider).setBool(_key, on);
  }
}

final appLockProvider = NotifierProvider<AppLockNotifier, bool>(AppLockNotifier.new);

/// Folders shown as a list or a grid of tiles.
class FolderLayoutNotifier extends Notifier<bool> {
  static const _key = 'folder_grid';

  @override
  bool build() => ref.watch(prefsProvider).getBool(_key) ?? false;

  Future<void> set(bool grid) async {
    state = grid;
    await ref.read(prefsProvider).setBool(_key, grid);
  }
}

final folderGridProvider = NotifierProvider<FolderLayoutNotifier, bool>(FolderLayoutNotifier.new);

/// Hidden files (names starting with a dot) shown in folders.
class ShowHiddenNotifier extends Notifier<bool> {
  static const _key = 'show_hidden';

  @override
  bool build() => ref.watch(prefsProvider).getBool(_key) ?? false;

  Future<void> set(bool show) async {
    state = show;
    await ref.read(prefsProvider).setBool(_key, show);
  }
}

final showHiddenProvider = NotifierProvider<ShowHiddenNotifier, bool>(ShowHiddenNotifier.new);
