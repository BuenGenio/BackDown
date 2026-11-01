import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:workmanager/workmanager.dart';

import 'notices.dart';
import 'runner.dart';

/// What the background check needs from the phone: scheduling, and notifications with their
/// permission. Android and iOS have it; elsewhere (Linux, tests) nothing runs in the background.
abstract class BackgroundPlatform {
  /// Background checks can run here.
  bool get supported;

  /// iOS decides itself when background work runs.
  bool get isIos;

  /// Notifications may show (permission given, not turned off for the app).
  Future<bool> allowed();

  /// Asks for the notification permission (Android 13 and later, iOS); true when given.
  Future<bool> requestPermission();

  /// The phone's notification settings for BackDown.
  Future<void> openSettings();

  /// Checks every [everyMinutes] (as far as the phone allows), or stops checking (0).
  Future<void> schedule(int everyMinutes);

  Future<void> showTest();

  /// The plugins on Android and iOS; nothing anywhere else.
  static BackgroundPlatform forThisDevice() {
    if (kIsWeb) return const NoBackground();
    if (Platform.isAndroid || Platform.isIOS) return PluginBackground(ios: Platform.isIOS);
    return const NoBackground();
  }
}

/// Where nothing runs in the background (Linux, desktop, tests).
class NoBackground implements BackgroundPlatform {
  const NoBackground();
  @override
  bool get supported => false;
  @override
  bool get isIos => false;
  @override
  Future<bool> allowed() async => false;
  @override
  Future<bool> requestPermission() async => false;
  @override
  Future<void> openSettings() async {}
  @override
  Future<void> schedule(int everyMinutes) async {}
  @override
  Future<void> showTest() async {}
}

/// WorkManager (Android) or a BGAppRefreshTask (iOS) through the workmanager plugin, and
/// flutter_local_notifications.
class PluginBackground implements BackgroundPlatform {
  PluginBackground({required this.ios});

  final bool ios;

  @override
  bool get supported => true;

  @override
  bool get isIos => ios;

  Future<PhoneNotifications> get _n => PhoneNotifications.inApp();

  @override
  Future<bool> allowed() async => (await _n).allowed();

  @override
  Future<bool> requestPermission() async => (await _n).requestPermission();

  @override
  Future<void> openSettings() async => (await _n).openSettings();

  @override
  Future<void> showTest() async => (await _n).showTest();

  @override
  Future<void> schedule(int everyMinutes) => scheduleBackgroundCheck(everyMinutes, ios: ios);
}

/// Registers (or, with 0, cancels) the periodic check. Android runs it at most every 15 minutes,
/// with a network; iOS when it sees fit, no sooner than asked.
Future<void> scheduleBackgroundCheck(int everyMinutes, {required bool ios}) async {
  final wm = Workmanager();
  if (everyMinutes <= 0) {
    await wm.cancelByUniqueName(ios ? iosRefreshTask : androidPeriodicName);
    return;
  }
  final every = Duration(minutes: everyMinutes < 15 ? 15 : everyMinutes);
  if (ios) {
    // iOS knows the job only by its identifier, which Info.plist permits
    await wm.registerPeriodicTask(iosRefreshTask, iosRefreshTask, frequency: every, initialDelay: every);
    return;
  }
  await wm.registerPeriodicTask(
    androidPeriodicName,
    backgroundTask,
    frequency: every,
    constraints: Constraints(networkType: NetworkType.connected),
    existingWorkPolicy: ExistingPeriodicWorkPolicy.update,
    tag: 'backdown',
  );
}

/// The phone's background checks: replaced in tests.
final backgroundPlatformProvider = Provider<BackgroundPlatform>((ref) => BackgroundPlatform.forThisDevice());
