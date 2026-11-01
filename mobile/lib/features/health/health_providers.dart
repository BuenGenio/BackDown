import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../api/api.dart';
import '../../api/json.dart';
import '../../providers.dart';
import 'health_model.dart';

/// The service's drive-health counter: it goes up when readings, notifications or what's known of
/// another machine's drives change, so the providers below fetch again only then (health.js
/// daemonChanged). Null for `backdown serve`, which has no such counter.
final _healthChangeProvider = Provider.autoDispose<int?>(
  (ref) => ref.watch(daemonProvider.select((d) => d.value?.health?.change)),
);

/// This computer's drives (GET /api/health).
final healthProvider = FutureProvider.autoDispose<HealthOverview>((ref) async {
  ref.watch(_healthChangeProvider);
  return HealthOverview.fromJson(asJson(await requireApi(ref).get('health')));
});

/// The other machines' drives (GET /api/health-machines). A paid-edition refusal comes back as an
/// [ApiException] with [ApiFailure.needsTier], which the Health tab says only to someone who has
/// added machines.
final machinesHealthProvider = FutureProvider.autoDispose<MachinesHealth>((ref) async {
  ref.watch(_healthChangeProvider);
  return MachinesHealth.fromJson(asJson(await requireApi(ref).get('health-machines')));
});

/// How many other machines there are (GET /api/machines), for whether to mention a refusal.
final machineCountProvider = FutureProvider.autoDispose<int>((ref) async {
  try {
    return asJson(await requireApi(ref).get('machines')).list('machines').length;
  } on ApiException {
    return 0;
  }
});

/// The service's notifications, newest first (GET /api/notifications).
final notificationsProvider = FutureProvider.autoDispose<Notifications>((ref) async {
  ref.watch(_healthChangeProvider);
  return Notifications.fromJson(asJson(await requireApi(ref).get('notifications', {'limit': 100})));
});

/// Which news pops up on the computer's desktop (GET /api/notify-settings).
final notifySettingsProvider = FutureProvider.autoDispose<NotifySettings>(
  (ref) async => NotifySettings.fromJson(asJson(await requireApi(ref).get('notify-settings'))),
);

/// A drive by its key ([DriveHealth.key]): this computer's, or another machine's (`@name:id`).
/// Null when it isn't known (any more).
final driveHealthProvider = FutureProvider.autoDispose.family<DriveHealth?, String>((ref, key) async {
  if (key.startsWith('@')) {
    final m = await ref.watch(machinesHealthProvider.future);
    for (final machine in m.machines) {
      for (final d in machine.drives) {
        if (d.key == key) return d;
      }
    }
    return null;
  }
  final h = await ref.watch(healthProvider.future);
  for (final d in h.drives) {
    if (d.key == key || d.id == key || d.health.device == key) return d;
  }
  return null;
});
