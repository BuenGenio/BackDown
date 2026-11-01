import '../api/json.dart';

/// A BackDown service this phone knows: a computer's `backdown daemon` (or `backdown serve`). Its
/// token is kept apart, in secure storage ([ServerStore]).
class ServerEntry {
  const ServerEntry({required this.id, required this.name, required this.url, this.demo = false});

  ServerEntry.fromJson(Json j)
    : id = j.str('id'),
      name = j.str('name'),
      url = Uri.parse(j.str('url')),
      demo = j.flag('demo');

  final String id;

  /// What the person calls it ("Studio", "nextcloud").
  final String name;

  /// Scheme, host and port: `http://studio.tailnet-1234.ts.net:8787`.
  final Uri url;

  /// The built-in demo, which needs no network.
  final bool demo;

  ServerEntry copyWith({String? name, Uri? url}) =>
      ServerEntry(id: id, name: name ?? this.name, url: url ?? this.url, demo: demo);

  Json toJson() => {'id': id, 'name': name, 'url': url.toString(), if (demo) 'demo': true};

  /// "studio.tailnet-1234.ts.net:8787" for lists.
  String get address => url.hasPort ? '${url.host}:${url.port}' : url.host;
}

/// What a pasted or scanned link says: where the service is and its token.
class ServerLink {
  const ServerLink(this.url, this.token);
  final Uri url;
  final String token;

  /// A name to start with: the host's first label ("studio" from studio.tailnet-1234.ts.net), or
  /// the address for an IP.
  String get suggestedName {
    final h = url.host;
    if (RegExp(r'^[\d.]+$').hasMatch(h) || h.contains(':')) return h;
    final first = h.split('.').first;
    return first.isEmpty ? h : first[0].toUpperCase() + first.substring(1);
  }
}

/// Reads the link `backdown token --links` prints (`http://host:8787/?t=TOKEN`), with or without the
/// scheme, or a `backdown://` link from a QR code. Null when it isn't one.
ServerLink? parseServerLink(String text) {
  var t = text.trim();
  if (t.isEmpty) return null;
  if (t.startsWith('backdown://')) t = 'http://${t.substring('backdown://'.length)}';
  if (!t.contains('://')) t = 'http://$t';
  final u = Uri.tryParse(t);
  if (u == null || u.host.isEmpty || (u.scheme != 'http' && u.scheme != 'https')) return null;
  final token = u.queryParameters['t'] ?? '';
  if (!isToken(token)) return null;
  return ServerLink(serverUrl(u.scheme, u.host, u.hasPort ? u.port : 8787), token);
}

/// The service's tokens: 48 hex digits (at least 32).
bool isToken(String t) => t.length >= 32 && RegExp(r'^[0-9a-fA-F]+$').hasMatch(t);

/// `http://host:port` alone, without a path or query.
Uri serverUrl(String scheme, String host, int port) => Uri(scheme: scheme, host: host, port: port);
