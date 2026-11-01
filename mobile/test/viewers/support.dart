import 'package:backdown_mobile/api/demo_api.dart';
import 'package:backdown_mobile/demo/demo_fs.dart';
import 'package:backdown_mobile/features/viewers/open.dart';
import 'package:backdown_mobile/models/file_ref.dart';
import 'package:backdown_mobile/router.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

import '../support/harness.dart';

/// The demo's first file called [name].
FileRef demoFile(String name) {
  final n = DemoFs.instance.byName(name)!;
  return FileRef(id: n.id!, name: n.name, size: n.size, mtime: n.mtime);
}

/// Opens a demo file in the viewer, as a folder's tap would (with its folder's files to swipe).
Future<void> openInViewer(WidgetTester tester, ProviderContainer c, String name, {bool withSiblings = false}) async {
  final f = demoFile(name);
  final siblings = withSiblings
      ? [
          for (final x in DemoFs.instance.byName(name)!.parent!.children!)
            if (!x.isDir) FileRef(id: x.id!, name: x.name, size: x.size, mtime: x.mtime),
        ]
      : <FileRef>[];
  await goTo(tester, c, Routes.view(f.id), extra: ViewerArgs(f, siblings));
}

Future<void> openRef(WidgetTester tester, ProviderContainer c, FileRef f) =>
    goTo(tester, c, Routes.view(f.id), extra: ViewerArgs(f));

/// A presentation for the slides viewer (the demo's drives have none): it stands in for notes.md.
final deck = FileRef(id: demoFile('notes.md').id, name: 'Keeping what matters.pptx', size: 2418311, mtime: 1347000000);

const sampleDeck = {
  'format': 'pptx',
  'count': 3,
  'thumbnail': null,
  'slides': [
    {
      'n': 1,
      'title': 'Keeping what matters',
      'paras': [
        {'t': 'Family photos, mail and documents across old drives', 'lvl': 0},
        {'t': 'Sam Taylor and Marta Novak', 'lvl': 0},
      ],
      'images': ['ppt/media/image1.jpg'],
      'notes': '',
      'cut': false,
    },
    {
      'n': 2,
      'title': 'Why people lose photos',
      'paras': [
        {'t': 'One copy is no copy', 'lvl': 0},
        {'t': 'Old drives in drawers', 'lvl': 0},
        {'t': 'Nobody remembers which one has 2009', 'lvl': 1},
        {'t': 'Phones that fill up and get replaced', 'lvl': 0},
      ],
      'images': <String>[],
      'notes': 'Ask about the drawer of old drives.',
      'cut': false,
    },
    {
      'n': 3,
      'title': null,
      'paras': [
        {'t': 'Questions?', 'lvl': 0},
      ],
      'images': ['ppt/media/image2.emf'],
      'notes': '',
      'cut': false,
    },
  ],
};

/// The demo, with a presentation to show.
class SlidesDemo extends DemoBackDownApi {
  @override
  Future<Object?> get(String endpoint, [Map<String, Object?> params = const {}, Duration? timeout]) async =>
      endpoint == 'slides' ? sampleDeck : super.get(endpoint, params, timeout);

  @override
  Future<Uint8List> bytes(String endpoint, [Map<String, Object?> params = const {}, int? limit]) async =>
      endpoint == 'entry' && '${params['name']}'.endsWith('.jpg')
      ? (await rootBundle.load('assets/demo/photo2.jpg')).buffer.asUint8List()
      : super.bytes(endpoint, params, limit);
}
