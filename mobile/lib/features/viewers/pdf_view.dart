import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:pdfx/pdfx.dart';

import '../../api/api.dart';
import '../../core/format.dart';
import '../../providers.dart';
import '../../theme.dart';
import 'common.dart';
import 'fallback.dart';

/// Where pdfx renders (with the platform's own PDF renderer: Android's PdfRenderer, iOS's CGPDF).
bool get pdfShowsHere => !kIsWeb && (Platform.isAndroid || Platform.isIOS);

/// The most of a PDF fetched to show it.
const pdfMax = 200 << 20;

/// A PDF's pages, to scroll through and pinch-zoom.
class PdfView extends ConsumerStatefulWidget {
  const PdfView({super.key, required this.source});
  final ViewSource source;

  @override
  ConsumerState<PdfView> createState() => _PdfViewState();
}

class _PdfViewState extends ConsumerState<PdfView> {
  PdfControllerPinch? _c;
  Object? _error;
  int _page = 1, _pages = 0;

  bool get _tooBig =>
      (widget.source.size ?? 0) > pdfMax ||
      (widget.source.inner != null && !widget.source.inner!.streams && (widget.source.size ?? 0) > entryMax);

  @override
  void initState() {
    super.initState();
    final api = ref.read(apiProvider);
    if (api == null || !pdfShowsHere || _tooBig) return;
    _c = PdfControllerPinch(document: PdfDocument.openData(widget.source.bytes(api)));
  }

  @override
  void dispose() {
    _c?.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    if (!pdfShowsHere) {
      return FallbackView(
        source: widget.source,
        title: 'PDFs show in the app on Android and iOS',
        message: 'Open it in another app here.',
        icon: Icons.picture_as_pdf_outlined,
      );
    }
    if (_tooBig) {
      return FallbackView(
        source: widget.source,
        title: 'Too big to show here',
        message: 'At ${bytes(widget.source.size)}, open it in another app instead.',
        icon: Icons.picture_as_pdf_outlined,
      );
    }
    if (_error != null) {
      final e = _error;
      return FallbackView(
        source: widget.source,
        title: 'Can\'t show this PDF',
        message: e is ApiException ? e.message : 'It may be damaged or protected with a password.',
        icon: Icons.error_outline,
      );
    }
    return Column(
      children: [
        ViewerBar(meta: _pages == 0 ? 'Opening…' : 'Page $_page of $_pages'),
        Expanded(
          child: PdfViewPinch(
            controller: _c!,
            padding: 8,
            backgroundDecoration: BoxDecoration(color: context.colors.surfaceContainerHigh),
            onDocumentLoaded: (d) => setState(() => _pages = d.pagesCount),
            onPageChanged: (p) => setState(() => _page = p),
            onDocumentError: (e) => setState(() => _error = e),
            onInteractionEnd: (_) => ViewerPage.of(context)?.onZoom(_c!.value.getMaxScaleOnAxis() > 1.01),
          ),
        ),
      ],
    );
  }
}
