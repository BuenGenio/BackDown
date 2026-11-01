import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../../core/actions.dart';
import '../../core/format.dart';
import '../../core/widgets.dart';
import '../../providers.dart';
import '../../theme.dart';
import 'models.dart';
import 'providers.dart';
import 'widgets.dart';

/// A plan's state in words and a colour.
(String, Color) planState(BuildContext context, String state) => switch (state) {
  'new' => ('not copied yet', context.bd.info),
  'sending' => ('copying', context.bd.info),
  'partial' => ('partly copied', context.bd.warn),
  'done' => ('in the pool', context.bd.good),
  _ => (state, context.bd.muted),
};

/// The plans for drives of this machine into the pool (the wizard's, kept), newest first.
class PoolPlansScreen extends ConsumerWidget {
  const PoolPlansScreen({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final plans = ref.watch(poolPlansProvider);
    final skew = ref.watch(skewProvider);
    return Scaffold(
      appBar: AppBar(title: const Text('Plans into the pool')),
      body: plans.hasError && !plans.hasValue && isOlderService(plans.error)
          ? const EmptyState(
              icon: Icons.update,
              title: 'The pool needs a newer BackDown',
              message: 'This BackDown is from before the pool: update it on the computer to see its plans here.',
            )
          : AsyncBody(
              value: plans,
              onRetry: () => ref.invalidate(poolPlansProvider),
              data: (list) => RefreshIndicator(
                onRefresh: () => ref.refresh(poolPlansProvider.future).then((_) {}, onError: (_) {}),
                child: ListView.builder(
                  padding: wideInsets(context),
                  itemCount: list.length + 1,
                  itemBuilder: (c, i) {
                    if (i == 0) {
                      return list.isEmpty
                          ? const Padding(
                              padding: EdgeInsets.only(top: 40),
                              child: EmptyState(
                                icon: Icons.assignment_outlined,
                                title: 'No plans into the pool yet',
                                message:
                                    'A plan says what of a drive is in the pool already, by content on any machine, '
                                    'what\'s new, and which pool folder each thing on it goes into. They\'re made '
                                    'at the computer: in the Swarm, "Into the pool…" on a drive, or '
                                    '`backdown pool plan DRIVE` in a terminal.',
                              ),
                            )
                          : const LookOnlyNote(what: 'Plans and copying into the pool');
                    }
                    final p = list[i - 1];
                    final (state, color) = planState(context, p.state);
                    return ListTile(
                      leading: CircleAvatar(
                        backgroundColor: color.withValues(alpha: 0.14),
                        child: Icon(Icons.assignment_outlined, color: color),
                      ),
                      title: Text(p.drive, maxLines: 1, overflow: TextOverflow.ellipsis),
                      subtitle: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Text(p.root, maxLines: 1, overflow: TextOverflow.ellipsis),
                          Text(
                            [
                              'plan ${p.id}, made ${rel(p.createdAt, skew: skew)}',
                              if (p.state != 'done') '${bytes(p.toCopy)} to copy',
                              if (p.unplacedBytes > 0) '${bytes(p.unplacedBytes)} fits nowhere',
                            ].join(' · '),
                            maxLines: 2,
                            overflow: TextOverflow.ellipsis,
                          ),
                          const SizedBox(height: 4),
                          Pill(state, color: color),
                        ],
                      ),
                      isThreeLine: true,
                      trailing: const Icon(Icons.chevron_right),
                      onTap: () => context.push(MachinePaths.plan(p.id)),
                    );
                  },
                ),
              ),
            ),
    );
  }
}

const _kinds = {
  'mail-store': 'Mail',
  'maildir': 'Mail',
  'mdbox': 'Mail',
  'thunderbird': 'Mail',
  'mbox': 'Mail',
  'photos': 'Photos',
  'music': 'Music',
  'videos': 'Videos',
  'timemachine': 'Time Machine',
  'iphone-backup': 'iPhone backup',
  'disk-image': 'Disk image',
  'system': 'System',
  'git': 'Code',
  'unsorted': 'Folder',
};

/// A recogniser's kind in words: "disk-image" → "Disk image".
String kindWords(String kind) {
  final k = _kinds[kind];
  if (k != null) return k;
  final w = kind.replaceAll(RegExp(r'[-_]'), ' ').trim();
  return w.isEmpty ? '' : w[0].toUpperCase() + w.substring(1);
}

/// One plan into the pool, read-only: what of the drive is in the pool already, what's to copy,
/// where each thing goes and why. Choosing and copying are done at the computer.
class PoolPlanScreen extends ConsumerWidget {
  const PoolPlanScreen({super.key, required this.id});
  final int id;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final plan = ref.watch(poolPlanProvider(id));
    return Scaffold(
      appBar: AppBar(title: Text(plan.value == null ? 'Plan $id' : 'Into the pool: ${plan.value!.drive}')),
      body: AsyncBody(
        value: plan,
        onRetry: () => ref.invalidate(poolPlanProvider(id)),
        data: (p) => _PlanBody(plan: p),
      ),
    );
  }
}

class _PlanBody extends ConsumerWidget {
  const _PlanBody({required this.plan});
  final PoolPlan plan;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final p = plan;
    final skew = ref.watch(skewProvider);
    final (state, color) = planState(context, p.state);
    final used = [
      for (final u in p.areas)
        if (u.assignedBytes > 0 || u.indexed) u,
    ];
    final chips = <(int, String, Color?)>[
      (p.inPoolBytes, 'in the pool already', context.bd.good),
      if (p.twiceBytes > 0) (p.twiceBytes, 'twice on this drive (goes once)', null),
      (p.copyBytes, 'to copy', context.bd.info),
      if (p.unknownBytes > 0) (p.unknownBytes, 'not compared yet', context.bd.warn),
      if (p.leftBytes > 0) (p.leftBytes, 'left out', context.bd.muted),
      if (p.unplacedBytes > 0) (p.unplacedBytes, 'fits nowhere', context.bd.warn),
    ];
    final header = <Widget>[
      const LookOnlyNote(what: 'Choices and copying into the pool'),
      Padding(
        padding: const EdgeInsets.fromLTRB(16, 12, 16, 0),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(p.root, style: context.text.bodyMedium?.copyWith(color: context.colors.onSurfaceVariant)),
            const SizedBox(height: 2),
            Text(
              '${plural(p.files, 'file')}, ${bytes(p.bytes)} · on ${p.machine} · plan ${p.id}, made '
              '${rel(p.createdAt, skew: skew)}',
              style: context.text.bodySmall?.copyWith(color: context.bd.muted),
            ),
            const SizedBox(height: 8),
            Pill(state, color: color),
          ],
        ),
      ),
      Padding(
        padding: const EdgeInsets.fromLTRB(16, 12, 16, 0),
        child: Wrap(
          spacing: 8,
          runSpacing: 8,
          children: [for (final (n, label, c) in chips) _Chip(amount: n, label: label, color: c)],
        ),
      ),
      if (p.unknownBytes > 0)
        NoteCard(
          icon: Icons.help_outline,
          color: context.bd.warn,
          margin: const EdgeInsets.fromLTRB(16, 12, 16, 0),
          text:
              '${bytes(p.unknownBytes)} on this drive has the size of something in the pool, but hasn\'t been compared '
              'by content yet. It\'s counted as needing room until it is.',
        ),
      for (final n in p.notes)
        NoteCard(icon: Icons.info_outline, margin: const EdgeInsets.fromLTRB(16, 8, 16, 0), text: n),
      const SectionHeader('Where it goes'),
      if (used.isEmpty)
        Padding(
          padding: const EdgeInsets.symmetric(horizontal: 16),
          child: Text(
            'There\'s no pool folder yet.',
            style: context.text.bodyMedium?.copyWith(color: context.bd.muted),
          ),
        ),
      for (final u in used) _AreaRow(use: u),
      SectionHeader('What\'s on it', trailing: Text('${p.things.length}', style: context.text.labelMedium)),
      Padding(
        padding: const EdgeInsets.fromLTRB(16, 0, 16, 4),
        child: Text(
          'Each thing is filed whole, in one pool folder.',
          style: context.text.bodySmall?.copyWith(color: context.bd.muted),
        ),
      ),
    ];
    return ListView.builder(
      padding: wideInsets(context),
      itemCount: header.length + p.things.length + 1,
      itemBuilder: (c, i) {
        if (i < header.length) return header[i];
        final j = i - header.length;
        if (j == p.things.length) return _CopyNote(plan: p);
        return _ThingTile(thing: p.things[j]);
      },
    );
  }
}

class _Chip extends StatelessWidget {
  const _Chip({required this.amount, required this.label, this.color});
  final int amount;
  final String label;
  final Color? color;

  @override
  Widget build(BuildContext context) {
    final c = color ?? context.colors.onSurfaceVariant;
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
      decoration: BoxDecoration(
        color: c.withValues(alpha: 0.10),
        borderRadius: BorderRadius.circular(12),
        border: Border.all(color: c.withValues(alpha: 0.25)),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        mainAxisSize: MainAxisSize.min,
        children: [
          Text(
            amount == 0 ? 'nothing' : bytes(amount),
            style: context.text.titleSmall?.copyWith(color: c, fontWeight: FontWeight.w700),
          ),
          Text(label, style: context.text.labelSmall?.copyWith(color: context.colors.onSurfaceVariant)),
        ],
      ),
    );
  }
}

class _AreaRow extends StatelessWidget {
  const _AreaRow({required this.use});
  final AreaUse use;

  @override
  Widget build(BuildContext context) {
    final u = use;
    final after = (u.room - u.assignedBytes).clamp(0, u.room);
    final fill = u.room == 0 ? 0.0 : (u.assignedBytes / u.room).clamp(0.0, 1.0);
    final state = !u.indexed
        ? 'not indexed yet'
        : !u.present
        ? 'not there'
        : null;
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 4, 16, 10),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Wrap(
            spacing: 6,
            crossAxisAlignment: WrapCrossAlignment.center,
            children: [
              Text(u.at?.machine ?? '?', style: context.text.titleSmall),
              Text(u.at?.path ?? '', style: context.text.bodyMedium?.copyWith(color: context.colors.onSurfaceVariant)),
              if (state != null) Pill(state, color: context.bd.warn),
            ],
          ),
          Text(
            [if (u.drive.isNotEmpty) u.drive, if (u.takes.isNotEmpty) 'takes ${u.takes.join(', ')}'].join(' · '),
            style: context.text.bodySmall?.copyWith(color: context.bd.muted),
          ),
          const SizedBox(height: 6),
          Meter(
            fraction: fill,
            height: 8,
            color: fill > 0.95
                ? context.bd.crit
                : fill > 0.85
                ? context.bd.warn
                : context.bd.folder,
          ),
          const SizedBox(height: 3),
          Text(
            '${u.assignedBytes > 0 ? '+${bytes(u.assignedBytes)} (${plural(u.assignedFiles, 'file')}) · ' : ''}'
            '${bytes(after)} left to fill',
            style: context.text.bodySmall,
          ),
        ],
      ),
    );
  }
}

class _ThingTile extends StatelessWidget {
  const _ThingTile({required this.thing});
  final PoolThing thing;

  @override
  Widget build(BuildContext context) {
    final t = thing;
    final kind = kindWords(t.item.kind);
    final parts = [
      if (t.need > 0) '${bytes(t.need)} to copy',
      if (t.inPoolBytes > 0) '${bytes(t.inPoolBytes)} in the pool already',
      if (t.twiceBytes > 0) '${bytes(t.twiceBytes)} twice',
      if (t.leftBytes > 0) '${bytes(t.leftBytes)} left out',
    ];
    final canCopy = t.newBytes + t.unknownBytes > 0;
    final String where;
    final Color whereColor;
    var into = false;
    if (t.skip) {
      where = 'Left out';
      whereColor = context.bd.muted;
    } else if (!canCopy) {
      where = 'Nothing to copy';
      whereColor = context.bd.good;
    } else if (t.to != null) {
      where = t.to!.label;
      whereColor = context.colors.primary;
      into = true;
    } else {
      where = 'Nowhere has room';
      whereColor = context.bd.warn;
    }
    return Opacity(
      opacity: t.skip ? 0.55 : 1,
      child: Padding(
        padding: const EdgeInsets.fromLTRB(16, 8, 16, 8),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Padding(
              padding: const EdgeInsets.only(top: 2),
              child: Icon(_icon(t.item.kind), size: 22, color: context.bd.folder),
            ),
            const SizedBox(width: 14),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Wrap(
                    spacing: 6,
                    runSpacing: 2,
                    crossAxisAlignment: WrapCrossAlignment.center,
                    children: [
                      Text(t.item.what, style: context.text.bodyMedium?.copyWith(fontWeight: FontWeight.w600)),
                      if (kind.isNotEmpty && !t.item.what.startsWith(kind)) Pill(kind, color: context.bd.muted),
                    ],
                  ),
                  Text(t.item.dest, style: context.text.bodySmall?.copyWith(color: context.colors.onSurfaceVariant)),
                  if (parts.isNotEmpty || t.inPoolAt != null)
                    Text(
                      [...parts, if (t.inPoolAt != null) 'e.g. ${t.inPoolAt}'].join(' · '),
                      style: context.text.bodySmall?.copyWith(color: context.bd.muted),
                    ),
                  const SizedBox(height: 4),
                  Row(
                    children: [
                      if (into) ...[
                        Icon(Icons.arrow_forward, size: 14, color: whereColor, semanticLabel: 'into'),
                        const SizedBox(width: 4),
                      ],
                      Flexible(
                        child: Text(
                          where,
                          style: context.text.bodySmall?.copyWith(color: whereColor, fontWeight: FontWeight.w700),
                        ),
                      ),
                    ],
                  ),
                  if (t.whyTo.isNotEmpty)
                    Text(t.whyTo, style: context.text.bodySmall?.copyWith(color: context.bd.muted)),
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }

  static IconData _icon(String kind) => switch (kind) {
    'mail-store' || 'maildir' || 'mdbox' || 'thunderbird' || 'mbox' => Icons.mail_outline,
    'photos' => Icons.photo_library_outlined,
    'music' => Icons.library_music_outlined,
    'videos' => Icons.video_library_outlined,
    'timemachine' => Icons.history,
    'iphone-backup' || 'ios-backup' => Icons.phone_iphone,
    'disk-image' => Icons.album_outlined,
    'git' => Icons.code,
    _ => Icons.folder_outlined,
  };
}

/// What's left to do, and that it's done at the computer.
class _CopyNote extends StatelessWidget {
  const _CopyNote({required this.plan});
  final PoolPlan plan;

  @override
  Widget build(BuildContext context) {
    final p = plan;
    final folders = p.areas.where((u) => u.assignedBytes > 0).length;
    final String title, text;
    switch (p.state) {
      case 'done':
        title = 'In the pool';
        text =
            'This plan has been carried out. Freeing the drive is done at the computer, in Free up, once every file on '
            'it has a copy that counts.';
      case 'sending':
        title = 'Copying into the pool';
        text = 'The copy runs in BackDown\'s service: Activity shows how it\'s going.';
      default:
        if (p.copyBytes == 0) {
          title = 'Nothing to copy';
          text = 'Everything on it is in the pool already, or left out.';
        } else {
          title = p.state == 'partial' ? 'Partly copied' : 'Copy it into the pool';
          text =
              '${bytes(p.copyBytes)} goes into ${plural(folders, 'pool folder')}'
              '${p.unplacedBytes > 0 ? '; ${bytes(p.unplacedBytes)} fits nowhere and stays on the drive' : ''}. Each '
              'file is written beside its place, read back there and compared by SHA-256; nothing is ever overwritten, '
              'and nothing on the drive is changed. Choosing where things go and starting the copy are done at the '
              'computer: in the Swarm, or `backdown pool send ${p.id}`.';
        }
    }
    return SectionCard(
      margin: const EdgeInsets.fromLTRB(16, 16, 16, 0),
      children: [
        Text(title, style: context.text.titleSmall),
        const SizedBox(height: 4),
        Text(text, style: context.text.bodyMedium),
      ],
    );
  }
}
