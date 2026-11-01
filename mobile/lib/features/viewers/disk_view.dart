import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../../core/format.dart';
import '../../core/widgets.dart';
import '../../providers.dart';
import '../../theme.dart';
import 'common.dart';
import 'models.dart';
import 'routes.dart';

final _cantMount = RegExp('LUKS|BitLocker|encrypt|swap|LVM|RAID|member|ZFS|Storage Spaces', caseSensitive: false);

/// A disk image: what its headers and tables say (partitions and their filesystems, a virtual
/// disk's size), with the files of each filesystem BackDown reads itself one tap away.
class DiskView extends StatelessWidget {
  const DiskView({super.key, required this.id, required this.name, required this.disk});
  final int id;
  final String name;
  final DiskInfo disk;

  @override
  Widget build(BuildContext context) {
    final d = disk;
    final whole = d.fs?.browsable ?? false;
    final layoutless = d.table == null && d.fs == null && !const {'raw', 'iso9660', 'dmg'}.contains(d.format);
    final other = [
      d.fs,
      for (final p in d.partitions) p.fs,
    ].whereType<FsKind>().where((f) => !f.browsable && !_cantMount.hasMatch(f.kind)).map((f) => f.kind).toSet();
    void browse(String part, String root) => context.push(fsLocation(id, part, root));
    return Column(
      children: [
        ViewerBar(meta: [d.title, if (d.virtualSize != null) bytes(d.virtualSize)].join(' · ')),
        Expanded(
          child: Readable(
            child: ListView(
              padding: const EdgeInsets.only(bottom: 32),
              children: [
                Padding(
                  padding: const EdgeInsets.fromLTRB(12, 12, 12, 0),
                  child: Card(
                    child: Padding(
                      padding: const EdgeInsets.symmetric(vertical: 12),
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Padding(
                            padding: const EdgeInsets.fromLTRB(16, 0, 16, 6),
                            child: Text([d.title, ?d.label].join(' · '), style: context.text.titleMedium),
                          ),
                          if (d.virtualSize != null)
                            InfoRow(
                              d.format == 'raw' ? 'Size' : 'Virtual size',
                              '${bytes(d.virtualSize)} (${count(d.virtualSize)} bytes)',
                            ),
                          if (d.format != 'raw' && !d.iso) InfoRow('File size', bytes(d.fileSize)),
                          if (d.table != null) InfoRow('Partition table', d.table!),
                          if (d.fs != null)
                            InfoRow('Filesystem', [d.fs!.text, if (d.fs!.size != null) bytes(d.fs!.size)].join(' · ')),
                          for (final (k, v) in d.details) InfoRow(k, v),
                          if (d.iso || whole)
                            Padding(
                              padding: const EdgeInsets.fromLTRB(16, 10, 16, 0),
                              child: FilledButton.icon(
                                onPressed: d.iso
                                    ? () => browse('iso', d.label ?? 'Disc')
                                    : () => browse('0', d.fs!.label ?? d.fs!.kind),
                                icon: const Icon(Icons.folder_open),
                                label: Text(d.iso ? 'Browse the disc' : 'Browse files'),
                              ),
                            ),
                        ],
                      ),
                    ),
                  ),
                ),
                if (d.partitions.isNotEmpty) ...[
                  SectionHeader(
                    'Partitions',
                    trailing: Text('${d.partitions.length}', style: context.text.labelMedium),
                  ),
                  for (final p in d.partitions)
                    _PartitionTile(p: p, onBrowse: () => browse('${p.index}', p.fs!.label ?? p.fs!.kind)),
                ],
                if (d.dmgParts.isNotEmpty) ...[
                  const SectionHeader('Partitions'),
                  for (final (n, size) in d.dmgParts)
                    ListTile(dense: true, title: Text(n), trailing: Text(bytes(size))),
                ],
                if (layoutless)
                  const ViewerNote(
                    'To browse the files inside, convert it to a raw image first (for example with '
                    'qemu-img convert -O raw).',
                  ),
                if (other.isNotEmpty)
                  ViewerNote(
                    'BackDown reads FAT, exFAT, ext and NTFS itself; this image has ${other.join(', ')}. On the computer, '
                    'BackDown can mount it read-only, so it can be indexed and browsed.',
                    icon: Icons.computer_outlined,
                  ),
              ],
            ),
          ),
        ),
      ],
    );
  }
}

class _PartitionTile extends StatelessWidget {
  const _PartitionTile({required this.p, required this.onBrowse});
  final Partition p;
  final VoidCallback onBrowse;

  @override
  Widget build(BuildContext context) {
    final can = p.fs?.browsable ?? false;
    return ListTile(
      leading: CircleAvatar(
        radius: 16,
        backgroundColor: context.colors.surfaceContainerHigh,
        child: Text('${p.index}', style: context.text.labelLarge),
      ),
      title: Row(
        children: [
          Flexible(child: Text(p.type, maxLines: 1, overflow: TextOverflow.ellipsis)),
          if (p.bootable) ...[const SizedBox(width: 6), Pill('boot', color: context.bd.info)],
        ],
      ),
      subtitle: Text(
        [?p.name, p.fs?.text ?? 'filesystem not recognised', bytes(p.size)].join(' · '),
        maxLines: 2,
        overflow: TextOverflow.ellipsis,
      ),
      trailing: can ? TextButton(onPressed: onBrowse, child: const Text('Browse')) : null,
      onTap: can ? onBrowse : null,
    );
  }
}

/// A folder of a filesystem in a disk image, at [path]; `part: 'iso'` lists a disc's own files.
typedef FsAt = ({int id, String part, String path, bool deleted});

final fsListingProvider = FutureProvider.autoDispose.family<FsListing, FsAt>((ref, at) {
  final api = requireApi(ref);
  return at.part == 'iso'
      ? fetchParsed(api, 'disk', {'id': at.id, 'path': at.path}, FsListing.fromJson)
      : fetchParsed(api, 'fs', {
          'id': at.id,
          'part': at.part,
          'path': at.path,
          'deleted': at.deleted ? 1 : null,
        }, FsListing.fromJson);
});

/// The files of a filesystem inside a disk image, folder by folder, deleted ones too when asked
/// (with how likely each comes back); a file opens in the viewer that suits it.
class FsBrowseScreen extends ConsumerStatefulWidget {
  const FsBrowseScreen({super.key, required this.id, required this.part, required this.rootName});
  final int id;
  final String part;
  final String rootName;

  @override
  ConsumerState<FsBrowseScreen> createState() => _FsBrowseScreenState();
}

class _FsBrowseScreenState extends ConsumerState<FsBrowseScreen> {
  final _path = <String>[];
  final _shown = <String>[];
  bool _deleted = false;
  InnerSort _sort = InnerSort.name;
  bool _descending = false;

  bool get _iso => widget.part == 'iso';
  FsAt get _at => (id: widget.id, part: widget.part, path: _path.join('/'), deleted: _deleted && !_iso);

  void _pick(InnerSort s) => setState(() {
    if (s == _sort) {
      _descending = !_descending;
    } else {
      _sort = s;
      _descending = false;
    }
  });

  List<FsEntry> _sorted(List<FsEntry> all) {
    final flip = _sort == InnerSort.name ? _descending : !_descending;
    int by(FsEntry a, FsEntry b) {
      final c = switch (_sort) {
        InnerSort.name => 0,
        InnerSort.size => a.size.compareTo(b.size),
        InnerSort.date => (a.mtime ?? 0).compareTo(b.mtime ?? 0),
      };
      final r = c != 0 ? c : naturalCompare(a.name, b.name);
      return flip ? -r : r;
    }

    return [...all.where((e) => e.dir).toList()..sort(by), ...all.where((e) => !e.dir).toList()..sort(by)];
  }

  void _open(FsEntry e) {
    if (e.dir) {
      setState(() {
        _path.add(e.seg);
        _shown.add(e.name);
      });
      return;
    }
    final why = e.unreadable ?? (e.link != null ? 'A link to ${e.link}. Links are shown, not followed.' : null);
    if (why != null) {
      showDialog<void>(
        context: context,
        builder: (c) => AlertDialog(
          title: Text(e.name),
          content: Text(why),
          actions: [TextButton(onPressed: () => Navigator.pop(c), child: const Text('OK'))],
        ),
      );
      return;
    }
    final del = e.deleted;
    context.push(
      innerLocation(
        InnerFile.entry(
          id: widget.id,
          name: [..._path, e.seg].join('/'),
          part: _iso ? null : widget.part,
          size: e.size,
          streams: !_iso,
        ),
      ),
      extra: del == null
          ? null
          : 'Deleted: ${del.why}${del.state == 'recoverable' ? '' : ' Parts other files have taken since show as empty.'}',
    );
  }

  @override
  Widget build(BuildContext context) {
    final listing = ref.watch(fsListingProvider(_at));
    final l = listing.value;
    return Scaffold(
      appBar: AppBar(
        title: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(widget.rootName, maxLines: 1, overflow: TextOverflow.ellipsis),
            if (l?.fs != null)
              Text(
                [l!.fs!.kind, if (l.fs!.size != null) bytes(l.fs!.size)].join(' · '),
                style: context.text.bodySmall?.copyWith(color: context.colors.onSurfaceVariant),
              ),
          ],
        ),
        actions: [
          InnerSortButton(sort: _sort, descending: _descending, onPick: _pick),
          if (!_iso)
            PopupMenuButton<String>(
              onSelected: (v) => setState(() => _deleted = !_deleted),
              itemBuilder: (c) => [
                CheckedPopupMenuItem(value: 'deleted', checked: _deleted, child: const Text('Show deleted files')),
              ],
            ),
        ],
      ),
      body: PopScope(
        canPop: _path.isEmpty,
        onPopInvokedWithResult: (didPop, _) {
          if (!didPop && _path.isNotEmpty) {
            setState(() {
              _path.removeLast();
              _shown.removeLast();
            });
          }
        },
        child: Column(
          children: [
            if (_path.isNotEmpty) ...[
              PathCrumbs(
                names: [widget.rootName, ..._shown],
                onTap: (depth) => setState(() {
                  _path.removeRange(depth, _path.length);
                  _shown.removeRange(depth, _shown.length);
                }),
              ),
              const Divider(),
            ],
            Expanded(
              child: AsyncBody(
                value: listing,
                onRetry: () => ref.invalidate(fsListingProvider(_at)),
                data: (r) {
                  final rows = _sorted([...r.entries, ...?r.deleted]);
                  final notes = [
                    if (r.inDeleted) 'A deleted folder: everything in it was deleted with it.',
                    if (r.truncated) 'Showing the first ${count(r.entries.length)} entries of this folder.',
                    for (final n in r.fs?.notes ?? const <String>[]) _sentence(n),
                    if (_deleted && r.undelete != null) _sentence(r.undelete!),
                    if (_deleted && r.deletedNote != null) _sentence(r.deletedNote!),
                    if (_deleted && !r.inDeleted && r.deleted != null && r.deleted!.isEmpty)
                      'No deleted files in this folder.',
                  ];
                  return Readable(
                    child: ListView.builder(
                      itemCount: notes.length + (rows.isEmpty ? 1 : rows.length),
                      itemBuilder: (c, i) {
                        if (i < notes.length) return ViewerNote(notes[i]);
                        if (rows.isEmpty) {
                          return const Padding(
                            padding: EdgeInsets.only(top: 48),
                            child: EmptyState(icon: Icons.folder_open, title: 'Empty folder'),
                          );
                        }
                        return _FsTile(e: rows[i - notes.length], onTap: () => _open(rows[i - notes.length]));
                      },
                    ),
                  );
                },
              ),
            ),
          ],
        ),
      ),
    );
  }
}

String _sentence(String s) => s.isEmpty ? s : '${s[0].toUpperCase()}${s.substring(1)}${s.endsWith('.') ? '' : '.'}';

class _FsTile extends StatelessWidget {
  const _FsTile({required this.e, required this.onTap});
  final FsEntry e;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final d = e.deleted;
    final (stateColor, stateText) = switch (d?.state) {
      'recoverable' => (context.bd.good, 'recoverable'),
      'partly overwritten' => (context.bd.warn, 'partly overwritten'),
      'overwritten' => (context.bd.crit, 'overwritten'),
      null => (context.bd.muted, ''),
      final s => (context.bd.crit, s),
    };
    final shownFlags = [
      for (final f in e.flags)
        if (const {'compressed', 'sparse', 'encrypted'}.contains(f)) f,
    ];
    final faded = d != null ? context.bd.muted : null;
    return ListTile(
      leading: Opacity(
        opacity: d != null ? 0.55 : 1,
        child: e.dir ? Icon(Icons.folder, size: 28, color: context.bd.folder) : FileGlyph(name: e.name),
      ),
      title: Text(
        e.name,
        maxLines: 2,
        overflow: TextOverflow.ellipsis,
        style: TextStyle(
          color: faded,
          decoration: d != null && d.state == 'overwritten' ? TextDecoration.lineThrough : null,
        ),
      ),
      subtitle: Text(
        [
          if (e.link != null) '→ ${e.link}' else if (!e.dir) bytes(e.size),
          ?dayOrNull(e.mtime),
          if (d != null) '${d.why}${d.state == 'overwritten' ? '' : ' (${d.confidence}% sure)'}',
        ].join(' · '),
        maxLines: d != null ? 3 : 1,
        overflow: TextOverflow.ellipsis,
      ),
      isThreeLine: d != null,
      trailing: d != null
          ? Pill(stateText, color: stateColor)
          : shownFlags.isNotEmpty
          ? Pill(shownFlags.join(', '))
          : null,
      onTap: onTap,
    );
  }
}
