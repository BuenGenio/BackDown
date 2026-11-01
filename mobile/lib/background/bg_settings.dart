import 'package:shared_preferences/shared_preferences.dart';

/// What the background check tells this phone about, and how often it looks (in the app's
/// preferences, which the background isolate reads too).
class BgSettings {
  const BgSettings({
    this.everyMinutes = 0,
    this.failed = true,
    this.finished = true,
    this.health = true,
    this.unreachable = false,
  });

  /// How often to look: 15 minutes (Android's shortest), an hour, six hours; 0 is off.
  final int everyMinutes;

  /// A job failed.
  final bool failed;

  /// A long job finished (a scan, hashing, copying, freeing up, a rescue, a sync).
  final bool finished;

  /// The service has news about a drive or a mirror (its own notifications, with their text).
  final bool health;

  /// A computer hasn't answered for a long time.
  final bool unreachable;

  static const choices = [0, 15, 60, 360];

  bool get on => everyMinutes > 0;

  static const _every = 'bg_every', _failed = 'bg_failed', _finished = 'bg_finished';
  static const _health = 'bg_health', _unreachable = 'bg_unreachable';

  static BgSettings read(SharedPreferences p) {
    final every = p.getInt(_every) ?? 0;
    return BgSettings(
      everyMinutes: choices.contains(every) ? every : 60,
      failed: p.getBool(_failed) ?? true,
      finished: p.getBool(_finished) ?? true,
      health: p.getBool(_health) ?? true,
      unreachable: p.getBool(_unreachable) ?? false,
    );
  }

  Future<void> write(SharedPreferences p) async {
    await p.setInt(_every, everyMinutes);
    await p.setBool(_failed, failed);
    await p.setBool(_finished, finished);
    await p.setBool(_health, health);
    await p.setBool(_unreachable, unreachable);
  }

  BgSettings copyWith({int? everyMinutes, bool? failed, bool? finished, bool? health, bool? unreachable}) => BgSettings(
    everyMinutes: everyMinutes ?? this.everyMinutes,
    failed: failed ?? this.failed,
    finished: finished ?? this.finished,
    health: health ?? this.health,
    unreachable: unreachable ?? this.unreachable,
  );

  /// "15 minutes", "hour", "6 hours": how often, after "every".
  static String every(int minutes) => switch (minutes) {
    60 => 'hour',
    360 => '6 hours',
    _ => '$minutes minutes',
  };
}
