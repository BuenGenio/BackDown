import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../../api/keys.dart';
import '../../core/format.dart';
import '../../core/widgets.dart';
import '../../models/browse.dart';
import '../../providers.dart';
import '../../theme.dart';
import 'models.dart';
import 'providers.dart';
import 'widgets.dart';

/// A folder on another machine, read-only: asked of that machine live over ssh when it can be
/// (its own index's answer, with folder sizes), else as last synced here (folders without sizes),
/// with why it couldn't be asked. Without [dir], its drives.
class MachineFolderScreen extends ConsumerWidget {
  const MachineFolderScreen({super.key, required this.machineId, this.dir, this.select});

  final int machineId;
  final String? dir;

  /// A file's key there to show highlighted ("Show in folder").
  final String? select;

  MachineDir get _at => (m: machineId, dir: dir);

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final listing = ref.watch(machineLsProvider(_at));
    final machine = ref.watch(machinesProvider).value?.byId(machineId);
    final name = machine?.name ?? 'the machine';
    final l = listing.value;
    final title = dir == null ? 'Drives on $name' : (l == null ? '' : baseName(l.name.isEmpty ? l.path : l.name));
    return Scaffold(
      appBar: AppBar(
        title: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(title, maxLines: 1, overflow: TextOverflow.ellipsis),
            Text(
              [
                'on $name',
                if (l != null && l.files != null && l.live) plural(l.files!, 'file'),
                if (l != null && l.bytes != null && l.live) bytes(l.bytes),
              ].join(' · '),
              style: context.text.bodySmall?.copyWith(color: context.colors.onSurfaceVariant),
            ),
          ],
        ),
        actions: [
          IconButton(
            tooltip: 'Search here on $name',
            icon: const Icon(Icons.search),
            onPressed: () => context.push(MachinePaths.find(machineId, dir: dir)),
          ),
          if (l != null && l.path.isNotEmpty)
            IconButton(
              tooltip: 'Copy path',
              icon: const Icon(Icons.copy),
              onPressed: () async {
                await Clipboard.setData(ClipboardData(text: l.path));
                if (context.mounted) showSnack(context, 'Path copied');
              },
            ),
        ],
      ),
      body: AsyncBody(
        value: listing,
        onRetry: () => ref.invalidate(machineLsProvider(_at)),
        loading: _Asking(name: name),
        data: (l) => RefreshIndicator(
          onRefresh: () => ref.refresh(machineLsProvider(_at).future).then((_) {}, onError: (_) {}),
          child: _Listing(listing: l, machineId: machineId, name: name, dir: dir, select: select),
        ),
      ),
    );
  }
}

class _Asking extends StatelessWidget {
  const _Asking({required this.name});
  final String name;

  @override
  Widget build(BuildContext context) => Center(
    child: Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        const CircularProgressIndicator(),
        const SizedBox(height: 16),
        Text('Asking $name…', style: context.text.bodyMedium?.copyWith(color: context.bd.muted)),
      ],
    ),
  );
}

/// How the listing was got: live, or as last synced and why.
class LiveBanner extends ConsumerWidget {
  const LiveBanner({
    super.key,
    required this.live,
    required this.name,
    this.whyNot,
    this.scannedAt,
    this.syncedAt,
    this.what = 'as its last scan there saw it',
  });

  final bool live;
  final String name;
  final String? whyNot;
  final int? scannedAt, syncedAt;
  final String what;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final skew = ref.watch(skewProvider);
    if (live) {
      return NoteCard(
        icon: Icons.visibility_outlined,
        color: context.bd.good,
        text: 'Read-only · live from $name over ssh, $what${scannedAt == null ? '' : ' (${when(scannedAt)})'}.',
      );
    }
    return NoteCard(
      icon: Icons.cloud_off_outlined,
      color: context.bd.warn,
      title: 'As synced ${syncedAt == null ? 'here' : rel(syncedAt, skew: skew)}',
      text: [
        'Read-only, from what this BackDown last synced of $name (folders without sizes).',
        if (whyNot != null) '$name can\'t be asked now: ${whyText(whyNot!)}',
      ].join(' '),
    );
  }
}

/// Why a machine couldn't be asked, in words: the service's own reason, with what it means when
/// it's the machine-wide service (which has no ssh keys of its own).
String whyText(String why) {
  final w = firstLine(why, 300);
  if (w.contains('use your own BackDown')) {
    return 'this is the machine-wide BackDown service, which has no ssh keys of its own; the BackDown of the person '
        'whose keys reach it can ask ($w).';
  }
  return w.endsWith('.') ? w : '$w.';
}

class _Listing extends ConsumerWidget {
  const _Listing({required this.listing, required this.machineId, required this.name, this.dir, this.select});
  final MachineListing listing;
  final int machineId;
  final String name;
  final String? dir, select;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final l = listing;
    final skew = ref.watch(skewProvider);
    final header = <Widget>[
      LiveBanner(
        live: l.live,
        name: name,
        whyNot: l.whyNotLive,
        scannedAt: l.scannedAt,
        syncedAt: l.syncedAt,
        what: dir == null ? 'as its own index has them' : 'as its last scan there saw it',
      ),
      if (dir != null) _Crumbs(crumbs: l.crumbs, machineId: machineId),
      if (l.truncated)
        Padding(
          padding: const EdgeInsets.fromLTRB(16, 4, 16, 4),
          child: Text('The first 10,000 entries.', style: context.text.bodySmall?.copyWith(color: context.bd.muted)),
        ),
    ];
    // its drives
    final roots = l.roots;
    if (roots != null) {
      return ListView.builder(
        padding: wideInsets(context),
        itemCount: header.length + (roots.isEmpty ? 1 : roots.length),
        itemBuilder: (c, i) {
          if (i < header.length) return header[i];
          if (roots.isEmpty) {
            return const Padding(
              padding: EdgeInsets.only(top: 40),
              child: EmptyState(icon: Icons.storage_outlined, title: 'Nothing indexed there'),
            );
          }
          final r = roots[i - header.length];
          return ListTile(
            leading: Icon(Icons.storage, color: context.bd.folder, size: 30),
            title: Text(r.title, maxLines: 1, overflow: TextOverflow.ellipsis),
            subtitle: Text(
              [
                if (r.path != r.name) r.path,
                '${bytes(r.bytes)} · ${plural(r.files, 'file')}',
                'scanned there ${rel(r.scannedAt, skew: skew)}',
              ].join('\n'),
              maxLines: 3,
              overflow: TextOverflow.ellipsis,
            ),
            isThreeLine: true,
            trailing: const Icon(Icons.chevron_right),
            onTap: () => context.push(MachinePaths.ls(machineId, dir: r.key)),
          );
        },
      );
    }
    final entries = [...l.entries]
      ..sort((a, b) => a.isDir != b.isDir ? (a.isDir ? -1 : 1) : naturalCompare(a.name, b.name));
    if (entries.isEmpty) {
      return ListView(
        padding: wideInsets(context),
        children: [
          ...header,
          const Padding(
            padding: EdgeInsets.only(top: 40),
            child: EmptyState(icon: Icons.folder_open, title: 'Empty folder'),
          ),
        ],
      );
    }
    return ListView.builder(
      padding: wideInsets(context),
      itemCount: header.length + entries.length + 1,
      itemBuilder: (c, i) {
        if (i < header.length) return header[i];
        final j = i - header.length;
        if (j == entries.length) {
          return Padding(
            padding: const EdgeInsets.all(16),
            child: Text(
              'Files on $name are shown, never copied or changed. Tap one for where else its content is.',
              textAlign: TextAlign.center,
              style: context.text.bodySmall?.copyWith(color: context.bd.muted),
            ),
          );
        }
        final e = entries[j];
        return _EntryTile(entry: e, machineId: machineId, selected: e.key == select, live: l.live);
      },
    );
  }
}

class _EntryTile extends StatelessWidget {
  const _EntryTile({required this.entry, required this.machineId, required this.selected, required this.live});
  final MachineEntry entry;
  final int machineId;
  final bool selected, live;

  @override
  Widget build(BuildContext context) {
    final e = entry;
    final sub = e.isDir
        ? [if (e.files != null) plural(e.files!, 'file'), if (live || e.size > 0) bytes(e.size)].join(' · ')
        : '${bytes(e.size)} · ${day(e.mtime)}';
    return ListTile(
      selected: selected,
      selectedTileColor: context.bd.selection,
      leading: SizedBox(
        width: 40,
        child: Center(
          child: FileGlyph(name: e.name, isDir: e.isDir, size: 32),
        ),
      ),
      title: Text(e.name, maxLines: 1, overflow: TextOverflow.ellipsis),
      subtitle: Row(
        children: [
          Flexible(child: Text(sub.isEmpty ? 'folder' : sub, maxLines: 1, overflow: TextOverflow.ellipsis)),
          if (e.copies > 0) ...[const SizedBox(width: 6), Pill('×${e.copies + 1} there', color: context.bd.info)],
        ],
      ),
      trailing: e.isDir ? const Icon(Icons.chevron_right) : null,
      onTap: () {
        if (e.isDir) {
          context.push(MachinePaths.ls(machineId, dir: e.key));
        } else if (e.id != null) {
          context.push(MachinePaths.file(machineId, e.id!));
        }
      },
    );
  }
}

/// Where this folder is on that machine: its drive, then each folder down to here.
class _Crumbs extends StatelessWidget {
  const _Crumbs({required this.crumbs, required this.machineId});
  final List<Crumb> crumbs;
  final int machineId;

  @override
  Widget build(BuildContext context) {
    if (crumbs.length < 2) return const SizedBox.shrink();
    return SizedBox(
      height: 40,
      child: ListView.separated(
        scrollDirection: Axis.horizontal,
        reverse: true,
        padding: const EdgeInsets.symmetric(horizontal: 12),
        itemCount: crumbs.length,
        separatorBuilder: (c, i) => Icon(Icons.chevron_left, size: 16, color: context.bd.muted),
        itemBuilder: (c, i) {
          final crumb = crumbs[crumbs.length - 1 - i];
          final last = i == 0;
          return Center(
            child: InkWell(
              borderRadius: BorderRadius.circular(6),
              onTap: last ? null : () => context.go(MachinePaths.ls(machineId, dir: crumb.key)),
              child: Padding(
                padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 4),
                child: Text(
                  i == crumbs.length - 1 ? baseName(crumb.name) : crumb.name,
                  style: context.text.labelLarge?.copyWith(
                    color: last ? context.colors.onSurface : context.colors.primary,
                    fontWeight: last ? FontWeight.w700 : FontWeight.w500,
                  ),
                ),
              ),
            ),
          );
        },
      ),
    );
  }
}
