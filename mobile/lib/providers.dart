import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'api/api.dart';
import 'api/demo_api.dart';
import 'api/http_api.dart';
import 'api/json.dart';
import 'core/format.dart';
import 'models/browse.dart';
import 'models/daemon.dart';
import 'servers/server.dart';
import 'servers/server_store.dart';

/// Opened in main() (and in tests) and handed in with an override.
final serverStoreProvider = Provider<ServerStore>((ref) => throw UnimplementedError('override serverStoreProvider'));

final prefsProvider = Provider<SharedPreferences>((ref) => throw UnimplementedError('override prefsProvider'));

class ServersState {
  const ServersState(this.servers, this.currentId);
  final List<ServerEntry> servers;
  final String? currentId;

  ServerEntry? get current {
    for (final s in servers) {
      if (s.id == currentId) return s;
    }
    return null;
  }
}

/// The servers this phone knows and which one is shown.
class ServersNotifier extends Notifier<ServersState> {
  ServerStore get _store => ref.read(serverStoreProvider);

  @override
  ServersState build() {
    final s = ref.watch(serverStoreProvider);
    return ServersState(s.servers, s.currentId);
  }

  void _sync() => state = ServersState(_store.servers, _store.currentId);

  /// Adds a server (or replaces one with the same address) and shows it.
  Future<ServerEntry> add({required String name, required Uri url, required String token}) async {
    final existing = _store.servers.where((s) => !s.demo && s.url == url).firstOrNull;
    final entry = existing?.copyWith(name: name) ?? ServerEntry(id: _newId(), name: name, url: url);
    await _store.add(entry, token);
    await _store.select(entry.id);
    _sync();
    return entry;
  }

  /// Adds the built-in demo and shows it.
  Future<void> addDemo() async {
    final existing = _store.servers.where((s) => s.demo).firstOrNull;
    final entry =
        existing ??
        ServerEntry(id: 'demo', name: 'Demo', url: Uri.parse('http://demo.backdown.invalid:8787'), demo: true);
    if (existing == null) await _store.add(entry, null);
    await _store.select(entry.id);
    _sync();
  }

  Future<void> select(String id) async {
    await _store.select(id);
    _sync();
  }

  Future<void> rename(String id, String name) async {
    final s = _store.servers.where((x) => x.id == id).firstOrNull;
    if (s == null) return;
    await _store.update(s.copyWith(name: name));
    _sync();
  }

  Future<void> replaceToken(String id, Uri url, String token) async {
    final s = _store.servers.where((x) => x.id == id).firstOrNull;
    if (s == null) return;
    await _store.update(s.copyWith(url: url), token: token);
    _sync();
  }

  Future<void> remove(String id) async {
    await _store.remove(id);
    _sync();
  }

  static String _newId() => DateTime.now().microsecondsSinceEpoch.toRadixString(36);
}

final serversProvider = NotifierProvider<ServersNotifier, ServersState>(ServersNotifier.new);

/// The server being shown.
final currentServerProvider = Provider<ServerEntry?>((ref) => ref.watch(serversProvider).current);

/// The client for the server being shown; null before one is added. Screens use [requireApi].
final apiProvider = Provider<BackDownApi?>((ref) {
  final server = ref.watch(currentServerProvider);
  if (server == null) return null;
  final BackDownApi api = server.demo
      ? DemoBackDownApi(animate: true)
      : HttpBackDownApi(server.url, ref.read(serverStoreProvider).tokenOf(server.id) ?? '');
  ref.onDispose(api.close);
  return api;
});

/// The client for the server being shown, inside a provider that only runs once one is.
BackDownApi requireApi(Ref ref) =>
    ref.watch(apiProvider) ?? (throw ApiException(ApiFailure.offline, 'No BackDown service added yet.'));

/// A GET by endpoint and parameters, value-equal so it can key [apiGetProvider].
@immutable
class ApiQuery {
  const ApiQuery(this.endpoint, [this.params = const {}]);
  final String endpoint;
  final Map<String, Object?> params;

  @override
  bool operator ==(Object other) =>
      other is ApiQuery && other.endpoint == endpoint && mapEquals(queryOf(other.params), queryOf(params));

  @override
  int get hashCode =>
      Object.hash(endpoint, Object.hashAllUnordered(queryOf(params).entries.map((e) => '${e.key}=${e.value}')));

  @override
  String toString() => 'ApiQuery($endpoint, $params)';
}

/// Any GET, decoded but untyped: for screens that read a few fields and don't need a model class.
final apiGetProvider = FutureProvider.autoDispose.family<Object?, ApiQuery>(
  (ref, q) => requireApi(ref).get(q.endpoint, q.params),
);

/// The same, as an object.
final apiJsonProvider = FutureProvider.autoDispose.family<Json, ApiQuery>(
  (ref, q) async => asJson(await requireApi(ref).get(q.endpoint, q.params)),
);

// ---- browsing (used by several screens)

final rootsProvider = FutureProvider.autoDispose<List<Root>>((ref) async {
  final j = asJson(await requireApi(ref).get('roots'));
  return [for (final r in j.list('roots')) Root.fromJson(r)];
});

final indexStatusProvider = FutureProvider.autoDispose<IndexStatus>(
  (ref) async => IndexStatus.fromJson(asJson(await requireApi(ref).get('index-status'))),
);

/// A folder: its key, and the scan to show it at (null: the latest).
typedef FolderAt = ({String key, int? scan});

final listingProvider = FutureProvider.autoDispose.family<Listing, FolderAt>(
  (ref, at) async => Listing.fromJson(asJson(await requireApi(ref).get('ls', {'dir': at.key, 'scan': at.scan}))),
);

final fileInfoProvider = FutureProvider.autoDispose.family<FileInfo, int>(
  (ref, id) async => FileInfo.fromJson(asJson(await requireApi(ref).get('file', {'id': id}))),
);

final timelineProvider = FutureProvider.autoDispose.family<List<ScanPoint>, int>((ref, root) async {
  final v = await requireApi(ref).get('timeline', {'root': root});
  return [for (final s in asJsonList(v)) ScanPoint.fromJson(s)];
});

// ---- the service's queue, live

/// What the service is doing, kept current with the long poll (`?since=CHANGE&wait=50`): the
/// service answers as soon as something changes, so an idle screen costs one request a minute.
/// `backdown serve` has no queue; it's asked again every half minute in case a daemon took over.
final daemonProvider = StreamProvider.autoDispose<DaemonSnapshot>((ref) {
  final api = requireApi(ref);
  final out = StreamController<DaemonSnapshot>();
  var alive = true;
  ref.onDispose(() {
    alive = false;
    unawaited(out.close());
  });
  Future<void> loop() async {
    var since = -1;
    while (alive) {
      try {
        final v = since < 0
            ? await api.get('daemon')
            : await api.get('daemon', {'since': since, 'wait': 50}, const Duration(seconds: 70));
        if (!alive) return;
        final s = DaemonSnapshot.fromJson(asJson(v));
        out.add(s);
        if (!s.running) {
          await Future<void>.delayed(const Duration(seconds: 30));
          since = -1;
        } else {
          since = s.change;
        }
      } on Object catch (e, st) {
        if (!alive) return;
        out.addError(e, st);
        since = -1;
        await Future<void>.delayed(const Duration(seconds: 10));
      }
    }
  }

  unawaited(loop());
  return out.stream;
});

/// The server's clock minus this phone's, in seconds (0 until the service has said).
final skewProvider = Provider.autoDispose<int>((ref) {
  final now = ref.watch(daemonProvider).value?.now;
  return now == null ? 0 : now - nowSeconds();
});
