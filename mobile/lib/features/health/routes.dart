import 'package:go_router/go_router.dart';

import 'drive_screen.dart';
import 'health_screen.dart';

/// A drive's health in full, by its key (`sn:…`, a device, or `@machine:id` for another machine's).
String healthDrivePath(String key) => '/health/drive/${Uri.encodeComponent(key)}';

/// The Health tab: drive health here and on other machines, and notifications.
final healthRoute = GoRoute(
  path: '/health',
  builder: (c, s) => const HealthScreen(),
  routes: [
    GoRoute(
      path: 'drive/:key',
      builder: (c, s) => DriveScreen(driveKey: s.pathParameters['key']!),
    ),
  ],
);
