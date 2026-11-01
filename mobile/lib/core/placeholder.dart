import 'package:flutter/material.dart';

import 'widgets.dart';

/// A screen that isn't built yet.
class PlaceholderScreen extends StatelessWidget {
  const PlaceholderScreen(this.title, {super.key});
  final String title;

  @override
  Widget build(BuildContext context) => Scaffold(
    appBar: AppBar(title: Text(title)),
    body: const EmptyState(icon: Icons.construction_outlined, title: 'Coming soon'),
  );
}
