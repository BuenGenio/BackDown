import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../core/actions.dart';
import '../../core/format.dart';
import '../../core/widgets.dart';
import '../../models/daemon.dart';
import '../../providers.dart';
import '../../theme.dart';
import 'common.dart';
import 'data.dart';
import 'models.dart';

/// Free up: the drives and folders emptied once everything on them was safe elsewhere, and what
/// became of what existed only there. Choosing what to free is done at the computer.
class FreeUpScreen extends ConsumerWidget {
  const FreeUpScreen({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final wipes = ref.watch(wipesProvider);
    final daemon = ref.watch(daemonProvider).value;
    // wipes the service is doing or about to, by their job's path ("wipe 3")
    final live = <String, Job>{
      for (final j in [?daemon?.current, ...?daemon?.queued])
        if (j.kind == 'wipe' && j.path != null) j.path!: j,
    };
    return Scaffold(
      appBar: AppBar(title: const Text('Free up'), actions: const [CustodyButton()]),
      body: RefreshIndicator(
        onRefresh: () => ref.refresh(wipesProvider.future).catchError((_) => <Wipe>[]),
        child: LibraryBody(
          value: wipes,
          onRetry: () => ref.invalidate(wipesProvider),
          data: (list) {
            final rows = <Widget Function()>[
              () => const LookOnlyNote(what: 'Wipes'),
              () => const Intro(
                'A file is deleted only when an identical copy exists outside what\'s wiped, and both still read the '
                'same. What exists only there is moved to a folder chosen first, or left where it is.',
              ),
              if (list.isEmpty)
                () => const Padding(
                  padding: EdgeInsets.all(24),
                  child: EmptyState(
                    icon: Icons.delete_sweep_outlined,
                    title: 'Nothing freed up yet',
                    message:
                        'On the computer, Free up shows what wiping a drive or folder would delete, then deletes '
                        'only what has a verified copy.',
                  ),
                ),
              if (list.isNotEmpty) () => SectionHeader('Wipes', trailing: Text('${list.length}')),
              for (final w in list)
                () => WipeCard(
                  wipe: w,
                  job: live['wipe ${w.id}'],
                  running: daemon?.current != null && identical(live['wipe ${w.id}'], daemon!.current),
                ),
            ];
            return ReadableList(itemCount: rows.length, itemBuilder: (c, i) => rows[i]());
          },
        ),
      ),
    );
  }
}

class WipeCard extends ConsumerWidget {
  const WipeCard({super.key, required this.wipe, this.job, this.running = false});

  final Wipe wipe;
  final Job? job;
  final bool running;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final w = wipe;
    final skew = ref.watch(skewProvider);
    final j = job;
    final (state, label) = j != null
        ? (running ? 'running' : 'queued', running ? 'wiping${j.progress.isEmpty ? '' : ' · ${j.progress}'}' : 'queued')
        : (
            w.state,
            switch (w.state) {
              'new' => 'waiting',
              'running' => 'stopped',
              _ => w.state,
            },
          );
    return ListCard(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Icon(Icons.delete_sweep_outlined, color: context.bd.muted, size: 20),
              const SizedBox(width: 8),
              Expanded(
                child: Text('${when(w.createdAt)} · ${rel(w.createdAt, skew: skew)}', style: context.text.labelLarge),
              ),
              StatePill(state == 'running' ? 'wiping' : state, label: label),
            ],
          ),
          const SizedBox(height: 8),
          for (final p in w.paths)
            Padding(
              padding: const EdgeInsets.only(bottom: 2),
              child: PathText(p, color: context.colors.onSurface),
            ),
          const SizedBox(height: 6),
          Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Icon(
                w.moveTo == null ? Icons.do_not_disturb_on_outlined : Icons.drive_file_move_outline,
                size: 16,
                color: context.bd.muted,
              ),
              const SizedBox(width: 6),
              Expanded(
                child: w.moveTo == null
                    ? const Muted('What existed only there: left in place')
                    : InkWell(
                        onTap: () => openPath(context, w.moveTo!),
                        child: Text.rich(
                          TextSpan(
                            children: [
                              const TextSpan(text: 'What existed only there: moved to '),
                              TextSpan(
                                text: w.moveTo,
                                style: TextStyle(fontFamily: 'monospace', color: context.linkColor),
                              ),
                            ],
                          ),
                          style: context.text.bodySmall?.copyWith(color: context.colors.onSurfaceVariant),
                        ),
                      ),
              ),
            ],
          ),
          if (j != null && running && j.fraction != null) ...[
            const SizedBox(height: 8),
            Meter(fraction: j.fraction, color: context.bd.info),
          ],
          if (w.summary != null && w.summary!.isNotEmpty) ...[
            const SizedBox(height: 8),
            Text(
              w.summary!,
              style: context.text.bodySmall?.copyWith(color: w.state == 'failed' ? context.bd.crit : null),
            ),
          ],
        ],
      ),
    );
  }
}
