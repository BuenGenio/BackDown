import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../../core/format.dart';
import '../../core/kinds.dart';
import '../../core/widgets.dart';
import '../../providers.dart';
import '../../theme.dart';
import 'common.dart';
import 'fallback.dart';
import 'image_view.dart';
import 'models.dart';
import 'routes.dart';

final slidesProvider = FutureProvider.autoDispose.family<SlideDeck, int>(
  (ref, id) => fetchParsed(requireApi(ref), 'slides', {'id': id}, SlideDeck.fromJson),
);

/// A presentation (pptx, odp) as the service reads it: each slide's title, text, pictures and
/// speaker notes, one card per slide.
class SlidesView extends ConsumerWidget {
  const SlidesView({super.key, required this.source});
  final ViewSource source;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final deck = ref.watch(slidesProvider(source.id));
    return switch (deck) {
      AsyncValue(:final value?) => _deck(context, ref, value),
      AsyncValue(:final error?) => viewerError(source, error, () => ref.invalidate(slidesProvider(source.id))),
      _ => const Center(child: CircularProgressIndicator()),
    };
  }

  Widget _deck(BuildContext context, WidgetRef ref, SlideDeck d) {
    if (d.slides.isEmpty) {
      return const EmptyState(
        icon: Icons.slideshow_outlined,
        title: 'No slides',
        message: 'BackDown found no slides in this presentation.',
      );
    }
    final api = ref.watch(apiProvider);
    return Column(
      children: [
        ViewerBar(meta: '${plural(d.count, 'slide')} · ${d.format.toUpperCase()}'),
        Expanded(
          child: Readable(
            child: ListView.builder(
              padding: const EdgeInsets.fromLTRB(12, 12, 12, 32),
              itemCount: d.slides.length + 1,
              itemBuilder: (c, i) {
                if (i == d.slides.length) {
                  return d.count > d.slides.length
                      ? ViewerNote('Showing the first ${d.slides.length} of ${plural(d.count, 'slide')}.')
                      : const SizedBox.shrink();
                }
                return _SlideCard(id: source.id, s: d.slides[i], showImages: api != null);
              },
            ),
          ),
        ),
      ],
    );
  }
}

class _SlideCard extends ConsumerWidget {
  const _SlideCard({required this.id, required this.s, required this.showImages});
  final int id;
  final Slide s;
  final bool showImages;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final api = ref.watch(apiProvider);
    return Padding(
      padding: const EdgeInsets.only(bottom: 12),
      child: Card(
        child: Padding(
          padding: const EdgeInsets.fromLTRB(16, 14, 16, 14),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Container(
                    padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 2),
                    decoration: BoxDecoration(
                      color: FileKind.slides.color.withValues(alpha: 0.14),
                      borderRadius: BorderRadius.circular(6),
                    ),
                    child: Text(
                      '${s.n}',
                      style: context.text.labelMedium?.copyWith(
                        color: FileKind.slides.color,
                        fontWeight: FontWeight.w700,
                      ),
                    ),
                  ),
                  const SizedBox(width: 10),
                  Expanded(
                    child: Text(
                      s.title == null || s.title!.isEmpty ? 'Slide ${s.n}' : s.title!,
                      style: context.text.titleMedium?.copyWith(
                        color: s.title == null || s.title!.isEmpty ? context.bd.muted : null,
                      ),
                    ),
                  ),
                ],
              ),
              if (s.paras.isNotEmpty) ...[
                const SizedBox(height: 8),
                for (final (t, lvl) in s.paras)
                  Padding(
                    padding: EdgeInsets.only(left: 4 + 16.0 * lvl.clamp(0, 8), top: 3),
                    child: Row(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Text(
                          lvl == 0 ? '•  ' : '–  ',
                          style: context.text.bodyMedium?.copyWith(color: context.bd.muted),
                        ),
                        Expanded(child: Text(t, style: context.text.bodyMedium)),
                      ],
                    ),
                  ),
              ],
              if (s.images.isNotEmpty) ...[
                const SizedBox(height: 10),
                Wrap(
                  spacing: 8,
                  runSpacing: 8,
                  children: [
                    for (final name in s.images)
                      if (showImages && api != null && isDecodableImage(name))
                        InkWell(
                          onTap: () => context.push(innerLocation(InnerFile.entry(id: id, name: name))),
                          borderRadius: BorderRadius.circular(8),
                          child: ClipRRect(
                            borderRadius: BorderRadius.circular(8),
                            child: Container(
                              width: 120,
                              height: 80,
                              color: context.colors.surfaceContainerHigh,
                              child: Image(
                                image: ResizeImage(
                                  InnerImage(api, InnerFile.entry(id: id, name: name)),
                                  width: 360,
                                  policy: ResizeImagePolicy.fit,
                                ),
                                fit: BoxFit.cover,
                                errorBuilder: (c, e, st) => Center(child: FileGlyph(name: name, size: 28)),
                              ),
                            ),
                          ),
                        )
                      else
                        Chip(
                          label: Text(InnerFile.entry(id: id, name: name).base),
                        ),
                  ],
                ),
              ],
              if (s.notes.isNotEmpty) ...[
                const SizedBox(height: 10),
                Container(
                  width: double.infinity,
                  padding: const EdgeInsets.all(10),
                  decoration: BoxDecoration(
                    color: context.colors.surfaceContainer,
                    borderRadius: BorderRadius.circular(8),
                  ),
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text('Notes', style: context.text.labelMedium?.copyWith(fontWeight: FontWeight.w700)),
                      const SizedBox(height: 2),
                      Text(s.notes, style: context.text.bodySmall),
                    ],
                  ),
                ),
              ],
              if (s.cut)
                Padding(
                  padding: const EdgeInsets.only(top: 8),
                  child: Text(
                    'This deck has more text than the viewer shows at once; this slide\'s is left out.',
                    style: context.text.bodySmall?.copyWith(color: context.bd.muted),
                  ),
                ),
            ],
          ),
        ),
      ),
    );
  }
}
