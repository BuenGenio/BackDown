import 'package:flutter/widgets.dart';
import 'package:go_router/go_router.dart';

import '../../models/file_ref.dart';

/// What the viewer route gets as `extra`: the file and the others it can step to (a folder's
/// files, a search's results), in order.
class ViewerArgs {
  const ViewerArgs(this.file, [this.siblings = const []]);
  final FileRef file;
  final List<FileRef> siblings;
}

/// Opens [file] in the viewer that suits it, with [siblings] to swipe through.
Future<void> openFile(BuildContext context, FileRef file, {List<FileRef> siblings = const []}) =>
    context.push('/view/${file.id}', extra: ViewerArgs(file, siblings));
