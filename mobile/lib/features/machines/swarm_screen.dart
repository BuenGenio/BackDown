import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../../api/api.dart';
import '../../core/format.dart';
import '../../core/widgets.dart';
import '../../providers.dart';
import '../../router.dart';
import '../../theme.dart';
import 'models.dart';
import 'providers.dart';
import 'widgets.dart';

/// Every machine as one (docs/SWARM.md): each machine's disks, their volumes as bars of used and
/// free space with what's indexed on them, the drives that aren't connected, the totals, and the
/// pool. This machine is probed when the view opens; the others show as last kept, then each is
/// asked over ssh (by the service, with its person's keys) and redrawn as it answers.
class SwarmScreen extends ConsumerWidget {
  const SwarmScreen({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final swarm = ref.watch(swarmProvider);
    return Scaffold(
      appBar: AppBar(
        title: const Text('Swarm'),
        actions: [
          IconButton(
            tooltip: 'Where is it?',
            icon: const Icon(Icons.travel_explore),
            onPressed: () => context.push(MachinePaths.where()),
          ),
          IconButton(
            tooltip: 'Other machines',
            icon: const Icon(Icons.dns_outlined),
            onPressed: () => context.push(Routes.machines),
          ),
        ],
      ),
      body: swarm.hasError && !swarm.hasValue && isOlderService(swarm.error)
          ? const EmptyState(
              icon: Icons.update,
              title: 'The swarm needs a newer BackDown',
              message:
                  'This BackDown is from before the swarm: update it on the computer to see every machine\'s drives, '
                  'their free space and the pool here. The machines it syncs are under Other machines.',
            )
          : AsyncBody(
              value: swarm,
              onRetry: () => ref.invalidate(swarmProvider),
              loading: const Center(child: Text('Looking at the drives…')),
              data: (o) => RefreshIndicator(
                onRefresh: () async {
                  ref.invalidate(swarmAskProvider);
                  await ref.refresh(swarmProvider.future).then((_) {}, onError: (_) {});
                },
                child: _SwarmBody(overview: o),
              ),
            ),
    );
  }
}

/// A machine as shown: as kept, or as it answered just now; whether it's being asked, and why it
/// couldn't be.
typedef Shown = ({SwarmMachine m, bool asking, String? error});

class _SwarmBody extends ConsumerWidget {
  const _SwarmBody({required this.overview});
  final SwarmOverview overview;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final o = overview;
    final shown = <Shown>[
      for (final m in o.machines)
        if (m.here || m.id == null || !o.canAsk)
          (m: m, asking: false, error: m.state == 'error' ? m.error : null)
        else
          _asked(ref.watch(swarmAskProvider(m.id!)), m),
    ];
    final t = SwarmTotals([for (final s in shown) s.m]);
    final pools = shown.fold(0, (a, s) => a + s.m.pool.length);
    final items = <Widget>[
      const _WhereBar(),
      _TotalsCard(totals: t),
      if (o.whyNot != null && o.machines.length > 1)
        NoteCard(
          icon: Icons.key_off_outlined,
          color: context.bd.warn,
          text: '${capitalised(o.whyNot!)}: the other machines show as last synced.',
        ),
      _PoolCard(shown: shown, totals: t.pool),
      SectionHeader(
        'Machines',
        trailing: o.canAsk && o.machines.length > 1
            ? TextButton.icon(
                onPressed: () => ref.invalidate(swarmAskProvider),
                icon: const Icon(Icons.refresh, size: 18),
                label: const Text('Ask them again'),
              )
            : Text('${o.machines.length}', style: context.text.labelMedium),
        padding: const EdgeInsets.fromLTRB(16, 12, 8, 0),
      ),
      for (final s in shown) _MachineSection(shown: s, canAsk: o.canAsk),
      if (o.machines.length < 2 && pools == 0)
        const NoteCard(
          icon: Icons.hub_outlined,
          text:
              'This is a swarm of one. Add the other machines you run BackDown on (a NAS, a server, another '
              'computer) at the computer, reached with its own ssh keys: their drives show up here too, and where '
              'every file is.',
        ),
    ];
    return ListView.builder(
      padding: wideInsets(context, top: 4),
      itemCount: items.length,
      itemBuilder: (c, i) => items[i],
    );
  }

  static Shown _asked(AsyncValue<SwarmMachine> ask, SwarmMachine kept) {
    final m = ask.value ?? kept;
    final String? error;
    if (ask.hasError) {
      final e = ask.error;
      error = e is ApiException ? e.message : '$e';
    } else if (ask.hasValue && m.asked == false) {
      error = m.askError ?? m.error;
    } else {
      error = m.state == 'error' ? m.error : null;
    }
    return (m: m, asking: ask.isLoading, error: error);
  }
}

class _WhereBar extends StatelessWidget {
  const _WhereBar();

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 8, 16, 4),
      child: Material(
        color: context.colors.surfaceContainerHigh,
        borderRadius: BorderRadius.circular(28),
        child: InkWell(
          borderRadius: BorderRadius.circular(28),
          onTap: () => context.push(MachinePaths.where()),
          child: Padding(
            padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 14),
            child: Row(
              children: [
                Icon(Icons.travel_explore, color: context.colors.onSurfaceVariant),
                const SizedBox(width: 12),
                Expanded(
                  child: Text(
                    'Where is it? A name, on every machine',
                    style: context.text.bodyLarge?.copyWith(color: context.colors.onSurfaceVariant),
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
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

class _TotalsCard extends StatelessWidget {
  const _TotalsCard({required this.totals});
  final SwarmTotals totals;

  @override
  Widget build(BuildContext context) {
    final t = totals;
    return Column(
      children: [
        SectionCard(
          margin: const EdgeInsets.fromLTRB(16, 8, 16, 4),
          children: [
            Row(
              crossAxisAlignment: CrossAxisAlignment.baseline,
              textBaseline: TextBaseline.alphabetic,
              children: [
                Text(bytes(t.free), style: context.text.headlineSmall?.copyWith(fontWeight: FontWeight.w700)),
                const SizedBox(width: 8),
                Expanded(
                  child: Text(
                    'free of ${bytes(t.capacity)}',
                    style: context.text.bodyMedium?.copyWith(color: context.colors.onSurfaceVariant),
                  ),
                ),
              ],
            ),
            const SizedBox(height: 10),
            SpaceBar(total: t.capacity, free: t.free, height: 10),
            const SizedBox(height: 8),
            Text(
              'Across ${plural(t.disks, 'drive')} on ${plural(t.machines, 'machine')}'
              '${t.reporting < t.machines ? ' (${t.reporting} saying how full they are)' : ''}',
              style: context.text.bodySmall?.copyWith(color: context.bd.muted),
            ),
          ],
        ),
        Padding(
          padding: const EdgeInsets.fromLTRB(16, 6, 16, 4),
          child: IntrinsicHeight(
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Expanded(
                  child: StatTile(
                    label: 'Indexed',
                    value: count(t.files),
                    sub: bytes(t.bytes),
                    icon: Icons.description_outlined,
                  ),
                ),
                const SizedBox(width: 10),
                Expanded(
                  child: StatTile(
                    label: 'Machines',
                    value: '${t.machines}',
                    sub: plural(t.disks, 'drive'),
                    icon: Icons.dns_outlined,
                  ),
                ),
                const SizedBox(width: 10),
                Expanded(
                  child: StatTile(
                    label: 'Pool',
                    value: bytes(t.pool.bytes),
                    sub: t.pool.areas == 0 ? 'no folders yet' : '${bytes(t.pool.room)} to fill',
                    icon: Icons.inventory_2_outlined,
                  ),
                ),
              ],
            ),
          ),
        ),
      ],
    );
  }
}

// ---------- the pool

class _PoolCard extends StatelessWidget {
  const _PoolCard({required this.shown, required this.totals});
  final List<Shown> shown;
  final PoolTotals totals;

  @override
  Widget build(BuildContext context) {
    final folders = [
      for (final s in shown)
        for (final a in s.m.pool) (machine: s.m, folder: a),
    ];
    final head = Row(
      children: [
        Icon(Icons.inventory_2_outlined, color: context.colors.primary, size: 22),
        const SizedBox(width: 10),
        Expanded(child: Text('The pool', style: context.text.titleMedium)),
      ],
    );
    final plans = ListTile(
      contentPadding: EdgeInsets.zero,
      dense: true,
      leading: const Icon(Icons.assignment_outlined),
      title: const Text('Plans into the pool'),
      subtitle: const Text('What of each drive is in it already, and where the rest goes'),
      trailing: const Icon(Icons.chevron_right),
      onTap: () => context.push(MachinePaths.plans),
    );
    if (folders.isEmpty) {
      return SectionCard(
        margin: const EdgeInsets.fromLTRB(16, 10, 16, 4),
        children: [
          head,
          const SizedBox(height: 8),
          Text(
            'The pool is where your drives end up: folders on any of your machines\' drives that together make one '
            'filed archive (Mail, Media/Photos, Machines, …). Each file goes in once, wherever it came from; then the '
            'old drive can be freed.',
            style: context.text.bodyMedium,
          ),
          const SizedBox(height: 10),
          Text(
            'A pool folder is added on the machine whose drive it\'s on:',
            style: context.text.bodySmall?.copyWith(color: context.bd.muted),
          ),
          const SizedBox(height: 6),
          const CommandLine('backdown pool add /path/to/folder'),
          const SizedBox(height: 4),
          plans,
        ],
      );
    }
    return SectionCard(
      margin: const EdgeInsets.fromLTRB(16, 10, 16, 4),
      children: [
        head,
        Padding(
          padding: const EdgeInsets.only(left: 32, top: 2),
          child: Text(
            '${plural(folders.length, 'folder')} · ${plural(totals.files, 'file')}, ${bytes(totals.bytes)} · '
            '${bytes(totals.room)} to fill',
            style: context.text.bodySmall?.copyWith(color: context.bd.muted),
          ),
        ),
        for (final (:machine, :folder) in folders) ...[
          const Divider(height: 20),
          _PoolFolderRow(machine: machine, folder: folder),
        ],
        const Divider(height: 20),
        plans,
        Text(
          'Pouring a drive into the pool, and adding or changing pool folders, is done at the computer.',
          style: context.text.bodySmall?.copyWith(color: context.bd.muted),
        ),
      ],
    );
  }
}

class _PoolFolderRow extends StatelessWidget {
  const _PoolFolderRow({required this.machine, required this.folder});
  final SwarmMachine machine;
  final PoolFolder folder;

  @override
  Widget build(BuildContext context) {
    final a = folder;
    final takes = a.takes.isEmpty ? 'takes anything' : 'takes ${a.takes.join(', ')}';
    final drive = a.drive?.words;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Wrap(
          spacing: 6,
          runSpacing: 4,
          crossAxisAlignment: WrapCrossAlignment.center,
          children: [
            Text(machine.name, style: context.text.titleSmall),
            Text(a.path, style: context.text.bodyMedium?.copyWith(color: context.colors.onSurfaceVariant)),
            if (!a.indexed) Pill('not indexed yet', color: context.bd.warn),
          ],
        ),
        const SizedBox(height: 2),
        Text(
          [?drive, takes, 'holds ${plural(a.files, 'file')}, ${bytes(a.bytes)}'].join(' · '),
          style: context.text.bodySmall?.copyWith(color: context.bd.muted),
        ),
        const SizedBox(height: 8),
        if (a.space case final s?) ...[
          SpaceBar(total: s.total, free: s.free, reserve: a.reserve),
          const SizedBox(height: 4),
          Text(
            '${bytes(a.room)} to fill · ${bytes(s.free)} free of ${bytes(s.total)}, ${bytes(a.reserve)} kept free',
            style: context.text.bodySmall,
          ),
        ] else
          Text(
            a.answering ? 'Its drive isn\'t there' : 'Its drive isn\'t answering',
            style: context.text.bodySmall?.copyWith(color: context.bd.warn),
          ),
      ],
    );
  }
}

// ---------- each machine

class _MachineSection extends ConsumerWidget {
  const _MachineSection({required this.shown, required this.canAsk});
  final Shown shown;
  final bool canAsk;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final m = shown.m;
    final skew = ref.watch(skewProvider);
    final (pill, line) = _state(context, skew);
    final t = m.totals;
    final sums = [
      if (t.capacity > 0) '${bytes(t.free)} free of ${bytes(t.capacity)}',
      if (t.files > 0) '${count(t.files)} files indexed',
    ].join(' · ');
    final extras = [
      for (final r in m.elsewhere) _RootRow(machine: m, root: r, kind: _Extra.elsewhere),
      for (final r in m.away) _RootRow(machine: m, root: r, kind: _Extra.away),
      for (final r in m.unanswered) _RootRow(machine: m, root: r, kind: _Extra.unanswered),
    ];
    final empty = m.disks.isEmpty && extras.isEmpty;
    return SectionCard(
      padding: const EdgeInsets.fromLTRB(14, 8, 4, 12),
      children: [
        Row(
          children: [
            Icon(m.here ? Icons.computer_outlined : Icons.dns_outlined, color: context.colors.primary),
            const SizedBox(width: 10),
            Expanded(
              child: Wrap(
                spacing: 8,
                runSpacing: 4,
                crossAxisAlignment: WrapCrossAlignment.center,
                children: [
                  Text(m.name, style: context.text.titleMedium),
                  pill,
                ],
              ),
            ),
            if (!m.here && m.id != null) ...[
              if (canAsk)
                IconButton(
                  tooltip: 'Ask ${m.name} again',
                  icon: const Icon(Icons.refresh),
                  onPressed: shown.asking ? null : () => ref.invalidate(swarmAskProvider(m.id!)),
                ),
              IconButton(
                tooltip: 'Open ${m.name}',
                icon: const Icon(Icons.chevron_right),
                onPressed: () => context.push(Routes.machine(m.id!)),
              ),
            ] else
              const SizedBox(height: 48),
          ],
        ),
        Padding(
          padding: const EdgeInsets.only(left: 34, right: 10),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              if (line != null)
                Text(line, style: context.text.bodySmall?.copyWith(color: context.colors.onSurfaceVariant)),
              if (sums.isNotEmpty) Text(sums, style: context.text.bodySmall?.copyWith(color: context.bd.muted)),
            ],
          ),
        ),
        Padding(
          padding: const EdgeInsets.only(right: 10),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              for (final d in m.disks) _DiskBlock(machine: m, disk: d),
              if (extras.isNotEmpty) ...[
                const SizedBox(height: 10),
                Text(
                  (m.disks.isEmpty ? 'Indexed drives' : 'Indexed, not on a drive above').toUpperCase(),
                  style: context.text.labelSmall?.copyWith(
                    color: context.bd.muted,
                    fontWeight: FontWeight.w700,
                    letterSpacing: 0.6,
                  ),
                ),
                ...extras,
              ],
              if (empty)
                Padding(
                  padding: const EdgeInsets.only(top: 10),
                  child: Text(
                    m.here
                        ? 'No drives found.'
                        : m.state == 'never'
                        ? 'Not asked yet.'
                        : 'No drives listed.',
                    style: context.text.bodyMedium?.copyWith(color: context.bd.muted),
                  ),
                ),
              for (final n in m.notes)
                Padding(
                  padding: const EdgeInsets.only(top: 8),
                  child: Text(n, style: context.text.bodySmall?.copyWith(color: context.bd.muted)),
                ),
            ],
          ),
        ),
      ],
    );
  }

  /// The machine's state as a pill, and a line saying more (swarm.js's stateLine).
  (Widget, String?) _state(BuildContext context, int skew) {
    final m = shown.m;
    final asOf = m.okAt == null ? null : 'drives as of ${rel(m.okAt, skew: skew)}';
    if (m.here) return (Pill('this machine', color: context.bd.good), null);
    if (shown.asking) {
      return (Pill('asking…', color: context.bd.info, icon: Icons.sync), asOf);
    }
    if (m.state == 'unsupported') {
      return (
        Pill('update BackDown there', color: context.bd.warn),
        'BackDown ${m.version ?? ''} on ${m.name} doesn\'t say which drives it has or how full they are',
      );
    }
    final err = shown.error;
    if (err != null) {
      return (Pill('couldn\'t be asked', color: context.bd.warn), [firstLine(err), ?asOf].join(' · '));
    }
    if (m.state == 'never') return (Pill('not asked yet', color: context.bd.muted), null);
    return (Pill('reached', color: context.bd.good), asOf);
  }
}

class _DiskBlock extends StatelessWidget {
  const _DiskBlock({required this.machine, required this.disk});
  final SwarmMachine machine;
  final SwarmDisk disk;

  @override
  Widget build(BuildContext context) {
    final d = disk;
    final what = [
      ?d.model,
      if (d.serial != null && d.serial!.isNotEmpty) 's/n ${d.serial}',
      bytes(d.size),
      ?d.transport,
      if (d.rotational == true) 'spinning' else if (d.rotational == false) 'solid state',
    ].join(' · ');
    final (healthText, healthColor) = switch (d.health) {
      'ok' => ('healthy', context.bd.good),
      'warning' => ('warning', context.bd.warn),
      'failing' => ('failing', context.bd.crit),
      _ => (null, null),
    };
    final icon = switch (d.transport) {
      'usb' => Icons.usb,
      'mmc' || 'sd' => Icons.sd_card_outlined,
      'nvme' => Icons.memory,
      _ => Icons.storage,
    };
    return Padding(
      padding: const EdgeInsets.only(top: 12),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Padding(
                padding: const EdgeInsets.only(top: 1),
                child: Icon(icon, size: 20, color: context.bd.muted),
              ),
              const SizedBox(width: 14),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Wrap(
                      spacing: 6,
                      runSpacing: 4,
                      crossAxisAlignment: WrapCrossAlignment.center,
                      children: [
                        Text(d.name, style: context.text.titleSmall),
                        if (healthText != null) Pill(healthText, color: healthColor),
                        if (d.system) Pill('system', color: context.bd.muted),
                      ],
                    ),
                    Text(what, style: context.text.bodySmall?.copyWith(color: context.bd.muted)),
                  ],
                ),
              ),
            ],
          ),
          if (d.volumes.isEmpty)
            Padding(
              padding: const EdgeInsets.only(left: 34, top: 6),
              child: Text(
                'Nothing on it BackDown can read.',
                style: context.text.bodySmall?.copyWith(color: context.bd.muted),
              ),
            ),
          for (final v in d.volumes) _VolumeRow(machine: machine, volume: v),
        ],
      ),
    );
  }
}

class _VolumeRow extends StatelessWidget {
  const _VolumeRow({required this.machine, required this.volume});
  final SwarmMachine machine;
  final SwarmVolume volume;

  @override
  Widget build(BuildContext context) {
    final v = volume;
    final kind = [?v.fsName, if (v.via != null) 'in ${v.via}'].join(' ');
    final s = v.space;
    return Padding(
      padding: const EdgeInsets.only(left: 34, top: 8),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Wrap(
            spacing: 6,
            runSpacing: 2,
            crossAxisAlignment: WrapCrossAlignment.center,
            children: [
              Text(v.title, style: context.text.bodyMedium?.copyWith(fontWeight: FontWeight.w600)),
              if (kind.isNotEmpty) Text(kind, style: context.text.bodySmall?.copyWith(color: context.bd.muted)),
              if (v.inPool) Pill('pool', color: context.bd.good, icon: Icons.inventory_2_outlined),
            ],
          ),
          if (v.mounts.isNotEmpty)
            Text(
              v.mounts.join(', '),
              style: context.text.bodySmall?.copyWith(color: context.colors.onSurfaceVariant),
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
            ),
          const SizedBox(height: 6),
          if (s != null) ...[
            SpaceBar(total: s.total, free: s.free),
            const SizedBox(height: 3),
            Text(
              '${bytes(s.free)} free of ${bytes(s.total)} · ${percent(s.fraction)} used',
              style: context.text.bodySmall,
            ),
          ] else
            Text(
              '${bytes(v.size)}, ${v.state == 'encrypted' ? 'locked' : 'not mounted'}',
              style: context.text.bodySmall?.copyWith(color: context.bd.muted),
            ),
          if (v.indexed.isNotEmpty) ...[
            const SizedBox(height: 6),
            Wrap(
              spacing: 6,
              runSpacing: 6,
              children: [for (final i in v.indexed) _IndexedChip(machine: machine, indexed: i)],
            ),
          ],
        ],
      ),
    );
  }
}

class _IndexedChip extends StatelessWidget {
  const _IndexedChip({required this.machine, required this.indexed});
  final SwarmMachine machine;
  final IndexedOn indexed;

  @override
  Widget build(BuildContext context) {
    final i = indexed;
    return ActionChip(
      avatar: Icon(Icons.folder, size: 18, color: context.bd.folder),
      label: Text('${i.title} · ${count(i.files)} files · ${bytes(i.bytes)}'),
      labelStyle: context.text.labelMedium,
      visualDensity: VisualDensity.compact,
      onPressed: () => openIndexed(context, machine, i.key),
    );
  }
}

/// Opens an indexed drive: here, its folder; on another machine, browsing it there.
void openIndexed(BuildContext context, SwarmMachine m, String key) {
  if (m.here || m.id == null) {
    context.push(Routes.folder(key));
  } else {
    context.push(MachinePaths.ls(m.id!, dir: key));
  }
}

enum _Extra { elsewhere, away, unanswered }

class _RootRow extends StatelessWidget {
  const _RootRow({required this.machine, required this.root, required this.kind});
  final SwarmMachine machine;
  final SwarmRoot root;
  final _Extra kind;

  @override
  Widget build(BuildContext context) {
    final r = root;
    final s = r.space;
    final (Widget? pill, String detail) = switch (kind) {
      _Extra.away => (
        Pill('not connected', color: context.bd.muted, icon: Icons.usb_off_outlined),
        [r.path, if (r.drive != null) 'last on ${r.drive!.words}'].join(' · '),
      ),
      _Extra.unanswered => (Pill('not answering', color: context.bd.warn), r.path),
      _Extra.elsewhere => (null, r.path),
    };
    return InkWell(
      borderRadius: BorderRadius.circular(8),
      onTap: () => openIndexed(context, machine, r.key),
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: 8),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Padding(
              padding: const EdgeInsets.only(top: 1),
              child: Icon(
                kind == _Extra.away ? Icons.folder_off_outlined : Icons.folder,
                size: 20,
                color: kind == _Extra.away ? context.bd.muted : context.bd.folder,
              ),
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
                      Text(r.title, style: context.text.bodyMedium?.copyWith(fontWeight: FontWeight.w600)),
                      ?pill,
                    ],
                  ),
                  Text(
                    detail,
                    style: context.text.bodySmall?.copyWith(color: context.colors.onSurfaceVariant),
                    maxLines: 2,
                    overflow: TextOverflow.ellipsis,
                  ),
                  Text(
                    [
                      '${count(r.files)} files · ${bytes(r.bytes)}',
                      if (kind == _Extra.away && r.scannedAt != null) 'scanned ${day(r.scannedAt)}',
                    ].join(' · '),
                    style: context.text.bodySmall?.copyWith(color: context.bd.muted),
                  ),
                  if (s != null) ...[
                    const SizedBox(height: 6),
                    SpaceBar(total: s.total, free: s.free),
                    const SizedBox(height: 3),
                    Text('${bytes(s.free)} free of ${bytes(s.total)}', style: context.text.bodySmall),
                  ],
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }
}
