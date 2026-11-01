import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../../providers.dart';
import '../../router.dart';
import '../../theme.dart';

/// The server's name as a title that opens [showServerSwitcher].
class ServerTitle extends ConsumerWidget {
  const ServerTitle({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final s = ref.watch(currentServerProvider);
    return InkWell(
      borderRadius: BorderRadius.circular(8),
      onTap: () => showServerSwitcher(context, ref),
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: 4, horizontal: 2),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            Flexible(child: Text(s?.name ?? 'BackDown', overflow: TextOverflow.ellipsis)),
            const SizedBox(width: 2),
            Icon(Icons.expand_more, color: context.colors.onSurfaceVariant),
          ],
        ),
      ),
    );
  }
}

/// The computers this phone knows, to switch between, add or manage.
Future<void> showServerSwitcher(BuildContext context, WidgetRef ref) {
  return showModalBottomSheet<void>(
    context: context,
    showDragHandle: true,
    builder: (c) => Consumer(
      builder: (c, ref, _) {
        final st = ref.watch(serversProvider);
        return SafeArea(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              for (final s in st.servers)
                ListTile(
                  leading: Icon(s.demo ? Icons.science_outlined : Icons.computer_outlined),
                  title: Text(s.name),
                  subtitle: Text(s.demo ? 'Built-in examples, no network' : s.address),
                  trailing: s.id == st.currentId ? Icon(Icons.check, color: c.colors.primary) : null,
                  onTap: () async {
                    Navigator.pop(c);
                    await ref.read(serversProvider.notifier).select(s.id);
                    if (context.mounted) context.go(Routes.drives);
                  },
                ),
              const Divider(),
              ListTile(
                leading: const Icon(Icons.add),
                title: const Text('Add a computer'),
                onTap: () {
                  Navigator.pop(c);
                  context.push(Routes.connect);
                },
              ),
              ListTile(
                leading: const Icon(Icons.tune),
                title: const Text('Manage computers'),
                onTap: () {
                  Navigator.pop(c);
                  context.push(Routes.servers);
                },
              ),
            ],
          ),
        );
      },
    ),
  );
}
