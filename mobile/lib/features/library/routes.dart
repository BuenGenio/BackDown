import 'package:go_router/go_router.dart';

import '../../core/placeholder.dart';

/// Under More: what's here, duplicates, plans, freeing up, rescues, mirrors, manifests, and the
/// service's own settings and version (relative to /more).
final libraryRoutes = <RouteBase>[
  GoRoute(path: 'entities', builder: (c, s) => const PlaceholderScreen("What's here")),
  GoRoute(path: 'dupes', builder: (c, s) => const PlaceholderScreen('Duplicates')),
  GoRoute(path: 'plans', builder: (c, s) => const PlaceholderScreen('Plans')),
  GoRoute(path: 'free-up', builder: (c, s) => const PlaceholderScreen('Free up')),
  GoRoute(path: 'rescues', builder: (c, s) => const PlaceholderScreen('Rescues')),
  GoRoute(path: 'mirrors', builder: (c, s) => const PlaceholderScreen('Mirrors')),
  GoRoute(path: 'manifests', builder: (c, s) => const PlaceholderScreen('Manifests')),
  GoRoute(path: 'server', builder: (c, s) => const PlaceholderScreen('This BackDown')),
];
