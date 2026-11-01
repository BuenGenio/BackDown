import 'package:go_router/go_router.dart';

import '../../core/placeholder.dart';

/// The Health tab: drive health here and on other machines, and notifications.
final healthRoute = GoRoute(path: '/health', builder: (c, s) => const PlaceholderScreen('Health'));
