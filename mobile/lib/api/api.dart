import 'dart:io';
import 'dart:typed_data';

/// Why a request to a BackDown service failed, so screens can say what to do about it.
enum ApiFailure {
  /// The computer couldn't be reached: off, asleep, another network, Tailscale off.
  offline,

  /// The service doesn't know this phone's token (it was reinstalled, or `backdown serve` restarted).
  unauthorized,

  /// The service doesn't answer to the address used (its allowlist of names, against DNS rebinding).
  wrongHost,

  /// Over the network the service only shows things: this needs someone at the computer.
  atTheComputer,

  /// Part of a paid edition this service's licence doesn't include.
  needsTier,

  notFound,

  /// The service can't do it now (no daemon, a job in the way).
  conflict,

  /// The service said no, with its reason.
  refused,

  /// The service failed, or answered with something this app can't read.
  server,
}

class ApiException implements Exception {
  ApiException(this.failure, this.message, {this.status, this.detail});

  final ApiFailure failure;

  /// For the person, in BackDown's words where the service gave them.
  final String message;
  final int? status;

  /// The service's whole answer when it says more than a message (the 402 refusal: tier, pricing).
  final Map<String, Object?>? detail;

  @override
  String toString() => message;
}

/// One BackDown service: the requests every screen makes. [HttpBackDownApi] speaks to a real one;
/// [DemoBackDownApi] answers from built-in examples for the demo and for tests.
abstract class BackDownApi {
  /// Where the service is (scheme, host and port).
  Uri get base;

  /// Whether this connection may change things. The service accepts changes (other than queuing a
  /// rescan) only from its own computer, so from a phone this is false.
  bool get canChange;

  /// Whether this is the built-in demo.
  bool get isDemo => false;

  /// What to send with a request made outside this class (images, video): the session cookie.
  Map<String, String> get authHeaders;

  /// `/api/<endpoint>?<params>` on this service. Null and empty values are left out.
  Uri uri(String endpoint, [Map<String, Object?> params = const {}]);

  /// GET `/api/<endpoint>`: the decoded JSON.
  Future<Object?> get(String endpoint, [Map<String, Object?> params = const {}, Duration? timeout]);

  /// POST `/api/<endpoint>` with the parameters in the query, as the web UI sends them, and an
  /// optional JSON body.
  Future<Object?> post(String endpoint, {Map<String, Object?> params = const {}, Object? body});

  /// The bytes of `/api/<endpoint>` (a file's content, an entry in an archive, a document the
  /// service makes), at most [limit], waiting up to [timeout] (45 s unless said).
  Future<Uint8List> bytes(String endpoint, [Map<String, Object?> params = const {}, int? limit, Duration? timeout]);

  /// Downloads `/api/<endpoint>` into [into], reporting progress (bytes so far, total if known),
  /// and stopping (with nothing left behind) once [cancelled] says so. Returns [into], or null when
  /// cancelled.
  Future<File?> download(
    String endpoint,
    Map<String, Object?> params,
    File into, {
    void Function(int received, int? total)? onProgress,
    bool Function()? cancelled,
  });

  void close();
}

/// The query string the web UI builds: null and empty values left out, everything else as text.
Map<String, String> queryOf(Map<String, Object?> params) => {
  for (final MapEntry(:key, :value) in params.entries)
    if (value != null && value.toString().isNotEmpty) key: value is bool ? (value ? '1' : '0') : value.toString(),
};

/// Whether [host] is the computer this app runs on, where the service allows changes.
bool isLoopbackHost(String host) {
  final h = host.toLowerCase();
  if (h == 'localhost' || h == '::1' || h == '[::1]') return true;
  final ip = InternetAddress.tryParse(h.replaceAll('[', '').replaceAll(']', ''));
  return ip != null && ip.isLoopback;
}
