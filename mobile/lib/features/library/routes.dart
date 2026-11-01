import 'package:go_router/go_router.dart';

import 'dupes_screen.dart';
import 'entities_screen.dart';
import 'free_up_screen.dart';
import 'mirrors_screen.dart';
import 'plans_screen.dart';
import 'rescues_screen.dart';
import 'server_screen.dart';

/// The report, under This BackDown.
const reportRoute = '/more/server/report';

/// Under More: what's here, duplicates, plans, freeing up, rescues, mirrors, manifests, and the
/// service's own settings and version (relative to /more).
final libraryRoutes = <RouteBase>[
  GoRoute(path: 'entities', builder: (c, s) => const EntitiesScreen()),
  GoRoute(path: 'dupes', builder: (c, s) => const DupesScreen()),
  GoRoute(
    path: 'plans',
    builder: (c, s) => const PlansScreen(),
    routes: [
      GoRoute(
        path: ':id',
        builder: (c, s) => PlanScreen(id: int.tryParse(s.pathParameters['id'] ?? '') ?? 0),
      ),
    ],
  ),
  GoRoute(path: 'free-up', builder: (c, s) => const FreeUpScreen()),
  GoRoute(path: 'rescues', builder: (c, s) => const RescuesScreen()),
  GoRoute(path: 'mirrors', builder: (c, s) => const MirrorsScreen()),
  GoRoute(path: 'manifests', builder: (c, s) => const ManifestsScreen()),
  GoRoute(
    path: 'server',
    builder: (c, s) => const ServerScreen(),
    routes: [GoRoute(path: 'report', builder: (c, s) => const ReportScreen())],
  ),
];
