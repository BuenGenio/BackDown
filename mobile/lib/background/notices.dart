import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart' show Color;
import 'package:flutter_local_notifications/flutter_local_notifications.dart';

import 'check.dart';

/// The small status-bar icon (android/app/src/main/res/drawable-*/ic_stat_backdown.png).
const _icon = 'ic_stat_backdown';

/// BackDown's orange, for the small icon's circle.
const _accent = Color(0xffd8432b);

/// A tap on a notification, for the router to follow (see [attachNotificationTaps] in
/// background.dart): set when one is tapped while the app runs, or when one opened it.
final notificationTaps = ValueNotifier<NoticeTap?>(null);

InitializationSettings _settings() => const InitializationSettings(
  android: AndroidInitializationSettings(_icon),
  // nothing is asked at start: the permission is asked when the person turns notifications on
  iOS: DarwinInitializationSettings(
    requestAlertPermission: false,
    requestBadgePermission: false,
    requestSoundPermission: false,
  ),
);

/// The phone's notifications, through flutter_local_notifications (Android and iOS).
class PhoneNotifications {
  PhoneNotifications._(this._plugin);

  final FlutterLocalNotificationsPlugin _plugin;

  AndroidFlutterLocalNotificationsPlugin? get _android =>
      _plugin.resolvePlatformSpecificImplementation<AndroidFlutterLocalNotificationsPlugin>();

  IOSFlutterLocalNotificationsPlugin? get _ios =>
      _plugin.resolvePlatformSpecificImplementation<IOSFlutterLocalNotificationsPlugin>();

  /// In the app (set up once): taps go to [notificationTaps], and so does the tap that launched it.
  static Future<PhoneNotifications> inApp() => _app ??= _inApp();
  static Future<PhoneNotifications>? _app;

  static Future<PhoneNotifications> _inApp() async {
    final plugin = FlutterLocalNotificationsPlugin();
    await plugin.initialize(
      settings: _settings(),
      onDidReceiveNotificationResponse: (r) => notificationTaps.value = NoticeTap.decode(r.payload),
    );
    final launch = await plugin.getNotificationAppLaunchDetails();
    if (launch?.didNotificationLaunchApp ?? false) {
      notificationTaps.value = NoticeTap.decode(launch?.notificationResponse?.payload);
    }
    return PhoneNotifications._(plugin);
  }

  /// In the background check's isolate: it only shows notifications.
  static Future<PhoneNotifications> inBackground() async {
    final plugin = FlutterLocalNotificationsPlugin();
    await plugin.initialize(settings: _settings());
    return PhoneNotifications._(plugin);
  }

  Future<bool> allowed() async =>
      await _android?.areNotificationsEnabled() ?? (await _ios?.checkPermissions())?.isEnabled ?? false;

  /// Asks for the permission (Android 13 and later, iOS); true when notifications may show.
  Future<bool> requestPermission() async =>
      await _android?.requestNotificationsPermission() ??
      await _ios?.requestPermissions(alert: true, badge: false, sound: true) ??
      false;

  Future<void> openSettings() async =>
      await _android?.openAppNotificationSettings() ?? await _ios?.openAppNotificationSettings();

  Future<void> show(List<PhoneNotice> notices) async {
    for (final n in notices) {
      await _plugin.show(
        id: n.id,
        title: n.title,
        body: n.body,
        payload: n.payload,
        notificationDetails: NotificationDetails(
          android: AndroidNotificationDetails(
            n.channel.id,
            n.channel.title,
            channelDescription: n.channel.description,
            icon: _icon,
            color: _accent,
            // the channel's, as Android keeps it from the first one shown
            importance: n.channel.urgent ? Importance.high : Importance.defaultImportance,
            priority: n.urgent ? Priority.high : Priority.defaultPriority,
            category: n.urgent ? AndroidNotificationCategory.error : AndroidNotificationCategory.status,
            subText: n.serverName,
            groupKey: 'backdown.${n.serverId}',
            styleInformation: BigTextStyleInformation(n.body, contentTitle: n.title, summaryText: n.serverName),
            onlyAlertOnce: true,
          ),
          iOS: DarwinNotificationDetails(threadIdentifier: 'backdown.${n.serverId}', subtitle: n.serverName),
        ),
      );
    }
  }

  /// A notification to show that they work.
  Future<void> showTest() => show([
    PhoneNotice(
      id: noticeId('test', 'test'),
      channel: NoticeChannel.finished,
      title: 'Notifications work',
      body: 'BackDown will tell you here when a job fails or finishes, or a drive gets worse.',
      serverId: 'test',
      serverName: 'BackDown',
      route: '/activity',
    ),
  ]);
}
