import 'package:go_router/go_router.dart';

import 'activity_screen.dart';

/// The Activity tab: the service's queue, its drives' scan schedule, recent jobs.
final activityRoute = GoRoute(path: '/activity', builder: (c, s) => const ActivityScreen());
