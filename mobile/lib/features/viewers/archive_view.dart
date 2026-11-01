import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../../core/format.dart';
import '../../core/widgets.dart';
import '../../providers.dart';
import '../../theme.dart';
import 'common.dart';
import 'fallback.dart';
import 'models.dart';
import 'routes.dart';

final archiveProvider = FutureProvider.autoDispose.family<ArchiveListing, int>(
  (ref, id) => fetchParsed(requireApi(ref), 'archive', {'id': id}, ArchiveListing.fromJson),
);

/// An archive (zip, tar, plain or compressed) as folders: its listing read by the service, the
/// folders made up from the entries' paths, each file opening in the viewer that suits it.
class ArchiveView extends ConsumerStatefulWidget {
  const ArchiveView({super.key, required this.source});
  final ViewSource source;

  @override
  ConsumerState<ArchiveView> createState() => _ArchiveViewState();
}

class _ArchiveViewState extends ConsumerState<ArchiveView> {
  final _path = <String>[];
  InnerSort _sort = InnerSort.name;
  bool _descending = false;

  void _pick(InnerSort s) => setState(() {
    if (s == _sort) {
      _descending = !_descending;
    } else {
      _sort = s;
      _descending = false;
    }
  });

  int _by(String an, int? asize, int? atime, String bn, int? bsize, int? btime) {
    final flip = _sort == InnerSort.name ? _descending : !_descending;
    final c = switch (_sort) {
      InnerSort.name => 0,
      InnerSort.size => (asize ?? -1).compareTo(bsize ?? -1),
      InnerSort.date => (atime ?? 0).compareTo(btime ?? 0),
    };
    final r = c != 0 ? c : naturalCompare(an, bn);
    return flip ? -r : r;
  }

  @override
  Widget build(BuildContext context) {
    final listing = ref.watch(archiveProvider(widget.source.id));
    if (listing.hasError && !listing.hasValue) {
      return viewerError(widget.source, listing.error!, () => ref.invalidate(archiveProvider(widget.source.id)));
    }
    return AsyncBody(
      value: listing,
      onRetry: () => ref.invalidate(archiveProvider(widget.source.id)),
      data: (d) {
        var n = d.root;
        for (final p in _path) {
          n = n.dirs[p] ?? n;
        }
        final dirs = n.dirs.values.toList()..sort((a, b) => _by(a.name, a.size, a.mtime, b.name, b.size, b.mtime));
        final files = n.files.toList()..sort((a, b) => _by(a.$2, a.$1.size, a.$1.mtime, b.$2, b.$1.size, b.$1.mtime));
        final more = d.complete ? '' : '+';
        final notes = [
          if (d.entries.length < d.count)
            'Listing the first ${count(d.entries.length)} of ${count(d.count)}$more entries.',
          if (d.note != null && d.note!.isNotEmpty) '${d.note![0].toUpperCase()}${d.note!.substring(1)}.',
        ];
        final rows = dirs.length + files.length;
        return PopScope(
          canPop: _path.isEmpty,
          onPopInvokedWithResult: (didPop, _) {
            if (!didPop && _path.isNotEmpty) setState(_path.removeLast);
          },
          child: Column(
            children: [
              ViewerBar(
                meta:
                    '${d.format} · ${count(d.count)}$more ${d.count == 1 ? 'entry' : 'entries'} · ${bytes(d.totalSize)}$more unpacked',
                children: [InnerSortButton(sort: _sort, descending: _descending, onPick: _pick)],
              ),
              for (final t in notes) ViewerNote(t),
              if (_path.isNotEmpty) ...[
                PathCrumbs(
                  names: [widget.source.name, ..._path],
                  onTap: (depth) => setState(() => _path.removeRange(depth, _path.length)),
                ),
                const Divider(),
              ],
              Expanded(
                child: rows == 0
                    ? const EmptyState(icon: Icons.folder_open, title: 'Empty folder')
                    : Readable(
                        child: ListView.builder(
                          itemCount: rows,
                          itemBuilder: (c, i) {
                            if (i < dirs.length) {
                              final f = dirs[i];
                              return ListTile(
                                leading: Icon(Icons.folder, size: 28, color: context.bd.folder),
                                title: Text(f.name, maxLines: 2, overflow: TextOverflow.ellipsis),
                                subtitle: Text(
                                  [plural(f.count, 'item'), bytes(f.size), ?dayOrNull(f.mtime)].join(' · '),
                                ),
                                onTap: () => setState(() => _path.add(f.name)),
                              );
                            }
                            final (e, base) = files[i - dirs.length];
                            return ListTile(
                              leading: FileGlyph(name: base),
                              title: Text(base, maxLines: 2, overflow: TextOverflow.ellipsis),
                              subtitle: Text(
                                [
                                  if (e.link != null) '→ ${e.link}' else if (e.size != null) bytes(e.size),
                                  ?dayOrNull(e.mtime),
                                ].join(' · '),
                                maxLines: 1,
                                overflow: TextOverflow.ellipsis,
                              ),
                              trailing: e.encrypted
                                  ? Pill('encrypted', color: context.bd.warn, icon: Icons.lock)
                                  : null,
                              onTap: () => _openEntry(e, base),
                            );
                          },
                        ),
                      ),
              ),
            ],
          ),
        );
      },
    );
  }

  void _openEntry(ArchiveEntry e, String base) {
    if (e.link != null) {
      showSnack(context, 'A link to ${e.link}. Links are shown, not followed.');
      return;
    }
    context.push(innerLocation(InnerFile.entry(id: widget.source.id, name: e.name, size: e.size)));
  }
}
