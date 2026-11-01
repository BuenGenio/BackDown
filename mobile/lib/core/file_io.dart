import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:open_file/open_file.dart';
import 'package:path_provider/path_provider.dart';
import 'package:share_plus/share_plus.dart';

import '../api/api.dart';
import '../models/file_ref.dart';
import 'actions.dart';
import 'format.dart';
import 'widgets.dart';

/// Downloads a file from a drive into this app's cache (shown with a progress dialog that can be
/// cancelled), for sharing or opening in another app. Null if it failed or was cancelled. A file
/// inside a container (an archive's entry, a mail's attachment) comes from its own [endpoint] with
/// its [params], cached under [cacheKey].
Future<File?> fetchToCache(
  BuildContext context,
  BackDownApi api,
  FileRef file, {
  String endpoint = 'content',
  Map<String, Object?>? params,
  String? cacheKey,
}) async {
  final dir = Directory('${(await getTemporaryDirectory()).path}/files/${cacheKey ?? file.id}');
  await dir.create(recursive: true);
  final safe = file.name.replaceAll(RegExp(r'[/\\:]'), '_');
  final target = File('${dir.path}/$safe');
  if (await target.exists() && (file.size == 0 || await target.length() == file.size)) return target;
  if (!context.mounted) return null;

  final progress = ValueNotifier<(int, int?)>((0, file.size == 0 ? null : file.size));
  var cancelled = false;
  var open = true;
  final navigator = Navigator.of(context, rootNavigator: true);
  void close() {
    if (open) navigator.pop();
    open = false;
  }

  final dialog = showDialog<void>(
    context: context,
    barrierDismissible: false,
    builder: (c) => AlertDialog(
      title: Text(file.name, maxLines: 2, overflow: TextOverflow.ellipsis),
      content: ValueListenableBuilder(
        valueListenable: progress,
        builder: (c, p, _) {
          final (got, total) = p;
          return Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Meter(fraction: total == null || total == 0 ? null : got / total),
              const SizedBox(height: 10),
              Text(total == null ? bytes(got) : '${bytes(got)} of ${bytes(total)}'),
            ],
          );
        },
      ),
      actions: [
        TextButton(
          onPressed: () {
            cancelled = true;
            close();
          },
          child: const Text('Cancel'),
        ),
      ],
    ),
  );
  unawaited(
    dialog.whenComplete(() {
      open = false;
      progress.dispose();
    }),
  );
  try {
    final f = await api.download(
      endpoint,
      params ?? {'id': file.id},
      target,
      onProgress: (g, t) {
        if (!cancelled) progress.value = (g, t);
      },
      cancelled: () => cancelled,
    );
    close();
    return cancelled ? null : f;
  } on ApiException catch (e) {
    close();
    if (!cancelled && context.mounted) await showActionError(context, e);
    return null;
  } finally {
    // no more progress into a dialog that's going (it disposes the notifier when it's gone)
    cancelled = true;
  }
}

/// The phone's share sheet for a file from a drive.
Future<void> shareFile(BuildContext context, BackDownApi api, FileRef file) async {
  final f = await fetchToCache(context, api, file);
  if (f == null) return;
  await SharePlus.instance.share(ShareParams(files: [XFile(f.path)], title: file.name));
}

/// Opens a file from a drive in another app on the phone.
Future<void> openInAnotherApp(BuildContext context, BackDownApi api, FileRef file) async {
  final f = await fetchToCache(context, api, file);
  if (f == null) return;
  final r = await OpenFile.open(f.path);
  if (r.type != ResultType.done && context.mounted) {
    showSnack(context, r.type == ResultType.noAppToOpen ? 'No app here opens ${file.name}.' : r.message);
  }
}
