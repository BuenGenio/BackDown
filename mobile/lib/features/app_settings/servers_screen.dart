import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../../providers.dart';
import '../../router.dart';
import '../../servers/server.dart';
import '../../theme.dart';
import '../connect/connect_screen.dart';

/// The computers this phone knows: switch, rename, connect again, remove.
class ServersScreen extends ConsumerWidget {
  const ServersScreen({super.key});

  Future<void> _rename(BuildContext context, WidgetRef ref, ServerEntry s) async {
    final c = TextEditingController(text: s.name);
    final name = await showDialog<String>(
      context: context,
      builder: (d) => AlertDialog(
        title: const Text('Rename'),
        content: TextField(
          controller: c,
          autofocus: true,
          decoration: const InputDecoration(labelText: 'Name'),
        ),
        actions: [
          TextButton(onPressed: () => Navigator.pop(d), child: const Text('Cancel')),
          FilledButton(onPressed: () => Navigator.pop(d, c.text.trim()), child: const Text('Rename')),
        ],
      ),
    );
    c.dispose();
    if (name != null && name.isNotEmpty) await ref.read(serversProvider.notifier).rename(s.id, name);
  }

  Future<void> _remove(BuildContext context, WidgetRef ref, ServerEntry s) async {
    final ok = await showDialog<bool>(
      context: context,
      builder: (d) => AlertDialog(
        title: Text('Remove ${s.name}?'),
        content: const Text(
          'This phone forgets the computer and its token. Nothing changes on the computer; to connect again, scan '
          'its code again.',
        ),
        actions: [
          TextButton(onPressed: () => Navigator.pop(d, false), child: const Text('Cancel')),
          FilledButton(onPressed: () => Navigator.pop(d, true), child: const Text('Remove')),
        ],
      ),
    );
    if (ok ?? false) await ref.read(serversProvider.notifier).remove(s.id);
  }

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final st = ref.watch(serversProvider);
    return Scaffold(
      appBar: AppBar(title: const Text('Computers')),
      floatingActionButton: FloatingActionButton.extended(
        onPressed: () => context.push(Routes.connect),
        icon: const Icon(Icons.add),
        label: const Text('Add a computer'),
      ),
      body: ListView(
        padding: const EdgeInsets.only(bottom: 96),
        children: [
          for (final s in st.servers)
            ListTile(
              leading: Icon(
                s.id == st.currentId ? Icons.radio_button_checked : Icons.radio_button_unchecked,
                color: s.id == st.currentId ? context.colors.primary : null,
              ),
              title: Text(s.name),
              subtitle: Text(s.demo ? 'Built-in examples, no network' : s.address),
              onTap: () => ref.read(serversProvider.notifier).select(s.id),
              trailing: PopupMenuButton<String>(
                onSelected: (v) async {
                  switch (v) {
                    case 'rename':
                      await _rename(context, ref, s);
                    case 'again':
                      final route = ConnectScreen.canScan ? Routes.connectScan : Routes.connectLink;
                      await context.push(Uri(path: route, queryParameters: {'server': s.id}).toString());
                    case 'remove':
                      if (context.mounted) await _remove(context, ref, s);
                  }
                },
                itemBuilder: (c) => [
                  const PopupMenuItem(value: 'rename', child: Text('Rename')),
                  if (!s.demo) const PopupMenuItem(value: 'again', child: Text('Connect again (new token)')),
                  const PopupMenuItem(value: 'remove', child: Text('Remove')),
                ],
              ),
            ),
        ],
      ),
    );
  }
}
