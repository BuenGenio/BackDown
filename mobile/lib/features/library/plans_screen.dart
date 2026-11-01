import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../../api/api.dart';
import '../../api/json.dart';
import '../../core/actions.dart';
import '../../core/format.dart';
import '../../core/widgets.dart';
import '../../providers.dart';
import '../../router.dart';
import '../../theme.dart';
import 'common.dart';
import 'data.dart';
import 'models.dart';

/// The pill a plan's state shows as.
String planStateLabel(String s) => switch (s) {
  'new' => 'not applied',
  'running' => 'copying',
  _ => s,
};

/// Plans: proposals for filing the old drives into one archive, and how far each has got.
class PlansScreen extends ConsumerWidget {
  const PlansScreen({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final plans = ref.watch(plansProvider);
    final dedups = ref.watch(dedupsProvider).value ?? const [];
    final skew = ref.watch(skewProvider);
    return Scaffold(
      appBar: AppBar(title: const Text('Plans')),
      body: RefreshIndicator(
        onRefresh: () async {
          ref.invalidate(dedupsProvider);
          await ref.refresh(plansProvider.future).catchError((_) => <PlanSummary>[]);
        },
        child: LibraryBody(
          value: plans,
          onRetry: () => ref.invalidate(plansProvider),
          data: (list) {
            final rows = <Widget Function()>[
              () => const LookOnlyNote(what: 'Plans and copies'),
              () => const Intro(
                'A plan proposes where everything on your old drives goes in one archive: mail with mail, phones '
                'with phones, each thing as a unit, with the reason for every placement. It only proposes: nothing '
                'is copied until it\'s applied, and nothing is ever deleted.',
              ),
              if (list.isEmpty)
                () => const Padding(
                  padding: EdgeInsets.all(24),
                  child: EmptyState(
                    icon: Icons.assignment_outlined,
                    title: 'No plan yet',
                    message: 'Make one on the computer, in Plan, or with `backdown plan --to /archive`.',
                  ),
                ),
              if (list.isNotEmpty) () => SectionHeader('Plans', trailing: Text('${list.length}')),
              for (final p in list) () => _PlanTile(plan: p, skew: skew),
              if (dedups.isNotEmpty) ...[
                () => SectionHeader('Tidying the archive', trailing: Text('${dedups.length}')),
                for (final r in dedups) () => _DedupTile(run: r),
              ],
            ];
            return ReadableList(itemCount: rows.length, itemBuilder: (c, i) => rows[i]());
          },
        ),
      ),
    );
  }
}

class _PlanTile extends StatelessWidget {
  const _PlanTile({required this.plan, required this.skew});
  final PlanSummary plan;
  final int skew;

  @override
  Widget build(BuildContext context) {
    final p = plan;
    return ListCard(
      onTap: () => context.push(Routes.plan(p.id)),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Expanded(child: Text('Plan ${p.id}', style: context.text.titleMedium)),
              StatePill(p.state, label: planStateLabel(p.state)),
            ],
          ),
          const SizedBox(height: 4),
          Row(
            children: [
              Icon(Icons.subdirectory_arrow_right, size: 16, color: context.bd.muted),
              const SizedBox(width: 4),
              Expanded(child: PathText(p.target, maxLines: 2)),
            ],
          ),
          const SizedBox(height: 4),
          Muted(
            'made ${when(p.createdAt)} (${rel(p.createdAt, skew: skew)}) · ${plural(p.items, 'thing')} · '
            '${p.copyFiles == 0 ? 'nothing new to copy' : 'copies ${bytes(p.copyBytes)} in ${plural(p.copyFiles, 'file')}'}',
          ),
          if (p.drives.isNotEmpty) ...[
            const SizedBox(height: 8),
            Wrap(
              spacing: 6,
              runSpacing: 4,
              children: [
                for (final d in p.drives)
                  Tag(
                    d.covered ? '${d.drive}: every file will have a copy' : '${d.drive}: some stays only there',
                    color: d.covered ? context.bd.good : context.bd.warn,
                    icon: d.covered ? Icons.check : Icons.info_outline,
                  ),
              ],
            ),
          ],
        ],
      ),
    );
  }
}

class _DedupTile extends StatelessWidget {
  const _DedupTile({required this.run});
  final DedupRun run;

  @override
  Widget build(BuildContext context) {
    final r = run;
    return ListTile(
      leading: Icon(Icons.link, color: context.bd.muted),
      title: Text('${r.link == 'clone' ? 'Clones' : 'Hardlinks'} in ${baseNameOf(r.folder)}'),
      subtitle: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          PathText(r.folder, maxLines: 2),
          Muted([when(r.createdAt), ?r.summary].join(' · ')),
        ],
      ),
      trailing: StatePill(r.state),
      onTap: () => openPath(context, r.folder),
    );
  }
}

String baseNameOf(String p) => leafOf(p).isEmpty ? p : leafOf(p);

/// One plan: where it files things, what's done, each drive's outcome, and every item with the
/// reason for its place.
class PlanScreen extends ConsumerStatefulWidget {
  const PlanScreen({super.key, required this.id});
  final int id;

  @override
  ConsumerState<PlanScreen> createState() => _PlanScreenState();
}

class _PlanScreenState extends ConsumerState<PlanScreen> {
  DryRun? _dry;
  String? _dryError;
  bool _trying = false;

  Future<void> _dryRun() async {
    final api = ref.read(apiProvider);
    if (api == null) return;
    setState(() {
      _trying = true;
      _dryError = null;
    });
    try {
      final r = DryRun.fromJson(asJson(await api.get('plan-dry', {'id': widget.id}, const Duration(minutes: 5))));
      if (mounted) setState(() => _dry = r);
    } on ApiException catch (e) {
      if (mounted) setState(() => _dryError = e.message);
    } finally {
      if (mounted) setState(() => _trying = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final plan = ref.watch(planProvider(widget.id));
    final daemon = ref.watch(daemonProvider).value;
    final applying = daemon?.current?.kind == 'apply' || (daemon?.queued.any((j) => j.kind == 'apply') ?? false);
    return Scaffold(
      appBar: AppBar(title: Text('Plan ${widget.id}')),
      body: RefreshIndicator(
        onRefresh: () => ref.refresh(planProvider(widget.id).future).catchError((_) => PlanDetail.fromJson(const {})),
        child: LibraryBody(
          value: plan,
          onRetry: () => ref.invalidate(planProvider(widget.id)),
          data: (p) => _body(context, p, applying),
        ),
      ),
    );
  }

  Widget _body(BuildContext context, PlanDetail p, bool applying) {
    const groups = [
      ('copy', 'Copy'),
      ('copy-unique', 'Copy only what\'s unique'),
      ('merge', 'Merge snapshots of a mailbox'),
      ('skip', 'Not copied'),
    ];
    final rows = <Widget Function()>[
      () => const LookOnlyNote(what: 'Copies and other changes'),
      () => _PlanHead(
        plan: p,
        applying: applying,
        trying: _trying,
        onDry: _dryRun,
        onApply: () => runAction(
          context,
          ref,
          (api) => api.post('apply', params: {'id': p.id}),
          done: 'Copying: follow it in Activity',
        ),
      ),
      if (_dry != null || _dryError != null) () => _DryBanner(dry: _dry, error: _dryError),
      if (p.journal.isNotEmpty) () => _Journal(journal: p.journal),
      () => SectionHeader('Drives', trailing: Text('${p.drives.length}')),
      for (final d in p.drives) () => _DriveOutcomeTile(drive: d),
      for (final (action, label) in groups)
        if (p.items.where((i) => i.action == action).toList() case final items when items.isNotEmpty) ...[
          () => SectionHeader(label, trailing: Text('${items.length}')),
          for (final i in items) () => _ItemCard(item: i, target: p.target),
        ],
    ];
    return ReadableList(itemCount: rows.length, itemBuilder: (c, i) => rows[i]());
  }
}

class _PlanHead extends ConsumerWidget {
  const _PlanHead({
    required this.plan,
    required this.applying,
    required this.trying,
    required this.onDry,
    required this.onApply,
  });

  final PlanDetail plan;
  final bool applying, trying;
  final VoidCallback onDry, onApply;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final p = plan;
    final skew = ref.watch(skewProvider);
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 12, 16, 4),
      child: Card(
        child: Padding(
          padding: const EdgeInsets.all(16),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  Expanded(
                    child: Text('Into', style: context.text.labelLarge?.copyWith(color: context.bd.muted)),
                  ),
                  StatePill(p.state, label: planStateLabel(p.state)),
                ],
              ),
              const SizedBox(height: 4),
              InkWell(
                onTap: () => openPath(context, p.target),
                child: PathText(p.target, color: context.linkColor),
              ),
              const SizedBox(height: 8),
              Muted(
                [
                  'made ${when(p.createdAt)} (${rel(p.createdAt, skew: skew)})',
                  if (p.leavesOutCaches) 'caches left out',
                  plural(p.items.length, 'thing'),
                ].join(' · '),
              ),
              const SizedBox(height: 4),
              Text(
                p.copyFiles == 0
                    ? 'Applying it copies nothing new.'
                    : 'Applying it copies ${bytes(p.copyBytes)} (${plural(p.copyFiles, 'file')}). Every copy is read back '
                          'and verified. Nothing is overwritten or deleted.',
                style: context.text.bodyMedium,
              ),
              const SizedBox(height: 12),
              Wrap(
                spacing: 8,
                runSpacing: 8,
                children: [
                  OutlinedButton.icon(
                    onPressed: trying ? null : onDry,
                    icon: trying
                        ? const SizedBox(width: 16, height: 16, child: CircularProgressIndicator(strokeWidth: 2))
                        : const Icon(Icons.science_outlined, size: 18),
                    label: const Text('Dry run'),
                  ),
                  if (p.state != 'done')
                    FilledButton.icon(
                      onPressed: applying ? null : onApply,
                      icon: const Icon(Icons.copy_all_outlined, size: 18),
                      label: Text(applying ? 'Copying…' : 'Apply: copy ${bytes(p.copyBytes)}'),
                    ),
                ],
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _DryBanner extends StatelessWidget {
  const _DryBanner({required this.dry, required this.error});
  final DryRun? dry;
  final String? error;

  @override
  Widget build(BuildContext context) {
    final d = dry;
    final String text;
    if (error != null || d == null) {
      text = error ?? '';
    } else {
      text = [
        'Dry run: ${plural(d.wouldCopy, 'file')} (${bytes(d.wouldCopyBytes)}) would be copied',
        if (d.wouldMerge > 0)
          '${plural(d.wouldMerge, 'mailbox', 'mailboxes')} would be merged from ${d.wouldMerge == 1 ? 'its' : 'their'} '
              'snapshots, message by message',
        if (d.resumed > 0) '${count(d.resumed)} are done already',
        if (d.duplicates > 0) '${count(d.duplicates)} are already in the archive',
        if (d.conflicts > 0) '${count(d.conflicts)} destinations exist and will be compared, never overwritten',
      ].join('; ');
    }
    final c = error != null ? context.bd.warn : context.bd.info;
    return Container(
      margin: const EdgeInsets.fromLTRB(16, 8, 16, 4),
      padding: const EdgeInsets.all(12),
      decoration: BoxDecoration(color: c.withValues(alpha: 0.12), borderRadius: BorderRadius.circular(10)),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Icon(error != null ? Icons.error_outline : Icons.science_outlined, color: c, size: 20),
          const SizedBox(width: 10),
          Expanded(child: Text(error != null ? text : '$text. Nothing was written.')),
        ],
      ),
    );
  }
}

class _Journal extends StatelessWidget {
  const _Journal({required this.journal});
  final Map<String, ({int n, int bytes})> journal;

  @override
  Widget build(BuildContext context) {
    final j = journal;
    String n(String k) => count(j[k]!.n);
    final parts = <(String, Color?)>[
      if (j['copied'] != null)
        ('${n('copied')} files copied and verified (${bytes(j['copied']!.bytes)})', null)
      else if (j['merged'] == null)
        ('nothing copied yet', null),
      if (j['merged'] != null)
        ('${n('merged')} ${j['merged']!.n == 1 ? 'mailbox' : 'mailboxes'} merged (${bytes(j['merged']!.bytes)})', null),
      if (j['already-there'] != null) ('${n('already-there')} already there', null),
      if (j['duplicate'] != null) ('${n('duplicate')} already in the archive', null),
      if (j['conflict'] != null) ('${n('conflict')} conflicts', context.bd.warn),
      if (j['failed'] != null) ('${n('failed')} failed', context.bd.crit),
    ];
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 8, 16, 0),
      child: Text.rich(
        TextSpan(
          children: [
            const TextSpan(
              text: 'Done so far: ',
              style: TextStyle(fontWeight: FontWeight.w700),
            ),
            for (final (i, (t, c)) in parts.indexed) ...[
              if (i > 0) const TextSpan(text: ' · '),
              TextSpan(
                text: t,
                style: c == null ? null : TextStyle(color: c, fontWeight: FontWeight.w600),
              ),
            ],
          ],
        ),
        style: context.text.bodyMedium,
      ),
    );
  }
}

class _DriveOutcomeTile extends StatelessWidget {
  const _DriveOutcomeTile({required this.drive});
  final DriveOutcome drive;

  @override
  Widget build(BuildContext context) {
    final d = drive;
    return ListCard(
      onTap: () => openPath(context, d.path),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Icon(Icons.storage, color: context.bd.folder, size: 20),
              const SizedBox(width: 8),
              Expanded(child: Text(d.drive, style: context.text.titleSmall)),
              Text(bytes(d.bytes), style: context.text.labelLarge),
            ],
          ),
          const SizedBox(height: 2),
          PathText(d.path, maxLines: 1),
          const SizedBox(height: 8),
          Row(
            children: [
              Expanded(
                child: _Figure(label: 'To copy', value: bytes(d.copy)),
              ),
              Expanded(
                child: _Figure(label: 'Already archived', value: bytes(d.already)),
              ),
            ],
          ),
          const SizedBox(height: 8),
          d.covered
              ? Tag('every file will have a copy', color: context.bd.good, icon: Icons.check)
              : Tag('${bytes(d.notCopied)} stays only here', color: context.bd.warn, icon: Icons.info_outline),
        ],
      ),
    );
  }
}

class _Figure extends StatelessWidget {
  const _Figure({required this.label, required this.value});
  final String label, value;

  @override
  Widget build(BuildContext context) => Column(
    crossAxisAlignment: CrossAxisAlignment.start,
    children: [
      Muted(label),
      Text(value, style: context.text.titleSmall?.copyWith(fontWeight: FontWeight.w700)),
    ],
  );
}

class _ItemCard extends StatelessWidget {
  const _ItemCard({required this.item, required this.target});
  final PlanItem item;
  final String target;

  @override
  Widget build(BuildContext context) {
    final i = item;
    final color = kindColor(i.kind);
    return ListCard(
      onTap: () => openKey(context, i.sourceKey, isFile: i.isFile),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Icon(kindIcon(i.kind), color: color, size: 22),
          const SizedBox(width: 12),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Row(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Expanded(child: Text(i.what, style: context.text.titleSmall)),
                    const SizedBox(width: 8),
                    Text(bytes(i.bytes), style: context.text.labelLarge?.copyWith(fontWeight: FontWeight.w700)),
                  ],
                ),
                const SizedBox(height: 4),
                PathText(i.source, maxLines: 3, color: context.linkColor),
                for (final m in i.merge) PathText('+ $m', maxLines: 2),
                if (i.action != 'skip') ...[
                  const SizedBox(height: 4),
                  Row(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Padding(
                        padding: const EdgeInsets.only(top: 1, right: 4),
                        child: Icon(Icons.subdirectory_arrow_right, size: 14, color: context.bd.muted),
                      ),
                      Expanded(child: PathText(i.dest, maxLines: 3)),
                    ],
                  ),
                ],
                const SizedBox(height: 6),
                Text(i.why, style: context.text.bodySmall),
              ],
            ),
          ),
        ],
      ),
    );
  }
}
