import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../../core/format.dart';
import '../../core/widgets.dart';
import '../../providers.dart';
import '../../router.dart';
import '../../theme.dart';
import 'common.dart';
import 'mail_view.dart';
import 'models.dart';
import 'routes.dart';

/// A mailbox: an mbox file (by index id) or a Maildir folder (by key).
@immutable
class MailboxSource {
  const MailboxSource.mbox(int this.id) : dir = null;
  const MailboxSource.maildir(String this.dir) : id = null;

  final int? id;
  final String? dir;

  bool get isMbox => id != null;
  Map<String, Object?> get params => isMbox ? {'id': id} : {'dir': dir};

  /// Where one of its messages is read from: the mbox at the message's offset, or the Maildir
  /// message's own file.
  MailAt message(int key) => isMbox ? (id!, key) : (key, null);

  @override
  bool operator ==(Object other) => other is MailboxSource && other.id == id && other.dir == dir;

  @override
  int get hashCode => Object.hash(id, dir);
}

/// One conversation of a mailbox, as found by a search ([q]) or listed message by message ([flat]).
typedef ThreadQuery = ({MailboxSource src, int key, String q, bool flat});

final threadProvider = FutureProvider.autoDispose.family<MailThread, ThreadQuery>(
  (ref, t) => fetchParsed(requireApi(ref), 'thread', {
    ...t.src.params,
    'key': t.key,
    'q': t.q,
    'flat': t.flat ? 1 : null,
  }, MailThread.fromJson),
);

const _page = 100;

/// A mailbox as conversations, newest activity first: threaded by the service from headers alone,
/// read a time-boxed slice at a time ("Keep reading" for more), with a search over senders and
/// subjects. A conversation opens beside the list on a wide screen, or on its own.
class MailboxView extends ConsumerStatefulWidget {
  const MailboxView({super.key, required this.source, this.onName});
  final MailboxSource source;

  /// The mailbox's name, once the service has said (a Maildir's).
  final ValueChanged<String>? onName;

  @override
  ConsumerState<MailboxView> createState() => _MailboxViewState();
}

class _MailboxViewState extends ConsumerState<MailboxView> {
  late MailboxSource _src = widget.source;
  final _search = TextEditingController();
  Timer? _typing;
  String _q = '';
  bool _flat = false;
  MailboxPage? _d;
  List<ThreadSummary> _rows = [];
  int _total = 0;
  List<MailFolder> _folders = const [];
  bool _reading = false, _stop = false, _paging = false;
  Object? _error;
  int _seq = 0;
  int? _open;

  @override
  void initState() {
    super.initState();
    _load();
  }

  @override
  void dispose() {
    _typing?.cancel();
    _search.dispose();
    _stop = true;
    super.dispose();
  }

  Map<String, Object?> get _query => {..._src.params, 'q': _q, 'flat': _flat ? 1 : null};

  /// The list from its start (after a search, a change of folder or mode, or reading further:
  /// then as many rows as were shown).
  Future<MailboxPage?> _load({bool more = false, bool keep = false}) async {
    final seq = ++_seq;
    final api = ref.read(apiProvider);
    if (api == null) return null;
    final limit = keep ? math.max(_page, _rows.length) : _page;
    try {
      final d = await fetchParsed(api, 'threads', {
        ..._query,
        'limit': limit,
        'more': more ? 1 : null,
      }, MailboxPage.fromJson);
      if (seq != _seq || !mounted) return null;
      setState(() {
        _d = d;
        _rows = d.threads;
        _total = d.total;
        if (d.folders.isNotEmpty) _folders = d.folders;
        _error = null;
        if (_open != null && !_rows.any((t) => t.key == _open)) _open = null;
        _open ??= _rows.firstOrNull?.key;
      });
      final name = d.name;
      if (name != null) {
        final here = d.folders.where((f) => f.here).firstOrNull;
        widget.onName?.call(here != null && d.folders.length > 1 ? '$name · ${here.name}' : name);
      }
      return d;
    } on Object catch (e) {
      if (seq == _seq && mounted) setState(() => _error = e);
      return null;
    }
  }

  Future<void> _more() async {
    final api = ref.read(apiProvider);
    if (api == null || _paging) return;
    final seq = _seq;
    setState(() => _paging = true);
    try {
      final d = await fetchParsed(api, 'threads', {
        ..._query,
        'offset': _rows.length,
        'limit': _page,
      }, MailboxPage.fromJson);
      if (seq != _seq || !mounted) return;
      setState(() {
        _rows = [..._rows, ...d.threads];
        _total = d.total;
      });
    } on Object catch (e) {
      if (mounted) showSnack(context, e.toString());
    } finally {
      if (mounted) setState(() => _paging = false);
    }
  }

  Future<void> _keepReading() async {
    if (_reading) return;
    setState(() {
      _reading = true;
      _stop = false;
    });
    try {
      while (!_stop && mounted) {
        final d = await _load(more: true, keep: true);
        if (d == null || d.status.done || d.status.capped) break;
      }
    } finally {
      if (mounted) setState(() => _reading = false);
    }
  }

  void _searchFor(String text, {bool now = false}) {
    _typing?.cancel();
    void go() {
      if (text.trim() == _q) return;
      _q = text.trim();
      _load();
    }

    if (now) {
      go();
    } else {
      _typing = Timer(const Duration(milliseconds: 300), go);
    }
  }

  void _openThread(ThreadSummary t, bool wide) {
    if (wide) {
      setState(() => _open = t.key);
    } else {
      context.push(threadLocation((src: _src, key: t.key, q: _q, flat: _flat)));
    }
  }

  List<Widget> _notes(MailboxPage d) {
    final s = d.status;
    final pct = math.min(99, (100 * s.read) ~/ math.max(1, s.size));
    final what = s.isMbox ? '$pct% of the file' : 'the newest ${count(s.read)} of ${plural(s.size, 'message')}';
    return [
      if (_reading)
        ViewerNote(
          'Reading… $what so far.',
          icon: Icons.hourglass_top,
          action: TextButton(onPressed: () => setState(() => _stop = true), child: const Text('Stop')),
        )
      else if (s.capped)
        ViewerNote('A very large mailbox: only its first ${count(s.messages)} messages are threaded.')
      else if (!s.done)
        ViewerNote(
          'Partial list: BackDown has read $what${s.isMbox ? ' (${plural(s.messages, 'message')})' : ''}, so threads '
          'may go on further in${_q.isEmpty ? '' : ' and the search covers only that'}.',
          action: TextButton(onPressed: _keepReading, child: const Text('Keep reading')),
        ),
      if (s.unreadable > 0)
        ViewerNote(
          '${plural(s.unreadable, 'message')} couldn\'t be read: their bytes aren\'t on any indexed drive now.',
          icon: Icons.warning_amber_outlined,
          color: context.bd.warn,
        ),
    ];
  }

  Widget _list(MailboxPage d, bool wide) {
    final s = d.status;
    final more = _rows.length < _total;
    final what = _flat ? 'message' : 'thread';
    final footer = Padding(
      padding: const EdgeInsets.fromLTRB(16, 8, 8, 24),
      child: Row(
        children: [
          Expanded(
            child: Text(
              '${more ? '${count(_rows.length)} of ' : ''}${plural(_total, what)}'
              '${_q.isEmpty ? '' : (_total == 1 ? ' matches' : ' match')}',
              style: context.text.bodySmall?.copyWith(color: context.bd.muted),
            ),
          ),
          if (more) TextButton(onPressed: _paging ? null : _more, child: Text(_paging ? 'Loading…' : 'Show more')),
        ],
      ),
    );
    return Column(
      children: [
        ViewerBar(
          meta: '${s.isMbox ? 'mbox' : 'Maildir'} · ${plural(s.messages, 'message')}${s.done ? '' : '+'}',
          children: [
            Padding(
              padding: const EdgeInsets.only(right: 8),
              child: SegmentedButton<bool>(
                showSelectedIcon: false,
                style: const ButtonStyle(visualDensity: VisualDensity.compact),
                segments: const [
                  ButtonSegment(value: false, label: Text('Threads')),
                  ButtonSegment(value: true, label: Text('Messages')),
                ],
                selected: {_flat},
                onSelectionChanged: (v) {
                  setState(() => _flat = v.first);
                  _open = null;
                  _load();
                },
              ),
            ),
          ],
        ),
        Padding(
          padding: const EdgeInsets.fromLTRB(12, 10, 12, 2),
          child: TextField(
            controller: _search,
            textInputAction: TextInputAction.search,
            decoration: InputDecoration(
              isDense: true,
              prefixIcon: const Icon(Icons.search),
              hintText: 'Search senders and subjects',
              suffixIcon: _search.text.isEmpty
                  ? null
                  : IconButton(
                      tooltip: 'Clear',
                      icon: const Icon(Icons.close),
                      onPressed: () {
                        _search.clear();
                        _searchFor('', now: true);
                      },
                    ),
            ),
            onChanged: (t) {
              setState(() {});
              _searchFor(t);
            },
            onSubmitted: (t) => _searchFor(t, now: true),
          ),
        ),
        if (_folders.length > 1)
          SizedBox(
            height: 48,
            child: ListView.separated(
              scrollDirection: Axis.horizontal,
              padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
              itemCount: _folders.length,
              separatorBuilder: (c, i) => const SizedBox(width: 6),
              itemBuilder: (c, i) {
                final f = _folders[i];
                final here = !_src.isMbox && f.key == _src.dir;
                return ChoiceChip(
                  label: Text(f.fresh > 0 ? '${f.name} · ${count(f.fresh)} new' : f.name),
                  selected: here,
                  onSelected: here
                      ? null
                      : (_) {
                          setState(() {
                            _src = MailboxSource.maildir(f.key);
                            _open = null;
                          });
                          _load();
                        },
                );
              },
            ),
          ),
        ..._notes(d),
        const SizedBox(height: 4),
        Expanded(
          child: _rows.isEmpty
              ? EmptyState(
                  icon: _q.isEmpty ? Icons.mail_outline : Icons.search_off,
                  title: _q.isNotEmpty
                      ? 'Nothing matches in what has been read'
                      : s.done
                      ? 'No messages in this mailbox'
                      : 'No messages found yet',
                )
              : ListView.builder(
                  itemCount: _rows.length + 1,
                  itemBuilder: (c, i) => i == _rows.length
                      ? footer
                      : _ThreadTile(
                          t: _rows[i],
                          selected: wide && _rows[i].key == _open,
                          onTap: () => _openThread(_rows[i], wide),
                        ),
                ),
        ),
      ],
    );
  }

  @override
  Widget build(BuildContext context) {
    final d = _d;
    if (d == null) {
      return _error != null
          ? ErrorPanel(error: _error!, onRetry: _load)
          : const ViewerLoading(text: 'Reading the mailbox…');
    }
    return LayoutBuilder(
      builder: (context, box) {
        final wide = box.maxWidth >= 900;
        if (!wide) return _list(d, false);
        final open = _open;
        return Row(
          children: [
            SizedBox(width: 400, child: _list(d, true)),
            const VerticalDivider(width: 1),
            Expanded(
              child: open == null
                  ? const EmptyState(icon: Icons.forum_outlined, title: 'Choose a conversation')
                  : ThreadView(
                      key: ValueKey((_src, open, _q, _flat)),
                      query: (src: _src, key: open, q: _q, flat: _flat),
                    ),
            ),
          ],
        );
      },
    );
  }
}

class _ThreadTile extends StatelessWidget {
  const _ThreadTile({required this.t, required this.selected, required this.onTap});
  final ThreadSummary t;
  final bool selected;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final unread = t.unread > 0;
    return ListTile(
      selected: selected,
      selectedTileColor: context.bd.selection,
      onTap: onTap,
      title: Row(
        children: [
          Expanded(
            child: Text(
              t.subject.isEmpty ? '(no subject)' : t.subject,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: TextStyle(fontWeight: unread ? FontWeight.w700 : FontWeight.w500),
            ),
          ),
          if (t.count > 1) ...[const SizedBox(width: 6), Pill(count(t.count))],
        ],
      ),
      subtitle: Row(
        children: [
          ...mailMarks(context, t.flags, unread: unread),
          Expanded(child: Text(t.people4, maxLines: 1, overflow: TextOverflow.ellipsis)),
          const SizedBox(width: 8),
          Text(shortWhen(t.last), style: context.text.bodySmall?.copyWith(color: context.bd.muted)),
        ],
      ),
    );
  }
}

/// One conversation: its messages in reading order, replies indented under what they answer, each
/// opening in place. The search's hits (or else the newest message) start open.
class ThreadView extends ConsumerStatefulWidget {
  const ThreadView({super.key, required this.query});
  final ThreadQuery query;

  @override
  ConsumerState<ThreadView> createState() => _ThreadViewState();
}

class _ThreadViewState extends ConsumerState<ThreadView> {
  Set<int>? _expanded;

  Set<int> _first(MailThread d) {
    final hits = [
      for (final m in d.messages)
        if (m.hit) m.key,
    ];
    if (hits.isNotEmpty) return hits.take(5).toSet();
    if (d.messages.isEmpty) return {};
    var newest = d.messages.first;
    for (final m in d.messages) {
      if ((m.date ?? 0) >= (newest.date ?? 0)) newest = m;
    }
    return {newest.key};
  }

  @override
  Widget build(BuildContext context) {
    final t = ref.watch(threadProvider(widget.query));
    return AsyncBody(
      value: t,
      onRetry: () => ref.invalidate(threadProvider(widget.query)),
      data: (d) {
        final open = _expanded ??= _first(d);
        final all = d.messages.isNotEmpty && d.messages.every((m) => open.contains(m.key));
        final th = d.thread;
        final src = widget.query.src;
        return Readable(
          child: ListView.builder(
            padding: const EdgeInsets.only(bottom: 32),
            itemCount: d.messages.length + 1,
            itemBuilder: (c, i) {
              if (i == 0) {
                return Padding(
                  padding: const EdgeInsets.fromLTRB(16, 16, 8, 8),
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      SelectableText(th.subject.isEmpty ? '(no subject)' : th.subject, style: context.text.titleLarge),
                      const SizedBox(height: 4),
                      Row(
                        children: [
                          Expanded(
                            child: Text(
                              [
                                plural(th.count, 'message'),
                                d.who.isEmpty ? '(unknown sender)' : d.who.join(', '),
                                dayRange(th.first, th.last),
                              ].where((s) => s.isNotEmpty).join(' · '),
                              style: context.text.bodySmall?.copyWith(color: context.colors.onSurfaceVariant),
                            ),
                          ),
                          if (d.messages.length > 1)
                            TextButton(
                              onPressed: () =>
                                  setState(() => _expanded = all ? <int>{} : {for (final m in d.messages) m.key}),
                              child: Text(all ? 'Collapse all' : 'Expand all'),
                            ),
                        ],
                      ),
                      if (th.count > d.messages.length)
                        ViewerNote(
                          'Showing the first ${count(d.messages.length)} of ${plural(th.count, 'message')}.',
                          margin: const EdgeInsets.only(top: 8, right: 8),
                        ),
                    ],
                  ),
                );
              }
              final m = d.messages[i - 1];
              return _MessageCard(
                m: m,
                first: i == 1,
                threadSubject: th.subject,
                where: src.message(m.key),
                open: open.contains(m.key),
                onToggle: () => setState(() => open.contains(m.key) ? open.remove(m.key) : open.add(m.key)),
                onWhole: () => context.push(src.isMbox ? mboxMessageLocation(src.id!, m.key) : Routes.view(m.key)),
              );
            },
          ),
        );
      },
    );
  }
}

class _MessageCard extends ConsumerWidget {
  const _MessageCard({
    required this.m,
    required this.first,
    required this.threadSubject,
    required this.where,
    required this.open,
    required this.onToggle,
    required this.onWhole,
  });

  final ThreadMessage m;
  final bool first;
  final String threadSubject;
  final MailAt where;
  final bool open;
  final VoidCallback onToggle, onWhole;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final indent = math.min(m.depth, MediaQuery.sizeOf(context).width < 600 ? 5 : 10) * 14.0;
    final differs = bareSubject(m.subject) != bareSubject(threadSubject) && m.subject.isNotEmpty;
    final trashed = m.flags.contains('T');
    final head = InkWell(
      onTap: onToggle,
      child: Padding(
        padding: const EdgeInsets.fromLTRB(14, 10, 12, 10),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Row(
                    children: [
                      Flexible(
                        child: Text(
                          senderName(m.from),
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: context.text.titleSmall?.copyWith(
                            fontWeight: m.unread ? FontWeight.w800 : FontWeight.w600,
                            color: trashed ? context.bd.muted : null,
                          ),
                        ),
                      ),
                      const SizedBox(width: 6),
                      ...mailMarks(context, m.flags, unread: m.unread),
                      if (m.dup) Pill('copy', color: context.bd.muted),
                    ],
                  ),
                  if (differs)
                    Padding(
                      padding: const EdgeInsets.only(top: 2),
                      child: Text(
                        m.subject,
                        maxLines: open ? 3 : 1,
                        overflow: TextOverflow.ellipsis,
                        style: context.text.bodySmall?.copyWith(color: context.colors.onSurfaceVariant),
                      ),
                    ),
                ],
              ),
            ),
            const SizedBox(width: 8),
            Text(m.date == null ? '' : when(m.date), style: context.text.bodySmall?.copyWith(color: context.bd.muted)),
          ],
        ),
      ),
    );
    return Padding(
      padding: EdgeInsets.fromLTRB(12 + indent, 4, 12, 4),
      child: Card(
        color: m.hit ? context.bd.selection : null,
        clipBehavior: Clip.antiAlias,
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            head,
            if (m.orphan && first)
              Padding(
                padding: const EdgeInsets.fromLTRB(14, 0, 14, 8),
                child: Text(
                  'In reply to a message that isn\'t in this mailbox',
                  style: context.text.bodySmall?.copyWith(color: context.bd.muted, fontStyle: FontStyle.italic),
                ),
              ),
            if (open) ...[const Divider(), _Body(where: where, onWhole: onWhole)],
          ],
        ),
      ),
    );
  }
}

class _Body extends ConsumerWidget {
  const _Body({required this.where, required this.onWhole});
  final MailAt where;
  final VoidCallback onWhole;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final mail = ref.watch(mailProvider(where));
    return switch (mail) {
      AsyncValue(:final value?) => Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: mailParts(context, value, where, inThread: true, openWhole: onWhole),
      ),
      AsyncValue(:final error?) => ErrorPanel(
        error: error,
        compact: true,
        onRetry: () => ref.invalidate(mailProvider(where)),
      ),
      _ => const Padding(
        padding: EdgeInsets.all(20),
        child: Center(child: CircularProgressIndicator()),
      ),
    };
  }
}

/// A conversation as a screen of its own (from the list on a phone).
class ThreadScreen extends ConsumerWidget {
  const ThreadScreen({super.key, required this.query});
  final ThreadQuery query;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final t = ref.watch(threadProvider(query)).value;
    return Scaffold(
      appBar: AppBar(title: Text(t == null ? 'Conversation' : plural(t.thread.count, 'message'))),
      body: ThreadView(query: query),
    );
  }
}

/// A Maildir folder read as a mailbox (from the folder's "Open as mailbox").
class MailboxScreen extends StatefulWidget {
  const MailboxScreen({super.key, required this.dirKey});
  final String dirKey;

  @override
  State<MailboxScreen> createState() => _MailboxScreenState();
}

class _MailboxScreenState extends State<MailboxScreen> {
  String? _name;

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(_name ?? 'Mailbox', maxLines: 1, overflow: TextOverflow.ellipsis),
            Text('Mailbox (Maildir)', style: context.text.bodySmall?.copyWith(color: context.colors.onSurfaceVariant)),
          ],
        ),
      ),
      body: MailboxView(
        source: MailboxSource.maildir(widget.dirKey),
        onName: (n) {
          if (n != _name) setState(() => _name = n);
        },
      ),
    );
  }
}
