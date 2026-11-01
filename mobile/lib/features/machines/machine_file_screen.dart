import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../../api/keys.dart';
import '../../core/format.dart';
import '../../core/kinds.dart';
import '../../core/widgets.dart';
import '../../providers.dart';
import '../../router.dart';
import '../../theme.dart';
import 'machine_folder_screen.dart';
import 'models.dart';
import 'providers.dart';
import 'widgets.dart';

/// A file on another machine: what that machine's index says of it (live, or as last synced), and
/// where else its content is: here, elsewhere on that machine, on other machines. Its bytes stay
/// there: BackDown doesn't bring files over from other machines.
class MachineFileScreen extends ConsumerWidget {
  const MachineFileScreen({super.key, required this.machineId, required this.rid});
  final int machineId, rid;

  MachineFileAt get _at => (m: machineId, id: rid);

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final f = ref.watch(machineFileProvider(_at));
    final name = ref.watch(machinesProvider).value?.byId(machineId)?.name ?? 'that machine';
    return Scaffold(
      appBar: AppBar(title: Text(f.value?.name ?? 'File')),
      body: AsyncBody(
        value: f,
        onRetry: () => ref.invalidate(machineFileProvider(_at)),
        data: (f) => _Body(file: f, machineId: machineId, name: name),
      ),
    );
  }
}

class _Body extends ConsumerWidget {
  const _Body({required this.file, required this.machineId, required this.name});
  final MachineFile file;
  final int machineId;
  final String name;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final f = file;
    final skew = ref.watch(skewProvider);
    return ListView(
      padding: wideInsets(context),
      children: [
        Padding(
          padding: const EdgeInsets.all(16),
          child: Row(
            children: [
              Container(
                width: 72,
                height: 72,
                decoration: BoxDecoration(
                  color: context.colors.surfaceContainerHigh,
                  borderRadius: BorderRadius.circular(12),
                ),
                child: Center(child: FileGlyph(name: f.name, size: 40)),
              ),
              const SizedBox(width: 16),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    SelectableText(f.name, style: context.text.titleMedium),
                    const SizedBox(height: 4),
                    Text(
                      '${bytes(f.size)} · on $name${f.live ? '' : ' (as last synced)'}',
                      style: context.text.bodyMedium?.copyWith(color: context.colors.onSurfaceVariant),
                    ),
                  ],
                ),
              ),
            ],
          ),
        ),
        if (!f.live && f.whyNotLive != null)
          NoteCard(
            icon: Icons.cloud_off_outlined,
            color: context.bd.warn,
            text: '$name can\'t be asked now: ${whyText(f.whyNotLive!)} This is what was last synced of it.',
          ),
        Padding(
          padding: const EdgeInsets.fromLTRB(16, 8, 16, 0),
          child: Wrap(
            spacing: 8,
            runSpacing: 8,
            children: [
              OutlinedButton.icon(
                onPressed: () => context.push(MachinePaths.ls(machineId, dir: f.dirKey, select: f.key)),
                icon: const Icon(Icons.folder_open_outlined),
                label: const Text('Show in folder'),
              ),
              if (f.local.isNotEmpty)
                FilledButton.icon(
                  onPressed: () => context.push(Routes.file(f.local.first.id)),
                  icon: const Icon(Icons.computer_outlined),
                  label: const Text('Open the copy here'),
                ),
            ],
          ),
        ),
        NoteCard(
          icon: Icons.do_not_disturb_on_outlined,
          color: context.bd.muted,
          margin: const EdgeInsets.fromLTRB(16, 12, 16, 0),
          text: f.local.isEmpty
              ? 'No bytes come from $name: to open this file, open it on $name. BackDown doesn\'t bring files over '
                    'from other machines.'
              : 'No bytes come from $name: BackDown doesn\'t bring files over from other machines. The identical copy '
                    'on this computer opens here.',
        ),
        const SectionHeader('Details'),
        InfoRow('Path there', f.path, copyable: true),
        InfoRow('Size', '${bytes(f.size)} (${count(f.size)} bytes)'),
        InfoRow('Modified', when(f.mtime)),
        InfoRow('Kind', FileKind.of(f.name).label),
        InfoRow(
          'SHA-256',
          f.sha256 ?? 'not hashed there (it is when a file here has its size)',
          mono: f.sha256 != null,
          copyable: f.sha256 != null,
          valueColor: f.sha256 == null ? context.bd.muted : null,
        ),
        if (f.hashedAt != null) InfoRow('Read there', '${when(f.hashedAt)} (hashed for BackDown here)'),
        if (f.error != null) InfoRow('Problem', f.error!, valueColor: context.bd.crit),
        SectionHeader('On this computer', trailing: Pill('${f.local.length}', color: context.bd.info)),
        if (f.local.isEmpty)
          _Muted(f.sha256 == null ? 'Unknown until it\'s hashed there.' : 'No identical file here.')
        else
          for (final c in f.local)
            ListTile(
              dense: true,
              leading: Icon(Icons.computer_outlined, color: context.bd.good),
              title: Text(baseName(c.path), maxLines: 1, overflow: TextOverflow.ellipsis),
              subtitle: Text(c.path, maxLines: 2, overflow: TextOverflow.ellipsis),
              trailing: const Icon(Icons.chevron_right),
              onTap: () => context.push(Routes.file(c.id)),
            ),
        if (f.there.isNotEmpty) ...[
          SectionHeader('Other copies on $name', trailing: Pill('${f.there.length}', color: context.bd.info)),
          for (final c in f.there)
            ListTile(
              dense: true,
              leading: Icon(Icons.content_copy_outlined, color: context.bd.info),
              title: Text(baseName(c.path), maxLines: 1, overflow: TextOverflow.ellipsis),
              subtitle: Text(c.path, maxLines: 2, overflow: TextOverflow.ellipsis),
              trailing: const Icon(Icons.chevron_right),
              onTap: () => context.push(MachinePaths.file(machineId, c.id)),
            ),
        ],
        if (f.others.isNotEmpty) ...[
          SectionHeader('On other machines', trailing: Pill('${f.others.length}', color: context.bd.info)),
          for (final c in f.others)
            ListTile(
              dense: true,
              leading: Icon(
                c.counts ? Icons.verified_outlined : Icons.dns_outlined,
                color: c.counts ? context.bd.good : context.bd.muted,
              ),
              title: Text('On ${c.machine}'),
              subtitle: Text(
                [
                  c.path,
                  if (c.counts)
                    'confirmed ${rel(c.confirmedAt, skew: skew)} · counts as a verified copy'
                  else
                    c.whyNot ?? 'doesn\'t count',
                ].join('\n'),
              ),
              isThreeLine: true,
              trailing: const Icon(Icons.chevron_right),
              onTap: () => context.push(MachinePaths.file(c.machineId, c.rid)),
            ),
        ],
        if (f.versions.length > 1) ...[
          SectionHeader('Versions there', trailing: Text('${f.versions.length}', style: context.text.labelMedium)),
          for (final v in f.versions.reversed)
            ListTile(
              dense: true,
              leading: Icon(
                v.current ? Icons.radio_button_checked : Icons.history,
                color: v.current ? context.colors.primary : context.bd.muted,
              ),
              title: Text('${bytes(v.size)} · seen from ${day(v.from)}'),
              subtitle: v.sha256 == null ? null : Text(shortHash(v.sha256)),
            ),
        ],
      ],
    );
  }
}

class _Muted extends StatelessWidget {
  const _Muted(this.text);
  final String text;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.fromLTRB(16, 2, 16, 6),
    child: Text(text, style: context.text.bodyMedium?.copyWith(color: context.bd.muted)),
  );
}
