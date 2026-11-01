import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/api.dart';
import '../theme.dart';
import 'kinds.dart';

/// A screen's data: a spinner while it first loads, what to do when it fails, and the data (kept
/// on screen while it refreshes, with a thin bar at the top).
class AsyncBody<T> extends StatelessWidget {
  const AsyncBody({super.key, required this.value, required this.data, this.onRetry, this.loading});

  final AsyncValue<T> value;
  final Widget Function(T data) data;
  final VoidCallback? onRetry;
  final Widget? loading;

  @override
  Widget build(BuildContext context) {
    if (value.hasValue) {
      final body = data(value.value as T);
      if (!value.isLoading) return body;
      return Stack(
        children: [
          body,
          const Positioned(top: 0, left: 0, right: 0, child: LinearProgressIndicator(minHeight: 2)),
        ],
      );
    }
    if (value.hasError) return ErrorPanel(error: value.error!, onRetry: onRetry);
    return loading ?? const Center(child: CircularProgressIndicator());
  }
}

/// What went wrong, in words, with what to do about it.
class ErrorPanel extends StatelessWidget {
  const ErrorPanel({super.key, required this.error, this.onRetry, this.compact = false});

  final Object error;
  final VoidCallback? onRetry;
  final bool compact;

  @override
  Widget build(BuildContext context) {
    final e = error;
    final failure = e is ApiException ? e.failure : null;
    final (icon, title) = switch (failure) {
      ApiFailure.offline => (Icons.cloud_off_outlined, 'Can\'t reach the computer'),
      ApiFailure.unauthorized => (Icons.key_off_outlined, 'Not connected any more'),
      ApiFailure.wrongHost => (Icons.dns_outlined, 'Wrong address'),
      ApiFailure.atTheComputer => (Icons.computer_outlined, 'Needs someone at the computer'),
      ApiFailure.needsTier => (Icons.workspace_premium_outlined, 'Part of a paid edition'),
      ApiFailure.notFound => (Icons.search_off, 'Not found'),
      ApiFailure.conflict => (Icons.hourglass_empty, 'Not now'),
      ApiFailure.refused => (Icons.info_outline, 'BackDown said no'),
      ApiFailure.server || null => (Icons.error_outline, 'Something went wrong'),
    };
    final message = e is ApiException ? e.message : e.toString();
    final children = [
      Icon(icon, size: compact ? 28 : 44, color: context.bd.muted),
      SizedBox(height: compact ? 8 : 14),
      Text(title, style: context.text.titleMedium, textAlign: TextAlign.center),
      const SizedBox(height: 6),
      Text(
        message,
        style: context.text.bodyMedium?.copyWith(color: context.colors.onSurfaceVariant),
        textAlign: TextAlign.center,
      ),
      if (onRetry != null) ...[
        const SizedBox(height: 16),
        OutlinedButton.icon(onPressed: onRetry, icon: const Icon(Icons.refresh), label: const Text('Try again')),
      ],
    ];
    final column = Padding(
      padding: EdgeInsets.all(compact ? 16 : 32),
      child: Column(mainAxisSize: MainAxisSize.min, children: children),
    );
    return compact ? column : Center(child: SingleChildScrollView(child: column));
  }
}

class EmptyState extends StatelessWidget {
  const EmptyState({super.key, required this.icon, required this.title, this.message, this.action});

  final IconData icon;
  final String title;
  final String? message;
  final Widget? action;

  @override
  Widget build(BuildContext context) {
    return Center(
      child: SingleChildScrollView(
        padding: const EdgeInsets.all(32),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(icon, size: 48, color: context.bd.muted),
            const SizedBox(height: 14),
            Text(title, style: context.text.titleMedium, textAlign: TextAlign.center),
            if (message != null) ...[
              const SizedBox(height: 6),
              Text(
                message!,
                style: context.text.bodyMedium?.copyWith(color: context.colors.onSurfaceVariant),
                textAlign: TextAlign.center,
              ),
            ],
            if (action != null) ...[const SizedBox(height: 16), action!],
          ],
        ),
      ),
    );
  }
}

/// A small uppercase heading over a group, like the web UI's sidebar sections.
class SectionHeader extends StatelessWidget {
  const SectionHeader(this.text, {super.key, this.trailing, this.padding});

  final String text;
  final Widget? trailing;
  final EdgeInsetsGeometry? padding;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: padding ?? const EdgeInsets.fromLTRB(16, 20, 16, 6),
      child: Row(
        children: [
          Expanded(
            child: Text(
              text.toUpperCase(),
              style: context.text.labelSmall?.copyWith(
                color: context.bd.muted,
                fontWeight: FontWeight.w700,
                letterSpacing: 0.6,
              ),
            ),
          ),
          ?trailing,
        ],
      ),
    );
  }
}

/// "Label    value" in a details list; long-press copies the value.
class InfoRow extends StatelessWidget {
  const InfoRow(this.label, this.value, {super.key, this.mono = false, this.copyable = false, this.valueColor});

  final String label;
  final String value;
  final bool mono;
  final bool copyable;
  final Color? valueColor;

  @override
  Widget build(BuildContext context) {
    final row = Padding(
      padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 7),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          SizedBox(
            width: 112,
            child: Text(label, style: context.text.bodyMedium?.copyWith(color: context.colors.onSurfaceVariant)),
          ),
          Expanded(
            child: Text(
              value,
              style: (mono ? context.text.bodySmall?.copyWith(fontFamily: 'monospace') : context.text.bodyMedium)
                  ?.copyWith(color: valueColor),
            ),
          ),
        ],
      ),
    );
    if (!copyable) return row;
    return InkWell(
      onLongPress: () async {
        await Clipboard.setData(ClipboardData(text: value));
        if (context.mounted) showSnack(context, '$label copied');
      },
      child: row,
    );
  }
}

/// A number with a label, for summaries ("6,979,126 files").
class StatTile extends StatelessWidget {
  const StatTile({super.key, required this.label, required this.value, this.sub, this.color, this.icon});

  final String label;
  final String value;
  final String? sub;
  final Color? color;
  final IconData? icon;

  @override
  Widget build(BuildContext context) {
    return Card(
      child: Padding(
        padding: const EdgeInsets.all(14),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          mainAxisSize: MainAxisSize.min,
          children: [
            Row(
              children: [
                if (icon != null) ...[Icon(icon, size: 16, color: color ?? context.bd.muted), const SizedBox(width: 6)],
                Expanded(
                  child: FittedBox(
                    fit: BoxFit.scaleDown,
                    alignment: Alignment.centerLeft,
                    child: Text(
                      label,
                      style: context.text.labelMedium?.copyWith(color: context.colors.onSurfaceVariant),
                      maxLines: 1,
                    ),
                  ),
                ),
              ],
            ),
            const SizedBox(height: 6),
            // a number is shrunk to fit a narrow phone rather than cut short
            FittedBox(
              fit: BoxFit.scaleDown,
              alignment: Alignment.centerLeft,
              child: Text(
                value,
                style: context.text.titleLarge?.copyWith(fontWeight: FontWeight.w700, color: color),
                maxLines: 1,
              ),
            ),
            if (sub != null) ...[
              const SizedBox(height: 2),
              FittedBox(
                fit: BoxFit.scaleDown,
                alignment: Alignment.centerLeft,
                child: Text(sub!, style: context.text.bodySmall?.copyWith(color: context.bd.muted), maxLines: 1),
              ),
            ],
          ],
        ),
      ),
    );
  }
}

/// A small rounded label ("×3", "new", "failed").
class Pill extends StatelessWidget {
  const Pill(this.text, {super.key, this.color, this.icon});

  final String text;
  final Color? color;
  final IconData? icon;

  @override
  Widget build(BuildContext context) {
    final c = color ?? context.colors.onSurfaceVariant;
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 7, vertical: 2),
      decoration: BoxDecoration(color: c.withValues(alpha: 0.12), borderRadius: BorderRadius.circular(20)),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          if (icon != null) ...[Icon(icon, size: 12, color: c), const SizedBox(width: 3)],
          // a long label shrinks with an ellipsis rather than overflowing a narrow row
          Flexible(
            child: Text(
              text,
              style: context.text.labelSmall?.copyWith(color: c, fontWeight: FontWeight.w700),
              overflow: TextOverflow.ellipsis,
            ),
          ),
        ],
      ),
    );
  }
}

class StatusDot extends StatelessWidget {
  const StatusDot(this.color, {super.key, this.size = 9});
  final Color color;
  final double size;

  @override
  Widget build(BuildContext context) => Container(
    width: size,
    height: size,
    decoration: BoxDecoration(color: color, shape: BoxShape.circle),
  );
}

/// The icon for a file or folder, in its kind's colour.
class FileGlyph extends StatelessWidget {
  const FileGlyph({super.key, required this.name, this.isDir = false, this.size = 28});

  final String name;
  final bool isDir;
  final double size;

  @override
  Widget build(BuildContext context) {
    if (isDir) return Icon(Icons.folder, size: size, color: context.bd.folder);
    final k = FileKind.of(name);
    return Icon(k.icon, size: size, color: k.color);
  }
}

/// A thin labelled progress bar (a job, a drive's space).
class Meter extends StatelessWidget {
  const Meter({super.key, required this.fraction, this.color, this.height = 6});

  final double? fraction;
  final Color? color;
  final double height;

  @override
  Widget build(BuildContext context) {
    return ClipRRect(
      borderRadius: BorderRadius.circular(height),
      child: LinearProgressIndicator(
        value: fraction?.clamp(0, 1).toDouble(),
        minHeight: height,
        color: color ?? context.colors.primary,
        backgroundColor: context.colors.surfaceContainerHighest,
      ),
    );
  }
}

void showSnack(BuildContext context, String text) {
  ScaffoldMessenger.of(context)
    ..hideCurrentSnackBar()
    ..showSnackBar(SnackBar(content: Text(text)));
}
