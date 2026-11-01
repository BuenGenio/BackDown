import 'package:go_router/go_router.dart';

import '../../core/placeholder.dart';

/// Under More: the swarm and the other machines (relative to /more).
final machineRoutes = <RouteBase>[
  GoRoute(path: 'swarm', builder: (c, s) => const PlaceholderScreen('Swarm')),
  GoRoute(path: 'machines', builder: (c, s) => const PlaceholderScreen('Machines')),
];
