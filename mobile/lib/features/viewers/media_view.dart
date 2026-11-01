import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:video_player/video_player.dart';

import '../../api/api.dart';
import '../../core/kinds.dart';
import '../../providers.dart';
import '../../theme.dart';
import 'common.dart';
import 'fallback.dart';

/// Where video_player can play (it has no Linux or Windows implementation).
bool get mediaPlaysHere => !kIsWeb && (Platform.isAndroid || Platform.isIOS || Platform.isMacOS);

/// "1:05", "1:02:09".
String clockTime(Duration d) {
  final h = d.inHours, m = d.inMinutes % 60, s = d.inSeconds % 60;
  String two(int n) => n.toString().padLeft(2, '0');
  return h > 0 ? '$h:${two(m)}:${two(s)}' : '$m:${two(s)}';
}

/// Video or audio, streamed from the service with the session cookie: play and pause, a seek bar
/// and the times. It pauses when swiped away from.
class MediaView extends ConsumerStatefulWidget {
  const MediaView({super.key, required this.source, required this.video});
  final ViewSource source;
  final bool video;

  @override
  ConsumerState<MediaView> createState() => _MediaViewState();
}

class _MediaViewState extends ConsumerState<MediaView> {
  VideoPlayerController? _c;
  Object? _error;
  double? _dragging;

  @override
  void initState() {
    super.initState();
    final api = ref.read(apiProvider);
    if (api == null || api.isDemo || !mediaPlaysHere) return;
    final c = VideoPlayerController.networkUrl(widget.source.uri(api), httpHeaders: api.authHeaders);
    _c = c;
    c.initialize().then(
      (_) {
        if (mounted) setState(() {});
      },
      onError: (Object e) {
        if (mounted) setState(() => _error = e);
      },
    );
  }

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    if (ViewerPage.of(context)?.active == false) _c?.pause();
  }

  @override
  void dispose() {
    _c?.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final api = ref.watch(apiProvider);
    final what = widget.video ? 'Video' : 'Audio';
    if (api == null || api.isDemo) {
      return FallbackView(
        source: widget.source,
        title: 'Nothing to play in the demo',
        message: '$what plays here from the computer\'s drives; the demo has no recordings.',
        icon: Icons.play_disabled_outlined,
        canOpen: false,
      );
    }
    if (!mediaPlaysHere) {
      return FallbackView(
        source: widget.source,
        title: '$what plays in the app on Android and iOS',
        message: 'Open it in another app here.',
        icon: Icons.play_disabled_outlined,
      );
    }
    if (_error != null) {
      final e = _error;
      return FallbackView(
        source: widget.source,
        title: 'Can\'t play this here',
        message: e is ApiException ? e.message : 'The phone can\'t play this format, or the stream stopped.',
        icon: Icons.error_outline,
      );
    }
    final c = _c!;
    return ValueListenableBuilder<VideoPlayerValue>(
      valueListenable: c,
      builder: (context, v, _) {
        if (!v.isInitialized) return const Center(child: CircularProgressIndicator());
        final picture = widget.video
            ? GestureDetector(
                onTap: () => v.isPlaying ? c.pause() : c.play(),
                child: Center(
                  child: AspectRatio(aspectRatio: v.aspectRatio, child: VideoPlayer(c)),
                ),
              )
            : Center(
                child: Column(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    Container(
                      width: 160,
                      height: 160,
                      decoration: BoxDecoration(
                        color: FileKind.audio.color.withValues(alpha: 0.12),
                        borderRadius: BorderRadius.circular(28),
                      ),
                      child: Icon(Icons.audiotrack, size: 72, color: FileKind.audio.color),
                    ),
                    const SizedBox(height: 18),
                    Padding(
                      padding: const EdgeInsets.symmetric(horizontal: 24),
                      child: Text(widget.source.name, style: context.text.titleMedium, textAlign: TextAlign.center),
                    ),
                  ],
                ),
              );
        final total = v.duration.inMilliseconds.toDouble();
        final at = (_dragging ?? v.position.inMilliseconds.toDouble()).clamp(0, total > 0 ? total : 1).toDouble();
        return Column(
          children: [
            Expanded(child: picture),
            SafeArea(
              top: false,
              child: Readable(
                maxWidth: 720,
                child: Padding(
                  padding: const EdgeInsets.fromLTRB(8, 4, 16, 8),
                  child: Row(
                    children: [
                      IconButton.filledTonal(
                        tooltip: v.isPlaying ? 'Pause' : 'Play',
                        iconSize: 30,
                        onPressed: () => v.isPlaying ? c.pause() : c.play(),
                        icon: Icon(v.isPlaying ? Icons.pause : Icons.play_arrow),
                      ),
                      Expanded(
                        child: Slider(
                          value: at,
                          max: total > 0 ? total : 1,
                          onChanged: total > 0 ? (x) => setState(() => _dragging = x) : null,
                          onChangeEnd: (x) async {
                            await c.seekTo(Duration(milliseconds: x.round()));
                            if (mounted) setState(() => _dragging = null);
                          },
                        ),
                      ),
                      Text(
                        '${clockTime(Duration(milliseconds: at.round()))} / ${clockTime(v.duration)}',
                        style: context.text.bodySmall?.copyWith(fontFeatures: const [FontFeature.tabularFigures()]),
                      ),
                    ],
                  ),
                ),
              ),
            ),
          ],
        );
      },
    );
  }
}
