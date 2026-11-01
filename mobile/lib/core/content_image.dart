import 'dart:async';
import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:flutter/widgets.dart';

import '../api/api.dart';

/// A file's picture, fetched from the service by index id (GET /api/content?id=N) through the
/// client, so it carries the session and works in the demo too. Cached by Flutter's image cache
/// under the service's address and the id.
@immutable
class ContentImage extends ImageProvider<ContentImage> {
  const ContentImage(this.api, this.id, {this.limit});

  final BackDownApi api;
  final int id;

  /// Read at most this many bytes (a guard for thumbnails of big files).
  final int? limit;

  @override
  Future<ContentImage> obtainKey(ImageConfiguration configuration) => SynchronousFuture(this);

  @override
  ImageStreamCompleter loadImage(ContentImage key, ImageDecoderCallback decode) {
    return MultiFrameImageStreamCompleter(
      codec: _load(decode),
      scale: 1,
      debugLabel: 'content $id',
      informationCollector: () => [DiagnosticsProperty<int>('File id', id)],
    );
  }

  Future<ui.Codec> _load(ImageDecoderCallback decode) async {
    final bytes = await api.bytes('content', {'id': id}, limit);
    return decode(await ui.ImmutableBuffer.fromUint8List(bytes));
  }

  @override
  bool operator ==(Object other) => other is ContentImage && other.api.base == api.base && other.id == id;

  @override
  int get hashCode => Object.hash(api.base, id);
}
