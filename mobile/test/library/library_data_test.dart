import 'dart:convert';

import 'package:backdown_mobile/api/api.dart';
import 'package:backdown_mobile/api/demo_api.dart';
import 'package:backdown_mobile/api/http_api.dart';
import 'package:backdown_mobile/features/library/data.dart';
import 'package:backdown_mobile/features/library/dupes_screen.dart';
import 'package:backdown_mobile/features/library/markdown.dart';
import 'package:backdown_mobile/features/library/models.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';

/// What a service's /api/entities looks like, small (the real one is megabytes).
Map<String, Object?> _entities() => {
  'daemon': true,
  'roots': [
    {
      'root_id': 1,
      'root': '/srv/data',
      'scan': 7,
      'computed_at': 1790000000,
      'read_drive': true,
      'fresh': false,
      'junk': [
        {
          'path': '/srv/data/app/node_modules',
          'key': 'x',
          'why': 'npm packages (reinstallable)',
          'files': 900,
          'bytes': 9000,
        },
      ],
      'entities': [
        {
          'kind': 'git',
          'label': 'Git repository',
          'title': 'app (main)',
          'path': '/srv/data/app',
          'key': 'L3Nydi9kYXRhL2FwcA',
          'is_file': false,
          'confidence': 0.75,
          'files': 1200,
          'bytes': 52000,
          'oldest': 1600000000,
          'newest': 1700000000,
          'facts': [
            ['Branch', 'main'],
            ['Remote', 'git.example.org/app'],
          ],
          'provenance': ['Cloned from git.example.org/app.'],
          'junk': [
            for (var i = 0; i < 40; i++)
              {
                'path': '/srv/data/app/m$i/__pycache__',
                'key': 'k$i',
                'why': 'Python bytecode cache',
                'files': 2,
                'bytes': 100 + i,
              },
            {'path': '/srv/data/app/target', 'key': 't', 'why': 'Rust build output', 'files': 10, 'bytes': 50000},
          ],
          'inside': null,
          'low_value': false,
        },
      ],
    },
  ],
};

void main() {
  test('what\'s recognised reads into kinds, junk totals and a search text', () {
    final d = parseEntities(_entities());
    final e = d.all.single;
    expect(e.relPath, 'app');
    expect(e.facts.first, ('Branch', 'main'));
    expect(e.junk.count, 41);
    expect(e.junk.biggest.length, 30);
    expect(e.junk.biggest.first.why, 'Rust build output');
    expect(e.junk.whys, ['Rust build output', 'Python bytecode cache']);
    expect(e.search, contains('git.example.org/app'));
    expect(d.drives.single.fresh, isFalse);
    expect(d.drives.single.junkBytes, 9000 + e.junk.bytes);
  });

  test('odd or missing values read as defaults instead of failing', () {
    final d = parseEntities({
      'roots': [
        {
          'root': '/',
          'entities': [
            {'kind': 'photos', 'path': '/', 'confidence': 'high', 'facts': 'none', 'junk': null},
          ],
        },
      ],
    });
    expect(d.all.single.confidence, 1);
    expect(d.all.single.facts, isEmpty);
    expect(d.all.single.relPath, '.');
  });

  test('a big answer from a service is read in another isolate', () async {
    final client = MockClient((r) async {
      expect(r.url.path, '/api/entities');
      return http.Response(jsonEncode(_entities()), 200);
    });
    final api = HttpBackDownApi(Uri.parse('http://computer.example:8787'), 'token', client: client);
    final d = await getParsed(api, 'entities', const {}, parseEntities);
    expect(d.all.single.title, 'app (main)');
  });

  test('a paid edition\'s refusal of a big answer still says what it is', () async {
    final client = MockClient(
      (r) async => http.Response(
        jsonEncode({
          'error': 'Filing suggestions is part of BackDown Pro.',
          'needs_name': 'Pro',
          'name': 'Filing suggestions',
        }),
        402,
      ),
    );
    final api = HttpBackDownApi(Uri.parse('http://computer.example:8787'), 'token', client: client);
    expect(
      () => getParsed(api, 'plan', {'id': 1}, parsePlan),
      throwsA(isA<ApiException>().having((e) => e.failure, 'failure', ApiFailure.needsTier)),
    );
  });

  test('the duplicates filter asks as the web UI does', () {
    const q = DupesQuery(min: '100K', type: DupeType.data, date: DupeDate.old, now: 1000000000);
    final p = q.params;
    expect(p['min'], '100K');
    expect((p['ext']! as String).split(','), containsAll(['csv', 'sql', 'sqlite']));
    expect(p['before'], 1000000000 - 365 * 86400);
    expect(p['nohidden'], 1);
    final other = const DupesQuery(type: DupeType.other).params;
    expect(other['except'], 1);
    expect((other['ext']! as String).split(','), contains('jpg'));
  });

  test('a group\'s places show what its folders share and where they part', () {
    DupeGroup g(List<String> paths) => DupeGroup.fromJson({
      'sha256': 'x',
      'size': 10,
      'copies': paths.length,
      'reclaimable': 10,
      'files': [
        for (final (i, p) in paths.indexed) {'path': p, 'key': '', 'dir': '', 'id': i + 1, 'mtime': 0, 'copy': i},
      ],
    });
    final p = placesOf(
      g([
        '/home/me/Projects/workspace-ask/data/m.json',
        '/home/me/Projects/workspace-chart/data/m.json',
        '/home/me/Projects/workspace-deploy/data/m.json',
        '/home/me/Projects/workspace-edit/data/m.json',
      ]),
    )!;
    expect(p.head, '/home/me/Projects/workspace-');
    expect(p.inner, ['ask', 'chart', 'deploy']);
    expect(p.more, 1);
    expect(p.tail, '/data');
    expect(placesOf(g(['/a/x.txt', '/a/y.txt'])), isNull);
  });

  test('the report\'s Markdown reads into headings, paragraphs and tables', () {
    final blocks = parseReport(
      '# BackDown report\n\nGenerated today · 1 drive\n\n## Drives\n\n| Drive | Files |\n|---|---:|\n'
      '| a (\\|b) | 3 |\n| c | 4 |\n\n---\nMade by BackDown\n',
    );
    expect(blocks[0], isA<MdHeading>().having((h) => h.level, 'level', 1));
    expect(blocks[1], isA<MdParagraph>());
    final t = blocks[3] as MdTable;
    expect(t.header, ['Drive', 'Files']);
    expect(t.rows.first, ['a (|b)', '3']);
    expect(t.rows.length, 2);
    expect(blocks[4], isA<MdRule>());
  });

  test('the demo\'s documents are there to share', () async {
    final demo = DemoBackDownApi();
    final custody = jsonDecode(utf8.decode(await documentBytes(demo, 'custody'))) as Map<String, Object?>;
    expect(custody['kind'], 'backdown-custody');
    expect((custody['events']! as List).length, 2);
    final report = utf8.decode(await documentBytes(demo, 'report', {'format': 'md'}));
    expect(report, startsWith('# BackDown report'));
    final manifest = jsonDecode(utf8.decode(await documentBytes(demo, 'manifest', {'id': 1}))) as Map<String, Object?>;
    expect(manifest['kind'], 'backdown-manifest');
  });

  test('the demo\'s duplicates come from its drives\' shared hashes', () async {
    final demo = DemoBackDownApi();
    final all = parseDupes(await demo.get('dupes', {'min': '1', 'limit': 100}));
    expect(all.groups, 25);
    expect(all.largest.every((g) => g.files.length == 2), isTrue);
    final photos2014 = all.largest.where((g) => g.name == 'IMG_2100.JPG').single;
    expect(photos2014.remote.single.machine, 'nextcloud');
    expect(all.across!.files, 2);
    final images = parseDupes(await demo.get('dupes', const DupesQuery(type: DupeType.image).params));
    expect(images.groups, 23);
    expect(images.narrowed, isFalse);
  });
}
