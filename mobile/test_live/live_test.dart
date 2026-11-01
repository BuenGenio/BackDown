// Checks the app's client and models against a real BackDown service. Not part of `flutter test`
// (it needs a service): BACKDOWN_URL=http://127.0.0.1:8787 BACKDOWN_TOKEN=$(backdown token) \
//   flutter test test_live/
@TestOn('vm')
library;

import 'dart:io';

import 'package:backdown_mobile/api/api.dart';
import 'package:backdown_mobile/api/http_api.dart';
import 'package:backdown_mobile/api/json.dart';
import 'package:backdown_mobile/models/browse.dart';
import 'package:backdown_mobile/models/daemon.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  final url = Platform.environment['BACKDOWN_URL'];
  final token = Platform.environment['BACKDOWN_TOKEN'];
  final skip = url == null || token == null ? 'set BACKDOWN_URL and BACKDOWN_TOKEN' : null;
  late HttpBackDownApi api;
  setUpAll(() => api = HttpBackDownApi(Uri.parse(url!), token!));
  tearDownAll(() => api.close());

  test(
    'roots, a folder at its latest and its first scan, a file, the timeline',
    () async {
      final roots = [for (final r in asJson(await api.get('roots')).list('roots')) Root.fromJson(r)];
      expect(roots, isNotEmpty);
      final root = roots.first;
      final l = Listing.fromJson(asJson(await api.get('ls', {'dir': root.key})));
      expect(l.entries, isNotEmpty);
      expect(l.crumbs.first.key, root.key);
      final timeline = [
        for (final s in asJsonList(await api.get('timeline', {'root': root.id}))) ScanPoint.fromJson(s),
      ];
      expect(timeline, isNotEmpty);
      final first = Listing.fromJson(asJson(await api.get('ls', {'dir': root.key, 'scan': timeline.first.id})));
      expect(first.isPast, timeline.length > 1);
      // a file somewhere below: walk into the biggest folder until one turns up
      // (below the top, where system files the service can't read live)
      var dir = l;
      Entry? file;
      for (var depth = 0; depth < 12 && file == null; depth++) {
        if (depth >= 2) file = dir.entries.where((e) => !e.isDir && e.id != null && e.size < (1 << 20)).firstOrNull;
        final sub = dir.entries.where((e) => e.isDir && e.size > 0).toList()..sort((a, b) => b.size.compareTo(a.size));
        if (file == null && sub.isNotEmpty) dir = Listing.fromJson(asJson(await api.get('ls', {'dir': sub.first.key})));
      }
      expect(file, isNotNull);
      final info = FileInfo.fromJson(asJson(await api.get('file', {'id': file!.id})));
      expect(info.name, file.name);
      expect(info.versions, isNotEmpty);
      final head = await api.bytes('content', {'id': file.id}, 64);
      expect(head.length, lessThanOrEqualTo(64));
    },
    skip: skip,
    timeout: const Timeout(Duration(minutes: 3)),
  );

  test(
    'find, index status, the daemon',
    () async {
      final found = FindResult.fromJson(
        asJson(await api.get('find', {'q': 'readme', 'limit': 20}, const Duration(minutes: 2))),
      );
      expect(found.hits.length, lessThanOrEqualTo(20));
      final st = IndexStatus.fromJson(asJson(await api.get('index-status')));
      expect(st.files, greaterThan(0));
      final d = DaemonSnapshot.fromJson(asJson(await api.get('daemon')));
      if (d.running) {
        expect(d.version, isNotNull);
        expect(d.drives, isNotEmpty);
        // the long poll answers at its timeout when nothing changes (here, 1 s)
        final again = DaemonSnapshot.fromJson(
          asJson(await api.get('daemon', {'since': d.change, 'wait': 1}, const Duration(seconds: 10))),
        );
        expect(again.change, greaterThanOrEqualTo(d.change));
      }
    },
    skip: skip,
    timeout: const Timeout(Duration(minutes: 3)),
  );

  test(
    'the service\'s refusals come back as typed failures',
    () async {
      // a wrong token
      final wrong = HttpBackDownApi(Uri.parse(url!), '0' * 48);
      await expectLater(
        wrong.get('roots'),
        throwsA(isA<ApiException>().having((e) => e.failure, 'failure', ApiFailure.unauthorized)),
      );
      wrong.close();
      // a missing parameter
      await expectLater(
        api.get('file'),
        throwsA(isA<ApiException>().having((e) => e.failure, 'failure', ApiFailure.refused)),
      );
    },
    skip: skip,
    timeout: const Timeout(Duration(minutes: 3)),
  );
}
