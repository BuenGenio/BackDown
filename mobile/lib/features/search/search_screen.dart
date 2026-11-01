import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../../api/json.dart';
import '../../api/keys.dart';
import '../../core/format.dart';
import '../../core/widgets.dart';
import '../../models/browse.dart';
import '../../models/file_ref.dart';
import '../../providers.dart';
import '../../router.dart';
import '../../theme.dart';
import '../viewers/open.dart';

typedef FindQuery = ({String q, String? dir, bool earlier});

/// The ranked name search (GET /api/find): every drive, or under one folder; with [earlier],
/// also files that are gone (with when they were last seen).
final findProvider = FutureProvider.autoDispose.family<FindResult, FindQuery>((ref, f) async {
  // a name search reads the whole index: seconds on millions of files, more on a busy service
  final query = {'q': f.q, 'dir': f.dir, 'earlier': f.earlier ? '1' : null, 'limit': 200};
  final j = await requireApi(ref).get('find', query, const Duration(minutes: 2));
  return FindResult.fromJson(asJson(j));
});

class RecentSearches extends Notifier<List<String>> {
  static const _key = 'recent_searches';

  @override
  List<String> build() => ref.watch(prefsProvider).getStringList(_key) ?? const [];

  Future<void> add(String q) async {
    final t = q.trim();
    if (t.isEmpty) return;
    state = [t, ...state.where((x) => x != t)].take(10).toList();
    await ref.read(prefsProvider).setStringList(_key, state);
  }

  Future<void> clear() async {
    state = const [];
    await ref.read(prefsProvider).remove(_key);
  }
}

final recentSearchesProvider = NotifierProvider<RecentSearches, List<String>>(RecentSearches.new);

class SearchScreen extends ConsumerStatefulWidget {
  const SearchScreen({super.key, this.dir});

  /// Search under this folder (a key) rather than everywhere.
  final String? dir;

  @override
  ConsumerState<SearchScreen> createState() => _SearchScreenState();
}

class _SearchScreenState extends ConsumerState<SearchScreen> {
  final _field = TextEditingController();
  Timer? _debounce;
  String _q = '';
  late String? _dir = widget.dir;
  bool _earlier = false;

  @override
  void didUpdateWidget(SearchScreen old) {
    super.didUpdateWidget(old);
    if (widget.dir != old.dir) setState(() => _dir = widget.dir);
  }

  @override
  void dispose() {
    _debounce?.cancel();
    _field.dispose();
    super.dispose();
  }

  void _changed(String v) {
    _debounce?.cancel();
    // one search per pause in typing, of two characters or more: each reads the whole index
    final t = v.trim();
    _debounce = Timer(const Duration(milliseconds: 600), () => setState(() => _q = t.length < 2 ? '' : t));
  }

  void _searchNow(String v) {
    _debounce?.cancel();
    _field.text = v;
    setState(() => _q = v.trim());
    unawaited(ref.read(recentSearchesProvider.notifier).add(v));
  }

  void _openHit(FindHit h, List<FindHit> all) {
    unawaited(ref.read(recentSearchesProvider.notifier).add(_q));
    if (h.isDir) {
      context.push(Routes.folder(h.key));
      return;
    }
    final id = h.id;
    if (id == null) return;
    if (h.gone) {
      context.push(Routes.file(id));
      return;
    }
    final files = [
      for (final x in all)
        if (!x.isDir && !x.gone && x.id != null) FileRef(id: x.id!, name: x.name, size: x.size, mtime: x.mtime),
    ];
    openFile(
      context,
      FileRef(id: id, name: h.name, size: h.size, mtime: h.mtime),
      siblings: files,
    );
  }

  @override
  Widget build(BuildContext context) {
    final query = (q: _q, dir: _dir, earlier: _earlier);
    return Scaffold(
      appBar: AppBar(
        titleSpacing: 8,
        title: TextField(
          controller: _field,
          textInputAction: TextInputAction.search,
          autocorrect: false,
          decoration: InputDecoration(
            hintText: _dir == null ? 'Search every drive' : 'Search in ${baseName(pathOfKey(_dir!))}',
            prefixIcon: const Icon(Icons.search),
            isDense: true,
            contentPadding: const EdgeInsets.symmetric(vertical: 10),
            suffixIcon: _field.text.isEmpty
                ? null
                : IconButton(
                    icon: const Icon(Icons.close),
                    onPressed: () {
                      _field.clear();
                      setState(() => _q = '');
                    },
                  ),
          ),
          onChanged: (v) {
            setState(() {});
            _changed(v);
          },
          onSubmitted: _searchNow,
        ),
      ),
      body: Column(
        children: [
          SizedBox(
            height: 48,
            child: ListView(
              scrollDirection: Axis.horizontal,
              padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
              children: [
                if (_dir != null) ...[
                  InputChip(
                    avatar: const Icon(Icons.folder_outlined, size: 18),
                    label: Text('In ${baseName(pathOfKey(_dir!))}'),
                    onDeleted: () => setState(() => _dir = null),
                  ),
                  const SizedBox(width: 8),
                ],
                FilterChip(
                  label: const Text('Include deleted'),
                  selected: _earlier,
                  onSelected: (v) => setState(() => _earlier = v),
                ),
              ],
            ),
          ),
          const Divider(height: 1),
          Expanded(
            child: _q.isEmpty ? _Recent(onPick: _searchNow) : _Results(query: query, onOpen: _openHit),
          ),
        ],
      ),
    );
  }
}

class _Recent extends ConsumerWidget {
  const _Recent({required this.onPick});
  final ValueChanged<String> onPick;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final recent = ref.watch(recentSearchesProvider);
    if (recent.isEmpty) {
      return const EmptyState(
        icon: Icons.manage_search,
        title: 'Find anything on any drive',
        message: 'By name, across every drive this computer has indexed, including ones that aren\'t plugged in.',
      );
    }
    return ListView(
      children: [
        SectionHeader(
          'Recent',
          trailing: TextButton(
            onPressed: () => ref.read(recentSearchesProvider.notifier).clear(),
            child: const Text('Clear'),
          ),
        ),
        for (final q in recent) ListTile(leading: const Icon(Icons.history), title: Text(q), onTap: () => onPick(q)),
      ],
    );
  }
}

class _Results extends ConsumerWidget {
  const _Results({required this.query, required this.onOpen});
  final FindQuery query;
  final void Function(FindHit, List<FindHit>) onOpen;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final r = ref.watch(findProvider(query));
    final skew = ref.watch(skewProvider);
    return AsyncBody(
      value: r,
      onRetry: () => ref.invalidate(findProvider(query)),
      loading: const _Searching(),
      data: (res) {
        if (res.hits.isEmpty) {
          return EmptyState(
            icon: Icons.search_off,
            title: 'Nothing called "${query.q}"',
            message: query.earlier ? null : 'Files that were deleted are left out: try "Include deleted".',
          );
        }
        return ListView.builder(
          itemCount: res.hits.length + 1,
          itemBuilder: (c, i) {
            if (i == res.hits.length) {
              return Padding(
                padding: const EdgeInsets.all(16),
                child: Text(
                  res.total > res.hits.length
                      ? 'The best ${count(res.hits.length)} of ${count(res.total)} matches'
                      : plural(res.total, 'match', 'matches'),
                  textAlign: TextAlign.center,
                  style: context.text.bodySmall?.copyWith(color: context.bd.muted),
                ),
              );
            }
            final h = res.hits[i];
            final where = h.folder.isEmpty ? h.drive : '${baseName(h.drive)} › ${h.folder}';
            return Opacity(
              opacity: h.gone ? 0.6 : 1,
              child: ListTile(
                leading: FileGlyph(name: h.name, isDir: h.isDir),
                title: _Highlighted(text: h.name, match: query.q),
                subtitle: Text(
                  [
                    where,
                    if (h.gone)
                      'deleted · last seen ${rel(h.seen, skew: skew)}'
                    else if (!h.isDir)
                      '${bytes(h.size)} · ${day(h.mtime)}',
                  ].join('\n'),
                  maxLines: 2,
                  overflow: TextOverflow.ellipsis,
                ),
                isThreeLine: true,
                onTap: () => onOpen(h, res.hits),
                onLongPress: h.id == null ? null : () => context.push(Routes.file(h.id!)),
              ),
            );
          },
        );
      },
    );
  }
}

/// A name with the part that matched in bold.
class _Highlighted extends StatelessWidget {
  const _Highlighted({required this.text, required this.match});
  final String text, match;

  @override
  Widget build(BuildContext context) {
    final i = match.isEmpty ? -1 : text.toLowerCase().indexOf(match.toLowerCase());
    if (i < 0) return Text(text, maxLines: 1, overflow: TextOverflow.ellipsis);
    final bold = TextStyle(fontWeight: FontWeight.w800, color: context.colors.primary);
    return Text.rich(
      TextSpan(
        children: [
          TextSpan(text: text.substring(0, i)),
          TextSpan(text: text.substring(i, i + match.length), style: bold),
          TextSpan(text: text.substring(i + match.length)),
        ],
      ),
      maxLines: 1,
      overflow: TextOverflow.ellipsis,
    );
  }
}

/// The spinner, and after a few seconds why it takes a while.
class _Searching extends StatefulWidget {
  const _Searching();

  @override
  State<_Searching> createState() => _SearchingState();
}

class _SearchingState extends State<_Searching> {
  late final Timer _timer;
  bool _slow = false;

  @override
  void initState() {
    super.initState();
    _timer = Timer(const Duration(seconds: 3), () => setState(() => _slow = true));
  }

  @override
  void dispose() {
    _timer.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return Center(
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          const CircularProgressIndicator(),
          if (_slow) ...[
            const SizedBox(height: 16),
            Text(
              'Still searching: every name in the index is read,\nwhich takes a while on millions of files.',
              textAlign: TextAlign.center,
              style: context.text.bodySmall?.copyWith(color: context.bd.muted),
            ),
          ],
        ],
      ),
    );
  }
}
