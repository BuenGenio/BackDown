import 'dart:convert';
import 'dart:io';
import 'dart:isolate';

import 'package:flutter/foundation.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../api/api.dart';
import '../../api/demo_api.dart';
import '../../api/http_api.dart';
import '../../api/json.dart';
import '../../core/kinds.dart';
import '../../demo/demo_library.dart';
import '../../providers.dart';
import 'models.dart';

/// A GET whose answer can be big (/api/entities is megabytes on a real index): from a service its
/// bytes are decoded and read into [parse]'s model in another isolate, so the screen keeps moving.
/// The demo's answers are small and already decoded.
Future<T> getParsed<T>(
  BackDownApi api,
  String endpoint,
  Map<String, Object?> params,
  T Function(Object? json) parse,
) async {
  if (api is HttpBackDownApi) {
    final raw = await api.bytes(endpoint, params);
    try {
      return await Isolate.run(() => parse(jsonDecode(utf8.decode(raw))));
    } on FormatException {
      throw ApiException(ApiFailure.server, 'The service answered with something that isn\'t JSON.', status: 200);
    }
  }
  return parse(await api.get(endpoint, params));
}

/// The bytes of a document the service makes (the report, the chain of custody, a manifest).
Future<Uint8List> documentBytes(BackDownApi api, String endpoint, [Map<String, Object?> params = const {}]) async {
  // the demo keeps these with this area's answers (demo_library.dart)
  final demo = api is DemoBackDownApi ? demoLibraryBytes[endpoint] : null;
  if (demo != null) return demo(api as DemoBackDownApi, queryOf(params));
  return api.bytes(endpoint, params);
}

/// Downloads a document into [into]: the demo's from its answers, a service's streamed to disk.
Future<File?> saveDocument(BackDownApi api, String endpoint, Map<String, Object?> params, File into) async {
  if (api is DemoBackDownApi && demoLibraryBytes.containsKey(endpoint)) {
    return into.writeAsBytes(await documentBytes(api, endpoint, params));
  }
  return api.download(endpoint, params, into);
}

// ---------- the parsers (top level, so they can run in another isolate)

EntitiesData parseEntities(Object? v) => EntitiesData.fromJson(asJson(v));
DupesData parseDupes(Object? v) => DupesData.fromJson(asJson(v));
PlanDetail parsePlan(Object? v) => PlanDetail.fromJson(asJson(v));

// ---------- what's here

final entitiesProvider = FutureProvider.autoDispose<EntitiesData>(
  (ref) => getParsed(requireApi(ref), 'entities', const {}, parseEntities),
);

/// How much of each drive, top-level folder and recognised thing exists elsewhere, by
/// "key:level". Slow on a big index, and part of Pro: empty when it isn't there.
final overlapsProvider = FutureProvider.autoDispose<Map<String, Overlap>>((ref) async {
  final api = requireApi(ref);
  try {
    final v = await api.get('overlaps', const {}, const Duration(minutes: 5));
    return {for (final o in asJsonList(v).map(Overlap.fromJson)) '${o.key}:${o.level}': o};
  } on ApiException catch (e) {
    if (e.failure == ApiFailure.needsTier || e.failure == ApiFailure.notFound) return const {};
    rethrow;
  }
});

// ---------- duplicates

/// The types the duplicates filter offers, as the web UI's filter bar has them.
enum DupeType {
  any('Any', []),
  image('Images', [FileKind.image]),
  video('Videos', [FileKind.video]),
  audio('Audio', [FileKind.audio]),
  doc('Documents', [FileKind.doc]),
  pdf('PDFs', [FileKind.pdf]),
  sheet('Spreadsheets', [FileKind.sheet]),
  slides('Presentations', [FileKind.slides]),
  text('Text', [FileKind.text, FileKind.diff]),
  code('Code', [FileKind.code]),
  data('Data and databases', [FileKind.data, FileKind.sql, FileKind.db]),
  archive('Archives', [FileKind.archive]),
  disk('Disk images', [FileKind.disk]),
  mail('Email', [FileKind.mail]),
  other('Other files', []);

  const DupeType(this.label, this.kinds);
  final String label;
  final List<FileKind> kinds;
}

enum DupeDate {
  any('Any time', null),
  week('In the past week', 7),
  month('In the past month', 30),
  year('In the past year', 365),
  old('More than a year ago', null);

  const DupeDate(this.label, this.days);
  final String label;
  final int? days;
}

/// The smallest file to count.
const dupeMins = [('1', 'Any size'), ('100K', '100 KB and up'), ('1M', '1 MB and up'), ('100M', '100 MB and up')];

/// What /api/dupes is asked: the filter, and how many groups.
@immutable
class DupesQuery {
  const DupesQuery({
    this.min = '1M',
    this.type = DupeType.any,
    this.date = DupeDate.any,
    this.hidden = false,
    this.limit = 20,
    this.now = 0,
  });

  final String min;
  final DupeType type;
  final DupeDate date;

  /// Hidden files and folders count too.
  final bool hidden;
  final int limit;

  /// The moment dates are counted back from (unix seconds), fixed when the filter is set.
  final int now;

  bool get filtering => type != DupeType.any || date != DupeDate.any;

  DupesQuery copyWith({String? min, DupeType? type, DupeDate? date, bool? hidden, int? limit, int? now}) => DupesQuery(
    min: min ?? this.min,
    type: type ?? this.type,
    date: date ?? this.date,
    hidden: hidden ?? this.hidden,
    limit: limit ?? this.limit,
    now: now ?? this.now,
  );

  Map<String, Object?> get params => {
    'min': min,
    'limit': limit,
    if (type == DupeType.other) ...{'ext': FileKind.extensionsOf(FileKind.values).join(','), 'except': 1},
    if (type.kinds.isNotEmpty) 'ext': FileKind.extensionsOf(type.kinds).join(','),
    if (date == DupeDate.old) 'before': now - 365 * 86400,
    if (date.days != null) 'after': now - date.days! * 86400,
    if (!hidden) 'nohidden': 1,
  };

  @override
  bool operator ==(Object other) =>
      other is DupesQuery &&
      other.min == min &&
      other.type == type &&
      other.date == date &&
      other.hidden == hidden &&
      other.limit == limit &&
      other.now == now;

  @override
  int get hashCode => Object.hash(min, type, date, hidden, limit, now);
}

final dupesProvider = FutureProvider.autoDispose.family<DupesData, DupesQuery>(
  (ref, q) => getParsed(requireApi(ref), 'dupes', q.params, parseDupes),
);

// ---------- plans and the rest of the tidying history

final plansProvider = FutureProvider.autoDispose<List<PlanSummary>>(
  (ref) async => [for (final p in asJsonList(await requireApi(ref).get('plans'))) PlanSummary.fromJson(p)],
);

final planProvider = FutureProvider.autoDispose.family<PlanDetail, int>(
  (ref, id) => getParsed(requireApi(ref), 'plan', {'id': id}, parsePlan),
);

/// Tidying the archive, earlier runs (shown with the plans, as the web UI does). Empty when the
/// service doesn't say.
final dedupsProvider = FutureProvider.autoDispose<List<DedupRun>>((ref) async {
  try {
    return [for (final r in asJsonList(await requireApi(ref).get('dedups'))) DedupRun.fromJson(r)];
  } on ApiException {
    return const [];
  }
});

final wipesProvider = FutureProvider.autoDispose<List<Wipe>>(
  (ref) async => [for (final w in asJsonList(await requireApi(ref).get('wipes'))) Wipe.fromJson(w)],
);

final rescuesProvider = FutureProvider.autoDispose<List<Rescue>>(
  (ref) async => [for (final r in asJsonList(await requireApi(ref).get('rescues'))) Rescue.fromJson(r)],
);

final mirrorsProvider = FutureProvider.autoDispose<List<Mirror>>(
  (ref) async => [for (final m in asJsonList(await requireApi(ref).get('mirrors'))) Mirror.fromJson(m)],
);

final manifestsProvider = FutureProvider.autoDispose<List<Manifest>>(
  (ref) async => [
    for (final m in asJson(await requireApi(ref).get('manifests')).list('manifests')) Manifest.fromJson(m),
  ],
);

// ---------- this BackDown

final featuresProvider = FutureProvider.autoDispose<Features>(
  (ref) async => Features.fromJson(asJson(await requireApi(ref).get('features'))),
);

final updateProvider = FutureProvider.autoDispose<UpdateInfo>(
  (ref) async => UpdateInfo.fromJson(asJson(await requireApi(ref).get('update'))),
);

/// How scans may be made (read-only here): null when this service has no such settings.
final scanSettingsProvider = FutureProvider.autoDispose<Json?>((ref) async {
  try {
    return asJson(await requireApi(ref).get('scan-settings'));
  } on ApiException catch (e) {
    if (e.failure == ApiFailure.notFound) return null;
    rethrow;
  }
});

/// The report, as Markdown.
final reportProvider = FutureProvider.autoDispose<String>(
  (ref) async => utf8.decode(await documentBytes(requireApi(ref), 'report', {'format': 'md'}), allowMalformed: true),
);
