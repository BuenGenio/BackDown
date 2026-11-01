import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import 'providers.dart';
import 'theme.dart';

/// The five places: Drives, Search, Activity, Health, More. A bar at the bottom on a phone, a rail
/// at the side on a tablet or desktop. Activity shows a dot while a job runs; Health shows unread
/// notifications, or a dot when a drive needs watching.
class AppShell extends ConsumerWidget {
  const AppShell({super.key, required this.shell});

  final StatefulNavigationShell shell;

  void _go(int i) => shell.goBranch(i, initialLocation: i == shell.currentIndex);

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final daemon = ref.watch(daemonProvider).value;
    final busy = daemon?.busy ?? false;
    final unread = daemon?.health?.unread ?? 0;
    final worst = daemon?.health?.worst ?? 'unknown';
    final healthDot = worst == 'watch' || worst == 'warn' || worst == 'fail';

    Widget badged(IconData icon, {bool dot = false, int count = 0, Color? color}) {
      if (count > 0) return Badge(label: Text('$count'), child: Icon(icon));
      if (dot) return Badge(smallSize: 8, backgroundColor: color, child: Icon(icon));
      return Icon(icon);
    }

    final items = [
      (Icons.storage_outlined, Icons.storage, 'Drives', false, 0, null),
      (Icons.search, Icons.search, 'Search', false, 0, null),
      (Icons.sync_outlined, Icons.sync, 'Activity', busy, 0, context.bd.info),
      (
        Icons.health_and_safety_outlined,
        Icons.health_and_safety,
        'Health',
        healthDot,
        unread,
        context.bd.forVerdict(worst),
      ),
      (Icons.more_horiz, Icons.more_horiz, 'More', false, 0, null),
    ];

    final wide = MediaQuery.sizeOf(context).width >= 840;
    if (wide) {
      return Scaffold(
        body: Row(
          children: [
            NavigationRail(
              selectedIndex: shell.currentIndex,
              onDestinationSelected: _go,
              labelType: NavigationRailLabelType.all,
              backgroundColor: context.colors.surfaceContainer,
              indicatorColor: context.colors.primaryContainer,
              leading: const Padding(
                padding: EdgeInsets.symmetric(vertical: 16),
                child: Wordmark(size: 16, withMark: false),
              ),
              destinations: [
                for (final (icon, selected, label, dot, n, color) in items)
                  NavigationRailDestination(
                    icon: badged(icon, dot: dot, count: n, color: color),
                    selectedIcon: badged(selected, dot: dot, count: n, color: color),
                    label: Text(label),
                  ),
              ],
            ),
            const VerticalDivider(width: 1),
            Expanded(child: shell),
          ],
        ),
      );
    }
    return Scaffold(
      body: shell,
      bottomNavigationBar: NavigationBar(
        selectedIndex: shell.currentIndex,
        onDestinationSelected: _go,
        destinations: [
          for (final (icon, selected, label, dot, n, color) in items)
            NavigationDestination(
              icon: badged(icon, dot: dot, count: n, color: color),
              selectedIcon: badged(selected, dot: dot, count: n, color: color),
              label: label,
            ),
        ],
      ),
    );
  }
}
