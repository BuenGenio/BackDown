import 'dart:convert';

import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:shared_preferences/shared_preferences.dart';

import '../api/json.dart';
import 'server.dart';

/// Where the servers are kept: the list (names and addresses) in preferences, each token in the
/// platform's secure storage. Tokens are read once at start and held in memory, since every
/// request needs one.
class ServerStore {
  ServerStore._(this._prefs, this._secure, this._servers, this._tokens, this._currentId);

  static const _listKey = 'servers';
  static const _currentKey = 'current_server';
  static String _tokenKey(String id) => 'token_$id';

  final SharedPreferences _prefs;
  final TokenVault _secure;
  final List<ServerEntry> _servers;
  final Map<String, String> _tokens;
  String? _currentId;

  static Future<ServerStore> open(SharedPreferences prefs, TokenVault secure) async {
    final raw = prefs.getString(_listKey);
    final servers = <ServerEntry>[];
    if (raw != null) {
      try {
        servers.addAll([for (final j in asJsonList(jsonDecode(raw))) ServerEntry.fromJson(j)]);
      } on FormatException {
        // a damaged list: start again rather than fail to open
      }
    }
    final tokens = <String, String>{};
    for (final s in servers) {
      if (s.demo) continue;
      final t = await secure.read(_tokenKey(s.id));
      if (t != null) tokens[s.id] = t;
    }
    return ServerStore._(prefs, secure, servers, tokens, prefs.getString(_currentKey));
  }

  List<ServerEntry> get servers => List.unmodifiable(_servers);
  String? get currentId => _servers.any((s) => s.id == _currentId) ? _currentId : _servers.firstOrNull?.id;
  String? tokenOf(String id) => _tokens[id];

  Future<void> add(ServerEntry s, String? token) async {
    _servers.removeWhere((x) => x.id == s.id);
    _servers.add(s);
    if (token != null) {
      _tokens[s.id] = token;
      await _secure.write(_tokenKey(s.id), token);
    }
    await _saveList();
  }

  Future<void> update(ServerEntry s, {String? token}) async {
    final i = _servers.indexWhere((x) => x.id == s.id);
    if (i < 0) return;
    _servers[i] = s;
    if (token != null) {
      _tokens[s.id] = token;
      await _secure.write(_tokenKey(s.id), token);
    }
    await _saveList();
  }

  Future<void> remove(String id) async {
    _servers.removeWhere((s) => s.id == id);
    _tokens.remove(id);
    await _secure.delete(_tokenKey(id));
    if (_currentId == id) _currentId = _servers.firstOrNull?.id;
    await _saveList();
  }

  Future<void> select(String id) async {
    _currentId = id;
    await _prefs.setString(_currentKey, id);
  }

  Future<void> _saveList() async {
    await _prefs.setString(_listKey, jsonEncode([for (final s in _servers) s.toJson()]));
    final c = currentId;
    if (c != null) await _prefs.setString(_currentKey, c);
  }
}

/// The secure storage, behind an interface so tests can keep tokens in a map.
abstract class TokenVault {
  Future<String?> read(String key);
  Future<void> write(String key, String value);
  Future<void> delete(String key);
}

class SecureTokenVault implements TokenVault {
  // On iOS, readable after the first unlock since boot, so the background check can read tokens
  // while the phone is locked (the default is only while unlocked).
  SecureTokenVault([FlutterSecureStorage? storage])
    : _s =
          storage ??
          const FlutterSecureStorage(
            iOptions: IOSOptions(accessibility: KeychainAccessibility.first_unlock_this_device),
          );
  final FlutterSecureStorage _s;

  @override
  Future<String?> read(String key) => _s.read(key: key);
  @override
  Future<void> write(String key, String value) => _s.write(key: key, value: value);
  @override
  Future<void> delete(String key) => _s.delete(key: key);
}

class MemoryTokenVault implements TokenVault {
  final Map<String, String> values = {};
  @override
  Future<String?> read(String key) async => values[key];
  @override
  Future<void> write(String key, String value) async => values[key] = value;
  @override
  Future<void> delete(String key) async => values.remove(key);
}
