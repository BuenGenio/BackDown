import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../../core/format.dart';
import '../../core/widgets.dart';
import '../../providers.dart';
import '../../router.dart';
import '../../theme.dart';
import 'models.dart';
import 'providers.dart';
import 'widgets.dart';

/// The other machines this service syncs: each with how it is, what it said about itself, and its
/// drives as last synced. Adding one is done at the computer.
class MachinesScreen extends ConsumerWidget {
  const MachinesScreen({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    refreshOnSyncs(ref);
    final info = ref.watch(machinesProvider);
    return Scaffold(
      appBar: AppBar(
        title: const Text('Other machines'),
        actions: [
          IconButton(
            tooltip: 'Swarm',
            icon: const Icon(Icons.hub_outlined),
            onPressed: () => context.push(Routes.swarm),
          ),
          IconButton(
            tooltip: 'Add a machine',
            icon: const Icon(Icons.add),
            onPressed: () => showAddMachineHelp(context),
          ),
        ],
      ),
      body: RefreshIndicator(
        onRefresh: () => ref.refresh(machinesProvider.future).then((_) {}, onError: (_) {}),
        child: AsyncBody(
          value: info,
          onRetry: () => ref.invalidate(machinesProvider),
          data: (d) {
            final notes = <Widget>[
              if (!d.allowed)
                const NoteCard(
                  icon: Icons.workspace_premium_outlined,
                  title: 'Not in this licence',
                  text: 'Reaching other machines isn\'t included in this BackDown\'s licence (backdown.io/pricing).',
                ),
              if (d.whyNot != null)
                NoteCard(
                  icon: Icons.key_off_outlined,
                  color: context.bd.warn,
                  title: 'This BackDown can\'t reach them itself',
                  text: '${capitalised(d.whyNot!)}. They show as last synced.',
                ),
            ];
            if (d.machines.isEmpty) {
              return ListView(
                padding: wideInsets(context),
                children: [
                  ...notes,
                  const SizedBox(height: 40),
                  EmptyState(
                    icon: Icons.dns_outlined,
                    title: 'No other machines yet',
                    message:
                        'Add a NAS, a server or another computer with BackDown on it, at the computer: its drives '
                        'then show here, copies there count as kept, and the swarm sees its free space.',
                    action: OutlinedButton.icon(
                      onPressed: () => showAddMachineHelp(context),
                      icon: const Icon(Icons.add),
                      label: const Text('How to add one'),
                    ),
                  ),
                ],
              );
            }
            return ListView.builder(
              padding: wideInsets(context, top: 4),
              itemCount: notes.length + d.machines.length + 1,
              itemBuilder: (c, i) {
                if (i < notes.length) return notes[i];
                final j = i - notes.length;
                if (j == d.machines.length) return const _Footer();
                return _MachineCard(machine: d.machines[j]);
              },
            );
          },
        ),
      ),
    );
  }
}

class _MachineCard extends ConsumerWidget {
  const _MachineCard({required this.machine});
  final Machine machine;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final m = machine;
    final skew = ref.watch(skewProvider);
    final (status, color, icon) = machineStatus(context, m, skew: skew);
    final about = [
      if (m.info.version != null) 'BackDown ${m.info.version}',
      if (m.info.host != null && m.info.host != m.name) m.info.host!,
    ].join(' · ');
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 8, 16, 4),
      child: Card(
        clipBehavior: Clip.antiAlias,
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            InkWell(
              onTap: () => context.push(Routes.machine(m.id)),
              child: Padding(
                padding: const EdgeInsets.fromLTRB(14, 12, 8, 10),
                child: Row(
                  children: [
                    CircleAvatar(
                      radius: 20,
                      backgroundColor: color.withValues(alpha: 0.14),
                      child: Icon(Icons.dns_outlined, color: color, size: 22),
                    ),
                    const SizedBox(width: 12),
                    Expanded(
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Text(m.name, style: context.text.titleMedium, maxLines: 1, overflow: TextOverflow.ellipsis),
                          const SizedBox(height: 2),
                          Row(
                            children: [
                              Icon(icon, size: 14, color: color),
                              const SizedBox(width: 4),
                              Flexible(
                                child: Text(
                                  status,
                                  style: context.text.bodySmall?.copyWith(color: color, fontWeight: FontWeight.w600),
                                  maxLines: 1,
                                  overflow: TextOverflow.ellipsis,
                                ),
                              ),
                            ],
                          ),
                          Text(
                            [m.url, if (about.isNotEmpty) about].join(' · '),
                            style: context.text.bodySmall?.copyWith(color: context.colors.onSurfaceVariant),
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                          ),
                        ],
                      ),
                    ),
                    Icon(Icons.chevron_right, color: context.bd.muted),
                  ],
                ),
              ),
            ),
            if (m.busy && m.job!.progress.isNotEmpty)
              Padding(
                padding: const EdgeInsets.fromLTRB(14, 0, 14, 10),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Meter(fraction: m.job!.state == 'running' ? m.job!.fraction : 0, color: context.bd.info, height: 4),
                    const SizedBox(height: 4),
                    Text(m.job!.progress, style: context.text.bodySmall?.copyWith(color: context.bd.muted)),
                  ],
                ),
              ),
            if (m.roots.isNotEmpty) const Divider(height: 1),
            for (final r in m.roots)
              ListTile(
                dense: true,
                visualDensity: VisualDensity.compact,
                leading: Icon(Icons.storage, color: context.bd.folder),
                title: Text(r.title, maxLines: 1, overflow: TextOverflow.ellipsis),
                subtitle: Text(
                  '${bytes(r.bytes)} · ${plural(r.files, 'file')}',
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                ),
                trailing: const Icon(Icons.chevron_right, size: 20),
                onTap: () => context.push(MachinePaths.ls(m.id, dir: r.key)),
              ),
            if (m.roots.isEmpty)
              Padding(
                padding: const EdgeInsets.fromLTRB(14, 0, 14, 12),
                child: Text(
                  m.lastSync == null ? 'Sync it to see its drives.' : 'Nothing is indexed there yet.',
                  style: context.text.bodySmall?.copyWith(color: context.bd.muted),
                ),
              ),
          ],
        ),
      ),
    );
  }
}

class _Footer extends StatelessWidget {
  const _Footer();

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 16, 16, 0),
      child: Column(
        children: [
          Text(
            'Each machine keeps its own index. This BackDown reads theirs over ssh, with your keys, and never '
            'changes anything there.',
            textAlign: TextAlign.center,
            style: context.text.bodySmall?.copyWith(color: context.bd.muted),
          ),
          const SizedBox(height: 8),
          TextButton.icon(
            onPressed: () => showAddMachineHelp(context),
            icon: const Icon(Icons.add),
            label: const Text('Add a machine'),
          ),
        ],
      ),
    );
  }
}
