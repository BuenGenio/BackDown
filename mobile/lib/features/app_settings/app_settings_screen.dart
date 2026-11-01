import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:package_info_plus/package_info_plus.dart';

import '../../background/background.dart';
import '../../core/widgets.dart';
import 'lock.dart';
import 'preferences.dart';

final _packageInfoProvider = FutureProvider<PackageInfo>((ref) => PackageInfo.fromPlatform());

/// The app's own settings: notifications, App Lock, appearance, folders, about.
class AppSettingsScreen extends ConsumerWidget {
  const AppSettingsScreen({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final mode = ref.watch(themeModeProvider);
    final lock = ref.watch(appLockProvider);
    final info = ref.watch(_packageInfoProvider).value;
    return Scaffold(
      appBar: AppBar(title: const Text('Settings')),
      body: ListView(
        padding: const EdgeInsets.only(bottom: 32),
        children: [
          const NotificationSettingsSection(),
          const SectionHeader('Privacy'),
          SwitchListTile(
            secondary: const Icon(Icons.fingerprint),
            title: const Text('App Lock'),
            subtitle: Text(
              appLockSupported
                  ? 'Ask for your fingerprint, face or screen lock when BackDown opens'
                  : 'Not on this platform',
            ),
            value: lock,
            onChanged: !appLockSupported
                ? null
                : (on) async {
                    // prove it works before relying on it
                    if (on && !await unlockWithDevice('Turn on App Lock')) return;
                    await ref.read(appLockProvider.notifier).set(on);
                  },
          ),
          const SectionHeader('Appearance'),
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
            child: SegmentedButton<ThemeMode>(
              segments: const [
                ButtonSegment(value: ThemeMode.system, label: Text('Automatic'), icon: Icon(Icons.brightness_auto)),
                ButtonSegment(value: ThemeMode.light, label: Text('Light'), icon: Icon(Icons.light_mode_outlined)),
                ButtonSegment(value: ThemeMode.dark, label: Text('Dark'), icon: Icon(Icons.dark_mode_outlined)),
              ],
              selected: {mode},
              onSelectionChanged: (s) => ref.read(themeModeProvider.notifier).set(s.first),
            ),
          ),
          SwitchListTile(
            secondary: const Icon(Icons.grid_view),
            title: const Text('Folders as a grid'),
            value: ref.watch(folderGridProvider),
            onChanged: (v) => ref.read(folderGridProvider.notifier).set(v),
          ),
          SwitchListTile(
            secondary: const Icon(Icons.visibility_outlined),
            title: const Text('Hidden files'),
            subtitle: const Text('Names that start with a dot'),
            value: ref.watch(showHiddenProvider),
            onChanged: (v) => ref.read(showHiddenProvider.notifier).set(v),
          ),
          const SectionHeader('About'),
          InfoRow('Version', info == null ? '…' : '${info.version} (${info.buildNumber})'),
          ListTile(
            leading: const Icon(Icons.gavel_outlined),
            title: const Text('Licences'),
            onTap: () =>
                showLicensePage(context: context, applicationName: 'BackDown', applicationVersion: info?.version),
          ),
        ],
      ),
    );
  }
}
