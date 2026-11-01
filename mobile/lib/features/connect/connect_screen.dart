import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';
import 'package:url_launcher/url_launcher.dart';

import '../../providers.dart';
import '../../router.dart';
import '../../theme.dart';

/// Where to scan the code, paste the link, or try the demo. The first screen, and the one for
/// adding another computer.
class ConnectScreen extends ConsumerWidget {
  const ConnectScreen({super.key});

  static bool get canScan => Platform.isAndroid || Platform.isIOS;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final adding = ref.watch(serversProvider).servers.isNotEmpty;
    return Scaffold(
      appBar: adding ? AppBar(title: const Text('Add a computer')) : null,
      body: SafeArea(
        child: Center(
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 480),
            child: ListView(
              padding: const EdgeInsets.fromLTRB(24, 32, 24, 24),
              children: [
                if (!adding) ...[
                  const Center(child: Wordmark(size: 34)),
                  const SizedBox(height: 14),
                  Text(
                    'Your drives, files and jobs,\nfrom your phone.',
                    textAlign: TextAlign.center,
                    style: context.text.titleMedium?.copyWith(color: context.colors.onSurfaceVariant),
                  ),
                  const SizedBox(height: 36),
                ],
                _Step(
                  n: 1,
                  title: 'Let BackDown on the computer be reached',
                  body:
                      'Its service listens on the computer only, at first. To reach it from this phone, start it '
                      'on its Tailscale address (or a LAN one):',
                  code: 'backdown service install --host 100.x.y.z',
                ),
                _Step(
                  n: 2,
                  title: 'Show the code',
                  body: 'On the computer, this prints a QR code and the link with its token:',
                  code: 'backdown token --qr',
                ),
                const SizedBox(height: 20),
                if (canScan) ...[
                  FilledButton.icon(
                    onPressed: () => context.push(Routes.connectScan),
                    icon: const Icon(Icons.qr_code_scanner),
                    label: const Text('Scan the code'),
                  ),
                  const SizedBox(height: 10),
                ],
                (canScan ? OutlinedButton.icon : FilledButton.icon)(
                  onPressed: () => context.push(Routes.connectLink),
                  icon: const Icon(Icons.link),
                  label: const Text('Paste the link'),
                ),
                const SizedBox(height: 10),
                TextButton(
                  onPressed: () async {
                    await ref.read(serversProvider.notifier).addDemo();
                    if (context.mounted) context.go(Routes.drives);
                  },
                  child: const Text('Try the demo'),
                ),
                const SizedBox(height: 24),
                Row(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Icon(Icons.lock_outline, size: 16, color: context.bd.muted),
                    const SizedBox(width: 8),
                    Expanded(
                      child: Text(
                        'This phone talks to your computer directly, over your tailnet or network. Nothing goes '
                        'through backdown.io. From here BackDown shows things; changes are made at the computer.',
                        style: context.text.bodySmall?.copyWith(color: context.bd.muted),
                      ),
                    ),
                  ],
                ),
                const SizedBox(height: 8),
                Align(
                  child: TextButton(
                    onPressed: () =>
                        launchUrl(Uri.parse('https://backdown.io/docs/'), mode: LaunchMode.externalApplication),
                    child: const Text('How BackDown works'),
                  ),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

class _Step extends StatelessWidget {
  const _Step({required this.n, required this.title, required this.body, required this.code});

  final int n;
  final String title, body, code;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.only(bottom: 18),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          CircleAvatar(
            radius: 13,
            backgroundColor: context.colors.primaryContainer,
            child: Text('$n', style: context.text.labelLarge?.copyWith(color: context.colors.onPrimaryContainer)),
          ),
          const SizedBox(width: 12),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(title, style: context.text.titleSmall),
                const SizedBox(height: 4),
                Text(body, style: context.text.bodyMedium?.copyWith(color: context.colors.onSurfaceVariant)),
                const SizedBox(height: 8),
                Container(
                  width: double.infinity,
                  padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 9),
                  decoration: BoxDecoration(
                    color: context.colors.surfaceContainerHigh,
                    borderRadius: BorderRadius.circular(8),
                  ),
                  child: SelectableText(code, style: context.text.bodySmall?.copyWith(fontFamily: 'monospace')),
                ),
              ],
            ),
          ),
        ],
      ),
    );
  }
}
