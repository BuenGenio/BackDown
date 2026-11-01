import 'package:flutter/widgets.dart';
import 'package:go_router/go_router.dart';

import '../../core/placeholder.dart';

/// The viewers, full screen over the tabs: /view/:id (with the folder's other files as `extra`, a
/// ViewerArgs, to step through).
List<RouteBase> viewerRoutes(GlobalKey<NavigatorState> rootKey) => [
  GoRoute(path: '/view/:id', parentNavigatorKey: rootKey, builder: (c, s) => const PlaceholderScreen('Viewer')),
];
