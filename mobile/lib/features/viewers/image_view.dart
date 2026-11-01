import 'dart:async';
import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../api/api.dart';
import '../../core/content_image.dart';
import '../../providers.dart';
import 'common.dart';
import 'fallback.dart';

/// A picture from inside a container (an archive entry, an attachment, a slide's image), fetched
/// by the client so it carries the session, like [ContentImage].
@immutable
class InnerImage extends ImageProvider<InnerImage> {
  const InnerImage(this.api, this.file);

  final BackDownApi api;
  final InnerFile file;

  @override
  Future<InnerImage> obtainKey(ImageConfiguration configuration) => SynchronousFuture(this);

  @override
  ImageStreamCompleter loadImage(InnerImage key, ImageDecoderCallback decode) =>
      MultiFrameImageStreamCompleter(codec: _load(decode), scale: 1, debugLabel: 'inner ${file.name}');

  Future<ui.Codec> _load(ImageDecoderCallback decode) async {
    final b = await file.bytes(api, entryMax);
    return decode(await ui.ImmutableBuffer.fromUint8List(b));
  }

  @override
  bool operator ==(Object other) => other is InnerImage && other.api.base == api.base && other.file == file;

  @override
  int get hashCode => Object.hash(api.base, file);
}

/// The provider for a source's picture, decoded at most 4096 pixels on its longer side (enough to
/// zoom into, without a 50-megapixel photo taking hundreds of MB).
ImageProvider imageOf(BackDownApi api, ViewSource s) => ResizeImage(
  s.inner == null ? ContentImage(api, s.id) : InnerImage(api, s.inner!),
  width: 4096,
  height: 4096,
  policy: ResizeImagePolicy.fit,
);

/// A picture to pinch and double-tap zoom.
class ImageView extends ConsumerStatefulWidget {
  const ImageView({super.key, required this.source});
  final ViewSource source;

  @override
  ConsumerState<ImageView> createState() => _ImageViewState();
}

class _ImageViewState extends ConsumerState<ImageView> with SingleTickerProviderStateMixin {
  final _t = TransformationController();
  late final AnimationController _anim = AnimationController(vsync: this, duration: const Duration(milliseconds: 220));
  Animation<Matrix4>? _to;
  Offset _tap = Offset.zero;
  bool _zoomed = false;
  Object? _error;

  @override
  void initState() {
    super.initState();
    _anim.addListener(() {
      if (_to != null) _t.value = _to!.value;
    });
    _anim.addStatusListener((s) {
      if (s == AnimationStatus.completed) _report();
    });
  }

  @override
  void dispose() {
    _anim.dispose();
    _t.dispose();
    super.dispose();
  }

  void _report() {
    final z = _t.value.getMaxScaleOnAxis() > 1.01;
    if (z != _zoomed) {
      setState(() => _zoomed = z);
      ViewerPage.of(context)?.onZoom(z);
    }
  }

  void _toggleZoom() {
    final Matrix4 end;
    if (_t.value.getMaxScaleOnAxis() > 1.01) {
      end = Matrix4.identity();
    } else {
      const s = 2.5;
      end = Matrix4.identity()
        ..translateByDouble(-_tap.dx * (s - 1), -_tap.dy * (s - 1), 0, 1)
        ..scaleByDouble(s, s, 1, 1);
    }
    _to = Matrix4Tween(begin: _t.value, end: end).animate(CurvedAnimation(parent: _anim, curve: Curves.easeOut));
    _anim.forward(from: 0);
  }

  @override
  Widget build(BuildContext context) {
    final api = ref.watch(apiProvider);
    if (api == null) return const SizedBox.shrink();
    final e = _error;
    if (e != null) {
      return FallbackView(
        source: widget.source,
        title: 'Can\'t display this picture',
        message: e is ApiException ? e.message : 'The phone can\'t decode it, or it isn\'t a picture after all.',
        icon: Icons.broken_image_outlined,
      );
    }
    return GestureDetector(
      onDoubleTapDown: (d) => _tap = d.localPosition,
      onDoubleTap: _toggleZoom,
      child: InteractiveViewer(
        transformationController: _t,
        maxScale: 8,
        // at normal size a swipe turns the page; zoomed in, it moves the picture
        panEnabled: _zoomed,
        onInteractionEnd: (_) => _report(),
        child: SizedBox.expand(
          child: Image(
            image: imageOf(api, widget.source),
            fit: BoxFit.contain,
            gaplessPlayback: true,
            loadingBuilder: (c, child, p) => p == null ? child : const Center(child: CircularProgressIndicator()),
            errorBuilder: (c, e, s) {
              // shown without the zoom around it, from the next frame
              WidgetsBinding.instance.addPostFrameCallback((_) {
                if (mounted && _error == null) setState(() => _error = e);
              });
              return const SizedBox.shrink();
            },
          ),
        ),
      ),
    );
  }
}
