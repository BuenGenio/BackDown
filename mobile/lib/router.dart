import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import 'features/activity/routes.dart';
import 'features/app_settings/app_settings_screen.dart';
import 'features/app_settings/servers_screen.dart';
import 'features/connect/connect_screen.dart';
import 'features/connect/link_screen.dart';
import 'features/connect/scan_screen.dart';
import 'features/files/drives_screen.dart';
import 'features/files/file_screen.dart';
import 'features/files/folder_screen.dart';
import 'features/health/routes.dart';
import 'features/library/routes.dart';
import 'features/machines/routes.dart';
import 'features/more/more_screen.dart';
import 'features/search/search_screen.dart';
import 'features/viewers/routes.dart';
import 'providers.dart';
import 'shell.dart';

/// Paths of every screen. Navigate with `context.push(Routes.folder(key))`. Each area of the app
/// mounts its own screens (features/*/routes.dart) under the paths listed here.
abstract final class Routes {
  static const connect = '/connect';

  /// Paste the link, or type the address and token. With `?server=ID`, a new token for that one.
  static const connectLink = '/connect/link';
  static const connectScan = '/connect/scan';

  static const drives = '/drives';
  static String folder(String key, {int? scan, String? select}) =>
      Uri(path: '/drives/folder/$key', queryParameters: {'scan': ?scan?.toString(), 'select': ?select}).toString();

  /// A file's details: where it is, its copies and versions, open or share it.
  static String file(int id) => '/file/$id';

  /// The file itself, in the viewer that suits it.
  static String view(int id) => '/view/$id';

  static const search = '/search';
  static const activity = '/activity';
  static const health = '/health';

  static const more = '/more';
  static const entities = '/more/entities';
  static const dupes = '/more/dupes';
  static const swarm = '/more/swarm';
  static const machines = '/more/machines';
  static String machine(int id) => '/more/machines/$id';
  static const plans = '/more/plans';
  static String plan(int id) => '/more/plans/$id';
  static const freeUp = '/more/free-up';
  static const rescues = '/more/rescues';
  static const mirrors = '/more/mirrors';
  static const manifests = '/more/manifests';

  /// The service: its version, updates, editions and licence, scan settings.
  static const serverInfo = '/more/server';
  static const appSettings = '/more/settings';
  static const servers = '/more/servers';
}

final _rootKey = GlobalKey<NavigatorState>(debugLabel: 'root');

final routerProvider = Provider<GoRouter>((ref) {
  final hasServer = ValueNotifier<bool>(ref.read(currentServerProvider) != null);
  ref.listen(currentServerProvider, (_, next) => hasServer.value = next != null);

  final router = GoRouter(
    navigatorKey: _rootKey,
    initialLocation: Routes.drives,
    refreshListenable: hasServer,
    debugLogDiagnostics: kDebugMode && false,
    redirect: (context, state) {
      final connecting = state.matchedLocation.startsWith(Routes.connect);
      if (!hasServer.value && !connecting) return Routes.connect;
      return null;
    },
    routes: [
      GoRoute(
        path: Routes.connect,
        builder: (c, s) => const ConnectScreen(),
        routes: [
          GoRoute(
            path: 'link',
            builder: (c, s) =>
                LinkScreen(serverId: s.uri.queryParameters['server'], initial: s.uri.queryParameters['link']),
          ),
          GoRoute(
            path: 'scan',
            builder: (c, s) => ScanScreen(serverId: s.uri.queryParameters['server']),
          ),
        ],
      ),
      GoRoute(
        path: '/file/:id',
        parentNavigatorKey: _rootKey,
        builder: (c, s) => FileScreen(id: int.parse(s.pathParameters['id']!)),
      ),
      ...viewerRoutes(_rootKey),
      StatefulShellRoute.indexedStack(
        builder: (context, state, shell) => AppShell(shell: shell),
        branches: [
          StatefulShellBranch(
            routes: [
              GoRoute(
                path: Routes.drives,
                builder: (c, s) => const DrivesScreen(),
                routes: [
                  GoRoute(
                    path: 'folder/:key',
                    builder: (c, s) => FolderScreen(
                      dirKey: s.pathParameters['key']!,
                      scan: int.tryParse(s.uri.queryParameters['scan'] ?? ''),
                      select: s.uri.queryParameters['select'],
                    ),
                  ),
                ],
              ),
            ],
          ),
          StatefulShellBranch(
            routes: [
              GoRoute(
                path: Routes.search,
                builder: (c, s) => SearchScreen(dir: s.uri.queryParameters['dir']),
              ),
            ],
          ),
          StatefulShellBranch(routes: [activityRoute]),
          StatefulShellBranch(routes: [healthRoute]),
          StatefulShellBranch(
            routes: [
              GoRoute(
                path: Routes.more,
                builder: (c, s) => const MoreScreen(),
                routes: [
                  GoRoute(path: 'settings', builder: (c, s) => const AppSettingsScreen()),
                  GoRoute(path: 'servers', builder: (c, s) => const ServersScreen()),
                  ...machineRoutes,
                  ...libraryRoutes,
                ],
              ),
            ],
          ),
        ],
      ),
    ],
  );
  ref.onDispose(() {
    hasServer.dispose();
    router.dispose();
  });
  return router;
});
