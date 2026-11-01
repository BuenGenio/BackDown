import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../../api/json.dart';
import '../../core/actions.dart';
import '../../core/format.dart';
import '../../core/widgets.dart';
import '../../models/daemon.dart';
import '../../providers.dart';
import '../../router.dart';
import '../../theme.dart';
import 'activity_data.dart';
import 'job_text.dart';

/// Stops a running job (it stops at the next file, keeping what's done) or takes a waiting one off
/// the queue (POST /api/job-stop; at the computer only).
Future<void> stopJob(BuildContext context, WidgetRef ref, Job job) async {
  final r = await runAction(context, ref, (api) => api.post('job-stop', params: {'id': job.id}));
  if (r == null || !context.mounted) return;
  showSnack(
    context,
    asJson(r).str('state') == 'stopping' ? 'Stopping: what\'s done so far is kept' : 'Taken off the queue',
  );
}

/// Queues a scan of an indexed drive (allowed from a phone).
Future<void> rescanDrive(BuildContext context, WidgetRef ref, int rootId, String name) =>
    runAction(context, ref, (api) => api.post('rescan', params: {'root': rootId}), done: 'Rescan of $name queued');

/// A job in full, kept live while it runs.
Future<void> showJobSheet(BuildContext context, Job job) => showModalBottomSheet<void>(
  context: context,
  isScrollControlled: true,
  showDragHandle: true,
  useSafeArea: true,
  builder: (c) => JobSheet(initial: job),
);

class JobSheet extends ConsumerWidget {
  const JobSheet({super.key, required this.initial});
  final Job initial;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final s = ref.watch(daemonProvider).value;
    final skew = ref.watch(skewProvider);
    final namer = JobNamer.watch(ref, s);
    final j = findJob(s, initial.id) ?? initial;
    final now = nowSeconds(skew);
    final p = j.fraction;
    final took = jobTook(j);
    final root = j.kind == 'index' ? namer.rootOf(j.path) : null;
    final stateColor = context.bd.forVerdict(j.state);
    String at(int? t) =>
        t == null ? '—' : '${when(t)} (${t > nowSeconds(skew) ? rel(t, skew: skew) : ago(t, skew: skew)})';
    final stage = jobStage(j);
    return ConstrainedBox(
      constraints: BoxConstraints(maxHeight: MediaQuery.sizeOf(context).height * 0.85, maxWidth: 640),
      child: ListView(
        shrinkWrap: true,
        padding: const EdgeInsets.only(bottom: 24),
        children: [
          Padding(
            padding: const EdgeInsets.fromLTRB(20, 0, 20, 8),
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Icon(jobIcon(j.kind), color: stateColor, size: 28),
                const SizedBox(width: 12),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(namer.title(j), style: context.text.titleLarge),
                      const SizedBox(height: 4),
                      Wrap(
                        spacing: 6,
                        children: [
                          Pill(jobState(j.state), color: stateColor),
                          if (j.reason.isNotEmpty) Pill(jobReason(j.reason)),
                        ],
                      ),
                    ],
                  ),
                ),
              ],
            ),
          ),
          if (j.running) ...[
            Padding(
              padding: const EdgeInsets.fromLTRB(20, 8, 20, 6),
              child: Row(
                children: [
                  Expanded(child: JobMeter(fraction: p)),
                  if (p != null) ...[
                    const SizedBox(width: 10),
                    Text(percent(p), style: context.text.titleSmall?.copyWith(fontWeight: FontWeight.w700)),
                  ],
                ],
              ),
            ),
            Padding(
              padding: const EdgeInsets.symmetric(horizontal: 20),
              child: Text(
                [jobDetail(j), ?jobEta(j, now)].where((x) => x.isNotEmpty).join(' · '),
                style: context.text.bodyMedium,
              ),
            ),
          ],
          if (j.error != null && j.error!.isNotEmpty)
            Padding(
              padding: const EdgeInsets.fromLTRB(16, 12, 16, 0),
              child: Container(
                padding: const EdgeInsets.all(12),
                decoration: BoxDecoration(
                  color: context.bd.crit.withValues(alpha: 0.08),
                  borderRadius: BorderRadius.circular(12),
                ),
                child: Row(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Icon(Icons.error_outline, color: context.bd.crit, size: 20),
                    const SizedBox(width: 10),
                    Expanded(child: SelectableText(j.error!, style: context.text.bodyMedium)),
                  ],
                ),
              ),
            ),
          if (!j.running && j.summary.isNotEmpty)
            Padding(
              padding: const EdgeInsets.fromLTRB(20, 12, 20, 0),
              child: Text(j.summary, style: context.text.bodyLarge),
            ),
          const SizedBox(height: 8),
          InfoRow('What', stage.isEmpty ? jobKind(j.kind) : '${jobKind(j.kind)} ($stage)'),
          if (j.path != null && j.path!.isNotEmpty)
            InfoRow(j.kind == 'index' ? 'Drive' : 'For', j.path!, mono: true, copyable: true),
          InfoRow('Queued', at(j.queuedAt)),
          if (j.startedAt != null) InfoRow('Started', at(j.startedAt)),
          if (j.finishedAt != null) InfoRow(j.state == 'done' ? 'Finished' : 'Ended', at(j.finishedAt)),
          if (took != null) InfoRow('Took', dur(took)),
          if (j.running && j.progress.isNotEmpty && j.progress != jobDetail(j)) InfoRow('Progress', j.progress),
          InfoRow('Job', '#${j.id}'),
          Padding(
            padding: const EdgeInsets.fromLTRB(16, 12, 16, 0),
            child: Wrap(
              spacing: 8,
              runSpacing: 8,
              children: [
                if (j.running && j.stoppable)
                  OutlinedButton.icon(
                    icon: const Icon(Icons.stop_circle_outlined),
                    label: const Text('Stop'),
                    onPressed: () => stopJob(context, ref, j),
                  ),
                if (j.state == 'queued')
                  OutlinedButton.icon(
                    icon: const Icon(Icons.playlist_remove),
                    label: const Text('Take it off the queue'),
                    onPressed: () => stopJob(context, ref, j),
                  ),
                if (root != null && !j.running && j.state != 'queued')
                  OutlinedButton.icon(
                    icon: const Icon(Icons.refresh),
                    label: const Text('Rescan'),
                    onPressed: () => rescanDrive(context, ref, root.id, namer.driveName(root.path)),
                  ),
                if (root != null)
                  FilledButton.tonalIcon(
                    icon: const Icon(Icons.folder_open),
                    label: const Text('Open the drive'),
                    onPressed: () {
                      final router = GoRouter.of(context);
                      Navigator.pop(context);
                      router.push(Routes.folder(root.key));
                    },
                  ),
              ],
            ),
          ),
          if (j.running && j.stoppable)
            Padding(
              padding: const EdgeInsets.fromLTRB(20, 8, 20, 0),
              child: Text(
                'Stopping keeps what\'s done; running it again carries on.',
                style: context.text.bodySmall?.copyWith(color: context.bd.muted),
              ),
            ),
        ],
      ),
    );
  }
}

/// A job's progress bar: how far it is, or a quiet full bar when there's nothing to compare with
/// yet (a drive's first scan), as the web UI's striped one. Never animates on its own.
class JobMeter extends StatelessWidget {
  const JobMeter({super.key, required this.fraction, this.height = 8});
  final double? fraction;
  final double height;

  @override
  Widget build(BuildContext context) {
    final info = context.bd.info;
    return Meter(fraction: fraction ?? 1, color: fraction == null ? info.withValues(alpha: 0.3) : info, height: height);
  }
}
