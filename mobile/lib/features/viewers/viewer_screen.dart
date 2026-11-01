import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../../core/file_io.dart';
import '../../core/format.dart';
import '../../core/widgets.dart';
import '../../models/file_ref.dart';
import '../../providers.dart';
import '../../router.dart';
import '../../theme.dart';
import 'choose.dart';
import 'common.dart';
import 'open.dart';

/// A file full screen, in the viewer that suits it, with the others it was opened among (a
/// folder's files, a search's results) a swipe away. Opened by a link without them, it asks the
/// service what the file is.
class ViewerScreen extends ConsumerWidget {
  const ViewerScreen({super.key, required this.id, this.args});
  final int id;
  final ViewerArgs? args;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final a = args;
    if (a != null && a.file.id == id) {
      final files = a.siblings.any((f) => f.id == id) ? a.siblings : [a.file];
      return _Pager(files: files, start: files.indexWhere((f) => f.id == id));
    }
    // a deep link: the file's name and size from its details
    final info = ref.watch(fileInfoProvider(id));
    return switch (info) {
      AsyncValue(:final value?) => _Pager(
        files: [FileRef(id: value.id, name: value.name, size: value.size, mtime: value.mtime)],
        start: 0,
      ),
      AsyncValue(:final error?) => Scaffold(
        appBar: AppBar(title: const Text('File')),
        body: ErrorPanel(error: error, onRetry: () => ref.invalidate(fileInfoProvider(id))),
      ),
      _ => Scaffold(
        appBar: AppBar(),
        body: const Center(child: CircularProgressIndicator()),
      ),
    };
  }
}

class _Pager extends ConsumerStatefulWidget {
  const _Pager({required this.files, required this.start});
  final List<FileRef> files;
  final int start;

  @override
  ConsumerState<_Pager> createState() => _PagerState();
}

class _PagerState extends ConsumerState<_Pager> {
  late int _index = widget.start.clamp(0, widget.files.length - 1);
  late final PageController _pages = PageController(initialPage: _index);
  bool _zoomed = false;

  FileRef get _file => widget.files[_index];

  @override
  void dispose() {
    _pages.dispose();
    super.dispose();
  }

  void _step(int by) {
    final to = _index + by;
    if (to < 0 || to >= widget.files.length) return;
    _pages.animateToPage(to, duration: const Duration(milliseconds: 250), curve: Curves.easeOut);
  }

  @override
  Widget build(BuildContext context) {
    final api = ref.watch(apiProvider);
    final f = _file;
    final many = widget.files.length > 1;
    final wide = MediaQuery.sizeOf(context).width >= 600;
    return CallbackShortcuts(
      bindings: {
        const SingleActivator(LogicalKeyboardKey.arrowLeft): () => _step(-1),
        const SingleActivator(LogicalKeyboardKey.arrowRight): () => _step(1),
      },
      child: Focus(
        autofocus: true,
        child: Scaffold(
          appBar: AppBar(
            title: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(f.name, maxLines: 1, overflow: TextOverflow.ellipsis),
                Text(
                  [
                    if (many) '${_index + 1} of ${widget.files.length}',
                    bytes(f.size),
                    if (f.mtime > 0) day(f.mtime),
                  ].join(' · '),
                  style: context.text.bodySmall?.copyWith(color: context.colors.onSurfaceVariant),
                ),
              ],
            ),
            actions: [
              if (many && wide) ...[
                IconButton(
                  tooltip: 'Previous',
                  onPressed: _index > 0 ? () => _step(-1) : null,
                  icon: const Icon(Icons.chevron_left),
                ),
                IconButton(
                  tooltip: 'Next',
                  onPressed: _index < widget.files.length - 1 ? () => _step(1) : null,
                  icon: const Icon(Icons.chevron_right),
                ),
              ],
              IconButton(
                tooltip: 'Details',
                icon: const Icon(Icons.info_outline),
                onPressed: () => context.push(Routes.file(f.id)),
              ),
              if (api != null)
                IconButton(
                  tooltip: 'Share',
                  icon: const Icon(Icons.share_outlined),
                  onPressed: () => shareFile(context, api, f),
                ),
              if (api != null)
                PopupMenuButton<String>(
                  onSelected: (v) {
                    if (v == 'open') openInAnotherApp(context, api, f);
                  },
                  itemBuilder: (c) => const [
                    PopupMenuItem(
                      value: 'open',
                      child: ListTile(
                        contentPadding: EdgeInsets.zero,
                        leading: Icon(Icons.open_in_new),
                        title: Text('Open in another app'),
                      ),
                    ),
                  ],
                ),
            ],
          ),
          body: PageView.builder(
            controller: _pages,
            itemCount: widget.files.length,
            physics: _zoomed ? const NeverScrollableScrollPhysics() : null,
            onPageChanged: (i) => setState(() {
              _index = i;
              _zoomed = false;
            }),
            itemBuilder: (c, i) => ViewerPage(
              active: i == _index,
              onZoom: (z) {
                if (i == _index && z != _zoomed) setState(() => _zoomed = z);
              },
              child: KeyedSubtree(
                key: ValueKey(widget.files[i].id),
                child: FileViewer(file: widget.files[i]),
              ),
            ),
          ),
        ),
      ),
    );
  }
}
