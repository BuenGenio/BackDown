import 'package:flutter/material.dart';
import 'package:shared_preferences/shared_preferences.dart';

/// Sets up the background check (finished jobs, drive health) at start.
Future<void> initBackground(SharedPreferences prefs) async {}

/// The notification settings, in App settings.
class NotificationSettingsSection extends StatelessWidget {
  const NotificationSettingsSection({super.key});

  @override
  Widget build(BuildContext context) => const SizedBox.shrink();
}
