import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:isolate';
import 'dart:typed_data';

import 'package:http/http.dart' as http;

import 'api.dart';

/// A BackDown service over HTTP: `backdown daemon` (the service) or `backdown serve`, reached on its
/// own computer, its tailnet name or a LAN address.
///
/// The service's rules, which this follows: the session token goes in the `bd` cookie on every
/// request; requests that do something are POSTs with an `X-BackDown` header; the Host header must
/// be a name the service answers to (so use the address `backdown token --links` prints).
class HttpBackDownApi extends BackDownApi {
  HttpBackDownApi(this.base, String token, {http.Client? client})
    : _cookie = 'bd=$token',
      _client = client ?? http.Client();

  @override
  final Uri base;
  final String _cookie;
  final http.Client _client;

  static const _timeout = Duration(seconds: 45);

  @override
  bool get canChange => isLoopbackHost(base.host);

  @override
  Map<String, String> get authHeaders => {'Cookie': _cookie};

  @override
  Uri uri(String endpoint, [Map<String, Object?> params = const {}]) {
    final q = queryOf(params);
    return base.replace(path: '/api/$endpoint', queryParameters: q.isEmpty ? null : q);
  }

  @override
  Future<Object?> get(String endpoint, [Map<String, Object?> params = const {}, Duration? timeout]) async {
    final r = await _send(() => _client.get(uri(endpoint, params), headers: authHeaders), timeout ?? _timeout);
    return _json(r);
  }

  @override
  Future<Object?> post(String endpoint, {Map<String, Object?> params = const {}, Object? body}) async {
    final headers = {...authHeaders, 'X-BackDown': '1', if (body != null) 'Content-Type': 'application/json'};
    final r = await _send(
      () => _client.post(uri(endpoint, params), headers: headers, body: body == null ? null : jsonEncode(body)),
      _timeout,
    );
    return _json(r);
  }

  @override
  Future<Uint8List> bytes(
    String endpoint, [
    Map<String, Object?> params = const {},
    int? limit,
    Duration? timeout,
  ]) async {
    final headers = {...authHeaders, if (limit != null) 'Range': 'bytes=0-${limit - 1}'};
    // a whole file (a big PDF over a slow link) gets longer than a peek at its start
    final wait = timeout ?? (limit == null ? const Duration(minutes: 5) : _timeout);
    final r = await _send(() => _client.get(uri(endpoint, params), headers: headers), wait);
    if (r.statusCode != 200 && r.statusCode != 206) {
      throw _failure(r.statusCode, utf8.decode(r.bodyBytes, allowMalformed: true));
    }
    return r.bodyBytes;
  }

  @override
  Future<File?> download(
    String endpoint,
    Map<String, Object?> params,
    File into, {
    void Function(int received, int? total)? onProgress,
    bool Function()? cancelled,
  }) async {
    final req = http.Request('GET', uri(endpoint, params))..headers.addAll(authHeaders);
    final http.StreamedResponse r;
    try {
      r = await _client.send(req).timeout(_timeout);
    } on Object catch (e) {
      throw _offline(e);
    }
    if (r.statusCode != 200) throw _failure(r.statusCode, await r.stream.bytesToString());
    final part = File('${into.path}.part');
    final sink = part.openWrite();
    var got = 0;
    try {
      await for (final chunk in r.stream) {
        if (cancelled?.call() ?? false) {
          await sink.close();
          await part.delete();
          return null;
        }
        sink.add(chunk);
        got += chunk.length;
        onProgress?.call(got, r.contentLength);
      }
      await sink.close();
    } on Object catch (e) {
      await sink.close();
      if (await part.exists()) await part.delete();
      throw _offline(e);
    }
    return part.rename(into.path);
  }

  @override
  void close() => _client.close();

  Future<http.Response> _send(Future<http.Response> Function() send, Duration timeout) async {
    try {
      return await send().timeout(timeout);
    } on Object catch (e) {
      throw _offline(e);
    }
  }

  ApiException _offline(Object e) {
    final why = switch (e) {
      TimeoutException() => 'it didn\'t answer in time',
      SocketException(:final osError?) => osError.message,
      http.ClientException(:final message) => message,
      _ => e.toString(),
    };
    return ApiException(
      ApiFailure.offline,
      'Can\'t reach ${base.host}: $why. Check that the computer is on, BackDown is running there, and this '
      'phone is on the same network or tailnet.',
    );
  }

  Future<Object?> _json(http.Response r) async {
    if (r.statusCode != 200) throw _failure(r.statusCode, utf8.decode(r.bodyBytes, allowMalformed: true));
    // Failures always come with a non-200 status. A 200's top-level "error" is data: a file's read
    // error in /api/file, a machine's last error.
    final body = r.bodyBytes;
    try {
      // a big answer (megabytes of entities or duplicates) is decoded in another isolate, so the
      // screen keeps moving
      if (body.length > _bigJson) return await Isolate.run(() => jsonDecode(utf8.decode(body)));
      return jsonDecode(utf8.decode(body));
    } on FormatException {
      throw ApiException(ApiFailure.server, 'The service answered with something that isn\'t JSON.', status: 200);
    }
  }

  static const _bigJson = 256 << 10;

  ApiException _failure(int status, String body) {
    Map<String, Object?>? j;
    try {
      final d = jsonDecode(body);
      if (d is Map<String, Object?>) j = d;
    } on FormatException {
      j = null;
    }
    final said = (j?['error'] as String?) ?? body.trim();
    return switch (status) {
      401 => ApiException(
        ApiFailure.unauthorized,
        'This BackDown service doesn\'t accept this phone\'s token any more. On the computer, run '
        '`backdown token --qr` and scan the code again.',
        status: status,
      ),
      403 when said == 'wrong host' => ApiException(
        ApiFailure.wrongHost,
        'The service doesn\'t answer to "${base.host}". Use the address `backdown token --links` prints on '
        'the computer.',
        status: status,
      ),
      403 => ApiException(ApiFailure.atTheComputer, said, status: status, detail: j),
      402 => ApiException(ApiFailure.needsTier, said, status: status, detail: j),
      404 => ApiException(ApiFailure.notFound, said.isEmpty ? 'Not found.' : said, status: status),
      409 => ApiException(ApiFailure.conflict, said, status: status, detail: j),
      >= 500 => ApiException(ApiFailure.server, said.isEmpty ? 'The service failed ($status).' : said, status: status),
      _ => ApiException(ApiFailure.refused, said.isEmpty ? 'The service said no ($status).' : said, status: status),
    };
  }
}
