import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';
import 'package:url_launcher/url_launcher.dart';

import '../../core/widgets.dart';
import '../../providers.dart';
import '../../router.dart';
import '../../theme.dart';
import '../app_settings/server_switcher.dart';

/// Everything that isn't a tab: the rest of BackDown's views, this server, the app's settings.
class MoreScreen extends ConsumerWidget {
  const MoreScreen({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final server = ref.watch(currentServerProvider);
    final daemon = ref.watch(daemonProvider).value;
    final api = ref.watch(apiProvider);
    Widget item(IconData icon, String title, String sub, String route) => ListTile(
      leading: Icon(icon),
      title: Text(title),
      subtitle: Text(sub),
      trailing: const Icon(Icons.chevron_right),
      onTap: () => context.push(route),
    );
    return Scaffold(
      appBar: AppBar(title: const Text('More')),
      body: ListView(
        padding: const EdgeInsets.only(bottom: 24),
        children: [
          Padding(
            padding: const EdgeInsets.fromLTRB(16, 8, 16, 0),
            child: Card(
              clipBehavior: Clip.antiAlias,
              child: ListTile(
                contentPadding: const EdgeInsets.fromLTRB(16, 8, 8, 8),
                leading: CircleAvatar(
                  backgroundColor: context.colors.primaryContainer,
                  child: Icon(
                    server?.demo ?? false ? Icons.science_outlined : Icons.computer_outlined,
                    color: context.colors.onPrimaryContainer,
                  ),
                ),
                title: Text(server?.name ?? '', style: context.text.titleMedium),
                subtitle: Text(
                  [
                    server?.demo ?? false ? 'Built-in examples' : server?.address ?? '',
                    if (daemon?.version != null) 'BackDown ${daemon!.version}',
                    if (api != null && !api.canChange) 'look only',
                  ].join(' · '),
                ),
                trailing: TextButton(onPressed: () => showServerSwitcher(context, ref), child: const Text('Switch')),
                onTap: () => context.push(Routes.serverInfo),
              ),
            ),
          ),
          const SectionHeader('Your data'),
          item(Icons.sell_outlined, "What's here", 'Mail stores, backups, phones, libraries…', Routes.entities),
          item(Icons.content_copy_outlined, 'Duplicates', 'What\'s stored more than once', Routes.dupes),
          item(Icons.hub_outlined, 'Swarm', 'Every machine\'s drives and free space as one', Routes.swarm),
          item(Icons.dns_outlined, 'Other machines', 'What they hold, from their last sync', Routes.machines),
          const SectionHeader('Tidying up'),
          item(Icons.assignment_outlined, 'Plans', 'Copying drives into one archive', Routes.plans),
          item(Icons.delete_sweep_outlined, 'Free up', 'Drives emptied once everything is safe', Routes.freeUp),
          item(Icons.healing_outlined, 'Rescues', 'Files recovered from damaged drives', Routes.rescues),
          item(Icons.sync_alt, 'Mirrors', 'Folders kept copied to another drive', Routes.mirrors),
          item(Icons.verified_outlined, 'Manifests', 'Signed lists of a drive\'s files', Routes.manifests),
          const SectionHeader('This app'),
          item(Icons.computer_outlined, 'Computers', 'Add, rename or remove', Routes.servers),
          item(Icons.settings_outlined, 'Settings', 'Notifications, App Lock, appearance', Routes.appSettings),
          ListTile(
            leading: const Icon(Icons.menu_book_outlined),
            title: const Text('Docs'),
            subtitle: const Text('backdown.io/docs'),
            trailing: const Icon(Icons.open_in_new, size: 18),
            onTap: () => launchUrl(Uri.parse('https://backdown.io/docs/'), mode: LaunchMode.externalApplication),
          ),
        ],
      ),
    );
  }
}
