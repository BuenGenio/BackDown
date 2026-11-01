/// The background check: every so often (as far as the phone allows), each saved computer is asked
/// what it's doing and what it has to tell, and what's new since last time becomes a notification:
/// a failed job, a long job finished, the service's drive-health news, and (if asked) a computer
/// that hasn't answered for hours. WorkManager runs it on Android; on iOS it's a BGAppRefreshTask,
/// which iOS runs when it sees fit; elsewhere nothing runs in the background.
library;

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';
import 'package:shared_preferences/shared_preferences.dart';
import 'package:workmanager/workmanager.dart';

import '../core/widgets.dart';
import '../providers.dart';
import '../theme.dart';
import 'bg_settings.dart';
import 'notices.dart';
import 'platform.dart';
import 'runner.dart';

export 'bg_settings.dart' show BgSettings;
export 'platform.dart' show BackgroundPlatform, backgroundPlatformProvider;

/// Sets up the background check at start: the notifications (and the tap that opened the app, if
/// one did), the dispatcher, and the schedule as the settings say. Never keeps the app from
/// starting.
Future<void> initBackground(SharedPreferences prefs) async {
  final platform = BackgroundPlatform.forThisDevice();
  if (!platform.supported) return;
  try {
    await PhoneNotifications.inApp();
    await Workmanager().initialize(backgroundDispatcher);
    await platform.schedule(BgSettings.read(prefs).everyMinutes);
  } on Object catch (e) {
    debugPrint('Background check unavailable: $e');
  }
}

/// Follows taps on the background check's notifications: shows the computer it's about and opens
/// its tab (Activity or Health). For the router to call once it's made.
void attachNotificationTaps(Ref ref, GoRouter router) {
  void follow() {
    final tap = notificationTaps.value;
    if (tap == null) return;
    notificationTaps.value = null;
    final servers = ref.read(serversProvider);
    if (tap.serverId != servers.currentId && servers.servers.any((s) => s.id == tap.serverId)) {
      unawaited(ref.read(serversProvider.notifier).select(tap.serverId));
    }
    router.go(tap.route);
  }

  notificationTaps.addListener(follow);
  ref.onDispose(() => notificationTaps.removeListener(follow));
  // the tap that launched the app, once the first frame has a router to go with
  if (notificationTaps.value != null) WidgetsBinding.instance.addPostFrameCallback((_) => follow());
}

/// The background check's settings, saved in preferences and applied to the schedule.
class BgSettingsNotifier extends Notifier<BgSettings> {
  @override
  BgSettings build() => BgSettings.read(ref.watch(prefsProvider));

  Future<void> set(BgSettings s) async {
    final before = state;
    state = s;
    await s.write(ref.read(prefsProvider));
    if (s.everyMinutes != before.everyMinutes) {
      try {
        await ref.read(backgroundPlatformProvider).schedule(s.everyMinutes);
      } on Object catch (e) {
        debugPrint('Background check not scheduled: $e');
      }
    }
  }
}

final bgSettingsProvider = NotifierProvider<BgSettingsNotifier, BgSettings>(BgSettingsNotifier.new);

/// The notification settings, in App settings: how often to check, what to tell, a test.
class NotificationSettingsSection extends ConsumerStatefulWidget {
  const NotificationSettingsSection({super.key});

  @override
  ConsumerState<NotificationSettingsSection> createState() => _NotificationSettingsSectionState();
}

class _NotificationSettingsSectionState extends ConsumerState<NotificationSettingsSection> {
  /// Whether the phone lets BackDown notify; null until asked.
  bool? _allowed;

  @override
  void initState() {
    super.initState();
    final p = ref.read(backgroundPlatformProvider);
    if (p.supported && ref.read(bgSettingsProvider).on) {
      unawaited(p.allowed().then((v) => mounted ? setState(() => _allowed = v) : null));
    }
  }

  Future<void> _setEvery(BgSettings s, int minutes) async {
    final p = ref.read(backgroundPlatformProvider);
    if (minutes > 0 && !s.on) {
      // turning it on: the phone's permission first
      final ok = await p.requestPermission();
      if (!mounted) return;
      setState(() => _allowed = ok);
      if (!ok) {
        showSnack(context, 'BackDown can\'t notify: notifications are off for it in the phone\'s settings');
        return;
      }
    }
    await ref.read(bgSettingsProvider.notifier).set(s.copyWith(everyMinutes: minutes));
  }

  @override
  Widget build(BuildContext context) {
    final p = ref.watch(backgroundPlatformProvider);
    final s = ref.watch(bgSettingsProvider);
    final muted = context.text.bodySmall?.copyWith(color: context.bd.muted);
    if (!p.supported) {
      return const Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          SectionHeader('Notifications'),
          ListTile(
            leading: Icon(Icons.notifications_off_outlined),
            title: Text('Not on this device'),
            subtitle: Text(
              'On Android and iOS, BackDown checks your computers in the background and tells you when a job '
              'fails or finishes, or a drive gets worse.',
            ),
          ),
        ],
      );
    }
    final how = !s.on
        ? 'Off: nothing is checked in the background.'
        : p.isIos
        ? 'iOS decides when background checks run, from how you use BackDown: at most every '
              '${BgSettings.every(s.everyMinutes)}, often less.'
        : 'Checks every ${BgSettings.every(s.everyMinutes)}, as far as the phone allows: Android may wait longer '
              'to save battery, and only checks with a network.';
    void set(BgSettings next) => unawaited(ref.read(bgSettingsProvider.notifier).set(next));
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        const SectionHeader('Notifications'),
        Padding(
          padding: const EdgeInsets.fromLTRB(16, 4, 16, 0),
          child: Text('Check your computers', style: context.text.titleSmall),
        ),
        Padding(
          padding: const EdgeInsets.fromLTRB(16, 8, 16, 6),
          child: SizedBox(
            width: double.infinity,
            child: SegmentedButton<int>(
              showSelectedIcon: false,
              segments: const [
                ButtonSegment(value: 0, label: Text('Off')),
                ButtonSegment(value: 15, label: Text('15 min')),
                ButtonSegment(value: 60, label: Text('1 hour')),
                ButtonSegment(value: 360, label: Text('6 hours')),
              ],
              selected: {s.everyMinutes},
              onSelectionChanged: (v) => _setEvery(s, v.first),
            ),
          ),
        ),
        Padding(
          padding: const EdgeInsets.fromLTRB(16, 0, 16, 4),
          child: Text(how, style: muted),
        ),
        if (s.on && _allowed == false)
          Padding(
            padding: const EdgeInsets.fromLTRB(16, 4, 8, 0),
            child: Row(
              children: [
                Icon(Icons.notifications_off_outlined, size: 18, color: context.bd.warn),
                const SizedBox(width: 8),
                Expanded(
                  child: Text(
                    'Notifications are off for BackDown in the phone\'s settings.',
                    style: context.text.bodySmall?.copyWith(color: context.bd.warn),
                  ),
                ),
                TextButton(onPressed: p.openSettings, child: const Text('Settings')),
              ],
            ),
          ),
        if (s.on) ...[
          SwitchListTile(
            title: const Text('A job failed'),
            subtitle: const Text('A scan, a sync, copying or freeing up that stopped with an error'),
            value: s.failed,
            onChanged: (v) => set(s.copyWith(failed: v)),
          ),
          SwitchListTile(
            title: const Text('A long job finished'),
            subtitle: const Text('Scans, hashing and syncs that took a while; copying, freeing up and rescues'),
            value: s.finished,
            onChanged: (v) => set(s.copyWith(finished: v)),
          ),
          SwitchListTile(
            title: const Text('Drive health news'),
            subtitle: const Text('What BackDown tells about your drives and mirrors, in its own words'),
            value: s.health,
            onChanged: (v) => set(s.copyWith(health: v)),
          ),
          SwitchListTile(
            title: const Text('A computer can\'t be reached'),
            subtitle: const Text('When one hasn\'t answered for three hours'),
            value: s.unreachable,
            onChanged: (v) => set(s.copyWith(unreachable: v)),
          ),
          Padding(
            padding: const EdgeInsets.fromLTRB(16, 4, 16, 8),
            child: OutlinedButton.icon(
              icon: const Icon(Icons.notifications_active_outlined),
              label: const Text('Send a test notification'),
              onPressed: () async {
                await p.showTest();
                if (context.mounted) showSnack(context, 'Sent: it should be in the phone\'s notifications now');
              },
            ),
          ),
        ],
      ],
    );
  }
}
