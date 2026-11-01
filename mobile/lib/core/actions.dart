import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:url_launcher/url_launcher.dart';

import '../api/api.dart';
import '../providers.dart';
import '../theme.dart';
import 'widgets.dart';

/// Runs something that changes things on the service (a POST), and says how it went: [done] in a
/// snackbar, or why not. Returns the service's answer, or null when it failed.
///
/// From a phone the service only lets you look (and queue a rescan): anything else comes back
/// "needs someone at the computer", which is explained in a dialog rather than a bare error.
Future<T?> runAction<T>(
  BuildContext context,
  WidgetRef ref,
  Future<T> Function(BackDownApi api) action, {
  String? done,
}) async {
  final api = ref.read(apiProvider);
  if (api == null) return null;
  try {
    final r = await action(api);
    if (done != null && context.mounted) showSnack(context, done);
    return r;
  } on ApiException catch (e) {
    if (context.mounted) await showActionError(context, e);
    return null;
  }
}

Future<void> showActionError(BuildContext context, ApiException e) async {
  switch (e.failure) {
    case ApiFailure.atTheComputer:
      await showDialog<void>(
        context: context,
        builder: (c) => AlertDialog(
          icon: const Icon(Icons.computer_outlined),
          title: const Text('Needs someone at the computer'),
          content: Text(
            '${e.message}\n\nFrom a phone, BackDown shows what\'s on your drives and can queue a rescan. '
            'Copying, freeing up drives, rescuing and changing settings are done on the computer itself.',
          ),
          actions: [TextButton(onPressed: () => Navigator.pop(c), child: const Text('OK'))],
        ),
      );
    case ApiFailure.needsTier:
      final d = e.detail ?? const {};
      final pricing = d['pricing'] as String?;
      await showDialog<void>(
        context: context,
        builder: (c) => AlertDialog(
          icon: const Icon(Icons.workspace_premium_outlined),
          title: Text('BackDown ${d['needs_name'] ?? 'Pro'}'),
          content: Text(
            [
              e.message,
              if (d['unlocks'] is String) d['unlocks'] as String,
              if (d['how'] is String) d['how'] as String,
            ].join('\n\n'),
          ),
          actions: [
            if (pricing != null)
              TextButton(
                onPressed: () => launchUrl(Uri.parse(pricing), mode: LaunchMode.externalApplication),
                child: const Text('Pricing'),
              ),
            TextButton(onPressed: () => Navigator.pop(c), child: const Text('OK')),
          ],
        ),
      );
    default:
      showSnack(context, e.message);
  }
}

/// Whether the server being shown lets this app change things (only from its own computer).
bool canChange(WidgetRef ref) => ref.watch(apiProvider)?.canChange ?? false;

/// A note at the top of a screen whose actions need someone at the computer.
class LookOnlyNote extends ConsumerWidget {
  const LookOnlyNote({super.key, this.what = 'Changes here'});

  final String what;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    if (canChange(ref)) return const SizedBox.shrink();
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 8, 16, 0),
      child: Row(
        children: [
          Icon(Icons.visibility_outlined, size: 16, color: context.bd.muted),
          const SizedBox(width: 8),
          Expanded(
            child: Text(
              '$what are made on the computer BackDown runs on; from here you can look.',
              style: context.text.bodySmall?.copyWith(color: context.bd.muted),
            ),
          ),
        ],
      ),
    );
  }
}
