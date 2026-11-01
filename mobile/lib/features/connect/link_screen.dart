import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../../api/api.dart';
import '../../api/http_api.dart';
import '../../api/json.dart';
import '../../core/widgets.dart';
import '../../providers.dart';
import '../../router.dart';
import '../../servers/server.dart';
import '../../theme.dart';

/// Checks that a service answers at [link] with its token: the drives it has, or why not.
Future<({String? version, int drives})> probeServer(ServerLink link) async {
  final api = HttpBackDownApi(link.url, link.token);
  try {
    final roots = asJson(await api.get('roots', const {}, const Duration(seconds: 15)));
    String? version;
    try {
      version = asJson(await api.get('daemon', const {}, const Duration(seconds: 15))).strOrNull('version');
    } on ApiException {
      version = null;
    }
    return (version: version, drives: roots.list('roots').length);
  } finally {
    api.close();
  }
}

/// Paste the link `backdown token --links` prints, or type the address and token. With
/// [serverId], it gives that computer a new token (after `backdown serve` restarted, say).
class LinkScreen extends ConsumerStatefulWidget {
  const LinkScreen({super.key, this.serverId, this.initial});

  final String? serverId;
  final String? initial;

  @override
  ConsumerState<LinkScreen> createState() => _LinkScreenState();
}

class _LinkScreenState extends ConsumerState<LinkScreen> {
  late final _link = TextEditingController(text: widget.initial ?? '');
  final _name = TextEditingController();
  final _host = TextEditingController();
  final _port = TextEditingController(text: '8787');
  final _token = TextEditingController();
  bool _manual = false;
  bool _busy = false;
  Object? _error;

  @override
  void initState() {
    super.initState();
    _link.addListener(_suggestName);
    _suggestName();
  }

  @override
  void dispose() {
    for (final c in [_link, _name, _host, _port, _token]) {
      c.dispose();
    }
    super.dispose();
  }

  void _suggestName() {
    final l = parseServerLink(_link.text);
    if (l != null && _name.text.isEmpty) _name.text = l.suggestedName;
    setState(() {});
  }

  ServerLink? get _parsed {
    if (!_manual) return parseServerLink(_link.text);
    final host = _host.text.trim();
    final port = int.tryParse(_port.text.trim());
    final token = _token.text.trim();
    if (host.isEmpty || port == null || !isToken(token)) return null;
    return ServerLink(serverUrl('http', host, port), token);
  }

  Future<void> _paste() async {
    final d = await Clipboard.getData(Clipboard.kTextPlain);
    if (d?.text != null) _link.text = d!.text!.trim();
  }

  Future<void> _connect() async {
    final link = _parsed;
    if (link == null) return;
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      await probeServer(link);
      final servers = ref.read(serversProvider.notifier);
      if (widget.serverId != null) {
        await servers.replaceToken(widget.serverId!, link.url, link.token);
        await servers.select(widget.serverId!);
      } else {
        final name = _name.text.trim().isEmpty ? link.suggestedName : _name.text.trim();
        await servers.add(name: name, url: link.url, token: link.token);
      }
      if (mounted) context.go(Routes.drives);
    } on Object catch (e) {
      if (mounted) setState(() => _error = e);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final parsed = _parsed;
    return Scaffold(
      appBar: AppBar(title: Text(widget.serverId == null ? 'Connect' : 'Connect again')),
      body: SafeArea(
        child: Center(
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 520),
            child: ListView(
              padding: const EdgeInsets.all(20),
              children: [
                if (!_manual) ...[
                  Text(
                    'The link from `backdown token --links` on the computer (the one for other devices):',
                    style: context.text.bodyMedium?.copyWith(color: context.colors.onSurfaceVariant),
                  ),
                  const SizedBox(height: 12),
                  TextField(
                    controller: _link,
                    autocorrect: false,
                    keyboardType: TextInputType.url,
                    minLines: 1,
                    maxLines: 3,
                    decoration: InputDecoration(
                      labelText: 'Link',
                      hintText: 'http://studio.tailnet-1234.ts.net:8787/?t=…',
                      suffixIcon: IconButton(
                        tooltip: 'Paste',
                        icon: const Icon(Icons.content_paste),
                        onPressed: _paste,
                      ),
                      errorText: _link.text.trim().isNotEmpty && parsed == null
                          ? 'That isn\'t a BackDown link: it needs the address and ?t= with the token'
                          : null,
                    ),
                  ),
                ] else ...[
                  TextField(
                    controller: _host,
                    autocorrect: false,
                    keyboardType: TextInputType.url,
                    decoration: const InputDecoration(
                      labelText: 'Address',
                      hintText: 'studio.tailnet-1234.ts.net or 192.168.1.20',
                    ),
                    onChanged: (_) => setState(() {}),
                  ),
                  const SizedBox(height: 12),
                  TextField(
                    controller: _port,
                    keyboardType: TextInputType.number,
                    decoration: const InputDecoration(labelText: 'Port'),
                    onChanged: (_) => setState(() {}),
                  ),
                  const SizedBox(height: 12),
                  TextField(
                    controller: _token,
                    autocorrect: false,
                    decoration: const InputDecoration(
                      labelText: 'Token',
                      helperText: 'What `backdown token` prints on the computer',
                    ),
                    onChanged: (_) {
                      if (_name.text.isEmpty && _host.text.isNotEmpty) {
                        _name.text = ServerLink(serverUrl('http', _host.text.trim(), 8787), '').suggestedName;
                      }
                      setState(() {});
                    },
                  ),
                ],
                if (widget.serverId == null) ...[
                  const SizedBox(height: 12),
                  TextField(
                    controller: _name,
                    decoration: const InputDecoration(labelText: 'Name', hintText: 'Studio'),
                  ),
                ],
                Align(
                  alignment: Alignment.centerLeft,
                  child: TextButton(
                    onPressed: () => setState(() => _manual = !_manual),
                    child: Text(_manual ? 'Paste a link instead' : 'Type the address and token instead'),
                  ),
                ),
                if (_error != null)
                  Card(
                    color: context.colors.errorContainer.withValues(alpha: 0.35),
                    child: ErrorPanel(error: _error!, compact: true),
                  ),
                const SizedBox(height: 12),
                FilledButton(
                  onPressed: parsed == null || _busy ? null : _connect,
                  child: _busy
                      ? const SizedBox(width: 18, height: 18, child: CircularProgressIndicator(strokeWidth: 2))
                      : const Text('Connect'),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}
