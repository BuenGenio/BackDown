import 'dart:convert';
import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';

/// Screenshots of the app with real fonts (Roboto from the Flutter SDK, the app's own fonts,
/// Noto/DejaVu for monospace and symbols), for looking at screens without a phone. Written to
/// build/shots/ (or $SHOTS_DIR). Not part of `flutter test test/`: run `flutter test screenshots/`.
final shotsDir = Platform.environment['SHOTS_DIR'] ?? 'build/shots';

bool _fontsLoaded = false;

Future<ByteData> _file(String path) async => ByteData.sublistView(await File(path).readAsBytes());

Future<void> _family(String family, Iterable<String> paths) async {
  final loader = FontLoader(family);
  var any = false;
  for (final p in paths) {
    if (File(p).existsSync()) {
      loader.addFont(_file(p));
      any = true;
    }
  }
  if (any) await loader.load();
}

Future<void> loadFonts() async {
  if (_fontsLoaded) return;
  _fontsLoaded = true;
  final manifest = jsonDecode(await rootBundle.loadString('FontManifest.json')) as List<dynamic>;
  for (final family in manifest.cast<Map<String, dynamic>>()) {
    final loader = FontLoader(family['family'] as String);
    for (final font in (family['fonts'] as List).cast<Map<String, dynamic>>()) {
      loader.addFont(rootBundle.load(font['asset'] as String));
    }
    await loader.load();
  }
  final root = Platform.environment['FLUTTER_ROOT'] ?? '${Platform.environment['HOME']}/development/flutter';
  final material = Directory('$root/bin/cache/artifacts/material_fonts');
  final roboto = [
    for (final f in material.listSync().whereType<File>())
      if (RegExp(r'/Roboto-\w+\.ttf$').hasMatch(f.path)) f.path,
  ];
  await _family('Roboto', roboto);
  const fonts = '/usr/share/fonts/truetype';
  await _family('monospace', ['$fonts/noto/NotoSansMono-Regular.ttf', '$fonts/noto/NotoSansMono-Bold.ttf']);
  await _family('Noto Sans Symbols 2', ['$fonts/noto/NotoSansSymbols2-Regular.ttf']);
  await _family('DejaVu Sans', ['$fonts/dejavu/DejaVuSans.ttf', '$fonts/dejavu/DejaVuSans-Bold.ttf']);
}

/// Decodes every image on screen for real (decoding needs real async), then lets frames land.
Future<void> resolveImages(WidgetTester tester) async {
  for (var round = 0; round < 3; round++) {
    final images = find.byType(Image, skipOffstage: false).evaluate().toList();
    await tester.runAsync(() async {
      for (final e in images) {
        try {
          await precacheImage((e.widget as Image).image, e);
        } on Object {
          // a broken image shows as broken; the screenshot reveals it
        }
      }
      await Future<void>.delayed(const Duration(milliseconds: 50));
    });
    await tester.pumpAndSettle();
  }
}

/// Saves what's on screen as [slug].png.
Future<void> shoot(WidgetTester tester, String slug) async {
  await resolveImages(tester);
  final view = tester.binding.renderViews.first;
  final layer = view.debugLayer! as OffsetLayer;
  await tester.runAsync(() async {
    final image = await layer.toImage(view.paintBounds);
    final bytes = await image.toByteData(format: ui.ImageByteFormat.png);
    final out = File('$shotsDir/$slug.png');
    out.parent.createSync(recursive: true);
    out.writeAsBytesSync(bytes!.buffer.asUint8List());
  });
}
