import 'package:clock/clock.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';
import 'package:intl/intl.dart';

import '../../core/format.dart';
import '../../core/kinds.dart';
import '../../core/widgets.dart';
import '../../providers.dart';
import '../../theme.dart';
import 'common.dart';
import 'image_view.dart';
import 'models.dart';
import 'routes.dart';

/// A message: by its file's index id (an .eml, a Maildir message), or `at` its offset in an mbox.
typedef MailAt = (int id, int? at);

final mailProvider = FutureProvider.autoDispose.family<MailMessage, MailAt>(
  (ref, k) => fetchParsed(requireApi(ref), 'mail', {'id': k.$1, 'at': k.$2}, MailMessage.fromJson),
);

/// `Ann Example <ann@example.org>` → `Ann Example`.
String senderName(String from) {
  final m = RegExp(r'^\s*"?([^"<]*?)"?\s*<[^>]*>\s*$').firstMatch(from);
  final n = m?.group(1)?.trim();
  return n != null && n.isNotEmpty ? n : (from.trim().isEmpty ? '(unknown sender)' : from.trim());
}

/// A subject without "Re:", "AW:", "Fwd:"…, to tell whether a reply's subject says something new.
String bareSubject(String s) => s
    .replaceFirst(
      RegExp(
        r'^\s*((re|aw|sv|fwd?|wg|tr|rv|vs|antw|odp|rif|r|i)\s*(\[\d+\]|\(\d+\))?\s*[:：]\s*)+',
        caseSensitive: false,
      ),
      '',
    )
    .trim();

/// A list's date: the time today, the day this year, else the whole date.
String shortWhen(int? t) {
  if (t == null || t == 0) return '';
  final d = DateTime.fromMillisecondsSinceEpoch(t * 1000);
  final now = clock.now();
  if (d.year == now.year && d.month == now.month && d.day == now.day) return DateFormat.Hm().format(d);
  return d.year == now.year ? DateFormat.MMMd().format(d) : DateFormat.yMMMd().format(d);
}

/// "3 Jan 2012 – 9 Feb 2012", or one day.
String dayRange(int? a, int? b) {
  if ((a ?? 0) == 0 && (b ?? 0) == 0) return '';
  final x = day(a ?? b), y = day(b ?? a);
  return x == y ? x : '$x – $y';
}

/// Maildir flags (or an mbox's status headers) as small markers.
List<Widget> mailMarks(BuildContext context, String flags, {bool unread = false}) => [
  if (unread) Padding(padding: const EdgeInsets.only(right: 6), child: StatusDot(context.colors.primary, size: 8)),
  if (flags.contains('F')) _mark(context, Icons.flag, 'Flagged', context.bd.crit),
  if (flags.contains('R')) _mark(context, Icons.reply, 'Replied', context.bd.info),
  if (flags.contains('P')) _mark(context, Icons.forward, 'Forwarded', context.bd.info),
  if (flags.contains('D'))
    Padding(
      padding: const EdgeInsets.only(right: 4),
      child: Pill('draft', color: context.bd.warn),
    ),
  if (flags.contains('T'))
    Padding(
      padding: const EdgeInsets.only(right: 4),
      child: Pill('trashed', color: context.bd.muted),
    ),
];

Widget _mark(BuildContext context, IconData icon, String label, Color color) => Padding(
  padding: const EdgeInsets.only(right: 4),
  child: Tooltip(
    message: label,
    child: Icon(icon, size: 14, color: color, semanticLabel: label),
  ),
);

/// Where a message's attachments are fetched from: the message's own file, or the mbox and the
/// message's offset in it.
InnerFile attachmentOf(MailAt m, MailAttachment a) =>
    InnerFile.attachment(id: m.$1, at: m.$2, attachment: a.part, name: a.name, size: a.size);

/// The longest body shown inside a conversation; the whole message opens on its own.
const _bodyInThread = 20000;

/// A message's headers, attachments and text, as widgets for a list. [inThread]: one message of a
/// conversation (the subject is the thread's, a very long body is cut).
List<Widget> mailParts(
  BuildContext context,
  MailMessage m,
  MailAt where, {
  bool inThread = false,
  VoidCallback? openWhole,
}) {
  final api = ProviderScope.containerOf(context).read(apiProvider);
  final rows = [
    ('From', m.from),
    ('To', m.to),
    ('Cc', m.cc),
    ('Reply-To', m.replyTo),
    ('Date', m.date != null ? when(m.date) : (m.dateText ?? '')),
  ].where((r) => r.$2.isNotEmpty);
  final pics = [
    for (final a in m.attachments)
      if (isDecodableImage(a.name) && a.size <= (25 << 20)) a,
  ];
  final cut = inThread && m.text.length > _bodyInThread;
  final body = cut ? '${m.text.substring(0, _bodyInThread)}…' : m.text;
  final muted = context.text.bodySmall?.copyWith(color: context.bd.muted);
  return [
    if (!inThread)
      Padding(
        padding: const EdgeInsets.fromLTRB(16, 16, 16, 6),
        child: SelectableText(m.subject.isEmpty ? '(no subject)' : m.subject, style: context.text.titleLarge),
      ),
    for (final (k, v) in rows) InfoRow(k, v, copyable: true),
    if (m.attachments.isNotEmpty) ...[
      SectionHeader('Attachments', trailing: Pill('${m.attachments.length}', color: context.bd.info)),
      if (pics.isNotEmpty && api != null)
        SizedBox(
          height: 96,
          child: ListView.separated(
            scrollDirection: Axis.horizontal,
            padding: const EdgeInsets.symmetric(horizontal: 16),
            itemCount: pics.length,
            separatorBuilder: (c, i) => const SizedBox(width: 8),
            itemBuilder: (c, i) {
              final f = attachmentOf(where, pics[i]);
              return InkWell(
                borderRadius: BorderRadius.circular(10),
                onTap: () => context.push(innerLocation(f)),
                child: ClipRRect(
                  borderRadius: BorderRadius.circular(10),
                  child: Container(
                    width: 96,
                    color: context.colors.surfaceContainerHigh,
                    child: Image(
                      image: ResizeImage(InnerImage(api, f), width: 256, policy: ResizeImagePolicy.fit),
                      fit: BoxFit.cover,
                      errorBuilder: (c, e, s) => Center(child: FileGlyph(name: f.name, size: 32)),
                    ),
                  ),
                ),
              );
            },
          ),
        ),
      for (final a in m.attachments)
        ListTile(
          dense: true,
          leading: FileGlyph(name: a.name),
          title: Text(a.name, maxLines: 2, overflow: TextOverflow.ellipsis),
          subtitle: Text([?a.type, bytes(a.size)].join(' · ')),
          trailing: const Icon(Icons.chevron_right),
          onTap: () => context.push(innerLocation(attachmentOf(where, a))),
        ),
      const SizedBox(height: 4),
    ],
    if (m.fromHtml)
      const ViewerNote(
        'This message only has an HTML version, shown here as plain text: nothing in it is loaded or run.',
      ),
    if (m.truncated) const ViewerNote('A very large message: only its beginning is shown.'),
    Padding(
      padding: const EdgeInsets.fromLTRB(16, 14, 16, 8),
      child: body.trim().isEmpty
          ? Text('(no text)', style: muted)
          : SelectableText(body, style: context.text.bodyMedium?.copyWith(height: 1.45)),
    ),
    if (cut && openWhole != null)
      Padding(
        padding: const EdgeInsets.fromLTRB(8, 0, 8, 8),
        child: Align(
          alignment: Alignment.centerLeft,
          child: TextButton.icon(
            onPressed: openWhole,
            icon: const Icon(Icons.open_in_full),
            label: const Text('Read the whole message'),
          ),
        ),
      ),
  ];
}

/// An .eml (or a Maildir message, or one message of an mbox) on its own.
class MailView extends ConsumerWidget {
  const MailView({super.key, required this.at, this.showBar = true});
  final MailAt at;
  final bool showBar;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final m = ref.watch(mailProvider(at));
    return AsyncBody(
      value: m,
      onRetry: () => ref.invalidate(mailProvider(at)),
      data: (m) => Column(
        children: [
          if (showBar)
            ViewerBar(
              meta: m.attachments.isEmpty
                  ? 'Mail message'
                  : 'Mail message · ${plural(m.attachments.length, 'attachment')}',
            ),
          Expanded(
            child: Readable(
              child: ListView(padding: const EdgeInsets.only(bottom: 32), children: mailParts(context, m, at)),
            ),
          ),
        ],
      ),
    );
  }
}

/// One message of an mbox as a screen (from a conversation, when it's too long to show there).
class MailScreen extends ConsumerWidget {
  const MailScreen({super.key, required this.id, this.at});
  final int id;
  final int? at;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final m = ref.watch(mailProvider((id, at))).value;
    return Scaffold(
      appBar: AppBar(title: Text(m == null ? 'Message' : senderName(m.from))),
      body: MailView(at: (id, at), showBar: false),
    );
  }
}
