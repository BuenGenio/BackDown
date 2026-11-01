import 'dart:async';
import 'dart:io';
import 'dart:typed_data';

import '../demo/demo_activity.dart';
import '../demo/demo_core.dart';
import '../demo/demo_library.dart';
import '../demo/demo_machines.dart';
import '../demo/demo_viewers.dart';
import 'api.dart';

/// Answers a demo GET or POST from its parameters (as the query string has them).
typedef DemoHandler = FutureOr<Object?> Function(DemoBackDownApi demo, Map<String, String> params);

/// Answers a demo request for bytes (a file's content, an entry in an archive).
typedef DemoBytesHandler = FutureOr<Uint8List> Function(DemoBackDownApi demo, Map<String, String> params);

/// The built-in demo: a made-up computer with old drives, answering every request from examples
/// in lib/demo/, with no network. Each area of the app keeps its examples in a file of its own
/// (demo_core, demo_viewers, demo_activity, demo_machines, demo_library); this merges them.
class DemoBackDownApi extends BackDownApi {
  /// [animate]: jobs move along on a timer, as they would on a real service. Off in tests, which
  /// mustn't leave timers running.
  DemoBackDownApi({this.animate = false, this.canChange = false}) {
    if (animate) _ticker = Timer.periodic(const Duration(seconds: 2), (_) => tick());
  }

  final bool animate;

  /// The demo acts as if it were reached over the network (look only) unless told otherwise, as a
  /// phone would be.
  @override
  final bool canChange;

  Timer? _ticker;

  static final Map<String, DemoHandler> gets = {
    ...demoCoreGets,
    ...demoViewerGets,
    ...demoActivityGets,
    ...demoMachineGets,
    ...demoLibraryGets,
  };
  static final Map<String, DemoHandler> posts = {
    ...demoCorePosts,
    ...demoViewerPosts,
    ...demoActivityPosts,
    ...demoMachinePosts,
    ...demoLibraryPosts,
  };
  static final Map<String, DemoBytesHandler> byteHandlers = {...demoCoreBytes, ...demoViewerBytes};

  /// State the demo's answers keep between requests (jobs, read notifications), by area.
  final Map<String, Object?> state = {};

  /// Goes up when something changes, like the service's own counter; the long poll waits on it.
  int change = 1;
  final List<Completer<void>> _waiters = [];

  /// Something changed: answer the long polls waiting.
  void changed() {
    change++;
    for (final w in _waiters) {
      if (!w.isCompleted) w.complete();
    }
    _waiters.clear();
  }

  /// Waits for the next change, with no timer (so tests don't leave one behind).
  Future<void> nextChange() {
    final c = Completer<void>();
    _waiters.add(c);
    return c.future;
  }

  /// One step of the demo's jobs (called by the timer, or by tests).
  void tick() {
    demoActivityTick(this);
    changed();
  }

  @override
  bool get isDemo => true;

  @override
  Uri get base => Uri.parse('http://demo.backdown.invalid:8787');

  @override
  Map<String, String> get authHeaders => const {};

  @override
  Uri uri(String endpoint, [Map<String, Object?> params = const {}]) {
    final q = queryOf(params);
    return base.replace(path: '/api/$endpoint', queryParameters: q.isEmpty ? null : q);
  }

  @override
  Future<Object?> get(String endpoint, [Map<String, Object?> params = const {}, Duration? timeout]) async {
    final h = gets[endpoint];
    if (h == null) throw ApiException(ApiFailure.notFound, 'The demo has no example for $endpoint.', status: 404);
    return h(this, queryOf(params));
  }

  @override
  Future<Object?> post(String endpoint, {Map<String, Object?> params = const {}, Object? body}) async {
    if (!canChange && endpoint != 'rescan') {
      throw ApiException(
        ApiFailure.atTheComputer,
        'Over the network, BackDown only shows what\'s there: freeing up drives, copying, rescuing, mounting, '
        'opening files and changing settings need someone at the computer BackDown runs on.',
        status: 403,
      );
    }
    final h = posts[endpoint];
    if (h == null) throw ApiException(ApiFailure.notFound, 'The demo can\'t do $endpoint.', status: 404);
    final p = queryOf(params);
    if (body is Map) {
      for (final MapEntry(:key, :value) in body.entries) {
        p.putIfAbsent(key.toString(), () => value.toString());
      }
    }
    return h(this, p);
  }

  @override
  Future<Uint8List> bytes(String endpoint, [Map<String, Object?> params = const {}, int? limit]) async {
    final h = byteHandlers[endpoint];
    if (h == null) throw ApiException(ApiFailure.notFound, 'No preview in the demo.', status: 404);
    final b = await h(this, queryOf(params));
    return limit != null && b.length > limit ? Uint8List.sublistView(b, 0, limit) : b;
  }

  @override
  Future<File?> download(
    String endpoint,
    Map<String, Object?> params,
    File into, {
    void Function(int received, int? total)? onProgress,
    bool Function()? cancelled,
  }) async {
    final b = await bytes(endpoint, params);
    onProgress?.call(b.length, b.length);
    return into.writeAsBytes(b);
  }

  @override
  void close() {
    _ticker?.cancel();
    _ticker = null;
  }
}
