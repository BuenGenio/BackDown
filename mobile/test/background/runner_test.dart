import 'dart:io';
import 'dart:typed_data';

import 'package:backdown_mobile/api/api.dart';
import 'package:backdown_mobile/background/check.dart';
import 'package:backdown_mobile/background/runner.dart';
import 'package:backdown_mobile/servers/server.dart';
import 'package:backdown_mobile/servers/server_store.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:shared_preferences/shared_preferences.dart';

/// A service that answers from [answers], or fails with [failure].
class _FakeService extends BackDownApi {
  _FakeService(this.answers);
  final Map<String, Object?> answers;
  ApiException? failure;
  int asked = 0;

  @override
  Uri get base => Uri.parse('http://studio:8787');
  @override
  bool get canChange => false;
  @override
  Map<String, String> get authHeaders => const {};
  @override
  Uri uri(String endpoint, [Map<String, Object?> params = const {}]) => base.replace(path: '/api/$endpoint');
  @override
  Future<Object?> get(String endpoint, [Map<String, Object?> params = const {}, Duration? timeout]) async {
    asked++;
    if (failure != null) throw failure!;
    if (!answers.containsKey(endpoint)) throw ApiException(ApiFailure.notFound, 'no $endpoint', status: 404);
    return answers[endpoint];
  }

  @override
  Future<Object?> post(String endpoint, {Map<String, Object?> params = const {}, Object? body}) =>
      throw UnimplementedError();
  @override
  Future<Uint8List> bytes(String endpoint, [Map<String, Object?> params = const {}, int? limit]) =>
      throw UnimplementedError();
  @override
  Future<File?> download(
    String endpoint,
    Map<String, Object?> params,
    File into, {
    void Function(int received, int? total)? onProgress,
    bool Function()? cancelled,
  }) => throw UnimplementedError();
  @override
  void close() {}
}

Map<String, Object?> _daemon(List<Map<String, Object?>> recent) => {
  'running': true,
  'now': 1_793_500_000,
  'started_at': 1_793_000_000,
  'change': 1,
  'recent': recent,
};

void main() {
  late SharedPreferences prefs;
  late MemoryTokenVault vault;
  late _FakeService service;
  late List<PhoneNotice> shown;

  Future<int> run() => runBackgroundCheck(
    prefs: prefs,
    vault: vault,
    connect: (s, token) {
      expect(token, 'a' * 48);
      return service;
    },
    show: (n) async => shown.addAll(n),
    keepInterval: false,
  );

  setUp(() async {
    SharedPreferences.setMockInitialValues({'bg_every': 60});
    prefs = await SharedPreferences.getInstance();
    vault = MemoryTokenVault();
    final store = await ServerStore.open(prefs, vault);
    await store.add(ServerEntry(id: 's1', name: 'Studio', url: Uri.parse('http://studio:8787')), 'a' * 48);
    await store.add(ServerEntry(id: 'demo', name: 'Demo', url: Uri.parse('http://demo.invalid'), demo: true), null);
    service = _FakeService({
      'daemon': _daemon([]),
      'notifications': {'notes': <Object?>[], 'unread': 0},
    });
    shown = [];
  });

  test('the first check remembers; the next tells what\'s new; then nothing again', () async {
    expect(await run(), 0);
    expect(prefs.getString(ServerMemory.key('s1')), isNotNull);
    expect(prefs.getString(ServerMemory.key('demo')), isNull); // the demo isn't checked

    service.answers['daemon'] = _daemon([
      {
        'id': 7,
        'kind': 'index',
        'path': '/media/sam/WD',
        'state': 'failed',
        'error': 'the drive went away',
        'started_at': 1_793_499_000,
        'finished_at': 1_793_499_900,
      },
    ]);
    service.answers['notifications'] = {
      'notes': [
        {
          'id': 3,
          'kind': 'health',
          'level': 'warning',
          'title': 'WD is getting worse',
          'body': '2 more.',
          'read': false,
        },
      ],
      'unread': 1,
    };
    expect(await run(), 2);
    expect(shown.map((n) => n.title), ['Failed: Scan WD', 'WD is getting worse']);
    expect(shown.every((n) => n.serverName == 'Studio' && n.serverId == 's1'), isTrue);

    shown.clear();
    expect(await run(), 0);
    expect(shown, isEmpty);
  });

  test('an older service without notifications still has its jobs checked', () async {
    service.answers.remove('notifications');
    await run();
    service.answers['daemon'] = _daemon([
      {'id': 2, 'kind': 'apply', 'path': 'plan 4', 'state': 'done', 'summary': 'copied 12 files', 'finished_at': 1},
    ]);
    expect(await run(), 1);
    expect(shown.single.title, 'Done: Apply plan 4');
  });

  test('a computer that can\'t be reached is remembered as such', () async {
    await run();
    service.failure = ApiException(ApiFailure.offline, 'no route');
    expect(await run(), 0);
    final m = ServerMemory.decode(prefs.getString(ServerMemory.key('s1')))!;
    expect(m.failures, 1);
    expect(m.okAt, isNotNull);
  });

  test('off, nothing is asked', () async {
    await prefs.setInt('bg_every', 0);
    expect(await run(), 0);
    expect(service.asked, 0);
  });

  test('where the phone runs it whenever it likes, the chosen interval is kept', () async {
    Future<int> again() => runBackgroundCheck(
      prefs: prefs,
      vault: vault,
      connect: (s, token) => service,
      show: (n) async => shown.addAll(n),
      keepInterval: true,
    );
    await again();
    final asked = service.asked;
    expect(await again(), 0);
    expect(service.asked, asked); // too soon: nothing asked
    await prefs.setInt('bg_last_run', 1);
    await again();
    expect(service.asked, greaterThan(asked));
  });

  test('a removed computer is forgotten', () async {
    await prefs.setString(ServerMemory.key('gone'), '{}');
    await run();
    expect(prefs.getString(ServerMemory.key('gone')), isNull);
  });
}
