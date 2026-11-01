import 'package:flutter/widgets.dart';
import 'package:go_router/go_router.dart';

import 'common.dart';
import 'disk_view.dart';
import 'inner_file.dart';
import 'mail_view.dart';
import 'mailbox_view.dart';
import 'models.dart';
import 'open.dart';
import 'sqldump_view.dart';
import 'sqlite_view.dart';
import 'viewer_screen.dart';

/// The viewers, full screen over the tabs: /view/:id (with the folder's other files as `extra`, a
/// ViewerArgs, to step through), and what opens from inside a file (an archive's entry, a disk
/// image's filesystem, a table, a conversation). A Maildir folder reads as a mailbox at
/// /mailbox/:key.
List<RouteBase> viewerRoutes(GlobalKey<NavigatorState> rootKey) => [
  GoRoute(
    path: '/view/:id',
    parentNavigatorKey: rootKey,
    builder: (c, s) => ViewerScreen(id: _id(s), args: s.extra is ViewerArgs ? s.extra! as ViewerArgs : null),
    routes: [
      GoRoute(
        path: 'entry',
        builder: (c, s) {
          final q = s.uri.queryParameters;
          return InnerFileScreen(
            file: InnerFile.entry(
              id: _id(s),
              name: q['name'] ?? '',
              part: q['part'],
              size: int.tryParse(q['size'] ?? ''),
              streams: q['streams'] == '1',
            ),
            note: s.extra is String ? s.extra! as String : null,
          );
        },
      ),
      GoRoute(
        path: 'attachment',
        builder: (c, s) {
          final q = s.uri.queryParameters;
          return InnerFileScreen(
            file: InnerFile.attachment(
              id: _id(s),
              name: q['name'] ?? 'attachment',
              attachment: int.tryParse(q['part'] ?? '') ?? 0,
              at: int.tryParse(q['at'] ?? ''),
              size: int.tryParse(q['size'] ?? ''),
            ),
          );
        },
      ),
      GoRoute(
        path: 'fs',
        builder: (c, s) => FsBrowseScreen(
          id: _id(s),
          part: s.uri.queryParameters['part'] ?? '0',
          rootName: s.uri.queryParameters['root'] ?? 'Files',
        ),
      ),
      GoRoute(
        path: 'table',
        builder: (c, s) => SqliteTableScreen(id: _id(s), table: s.uri.queryParameters['name'] ?? ''),
      ),
      GoRoute(
        path: 'sql-table',
        builder: (c, s) => SqlTableScreen(
          id: _id(s),
          tableKey: s.uri.queryParameters['key'] ?? '',
          dump: s.extra is SqlDump ? s.extra! as SqlDump : null,
        ),
      ),
      GoRoute(
        path: 'message',
        builder: (c, s) => MailScreen(id: _id(s), at: int.tryParse(s.uri.queryParameters['at'] ?? '')),
      ),
      GoRoute(
        path: 'thread',
        builder: (c, s) => ThreadScreen(query: _thread(MailboxSource.mbox(_id(s)), s)),
      ),
    ],
  ),
  GoRoute(
    path: '/mailbox/:key',
    parentNavigatorKey: rootKey,
    builder: (c, s) => MailboxScreen(dirKey: s.pathParameters['key']!),
    routes: [
      GoRoute(
        path: 'thread',
        builder: (c, s) => ThreadScreen(query: _thread(MailboxSource.maildir(s.pathParameters['key']!), s)),
      ),
    ],
  ),
];

int _id(GoRouterState s) => int.tryParse(s.pathParameters['id'] ?? '') ?? 0;

ThreadQuery _thread(MailboxSource src, GoRouterState s) {
  final q = s.uri.queryParameters;
  return (src: src, key: int.tryParse(q['key'] ?? '') ?? 0, q: q['q'] ?? '', flat: q['flat'] == '1');
}

/// A Maildir folder (by its key) read as a mailbox: its mail as threaded conversations.
String mailboxLocation(String dirKey) => '/mailbox/$dirKey';

/// A file inside a container, in the viewer that suits it.
String innerLocation(InnerFile f) => Uri(
  path: '/view/${f.id}/${f.endpoint}',
  queryParameters: {
    'name': f.name,
    'part': ?(f.endpoint == 'entry' ? f.part : f.attachment?.toString()),
    'at': ?f.at?.toString(),
    'size': ?f.size?.toString(),
    'streams': ?(f.streams ? '1' : null),
  },
).toString();

/// A filesystem inside a disk image ("0": the whole image's; "iso": the disc's own files).
String fsLocation(int id, String part, String rootName) =>
    Uri(path: '/view/$id/fs', queryParameters: {'part': part, 'root': rootName}).toString();

String sqliteTableLocation(int id, String table) =>
    Uri(path: '/view/$id/table', queryParameters: {'name': table}).toString();

String sqlTableLocation(int id, String key) =>
    Uri(path: '/view/$id/sql-table', queryParameters: {'key': key}).toString();

/// A conversation of a mailbox.
String threadLocation(ThreadQuery t) => Uri(
  path: t.src.isMbox ? '/view/${t.src.id}/thread' : '/mailbox/${t.src.dir}/thread',
  queryParameters: {'key': '${t.key}', 'q': ?(t.q.isEmpty ? null : t.q), 'flat': ?(t.flat ? '1' : null)},
).toString();

/// One message of an mbox, on its own.
String mboxMessageLocation(int id, int at) => '/view/$id/message?at=$at';
