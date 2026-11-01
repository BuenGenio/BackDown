import 'package:go_router/go_router.dart';

import '../../core/placeholder.dart';

/// The Activity tab: the service's queue, its drives' scan schedule, recent jobs.
final activityRoute = GoRoute(path: '/activity', builder: (c, s) => const PlaceholderScreen('Activity'));
