import 'package:go_router/go_router.dart';

import 'machine_file_screen.dart';
import 'machine_find_screen.dart';
import 'machine_folder_screen.dart';
import 'machine_screen.dart';
import 'machines_screen.dart';
import 'pool_plans_screen.dart';
import 'swarm_screen.dart';
import 'where_screen.dart';

int _id(GoRouterState s, String name) => int.tryParse(s.pathParameters[name] ?? '') ?? -1;

String? _q(GoRouterState s, String name) {
  final v = s.uri.queryParameters[name];
  return v == null || v.isEmpty ? null : v;
}

/// Under More: the swarm and the other machines (relative to /more). The paths below them are in
/// MachinePaths (providers.dart).
final machineRoutes = <RouteBase>[
  GoRoute(
    path: 'swarm',
    builder: (c, s) => const SwarmScreen(),
    routes: [
      GoRoute(
        path: 'where',
        builder: (c, s) => WhereScreen(q: _q(s, 'q')),
      ),
      GoRoute(
        path: 'plans',
        builder: (c, s) => const PoolPlansScreen(),
        routes: [
          GoRoute(
            path: ':plan',
            builder: (c, s) => PoolPlanScreen(id: _id(s, 'plan')),
          ),
        ],
      ),
    ],
  ),
  GoRoute(
    path: 'machines',
    builder: (c, s) => const MachinesScreen(),
    routes: [
      GoRoute(
        path: ':id',
        builder: (c, s) => MachineScreen(id: _id(s, 'id')),
        routes: [
          GoRoute(
            path: 'ls',
            builder: (c, s) => MachineFolderScreen(machineId: _id(s, 'id'), dir: _q(s, 'dir'), select: _q(s, 'select')),
          ),
          GoRoute(
            path: 'find',
            builder: (c, s) => MachineFindScreen(machineId: _id(s, 'id'), dir: _q(s, 'dir')),
          ),
          GoRoute(
            path: 'file/:rid',
            builder: (c, s) => MachineFileScreen(machineId: _id(s, 'id'), rid: _id(s, 'rid')),
          ),
        ],
      ),
    ],
  ),
];
