import 'dart:async';
import 'dart:io';
import 'dart:ui' show DartPluginRegistrant;

import 'package:flutter/foundation.dart';
import 'package:shared_preferences/shared_preferences.dart';
import 'package:workmanager/workmanager.dart';

import '../api/api.dart';
import '../api/http_api.dart';
import '../api/json.dart';
import '../core/format.dart';
import '../servers/server.dart';
import '../servers/server_store.dart';
import 'bg_settings.dart';
import 'check.dart';
import 'notices.dart';

/// The Dart task every background check runs as (Android's WorkManager).
const backgroundTask = 'backdown.check';

/// Android: the unique name of the periodic check.
const androidPeriodicName = 'backdown.check.periodic';

/// iOS: the BGAppRefreshTask the check runs as. The same string must be in Info.plist
/// (`BGTaskSchedulerPermittedIdentifiers`) and in AppDelegate.swift; iOS hands it to the
/// dispatcher as the task's name.
const iosRefreshTask = 'io.backdown.app.check';

/// When the check last ran (the phone's clock).
const _lastRunKey = 'bg_last_run';

/// How long one computer may take to answer in the background (iOS gives the whole job ~30 s).
const _answerWithin = Duration(seconds: 12);

/// Where background jobs start (WorkManager on Android, a BGAppRefreshTask on iOS): every job is
/// the check.
@pragma('vm:entry-point')
void backgroundDispatcher() {
  Workmanager().executeTask((task, input) async {
    try {
      DartPluginRegistrant.ensureInitialized();
      await runBackgroundCheck();
    } on Object catch (e) {
      // the next run tries again; retrying at once rarely helps
      debugPrint('Background check failed: $e');
    }
    return true;
  });
}

/// One check of every saved computer (not the demo): asks each what it's doing and what it has to
/// tell, compares with what the phone saw last time, shows what's new, and remembers. Returns how
/// many notifications it showed. Every argument is for tests; the defaults are the real thing.
///
/// With [keepInterval] (iOS, which runs the task as often as it likes once it's registered), a run
/// sooner than the chosen interval after the last one does nothing; Android's WorkManager keeps
/// the interval itself.
Future<int> runBackgroundCheck({
  SharedPreferences? prefs,
  TokenVault? vault,
  BackDownApi Function(ServerEntry server, String token)? connect,
  Future<void> Function(List<PhoneNotice> notices)? show,
  bool? keepInterval,
}) async {
  final p = prefs ?? await SharedPreferences.getInstance();
  await p.reload();
  final settings = BgSettings.read(p);
  if (!settings.on) return 0;
  final now = nowSeconds();
  final last = p.getInt(_lastRunKey);
  // a couple of minutes' grace: the phone rounds its schedule
  if ((keepInterval ?? (!kIsWeb && Platform.isIOS)) && last != null && now - last < settings.everyMinutes * 60 - 120) {
    return 0;
  }
  await p.setInt(_lastRunKey, now);
  final store = await ServerStore.open(p, vault ?? SecureTokenVault());
  final servers = [
    for (final s in store.servers)
      if (!s.demo && store.tokenOf(s.id) != null) s,
  ];
  // forget computers that were removed
  for (final k in p.getKeys().where((k) => k.startsWith('bg_seen_')).toList()) {
    if (!servers.any((s) => ServerMemory.key(s.id) == k)) await p.remove(k);
  }
  final shown = show ?? (await PhoneNotifications.inBackground()).show;
  var count = 0;
  for (final s in servers) {
    final token = store.tokenOf(s.id)!;
    final api = connect?.call(s, token) ?? HttpBackDownApi(s.url, token);
    try {
      final outcome = await checkServer(s, api, ServerMemory.decode(p.getString(ServerMemory.key(s.id))), settings);
      await p.setString(ServerMemory.key(s.id), outcome.memory.encode());
      if (outcome.notices.isNotEmpty) await shown(outcome.notices);
      count += outcome.notices.length;
    } on Object catch (e) {
      debugPrint('Background check of ${s.name} failed: $e'); // the others are still checked
    } finally {
      api.close();
    }
  }
  return count;
}

/// One computer: its snapshot and notifications, against what was seen before.
Future<CheckOutcome> checkServer(ServerEntry s, BackDownApi api, ServerMemory? before, BgSettings settings) async {
  Json? daemon, notes;
  ApiException? failure;
  try {
    daemon = asJson(await api.get('daemon', const {}, _answerWithin));
    try {
      notes = asJson(await api.get('notifications', {'limit': 50}, _answerWithin));
    } on ApiException {
      notes = null; // an older service, or a hiccup: the jobs are still worth telling
    }
  } on ApiException catch (e) {
    failure = e;
  }
  return whatsNew(
    serverId: s.id,
    serverName: s.name,
    before: before,
    settings: settings,
    now: nowSeconds(),
    daemon: daemon,
    notifications: notes,
    failure: failure,
  );
}
