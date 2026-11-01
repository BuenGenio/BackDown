import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../../api/keys.dart';
import '../../core/file_io.dart';
import '../../core/format.dart';
import '../../core/widgets.dart';
import '../../models/browse.dart';
import '../../models/file_ref.dart';
import '../../providers.dart';
import '../../router.dart';
import '../../theme.dart';
import '../app_settings/preferences.dart';
import '../viewers/mailbox_banner.dart';
import '../viewers/open.dart';
import 'entry_tile.dart';

enum FolderSort {
  name('Name'),
  size('Size'),
  date('Date modified');

  const FolderSort(this.label);
  final String label;
}

/// A folder of a drive, at its latest scan or (with [scan]) as an earlier scan saw it.
class FolderScreen extends ConsumerStatefulWidget {
  const FolderScreen({super.key, required this.dirKey, this.scan, this.select});

  final String dirKey;
  final int? scan;

  /// A file's key to show highlighted (from "Show in folder").
  final String? select;

  @override
  ConsumerState<FolderScreen> createState() => _FolderScreenState();
}

class _FolderScreenState extends ConsumerState<FolderScreen> {
  FolderSort _sort = FolderSort.name;
  bool _descending = false;

  FolderAt get _at => (key: widget.dirKey, scan: widget.scan);

  List<Entry> _sorted(List<Entry> all) {
    final hidden = ref.watch(showHiddenProvider);
    final list = [
      for (final e in all)
        if (hidden || !e.hidden) e,
    ];
    int by(Entry a, Entry b) => switch (_sort) {
      FolderSort.name => a.name.toLowerCase().compareTo(b.name.toLowerCase()),
      FolderSort.size => a.size.compareTo(b.size),
      FolderSort.date => a.mtime.compareTo(b.mtime),
    };
    // folders first, as file managers do; sizes and dates read biggest and newest first
    final flip = _sort == FolderSort.name ? _descending : !_descending;
    list.sort((a, b) {
      if (a.isDir != b.isDir) return a.isDir ? -1 : 1;
      final c = by(a, b);
      return flip ? -c : c;
    });
    return list;
  }

  void _open(Entry e, List<Entry> shown) {
    if (e.isDir) {
      context.push(Routes.folder(e.key, scan: widget.scan));
      return;
    }
    final id = e.id;
    if (id == null) return;
    final files = [
      for (final x in shown)
        if (!x.isDir && x.id != null) FileRef(id: x.id!, name: x.name, size: x.size, mtime: x.mtime),
    ];
    openFile(
      context,
      FileRef(id: id, name: e.name, size: e.size, mtime: e.mtime),
      siblings: files,
    );
  }

  Future<void> _actions(Entry e, Listing l) async {
    final api = ref.read(apiProvider);
    final id = e.id;
    final ref0 = id == null ? null : FileRef(id: id, name: e.name, size: e.size, mtime: e.mtime);
    final path = l.path.endsWith('/') ? '${l.path}${e.name}' : '${l.path}/${e.name}';
    await showModalBottomSheet<void>(
      context: context,
      showDragHandle: true,
      builder: (c) => SafeArea(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            ListTile(
              leading: EntryThumb(entry: e),
              title: Text(e.name, maxLines: 2, overflow: TextOverflow.ellipsis),
              subtitle: Text(entrySubtitle(e)),
            ),
            const Divider(),
            if (e.isDir)
              ListTile(
                leading: const Icon(Icons.folder_open),
                title: const Text('Open'),
                onTap: () {
                  Navigator.pop(c);
                  _open(e, const []);
                },
              ),
            if (ref0 != null) ...[
              ListTile(
                leading: const Icon(Icons.info_outline),
                title: const Text('Details'),
                subtitle: const Text('Where else it is, its versions and hash'),
                onTap: () {
                  Navigator.pop(c);
                  context.push(Routes.file(ref0.id));
                },
              ),
              if (api != null) ...[
                ListTile(
                  leading: const Icon(Icons.share_outlined),
                  title: const Text('Share'),
                  onTap: () {
                    Navigator.pop(c);
                    shareFile(context, api, ref0);
                  },
                ),
                ListTile(
                  leading: const Icon(Icons.open_in_new),
                  title: const Text('Open in another app'),
                  onTap: () {
                    Navigator.pop(c);
                    openInAnotherApp(context, api, ref0);
                  },
                ),
              ],
            ],
            ListTile(
              leading: const Icon(Icons.copy),
              title: const Text('Copy path'),
              onTap: () async {
                Navigator.pop(c);
                await Clipboard.setData(ClipboardData(text: path));
                if (mounted) showSnack(context, 'Path copied');
              },
            ),
          ],
        ),
      ),
    );
  }

  Future<void> _pickScan(Listing l) async {
    final picked = await showModalBottomSheet<int>(
      context: context,
      showDragHandle: true,
      isScrollControlled: true,
      builder: (c) => _ScanPicker(rootId: l.rootId, current: l.scan, latest: l.latestScan),
    );
    if (picked == null || !mounted) return;
    context.pushReplacement(Routes.folder(widget.dirKey, scan: picked == l.latestScan ? null : picked));
  }

  void _sortMenu() async {
    final picked = await showModalBottomSheet<FolderSort>(
      context: context,
      showDragHandle: true,
      builder: (c) => SafeArea(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            for (final s in FolderSort.values)
              ListTile(
                title: Text(s.label),
                trailing: s == _sort
                    ? Icon(_descending ? Icons.arrow_upward : Icons.arrow_downward, color: c.colors.primary)
                    : null,
                onTap: () => Navigator.pop(c, s),
              ),
          ],
        ),
      ),
    );
    if (picked == null) return;
    setState(() {
      if (picked == _sort) {
        _descending = !_descending;
      } else {
        _sort = picked;
        _descending = false;
      }
    });
  }

  @override
  Widget build(BuildContext context) {
    final listing = ref.watch(listingProvider(_at));
    final grid = ref.watch(folderGridProvider);
    final l = listing.value;
    return Scaffold(
      appBar: AppBar(
        title: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(l == null ? '' : baseName(l.name), maxLines: 1, overflow: TextOverflow.ellipsis),
            if (l != null)
              Text(
                '${plural(l.files, 'file')} · ${bytes(l.bytes)}',
                style: context.text.bodySmall?.copyWith(color: context.colors.onSurfaceVariant),
              ),
          ],
        ),
        actions: [
          IconButton(
            tooltip: 'Search in this folder',
            icon: const Icon(Icons.search),
            onPressed: () => context.go(Uri(path: Routes.search, queryParameters: {'dir': widget.dirKey}).toString()),
          ),
          IconButton(tooltip: 'Sort', icon: const Icon(Icons.sort), onPressed: _sortMenu),
          PopupMenuButton<String>(
            onSelected: (v) async {
              switch (v) {
                case 'grid':
                  await ref.read(folderGridProvider.notifier).set(!grid);
                case 'hidden':
                  await ref.read(showHiddenProvider.notifier).set(!ref.read(showHiddenProvider));
                case 'time':
                  if (l != null) await _pickScan(l);
                case 'path':
                  if (l != null) {
                    await Clipboard.setData(ClipboardData(text: l.path));
                    if (context.mounted) showSnack(context, 'Path copied');
                  }
              }
            },
            itemBuilder: (c) => [
              PopupMenuItem(value: 'grid', child: Text(grid ? 'Show as a list' : 'Show as a grid')),
              CheckedPopupMenuItem(
                value: 'hidden',
                checked: ref.read(showHiddenProvider),
                child: const Text('Hidden files'),
              ),
              const PopupMenuItem(value: 'time', child: Text('As at an earlier scan…')),
              const PopupMenuItem(value: 'path', child: Text('Copy path')),
            ],
          ),
        ],
      ),
      body: AsyncBody(
        value: listing,
        onRetry: () => ref.invalidate(listingProvider(_at)),
        data: (l) {
          final shown = _sorted(l.entries);
          final biggest = shown.fold<int>(0, (a, e) => e.size > a ? e.size : a);
          final header = <Widget>[
            _Crumbs(crumbs: l.crumbs, scan: widget.scan),
            if (l.isPast) _PastBanner(listing: l, onNow: () => context.pushReplacement(Routes.folder(widget.dirKey))),
            MailboxBanner(listing: l),
            if (l.needsReindex)
              const Padding(
                padding: EdgeInsets.all(16),
                child: Text('This drive was indexed by an older BackDown: rescan it to see folder sizes.'),
              ),
          ];
          if (shown.isEmpty) {
            return Column(
              children: [
                ...header,
                const Expanded(
                  child: EmptyState(icon: Icons.folder_open, title: 'Empty folder'),
                ),
              ],
            );
          }
          return RefreshIndicator(
            onRefresh: () => ref.refresh(listingProvider(_at).future),
            child: grid
                ? CustomScrollView(
                    slivers: [
                      SliverList.list(children: header),
                      SliverPadding(
                        padding: const EdgeInsets.all(8),
                        sliver: SliverGrid.builder(
                          gridDelegate: const SliverGridDelegateWithMaxCrossAxisExtent(
                            maxCrossAxisExtent: 120,
                            childAspectRatio: 0.78,
                          ),
                          itemCount: shown.length,
                          itemBuilder: (c, i) => EntryCard(
                            entry: shown[i],
                            onTap: () => _open(shown[i], shown),
                            onLongPress: () => _actions(shown[i], l),
                          ),
                        ),
                      ),
                    ],
                  )
                : ListView.builder(
                    itemCount: shown.length + header.length,
                    itemBuilder: (c, i) {
                      if (i < header.length) return header[i];
                      final e = shown[i - header.length];
                      return EntryTile(
                        entry: e,
                        selected: e.key == widget.select,
                        share: _sort == FolderSort.size && biggest > 0 ? e.size / biggest : null,
                        onTap: () => _open(e, shown),
                        onLongPress: () => _actions(e, l),
                      );
                    },
                  ),
          );
        },
      ),
    );
  }
}

/// Where this folder is: the drive, then each folder down to here.
class _Crumbs extends StatelessWidget {
  const _Crumbs({required this.crumbs, this.scan});
  final List<Crumb> crumbs;
  final int? scan;

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
              onTap: last ? null : () => context.go(Routes.folder(crumb.key, scan: scan)),
              child: Padding(
                padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 4),
                child: Text(
                  // the drive's own crumb is its whole path: its name is enough
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

class _PastBanner extends StatelessWidget {
  const _PastBanner({required this.listing, required this.onNow});
  final Listing listing;
  final VoidCallback onNow;

  @override
  Widget build(BuildContext context) {
    return Container(
      margin: const EdgeInsets.fromLTRB(12, 4, 12, 8),
      padding: const EdgeInsets.fromLTRB(14, 8, 6, 8),
      decoration: BoxDecoration(
        color: context.bd.info.withValues(alpha: 0.12),
        borderRadius: BorderRadius.circular(10),
      ),
      child: Row(
        children: [
          Icon(Icons.history, color: context.bd.info, size: 20),
          const SizedBox(width: 10),
          Expanded(child: Text('As the scan of ${when(listing.scannedAt)} saw it')),
          TextButton(onPressed: onNow, child: const Text('Back to now')),
        ],
      ),
    );
  }
}

/// The drive's scans, to see a folder as one of them saw it.
class _ScanPicker extends ConsumerWidget {
  const _ScanPicker({required this.rootId, required this.current, required this.latest});
  final int rootId, current, latest;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final scans = ref.watch(timelineProvider(rootId));
    return SafeArea(
      child: ConstrainedBox(
        constraints: BoxConstraints(maxHeight: MediaQuery.sizeOf(context).height * 0.7),
        child: AsyncBody(
          value: scans,
          onRetry: () => ref.invalidate(timelineProvider(rootId)),
          data: (list) => ListView(
            shrinkWrap: true,
            children: [
              Padding(
                padding: const EdgeInsets.fromLTRB(20, 0, 20, 8),
                child: Text('Show this folder as a scan saw it', style: context.text.titleMedium),
              ),
              for (final s in list.reversed)
                ListTile(
                  leading: Icon(s.id == latest ? Icons.today : Icons.history),
                  title: Text(s.id == latest ? 'Now (latest scan)' : when(s.finishedAt)),
                  subtitle: Text('${plural(s.files, 'file')} · ${bytes(s.bytes)}'),
                  trailing: s.id == current ? Icon(Icons.check, color: context.colors.primary) : null,
                  onTap: () => Navigator.pop(context, s.id),
                ),
            ],
          ),
        ),
      ),
    );
  }
}
