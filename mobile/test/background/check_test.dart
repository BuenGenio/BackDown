import 'package:backdown_mobile/api/api.dart';
import 'package:backdown_mobile/api/json.dart';
import 'package:backdown_mobile/background/bg_settings.dart';
import 'package:backdown_mobile/background/check.dart';
import 'package:flutter_test/flutter_test.dart';

const _started = 1_790_000_000;
const _t0 = 1_793_500_000;

Json _job(
  int id,
  String kind,
  String state, {
  String? path,
  int took = 60,
  int finished = _t0,
  String summary = '',
  String? error,
}) => {
  'id': id,
  'kind': kind,
  'path': path,
  'state': state,
  'summary': summary,
  'error': error,
  'reason': 'scheduled',
  'auto': true,
  'queued_at': finished - took - 5,
  'started_at': finished - took,
  'finished_at': finished,
};

Json _daemon(List<Json> recent, {int now = _t0, int started = _started}) => {
  'running': true,
  'version': '1.0.0',
  'change': 9,
  'now': now,
  'started_at': started,
  'current': null,
  'side': null,
  'queued': <Json>[],
  'recent': recent,
  'drives': <Json>[],
};

Json _note(int id, {String kind = 'health', String level = 'warning', bool read = false, String? title}) => {
  'id': id,
  'at': _t0,
  'kind': kind,
  'level': level,
  'title': title ?? 'Drive $id is getting worse',
  'body': 'Note $id. Make sure what\'s on it has another copy.',
  'drive': 'sn:$id',
  'shown': true,
  'read': read,
};

Json _notes(List<Json> notes) => {'notes': notes, 'unread': notes.where((n) => n['read'] != true).length};

const _on = BgSettings(everyMinutes: 60);

CheckOutcome _check(
  ServerMemory? before, {
  Json? daemon,
  Json? notes,
  ApiException? failure,
  BgSettings settings = _on,
  int now = _t0,
}) => whatsNew(
  serverId: 's1',
  serverName: 'Studio',
  before: before,
  settings: settings,
  now: now,
  daemon: daemon,
  notifications: notes,
  failure: failure,
);

void main() {
  final offline = ApiException(ApiFailure.offline, 'Can\'t reach studio');

  group('the first answer', () {
    test('tells nothing: what is there already isn\'t news', () {
      final r = _check(
        null,
        daemon: _daemon([_job(3, 'index', 'failed', error: 'boom')]),
        notes: _notes([_note(5)]),
      );
      expect(r.notices, isEmpty);
      expect(r.memory.lastJob, 3);
      expect(r.memory.lastNote, 5);
      expect(r.memory.started, _started);
      expect(r.memory.okAt, _t0);
    });

    test('after only failures is still the first answer', () {
      final failed = _check(null, failure: offline).memory;
      final r = _check(
        failed,
        daemon: _daemon([_job(3, 'index', 'failed', error: 'boom')]),
        notes: _notes([]),
      );
      expect(r.notices, isEmpty);
    });
  });

  group('jobs', () {
    final seen = _check(null, daemon: _daemon([_job(3, 'index', 'done')]), notes: _notes([])).memory;

    test('a job that failed since', () {
      final r = _check(
        seen,
        daemon: _daemon([
          _job(4, 'remote-sync', 'failed', path: 'nextcloud', error: 'ssh: Connection timed out'),
          _job(3, 'index', 'done'),
        ]),
      );
      expect(r.notices, hasLength(1));
      expect(r.notices.single.title, 'Failed: Sync the machine nextcloud');
      expect(r.notices.single.body, 'ssh: Connection timed out');
      expect(r.notices.single.route, '/activity');
      expect(r.notices.single.channel, NoticeChannel.failed);
      expect(r.notices.single.urgent, isTrue);
      expect(r.memory.lastJob, 4);
      // and not again
      expect(_check(r.memory, daemon: _daemon([_job(4, 'remote-sync', 'failed')])).notices, isEmpty);
    });

    test('a long scan finished; a quick one isn\'t news', () {
      final r = _check(
        seen,
        daemon: _daemon([
          _job(5, 'index', 'done', path: '/media/sam/Seagate', took: 12, summary: 'nothing changed'),
          _job(4, 'index', 'done', path: '/media/sam/WD', took: 640, summary: '24 files, 850.9 MB'),
        ]),
      );
      expect(r.notices.map((n) => n.title), ['Done: Scan WD']);
      expect(r.notices.single.body, '24 files, 850.9 MB · took 11 min');
    });

    test('copying, freeing up and rescues are news however quick', () {
      final r = _check(
        seen,
        daemon: _daemon([_job(4, 'wipe', 'done', path: 'plan 3', took: 20), _job(5, 'entities', 'done', took: 900)]),
      );
      expect(r.notices.map((n) => n.title), ['Done: Free up (plan 3)']);
    });

    test('stopped jobs aren\'t news', () {
      expect(_check(seen, daemon: _daemon([_job(4, 'hash', 'stopped', took: 900)])).notices, isEmpty);
    });

    test('the kinds can be turned off', () {
      final r = _check(
        seen,
        settings: _on.copyWith(failed: false, finished: false),
        daemon: _daemon([_job(4, 'hash', 'failed', error: 'x'), _job(5, 'apply', 'done')]),
      );
      expect(r.notices, isEmpty);
    });

    test('after the service restarted, its new jobs are told by time', () {
      // job numbers start again: #2 is new although 3 was seen before the restart
      final r = _check(
        seen,
        daemon: _daemon(
          [
            _job(2, 'hash', 'failed', finished: _t0 + 900, error: 'disk gone'),
            _job(1, 'index', 'done', finished: _t0 - 5),
          ],
          started: _started + 1000,
          now: _t0 + 1000,
        ),
      );
      expect(r.notices.map((n) => n.body), ['disk gone']);
      expect(r.memory.started, _started + 1000);
      expect(r.memory.lastJob, 2);
    });

    test('a running `backdown serve` has no jobs to tell', () {
      final r = _check(seen, daemon: {'running': false}, notes: _notes([]));
      expect(r.notices, isEmpty);
      expect(r.memory.okAt, _t0);
    });
  });

  group('the service\'s notifications', () {
    final seen = _check(null, daemon: _daemon([]), notes: _notes([_note(5, read: true)])).memory;

    test('new unread ones, with their words', () {
      final r = _check(seen, daemon: _daemon([]), notes: _notes([_note(7), _note(6, read: true), _note(5)]));
      expect(r.notices, hasLength(1));
      expect(r.notices.single.title, 'Drive 7 is getting worse');
      expect(r.notices.single.body, 'Note 7. Make sure what\'s on it has another copy.');
      expect(r.notices.single.route, '/health');
      expect(r.notices.single.channel, NoticeChannel.health);
      expect(r.memory.lastNote, 7);
    });

    test('a failing drive is urgent', () {
      final r = _check(
        seen,
        daemon: _daemon([]),
        notes: _notes([_note(6, level: 'failing')]),
      );
      expect(r.notices.single.urgent, isTrue);
    });

    test('a failed scheduled job is told once: as the job, unless those are off', () {
      final notes = _notes([_note(6, kind: 'job', title: 'A scheduled scan of /x failed')]);
      expect(_check(seen, daemon: _daemon([]), notes: notes).notices, isEmpty);
      final r = _check(seen, settings: _on.copyWith(failed: false), daemon: _daemon([]), notes: notes);
      expect(r.notices.single.title, 'A scheduled scan of /x failed');
      expect(r.notices.single.route, '/activity');
    });

    test('many at once: a few, then one for the rest', () {
      final r = _check(seen, daemon: _daemon([]), notes: _notes([for (var i = 12; i > 5; i--) _note(i)]));
      expect(r.notices, hasLength(maxNoteNotices + 1));
      expect(r.notices.take(3).map((n) => n.title), [
        'Drive 6 is getting worse',
        'Drive 7 is getting worse',
        'Drive 8 is getting worse',
      ]);
      expect(r.notices.last.title, '4 more from BackDown');
    });

    test('turned off, nothing', () {
      final r = _check(seen, settings: _on.copyWith(health: false), daemon: _daemon([]), notes: _notes([_note(9)]));
      expect(r.notices, isEmpty);
      expect(r.memory.lastNote, 9); // seen all the same: turning it on later doesn't bring old news
    });

    test('when the list couldn\'t be read, what was seen is kept', () {
      final r = _check(seen, daemon: _daemon([]));
      expect(r.memory.lastNote, 5);
    });
  });

  group('can\'t reach it', () {
    final ok = _check(null, daemon: _daemon([]), notes: _notes([])).memory;
    final on = _on.copyWith(unreachable: true);

    test('told after a while, twice in a row, once', () {
      final one = _check(ok, failure: offline, settings: on, now: _t0 + 4 * 3600);
      expect(one.notices, isEmpty); // one failed check could be a moment's trouble
      final two = _check(one.memory, failure: offline, settings: on, now: _t0 + 5 * 3600);
      expect(two.notices.single.title, 'Can\'t reach Studio');
      expect(two.notices.single.body, contains('hasn\'t answered for 5 h'));
      expect(two.notices.single.channel, NoticeChannel.connection);
      final three = _check(two.memory, failure: offline, settings: on, now: _t0 + 6 * 3600);
      expect(three.notices, isEmpty);
      // back: told again the next time it goes away
      final back = _check(three.memory, daemon: _daemon([]), notes: _notes([]), settings: on, now: _t0 + 7 * 3600);
      expect(back.memory.told, isFalse);
      expect(back.memory.failures, 0);
    });

    test('not before three hours', () {
      final one = _check(ok, failure: offline, settings: on, now: _t0 + 600);
      expect(_check(one.memory, failure: offline, settings: on, now: _t0 + 1200).notices, isEmpty);
    });

    test('off by default', () {
      final one = _check(ok, failure: offline, now: _t0 + 4 * 3600);
      expect(_check(one.memory, failure: offline, now: _t0 + 5 * 3600).notices, isEmpty);
    });

    test('a token it no longer accepts says what to do', () {
      final no = ApiException(ApiFailure.unauthorized, 'no');
      final one = _check(ok, failure: no, settings: on, now: _t0 + 4 * 3600);
      final two = _check(one.memory, failure: no, settings: on, now: _t0 + 5 * 3600);
      expect(two.notices.single.title, 'Studio doesn\'t know this phone any more');
      expect(two.notices.single.body, contains('backdown token --qr'));
    });

    test('what was seen before it went away is kept', () {
      final r = _check(ok, failure: offline);
      expect(r.memory.okAt, ok.okAt);
      expect(r.memory.started, ok.started);
    });
  });

  test('memory survives being saved', () {
    const m = ServerMemory(
      started: 1,
      lastJob: 2,
      seenAt: 3,
      lastNote: 4,
      okAt: 5,
      failingSince: 6,
      failures: 7,
      told: true,
    );
    final back = ServerMemory.decode(m.encode())!;
    expect(back.toJson(), m.toJson());
    expect(ServerMemory.decode('not json'), isNull);
    expect(ServerMemory.decode(null), isNull);
  });

  test('the same news gets the same id; a tap says where to go', () {
    expect(noticeId('s1', 'job:1:4'), noticeId('s1', 'job:1:4'));
    expect(noticeId('s1', 'job:1:4'), isNot(noticeId('s2', 'job:1:4')));
    expect(noticeId('s1', 'x'), inInclusiveRange(0, 0x7fffffff));
    const n = PhoneNotice(
      id: 1,
      channel: NoticeChannel.finished,
      title: 't',
      body: 'b',
      serverId: 's1',
      serverName: 'Studio',
      route: '/health',
    );
    final tap = NoticeTap.decode(n.payload)!;
    expect((tap.serverId, tap.route), ('s1', '/health'));
    expect(NoticeTap.decode('{"route": "http://x"}'), isNull);
    expect(NoticeTap.decode('garbage'), isNull);
  });
}
