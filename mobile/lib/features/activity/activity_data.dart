import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../api/api.dart';
import '../../api/json.dart';
import '../../api/keys.dart';
import '../../models/browse.dart';
import '../../models/daemon.dart';
import '../../providers.dart';
import 'job_text.dart';

/// A folder kept copied to another drive (GET /api/mirrors), as Activity sums it up.
class MirrorInfo {
  MirrorInfo.fromJson(Json j)
    : id = j.integer('id'),
      source = j.str('source'),
      target = j.str('target'),
      lastSync = j.intOrNull('last_sync'),
      lastVerify = j.intOrNull('last_verify'),
      lastVerifySummary = j.strOrNull('last_verify_summary'),
      lastVerifyOk = j.flagOrNull('last_verify_ok'),
      connected = j.flag('connected');

  final int id;
  final String source, target;
  final int? lastSync, lastVerify;
  final String? lastVerifySummary;

  /// The last check found every sampled file intact (null: not checked yet).
  final bool? lastVerifyOk;

  /// Both drives are connected to the computer.
  final bool connected;
}

/// The mirrors, or none when this service has none to say (an older one, or a locked edition).
final mirrorsProvider = FutureProvider.autoDispose<List<MirrorInfo>>((ref) async {
  try {
    return [for (final m in asJsonList(await requireApi(ref).get('mirrors'))) MirrorInfo.fromJson(m)];
  } on ApiException {
    return const [];
  }
});

/// How rescans are made, for Activity's footer (GET /api/scan-settings), or null.
final scanWaysProvider = FutureProvider.autoDispose<Json?>((ref) async {
  try {
    return asJson(await requireApi(ref).get('scan-settings'));
  } on ApiException {
    return null;
  }
});

const _rescueTitle = {
  'image': 'Copy',
  'carve': 'Find files',
  'parts': 'Look for partitions',
  'files': 'Copy files out',
};

/// What each rescue is, by its job's path (`rescue 7` → "Copy: sdb"), from GET /api/rescues.
final rescueNamesProvider = FutureProvider.autoDispose<Map<String, String>>((ref) async {
  try {
    return {
      for (final r in asJsonList(await requireApi(ref).get('rescues')))
        'rescue ${r.integer('id')}': '${_rescueTitle[r.str('mode')] ?? 'Rescue'}: ${baseName(r.str('source'))}',
    };
  } on ApiException {
    return const {};
  }
});

/// Names jobs as the web UI does: a drive by the index's name for it, a rescue by what it does.
class JobNamer {
  JobNamer(this.roots, this.rescues);

  /// Watches what naming needs: the drives, and the rescues when a rescue is in [s].
  factory JobNamer.watch(WidgetRef ref, DaemonSnapshot? s) {
    final roots = ref.watch(rootsProvider).value ?? const <Root>[];
    final jobs = [?s?.current, ?s?.side, ...?s?.queued, ...?s?.recent];
    final rescues = jobs.any((j) => j.kind == 'rescue')
        ? ref.watch(rescueNamesProvider).value ?? const <String, String>{}
        : const <String, String>{};
    return JobNamer({for (final r in roots) r.path: r}, rescues);
  }

  final Map<String, Root> roots;
  final Map<String, String> rescues;

  String driveName(String path) {
    final n = roots[path]?.name ?? '';
    return n.isEmpty ? baseName(path) : n;
  }

  Root? rootOf(String? path) => path == null ? null : roots[path];

  String title(Job j) => jobTitle(j, driveName: driveName, rescueName: rescues[j.path]);
}

/// A job by its id in the latest snapshot, wherever it is now (running, waiting or finished).
Job? findJob(DaemonSnapshot? s, int id) {
  if (s == null) return null;
  for (final j in [?s.current, ?s.side, ...s.queued, ...s.recent]) {
    if (j.id == id) return j;
  }
  return null;
}
