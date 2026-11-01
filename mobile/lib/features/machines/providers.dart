import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../api/api.dart';
import '../../api/json.dart';
import '../../models/browse.dart';
import '../../models/daemon.dart';
import '../../providers.dart';
import 'models.dart';

/// What asks another machine over ssh can take a while: the connection, then its own index.
const askTimeout = Duration(seconds: 90);

/// The paths of this area's screens, under the ones in Routes (Routes.machines, Routes.machine(id),
/// Routes.swarm).
abstract final class MachinePaths {
  /// A folder on machine [m], live or as last synced; without [dir], its drives.
  static String ls(int m, {String? dir, String? select}) =>
      Uri(path: '/more/machines/$m/ls', queryParameters: {'dir': ?dir, 'select': ?select}).toString();

  /// A file's details on machine [m], by its id in that machine's index.
  static String file(int m, int rid) => '/more/machines/$m/file/$rid';

  /// A name searched on machine [m] (under [dir], a folder there).
  static String find(int m, {String? dir}) =>
      Uri(path: '/more/machines/$m/find', queryParameters: {'dir': ?dir}).toString();

  /// Where is it: a name on every machine.
  static String where([String? q]) => Uri(path: '/more/swarm/where', queryParameters: {'q': ?q}).toString();

  /// The pool's plans made on this machine, and one of them.
  static const plans = '/more/swarm/plans';
  static String plan(int id) => '/more/swarm/plans/$id';
}

final machinesProvider = FutureProvider.autoDispose<MachinesInfo>(
  (ref) async => MachinesInfo.fromJson(asJson(await requireApi(ref).get('machines'))),
);

typedef MachineDir = ({int m, String? dir});

final machineLsProvider = FutureProvider.autoDispose.family<MachineListing, MachineDir>((ref, at) async {
  final v = await requireApi(ref).get('machine-ls', {'m': at.m, 'dir': at.dir}, askTimeout);
  return MachineListing.fromJson(asJson(v));
});

typedef MachineFileAt = ({int m, int id});

final machineFileProvider = FutureProvider.autoDispose.family<MachineFile, MachineFileAt>((ref, at) async {
  final v = await requireApi(ref).get('machine-file', {'m': at.m, 'id': at.id}, askTimeout);
  return MachineFile.fromJson(asJson(v));
});

typedef MachineFindQuery = ({int m, String q, String? dir});

final machineFindProvider = FutureProvider.autoDispose.family<FindResult, MachineFindQuery>((ref, f) async {
  final v = await requireApi(ref).get('machine-find', {'m': f.m, 'q': f.q, 'dir': f.dir}, askTimeout);
  return FindResult.fromJson(asJson(v));
});

final swarmProvider = FutureProvider.autoDispose<SwarmOverview>(
  (ref) async => SwarmOverview.fromJson(asJson(await requireApi(ref).get('swarm'))),
);

/// Another machine asked about its drives now (over ssh); what it said, or as kept with why not.
final swarmAskProvider = FutureProvider.autoDispose.family<SwarmMachine, int>((ref, id) async {
  final v = await requireApi(ref).get('swarm-machine', {'m': id}, askTimeout);
  return SwarmMachine.fromJson(asJson(v));
});

final whereProvider = FutureProvider.autoDispose.family<WhereResult, String>((ref, q) async {
  final v = await requireApi(ref).get('swarm-where', {'q': q, 'limit': 200}, const Duration(seconds: 120));
  return WhereResult.fromJson(asJson(v));
});

final poolPlansProvider = FutureProvider.autoDispose<List<PoolPlanBrief>>((ref) async {
  final j = asJson(await requireApi(ref).get('pool-plans'));
  return [for (final p in j.list('plans')) PoolPlanBrief.fromJson(p)];
});

final poolPlanProvider = FutureProvider.autoDispose.family<PoolPlan, int>(
  (ref, id) async => PoolPlan.fromJson(asJson(await requireApi(ref).get('pool-plan', {'id': id}))),
);

/// Whether [e] says the service is older than the endpoint asked for (it has no such endpoint).
bool isOlderService(Object? e) =>
    e is ApiException &&
    (e.failure == ApiFailure.notFound || e.failure == ApiFailure.refused) &&
    e.message.toLowerCase().contains('no such endpoint');

/// The machines' syncs in the service's queue, in brief: when it changes, a sync started or
/// ended, so the machines are read again (machines.js's machinesDaemon).
String? syncsKey(DaemonSnapshot? d) {
  if (d == null) return null;
  bool sync(Job j) => j.kind == 'remote-sync';
  return [
    if (d.side case final s? when sync(s)) '${s.path}:${s.state}:${s.progress}',
    if (d.current case final c? when sync(c)) '${c.path}:${c.state}:${c.progress}',
    for (final j in d.queued.where(sync)) 'q${j.id}',
    for (final j in d.recent.where(sync)) 'r${j.id}',
  ].join('|');
}

/// Reads the machines again whenever a sync starts or ends.
void refreshOnSyncs(WidgetRef ref) {
  ref.listen(daemonProvider, (prev, next) {
    final a = syncsKey(prev?.value), b = syncsKey(next.value);
    if (a != null && b != null && a != b) ref.invalidate(machinesProvider);
  });
}
