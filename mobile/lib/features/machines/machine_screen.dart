import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../../api/json.dart';
import '../../core/actions.dart';
import '../../core/format.dart';
import '../../core/widgets.dart';
import '../../providers.dart';
import '../../theme.dart';
import 'models.dart';
import 'providers.dart';
import 'widgets.dart';

/// One other machine: how it is (synced, or why it can't be reached), what it said about itself,
/// its drives as last synced, and how copies there count. Sync, test and settings go to the
/// service; from a phone they need someone at the computer.
class MachineScreen extends ConsumerWidget {
  const MachineScreen({super.key, required this.id});
  final int id;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    refreshOnSyncs(ref);
    final info = ref.watch(machinesProvider);
    final m = info.value?.byId(id);
    return Scaffold(
      appBar: AppBar(
        title: Text(m?.name ?? 'Machine'),
        actions: [
          if (m != null) ...[
            IconButton(
              tooltip: 'Search on ${m.name}',
              icon: const Icon(Icons.search),
              onPressed: () => context.push(MachinePaths.find(m.id)),
            ),
            PopupMenuButton<String>(
              onSelected: (v) {
                switch (v) {
                  case 'test':
                    _test(context, ref, m);
                  case 'settings':
                    _settings(context, ref, info.value!.settings);
                  case 'browse':
                    context.push(MachinePaths.ls(m.id));
                  case 'copy':
                    Clipboard.setData(ClipboardData(text: m.url));
                    showSnack(context, 'Address copied');
                  case 'remove':
                    showRemoveMachineHelp(context, m);
                }
              },
              itemBuilder: (c) => const [
                PopupMenuItem(value: 'test', child: Text('Test connection')),
                PopupMenuItem(value: 'browse', child: Text('Its drives, live')),
                PopupMenuItem(value: 'settings', child: Text('How copies count…')),
                PopupMenuItem(value: 'copy', child: Text('Copy address')),
                PopupMenuItem(value: 'remove', child: Text('Forget this machine…')),
              ],
            ),
          ],
        ],
      ),
      body: AsyncBody(
        value: info,
        onRetry: () => ref.invalidate(machinesProvider),
        data: (d) {
          final m = d.byId(id);
          if (m == null) {
            return const EmptyState(
              icon: Icons.dns_outlined,
              title: 'No such machine',
              message: 'It may have been removed at the computer.',
            );
          }
          return RefreshIndicator(
            onRefresh: () => ref.refresh(machinesProvider.future).then((_) {}, onError: (_) {}),
            child: _Body(info: d, machine: m),
          );
        },
      ),
    );
  }
}

Future<void> _sync(BuildContext context, WidgetRef ref, Machine m) async {
  final r = await runAction(
    context,
    ref,
    (api) => api.post('machine-sync', params: {'m': m.id}),
    done: 'Syncing: see Activity for progress',
  );
  if (r != null) ref.invalidate(machinesProvider);
}

Future<void> _test(BuildContext context, WidgetRef ref, Machine m) async {
  showSnack(context, 'Connecting to ${m.name}…');
  final r = await runAction(context, ref, (api) => api.post('machine-test', params: {'m': m.id}));
  if (r == null) return;
  final j = asJson(r);
  ref.invalidate(machinesProvider);
  if (context.mounted) {
    showSnack(
      context,
      j.flag('reached') ? j.str('test', 'Reached ${m.name}') : 'Can\'t be reached: see why on the page',
    );
  }
}

Future<void> _settings(BuildContext context, WidgetRef ref, MachineSettings s) async {
  final saved = await showDialog<(String, String)>(
    context: context,
    builder: (c) => _SettingsDialog(settings: s),
  );
  if (saved == null || !context.mounted) return;
  final r = await runAction(
    context,
    ref,
    (api) => api.post('machine-settings', params: {'proof_days': saved.$1, 'sync_hours': saved.$2}),
    done: 'Saved',
  );
  if (r != null) ref.invalidate(machinesProvider);
}

class _Body extends ConsumerWidget {
  const _Body({required this.info, required this.machine});
  final MachinesInfo info;
  final Machine machine;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final m = machine;
    final s = info.settings;
    final skew = ref.watch(skewProvider);
    final why = info.whyNot;
    final canSync = why == null && !m.busy && info.daemon;
    final skewNote = m.info.skew.abs() > 120
        ? ' (its clock is ${(m.info.skew.abs() / 60).round()} min ${m.info.skew > 0 ? 'ahead' : 'behind'}; allowed for)'
        : '';
    return ListView(
      padding: wideInsets(context),
      children: [
        const LookOnlyNote(what: 'Syncing, testing and settings'),
        if (why != null)
          NoteCard(
            icon: Icons.key_off_outlined,
            color: context.bd.warn,
            title: 'This BackDown can\'t reach ${m.name} itself',
            text: '${capitalised(why)}.',
          ),
        _StatusCard(machine: m, skew: skew),
        Padding(
          padding: const EdgeInsets.fromLTRB(16, 4, 16, 0),
          child: Wrap(
            spacing: 8,
            runSpacing: 8,
            children: [
              FilledButton.icon(
                onPressed: canSync ? () => _sync(context, ref, m) : null,
                icon: const Icon(Icons.sync),
                label: Text(m.busy ? 'Syncing…' : 'Sync now'),
              ),
              OutlinedButton.icon(
                onPressed: why == null ? () => _test(context, ref, m) : null,
                icon: const Icon(Icons.network_check),
                label: const Text('Test connection'),
              ),
            ],
          ),
        ),
        if (!info.daemon)
          Padding(
            padding: const EdgeInsets.fromLTRB(16, 8, 16, 0),
            child: Text(
              'Syncing runs in the BackDown service: this is `backdown serve`, so sync with '
              '`backdown remote sync` in a terminal there.',
              style: context.text.bodySmall?.copyWith(color: context.bd.muted),
            ),
          ),
        const SectionHeader('About it'),
        InfoRow('Address', m.url, copyable: true),
        InfoRow('Host', m.info.host ?? '—'),
        InfoRow('BackDown there', m.info.version ?? '—'),
        InfoRow('Its index', m.info.db ?? m.db ?? '—', mono: (m.info.db ?? m.db) != null, copyable: true),
        if (m.info.os != null) InfoRow('System', m.info.os == 'windows' ? 'Windows' : 'Linux, macOS or another Unix'),
        InfoRow('Last answered', m.info.seen == null ? '—' : '${when(m.info.seen)}$skewNote'),
        SectionHeader('Drives there', trailing: Text('${m.roots.length}', style: context.text.labelMedium)),
        if (m.roots.isEmpty)
          Padding(
            padding: const EdgeInsets.fromLTRB(16, 4, 16, 8),
            child: Text(
              m.lastSync != null
                  ? 'Nothing is indexed there yet: run `backdown index` on that machine.'
                  : 'Sync it to see its drives.',
              style: context.text.bodyMedium?.copyWith(color: context.bd.muted),
            ),
          ),
        for (final r in m.roots) _RootTile(machine: m, root: r, settings: s, skew: skew),
        const SectionHeader('How copies there count'),
        Padding(
          padding: const EdgeInsets.fromLTRB(16, 0, 16, 0),
          child: Text(
            'A file here counts as kept when an identical copy is on ${m.name} (by SHA-256, never by name or size) '
            'and ${m.name} confirmed it within the last ${plural(s.proofDays, 'day')}: its own scan saw the file, or '
            'it read it again for BackDown. Before Free up deletes anything whose only copy is there, ${m.name} reads '
            'that copy again and it must still match; if ${m.name} can\'t be reached then, the file stays.',
            style: context.text.bodyMedium,
          ),
        ),
        const SizedBox(height: 4),
        ListTile(
          leading: const Icon(Icons.verified_outlined),
          title: Text('Copies count for ${plural(s.proofDays, 'day')}'),
          subtitle: Text(
            'From when they were last confirmed there. Machines are synced every ${plural(s.syncHours, 'hour')}.',
          ),
          trailing: TextButton(onPressed: () => _settings(context, ref, s), child: const Text('Change…')),
        ),
        Padding(
          padding: const EdgeInsets.fromLTRB(16, 4, 16, 0),
          child: Text(
            'Nothing on ${m.name} is ever changed: `backdown serve-stdio` there only reads its index, and hashes a '
            'file only when asked.',
            style: context.text.bodySmall?.copyWith(color: context.bd.muted),
          ),
        ),
        const SectionHeader('Adding and forgetting'),
        ListTile(
          leading: const Icon(Icons.link_off),
          title: Text('Forget ${m.name}'),
          subtitle: const Text('Done at the computer'),
          trailing: const Icon(Icons.chevron_right),
          onTap: () => showRemoveMachineHelp(context, m),
        ),
      ],
    );
  }
}

class _StatusCard extends StatelessWidget {
  const _StatusCard({required this.machine, required this.skew});
  final Machine machine;
  final int skew;

  @override
  Widget build(BuildContext context) {
    final m = machine;
    final (_, color, icon) = machineStatus(context, m, skew: skew);
    final String title;
    final List<Widget> more;
    if (m.busy) {
      title = m.job!.state == 'running' ? 'Syncing…' : 'Sync waiting its turn';
      more = [
        if (m.job!.progress.isNotEmpty) Text(m.job!.progress, style: context.text.bodySmall),
        const SizedBox(height: 8),
        Meter(fraction: m.job!.state == 'running' ? m.job!.fraction : 0, color: context.bd.info),
      ];
    } else if (m.unreachable) {
      title = 'Can\'t be reached';
      more = [
        Text(
          'Tried ${rel(m.lastTry, skew: skew)}${m.lastSync == null ? '; never synced' : '; last synced ${rel(m.lastSync, skew: skew)}'}.',
          style: context.text.bodySmall,
        ),
        const SizedBox(height: 8),
        Container(
          width: double.infinity,
          padding: const EdgeInsets.all(10),
          decoration: BoxDecoration(
            color: context.bd.crit.withValues(alpha: 0.08),
            borderRadius: BorderRadius.circular(8),
          ),
          child: SelectableText(
            m.lastError!,
            style: context.text.bodySmall?.copyWith(fontFamily: 'monospace', color: context.colors.onSurface),
          ),
        ),
      ];
    } else if (m.lastSync != null) {
      title = 'Synced ${rel(m.lastSync, skew: skew)}';
      more = [Text(when(m.lastSync), style: context.text.bodySmall)];
    } else {
      title = 'Not synced yet';
      more = [Text('Its drives show once it has been synced.', style: context.text.bodySmall)];
    }
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 8, 16, 8),
      child: Card(
        child: Padding(
          padding: const EdgeInsets.all(14),
          child: Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Icon(icon, color: color),
              const SizedBox(width: 12),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(title, style: context.text.titleSmall?.copyWith(color: color)),
                    const SizedBox(height: 2),
                    ...more,
                  ],
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _RootTile extends StatelessWidget {
  const _RootTile({required this.machine, required this.root, required this.settings, required this.skew});
  final Machine machine;
  final MachineRoot root;
  final MachineSettings settings;
  final int skew;

  @override
  Widget build(BuildContext context) {
    final r = root;
    return ListTile(
      leading: Icon(Icons.storage, color: context.bd.folder, size: 30),
      title: Text(r.title, maxLines: 1, overflow: TextOverflow.ellipsis),
      subtitle: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          if (r.path != r.name) Text(r.path, maxLines: 1, overflow: TextOverflow.ellipsis),
          Text('${bytes(r.bytes)} · ${plural(r.files, 'file')}', maxLines: 1, overflow: TextOverflow.ellipsis),
          Text('scanned there ${rel(r.scannedAt, skew: skew)}', maxLines: 1, overflow: TextOverflow.ellipsis),
          const SizedBox(height: 4),
          Wrap(
            spacing: 6,
            runSpacing: 4,
            crossAxisAlignment: WrapCrossAlignment.center,
            children: [
              r.counts
                  ? Pill('counts as proof', color: context.bd.good, icon: Icons.verified_outlined)
                  : Pill('too long ago to count', color: context.bd.warn, icon: Icons.history),
              Text(
                '${count(r.hashed)} of ${count(r.cached)} hashed',
                style: context.text.labelSmall?.copyWith(color: context.bd.muted),
              ),
            ],
          ),
        ],
      ),
      isThreeLine: true,
      trailing: const Icon(Icons.chevron_right),
      onTap: () => context.push(MachinePaths.ls(machine.id, dir: r.key)),
    );
  }
}

class _SettingsDialog extends ConsumerStatefulWidget {
  const _SettingsDialog({required this.settings});
  final MachineSettings settings;

  @override
  ConsumerState<_SettingsDialog> createState() => _SettingsDialogState();
}

class _SettingsDialogState extends ConsumerState<_SettingsDialog> {
  late final _days = TextEditingController(text: '${widget.settings.proofDays}');
  late final _hours = TextEditingController(text: '${widget.settings.syncHours}');
  final _form = GlobalKey<FormState>();

  @override
  void dispose() {
    _days.dispose();
    _hours.dispose();
    super.dispose();
  }

  String? _range(String? v, int lo, int hi) {
    final n = int.tryParse(v?.trim() ?? '');
    return n == null || n < lo || n > hi ? '$lo to $hi' : null;
  }

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      title: const Text('How copies count'),
      content: Form(
        key: _form,
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            if (!canChange(ref))
              Padding(
                padding: const EdgeInsets.only(bottom: 12),
                child: Text(
                  'These are changed at the computer BackDown runs on.',
                  style: context.text.bodySmall?.copyWith(color: context.bd.muted),
                ),
              ),
            TextFormField(
              controller: _days,
              keyboardType: TextInputType.number,
              decoration: const InputDecoration(labelText: 'Count copies confirmed within (days)'),
              validator: (v) => _range(v, 1, 90),
            ),
            const SizedBox(height: 12),
            TextFormField(
              controller: _hours,
              keyboardType: TextInputType.number,
              decoration: const InputDecoration(labelText: 'Sync every (hours)'),
              validator: (v) => _range(v, 1, 168),
            ),
          ],
        ),
      ),
      actions: [
        TextButton(onPressed: () => Navigator.pop(context), child: const Text('Cancel')),
        FilledButton(
          onPressed: () {
            if (_form.currentState?.validate() ?? false) {
              Navigator.pop(context, (_days.text.trim(), _hours.text.trim()));
            }
          },
          child: const Text('Save'),
        ),
      ],
    );
  }
}
