/// The background check's thinking, apart from the plugins: given what a computer's BackDown
/// says now and what the phone remembers from last time, what's news worth a notification.
library;

import 'dart:convert';

import '../api/api.dart';
import '../api/json.dart';
import '../core/format.dart';
import '../features/activity/job_text.dart';
import '../features/health/health_model.dart';
import '../models/daemon.dart';
import 'bg_settings.dart';

/// What the phone remembers about one computer between checks (kept in preferences).
class ServerMemory {
  const ServerMemory({
    this.started,
    this.lastJob = 0,
    this.seenAt,
    this.lastNote = 0,
    this.okAt,
    this.failingSince,
    this.failures = 0,
    this.told = false,
  });

  ServerMemory.fromJson(Json j)
    : started = j.intOrNull('started'),
      lastJob = j.integer('last_job'),
      seenAt = j.intOrNull('seen_at'),
      lastNote = j.integer('last_note'),
      okAt = j.intOrNull('ok_at'),
      failingSince = j.intOrNull('failing_since'),
      failures = j.integer('failures'),
      told = j.flag('told');

  /// When the service started, as it said last time: its job numbers start again when it restarts.
  final int? started;

  /// The highest number of a finished job already looked at, in that run.
  final int lastJob;

  /// The service's clock at its last answer.
  final int? seenAt;

  /// The highest notification id already looked at.
  final int lastNote;

  /// The phone's clock at the last answer; null until it has answered once.
  final int? okAt;

  /// The phone's clock at the first of the checks in a row that couldn't reach it.
  final int? failingSince;
  final int failures;

  /// "Can't reach" was told for this run of failures.
  final bool told;

  Json toJson() => {
    'started': started,
    'last_job': lastJob,
    'seen_at': seenAt,
    'last_note': lastNote,
    'ok_at': okAt,
    'failing_since': failingSince,
    'failures': failures,
    'told': told,
  };

  static String key(String serverId) => 'bg_seen_$serverId';

  static ServerMemory? decode(String? raw) {
    if (raw == null) return null;
    try {
      return ServerMemory.fromJson(asJson(jsonDecode(raw)));
    } on FormatException {
      return null;
    }
  }

  String encode() => jsonEncode(toJson());
}

/// Which Android channel (and kind of news) a notice is. Android fixes a channel's importance when
/// it's first made, so news that should interrupt and news that can wait have channels of their own.
enum NoticeChannel {
  failed('backdown.failed', 'Failed jobs', 'A job on one of your computers stopped with an error', urgent: true),
  finished('backdown.finished', 'Finished jobs', 'A long job on one of your computers is done'),
  health('backdown.health', 'Drive health', 'What BackDown tells about your drives and mirrors', urgent: true),
  connection('backdown.connection', 'Connection', 'When a computer can\'t be reached for a long time');

  const NoticeChannel(this.id, this.title, this.description, {this.urgent = false});
  final String id, title, description;

  /// It may interrupt (heads-up, sound).
  final bool urgent;
}

/// A notification for the phone to show.
class PhoneNotice {
  const PhoneNotice({
    required this.id,
    required this.channel,
    required this.title,
    required this.body,
    required this.serverId,
    required this.serverName,
    required this.route,
    this.urgent = false,
  });

  /// The same news gets the same id, so showing it again replaces it.
  final int id;
  final NoticeChannel channel;
  final String title, body;
  final String serverId, serverName;

  /// Where a tap leads: Activity or Health.
  final String route;
  final bool urgent;

  /// What a tap carries, for [NoticeTap.decode].
  String get payload => jsonEncode({'server': serverId, 'route': route});

  @override
  String toString() => 'PhoneNotice($title: $body)';
}

/// A tap on a notice: which computer, and which tab.
class NoticeTap {
  const NoticeTap(this.serverId, this.route);
  final String serverId, route;

  static NoticeTap? decode(String? payload) {
    if (payload == null || payload.isEmpty) return null;
    try {
      final j = asJson(jsonDecode(payload));
      final route = j.str('route');
      if (!route.startsWith('/')) return null;
      return NoticeTap(j.str('server'), route);
    } on FormatException {
      return null;
    }
  }
}

/// After one check of a computer: what to remember, and what to show.
class CheckOutcome {
  const CheckOutcome(this.memory, this.notices);
  final ServerMemory memory;
  final List<PhoneNotice> notices;
}

/// How long a computer may not answer before the phone says so (when asked to).
const unreachableAfter = Duration(hours: 3);

/// How many notifications from one computer's list are shown one by one; more become one summary.
const maxNoteNotices = 3;

/// A job that's worth a "done": copying, freeing up and rescues always (someone started them), and
/// scans, hashing and syncs that took five minutes or more.
bool isLongJob(Job j) {
  if (!longJobKinds.contains(j.kind)) return false;
  if (const {'apply', 'wipe', 'rescue'}.contains(j.kind)) return true;
  final t = jobTook(j);
  return t != null && t >= 300;
}

/// A stable id for a notice (FNV-1a, 31 bits): the same news replaces itself.
int noticeId(String serverId, String what) {
  var h = 0x811c9dc5;
  for (final c in utf8.encode('$serverId|$what')) {
    h = ((h ^ c) * 0x01000193) & 0xffffffff;
  }
  return h & 0x7fffffff;
}

/// What's new on one computer since the last check. [daemon] and [notifications] are what the
/// service answered (GET /api/daemon and /api/notifications; [notifications] is null when that
/// failed alone); [failure] is why it couldn't be reached at all. [now] is the phone's clock.
///
/// The first time a computer answers nothing is told: what's there already isn't news.
CheckOutcome whatsNew({
  required String serverId,
  required String serverName,
  required ServerMemory? before,
  required BgSettings settings,
  required int now,
  Json? daemon,
  Json? notifications,
  ApiException? failure,
}) {
  if (failure != null || daemon == null) {
    return _unreachable(serverId, serverName, before ?? const ServerMemory(), settings, now, failure);
  }
  final snap = DaemonSnapshot.fromJson(daemon);
  final notes = notifications == null ? null : Notifications.fromJson(notifications).notes;
  final finished = [
    for (final j in snap.recent)
      if (j.finishedAt != null) j,
  ];
  final maxJob = finished.fold(0, (m, j) => j.id > m ? j.id : m);
  final maxNote = notes?.fold(0, (m, n) => n.id > m ? n.id : m);

  final b = before;
  if (b == null || b.okAt == null) {
    // the first answer: remember where things are
    return CheckOutcome(
      ServerMemory(started: snap.startedAt, lastJob: maxJob, seenAt: snap.now, lastNote: maxNote ?? 0, okAt: now),
      const [],
    );
  }

  final out = <PhoneNotice>[];
  PhoneNotice notice(NoticeChannel ch, String what, String title, String body, String route, {bool urgent = false}) =>
      PhoneNotice(
        id: noticeId(serverId, what),
        channel: ch,
        title: title,
        body: body,
        serverId: serverId,
        serverName: serverName,
        route: route,
        urgent: urgent,
      );

  // jobs that finished since: by number within the same run of the service, by time after it restarted
  final sameRun = b.started != null && b.started == snap.startedAt;
  final fresh = [
    for (final j in finished)
      if (sameRun ? j.id > b.lastJob : (b.seenAt == null || j.finishedAt! > b.seenAt!)) j,
  ]..sort((x, y) => x.finishedAt!.compareTo(y.finishedAt!));
  for (final j in fresh) {
    final what = 'job:${snap.startedAt}:${j.id}';
    if (j.failed && settings.failed) {
      out.add(
        notice(
          NoticeChannel.failed,
          what,
          'Failed: ${jobTitle(j)}',
          j.error?.isNotEmpty ?? false ? j.error! : 'It stopped with an error.',
          '/activity',
          urgent: true,
        ),
      );
    } else if (j.state == 'done' && settings.finished && isLongJob(j)) {
      final took = jobTook(j);
      out.add(
        notice(
          NoticeChannel.finished,
          what,
          'Done: ${jobTitle(j)}',
          [if (j.summary.isNotEmpty) j.summary, if (took != null) 'took ${dur(took)}'].join(' · '),
          '/activity',
        ),
      );
    }
  }

  // the service's own notifications, unread and new; a scheduled job's failure is told above already
  if (notes != null && settings.health) {
    final news = [
      for (final n in notes)
        if (n.id > b.lastNote && !n.read && !(n.kind == 'job' && settings.failed)) n,
    ]..sort((x, y) => x.id.compareTo(y.id));
    final one = news.length <= maxNoteNotices + 1;
    for (final n in one ? news : news.take(maxNoteNotices)) {
      out.add(
        notice(
          NoticeChannel.health,
          'note:${n.id}',
          n.title,
          n.body,
          n.kind == 'job' ? '/activity' : '/health',
          urgent: n.level == 'failing',
        ),
      );
    }
    if (!one) {
      final rest = news.length - maxNoteNotices;
      out.add(
        notice(
          NoticeChannel.health,
          'notes:${news.last.id}',
          '$rest more from BackDown',
          news.skip(maxNoteNotices).map((n) => n.title).join('; '),
          '/health',
        ),
      );
    }
  }

  return CheckOutcome(
    ServerMemory(
      started: snap.startedAt,
      lastJob: sameRun ? (maxJob > b.lastJob ? maxJob : b.lastJob) : maxJob,
      seenAt: snap.now ?? b.seenAt,
      lastNote: maxNote == null ? b.lastNote : (maxNote > b.lastNote ? maxNote : b.lastNote),
      okAt: now,
    ),
    out,
  );
}

CheckOutcome _unreachable(
  String serverId,
  String serverName,
  ServerMemory b,
  BgSettings settings,
  int now,
  ApiException? failure,
) {
  final failures = b.failures + 1;
  final since = b.failingSince ?? now;
  final lastOk = b.okAt ?? since;
  var told = b.told;
  final notices = <PhoneNotice>[];
  if (settings.unreachable && !told && failures >= 2 && now - lastOk >= unreachableAfter.inSeconds) {
    told = true;
    final key = failure?.failure == ApiFailure.unauthorized;
    notices.add(
      PhoneNotice(
        id: noticeId(serverId, 'unreachable'),
        channel: NoticeChannel.connection,
        title: key ? '$serverName doesn\'t know this phone any more' : 'Can\'t reach $serverName',
        body: key
            ? 'Its BackDown no longer accepts this phone: on the computer, run `backdown token --qr` and scan the '
                  'code again.'
            : 'BackDown on $serverName hasn\'t answered for ${dur(now - lastOk)}: the computer may be off or asleep, '
                  'or this phone isn\'t on its network or tailnet.',
        serverId: serverId,
        serverName: serverName,
        route: '/activity',
      ),
    );
  }
  return CheckOutcome(
    ServerMemory(
      started: b.started,
      lastJob: b.lastJob,
      seenAt: b.seenAt,
      lastNote: b.lastNote,
      okAt: b.okAt,
      failingSince: since,
      failures: failures,
      told: told,
    ),
    notices,
  );
}
