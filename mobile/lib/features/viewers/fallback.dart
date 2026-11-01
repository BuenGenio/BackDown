import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../api/api.dart';
import '../../core/file_io.dart';
import '../../core/format.dart';
import '../../core/kinds.dart';
import '../../core/widgets.dart';
import '../../models/file_ref.dart';
import '../../providers.dart';
import '../../theme.dart';
import 'common.dart';

/// A file with no preview here, or one that can't be read: its icon, size, why, and the ways to
/// open it anyway (another app on the phone, the share sheet), with the first bytes as hex when
/// there are some.
class FallbackView extends ConsumerStatefulWidget {
  const FallbackView({
    super.key,
    required this.source,
    required this.title,
    this.message,
    this.head,
    this.icon,
    this.canOpen = true,
  });

  final ViewSource source;
  final String title;
  final String? message;

  /// The file's first bytes, offered as hex.
  final Uint8List? head;
  final IconData? icon;

  /// Whether its bytes can be fetched (false when they aren't on any drive now).
  final bool canOpen;

  /// The fallback for a read that failed, in the words the service gave.
  factory FallbackView.error(ViewSource source, Object error, {Key? key}) {
    final e = error;
    final gone = e is ApiException && e.failure == ApiFailure.notFound;
    final message = e is ApiException ? e.message : e.toString();
    return FallbackView(
      key: key,
      source: source,
      title: gone && message.contains('any more') ? 'Not on any drive now' : 'Can\'t show this file',
      message: _sentence(message),
      icon: gone ? Icons.cloud_off_outlined : Icons.error_outline,
      canOpen: !(gone && message.contains('any more')),
    );
  }

  static String _sentence(String s) {
    final t = s.trim();
    if (t.isEmpty) return t;
    final up = '${t[0].toUpperCase()}${t.substring(1)}';
    return RegExp(r'[.!?]$').hasMatch(up) ? up : '$up.';
  }

  @override
  ConsumerState<FallbackView> createState() => _FallbackViewState();
}

class _FallbackViewState extends ConsumerState<FallbackView> {
  bool _hex = false;

  @override
  Widget build(BuildContext context) {
    final api = ref.watch(apiProvider);
    final s = widget.source;
    final kind = FileKind.of(s.name);
    final inner = s.inner;
    Future<void> share() => inner != null
        ? shareInner(context, api!, inner)
        : shareFile(context, api!, FileRef(id: s.id, name: s.name, size: s.size ?? 0));
    Future<void> open() => inner != null
        ? openInnerElsewhere(context, api!, inner)
        : openInAnotherApp(context, api!, FileRef(id: s.id, name: s.name, size: s.size ?? 0));
    final card = Card(
      child: Padding(
        padding: const EdgeInsets.fromLTRB(20, 24, 20, 20),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Stack(
              clipBehavior: Clip.none,
              children: [
                Icon(kind.icon, size: 64, color: kind.color),
                if (widget.icon != null)
                  Positioned(
                    right: -8,
                    bottom: -4,
                    child: CircleAvatar(
                      radius: 14,
                      backgroundColor: context.colors.surfaceContainerHigh,
                      child: Icon(widget.icon, size: 16, color: context.colors.onSurfaceVariant),
                    ),
                  ),
              ],
            ),
            const SizedBox(height: 14),
            Text(s.name, style: context.text.titleMedium, textAlign: TextAlign.center, maxLines: 3),
            const SizedBox(height: 4),
            Text(
              [kind.label, if (s.size != null) bytes(s.size)].join(' · '),
              style: context.text.bodySmall?.copyWith(color: context.bd.muted),
            ),
            const SizedBox(height: 16),
            Text(widget.title, style: context.text.titleSmall, textAlign: TextAlign.center),
            if (widget.message != null) ...[
              const SizedBox(height: 4),
              Text(
                widget.message!,
                style: context.text.bodyMedium?.copyWith(color: context.colors.onSurfaceVariant),
                textAlign: TextAlign.center,
              ),
            ],
            if (api != null && widget.canOpen) ...[
              const SizedBox(height: 18),
              Wrap(
                spacing: 8,
                runSpacing: 8,
                alignment: WrapAlignment.center,
                children: [
                  FilledButton.icon(
                    onPressed: open,
                    icon: const Icon(Icons.open_in_new),
                    label: const Text('Open in another app'),
                  ),
                  OutlinedButton.icon(
                    onPressed: share,
                    icon: const Icon(Icons.share_outlined),
                    label: const Text('Share'),
                  ),
                ],
              ),
            ],
            if (widget.head != null && widget.head!.isNotEmpty) ...[
              const SizedBox(height: 8),
              TextButton.icon(
                onPressed: () => setState(() => _hex = !_hex),
                icon: Icon(_hex ? Icons.expand_less : Icons.data_object),
                label: Text(
                  _hex ? 'Hide the bytes' : 'Show the first ${bytes(widget.head!.length.clamp(0, 4096))} as hex',
                ),
              ),
            ],
          ],
        ),
      ),
    );
    return Readable(
      maxWidth: _hex ? 860 : 520,
      child: ListView(
        padding: const EdgeInsets.fromLTRB(16, 24, 16, 32),
        children: [
          card,
          if (_hex) ...[
            const SizedBox(height: 12),
            Card(
              child: SingleChildScrollView(
                scrollDirection: Axis.horizontal,
                padding: const EdgeInsets.all(12),
                child: SelectableText(
                  hexDump(widget.head!),
                  style: TextStyle(
                    fontFamily: 'monospace',
                    fontSize: 11.5,
                    height: 1.4,
                    color: context.colors.onSurface,
                  ),
                ),
              ),
            ),
          ],
        ],
      ),
    );
  }
}

/// A viewer's failed read: what to do about a connection that failed (with Try again), else the
/// fallback card with the service's reason (the file isn't what its name says, its bytes are gone).
Widget viewerError(ViewSource s, Object e, VoidCallback retry) {
  const connection = {
    ApiFailure.offline,
    ApiFailure.slow,
    ApiFailure.unauthorized,
    ApiFailure.wrongHost,
    ApiFailure.server,
  };
  if (e is ApiException && connection.contains(e.failure)) return ErrorPanel(error: e, onRetry: retry);
  return FallbackView.error(s, e);
}
