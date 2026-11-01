import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';
import 'package:path_provider/path_provider.dart';
import 'package:share_plus/share_plus.dart';
import 'package:url_launcher/url_launcher.dart';

import '../../api/json.dart';
import '../../core/actions.dart';
import '../../core/format.dart';
import '../../core/widgets.dart';
import '../../providers.dart';
import '../../theme.dart';
import 'common.dart';
import 'data.dart';
import 'markdown.dart';
import 'models.dart';
import 'routes.dart';

/// This BackDown: the service's version and process, the index in numbers, updates, the edition
/// and licence with what each feature unlocks, how scans are made, the report and the chain of
/// custody.
class ServerScreen extends ConsumerWidget {
  const ServerScreen({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final server = ref.watch(currentServerProvider);
    return Scaffold(
      appBar: AppBar(title: const Text('This BackDown')),
      body: RefreshIndicator(
        onRefresh: () async {
          ref.invalidate(indexStatusProvider);
          ref.invalidate(updateProvider);
          ref.invalidate(scanSettingsProvider);
          await ref.refresh(featuresProvider.future).catchError((_) => Features.fromJson(const {}));
        },
        child: LayoutBuilder(
          builder: (c, box) => ListView(
            physics: const AlwaysScrollableScrollPhysics(),
            padding: EdgeInsets.fromLTRB(sidePad(box.maxWidth), 0, sidePad(box.maxWidth), 32),
            children: [
              _Head(name: server?.name ?? '', address: server?.demo ?? false ? 'Built-in examples' : server?.address),
              const _Service(),
              const _Index(),
              const _Updates(),
              const _Edition(),
              const _ScanSettings(),
              const SectionHeader('Records'),
              ListTile(
                leading: const Icon(Icons.summarize_outlined),
                title: const Text('Report'),
                subtitle: const Text('Every drive, what\'s on it, how much exists elsewhere, and the duplicates'),
                trailing: const Icon(Icons.chevron_right),
                onTap: () => context.push(reportRoute),
              ),
              ListTile(
                leading: const Icon(Icons.receipt_long_outlined),
                title: const Text('Chain of custody'),
                subtitle: const Text(
                  'Every copy, move, deletion, link and merge BackDown made, as a signed, hash-chained export anyone can '
                  'verify',
                ),
                trailing: const Icon(Icons.share_outlined),
                onTap: () => shareDocument(
                  context,
                  ref,
                  'custody',
                  const {},
                  custodyFileName(DateTime.now()),
                  title: 'BackDown chain of custody',
                ),
              ),
              const SectionHeader('Links'),
              Wrap(
                spacing: 8,
                runSpacing: 8,
                children: [
                  for (final (label, url) in const [
                    ('backdown.io', 'https://backdown.io'),
                    ('All releases', 'https://github.com/BuenGenio/backdown-releases/releases'),
                    ('Pricing', 'https://backdown.io/pricing'),
                  ])
                    ActionChip(
                      avatar: const Icon(Icons.open_in_new, size: 16),
                      label: Text(label),
                      onPressed: () => launchUrl(Uri.parse(url), mode: LaunchMode.externalApplication),
                    ),
                ],
              ).padded(),
            ],
          ),
        ),
      ),
    );
  }
}

extension on Widget {
  Widget padded() => Padding(padding: const EdgeInsets.symmetric(horizontal: 16), child: this);
}

class _Head extends ConsumerWidget {
  const _Head({required this.name, this.address});
  final String name;
  final String? address;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final d = ref.watch(daemonProvider).value;
    final f = ref.watch(featuresProvider).value;
    final version = d?.version ?? f?.version;
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 8, 16, 0),
      child: Card(
        child: Padding(
          padding: const EdgeInsets.all(16),
          child: Row(
            children: [
              CircleAvatar(
                radius: 24,
                backgroundColor: context.colors.primaryContainer,
                child: Image.asset('assets/icon/mark.png', height: 24, color: context.colors.primary),
              ),
              const SizedBox(width: 14),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      version == null ? 'BackDown' : 'BackDown $version',
                      style: context.text.titleLarge?.copyWith(fontWeight: FontWeight.w700),
                    ),
                    const SizedBox(height: 2),
                    Muted([name, ?address].where((s) => s.isNotEmpty).join(' · ')),
                    if (f != null) ...[
                      const SizedBox(height: 6),
                      Wrap(
                        spacing: 6,
                        runSpacing: 4,
                        children: [
                          Tag(f.tierName, color: context.colors.primary, icon: Icons.workspace_premium_outlined),
                          if (f.preview) Tag('preview', color: context.bd.info),
                          if (!canChange(ref)) Tag('look only', icon: Icons.visibility_outlined),
                        ],
                      ),
                    ],
                  ],
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _Service extends ConsumerWidget {
  const _Service();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final d = ref.watch(daemonProvider);
    final skew = ref.watch(skewProvider);
    final s = d.value;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        const SectionHeader('The service'),
        if (s == null && d.hasError)
          ErrorPanel(error: d.error!, compact: true)
        else if (s == null)
          const Padding(padding: EdgeInsets.all(16), child: LinearProgressIndicator())
        else if (!s.running)
          const InfoRow(
            'Background service',
            'Not running: this is `backdown serve`, which shows the index; scans run from the command line.',
          )
        else ...[
          InfoRow(
            'Running since',
            '${when(s.startedAt)} (${rel(s.startedAt, skew: skew)})${s.pid == null ? '' : ' · process ${s.pid}'}',
          ),
          if (s.db != null) InfoRow('Index', s.db!, mono: true, copyable: true),
          InfoRow(
            'Rescans',
            s.rescanEverySecs == null || s.rescanEverySecs == 0
                ? 'When you ask, or when a drive comes back'
                : 'Each drive every ${_every(s.rescanEverySecs!)}',
          ),
          if (s.checkedAt != null) InfoRow('Drives checked', rel(s.checkedAt, skew: skew)),
          InfoRow('Drives', plural(s.drives.length, 'drive')),
        ],
      ],
    );
  }
}

/// 86400 → "day", 172800 → "2 days", 21600 → "6 hours", as the web UI's settings say it.
String _every(int s) => s % 86400 == 0
    ? (s == 86400 ? 'day' : '${s ~/ 86400} days')
    : s % 3600 == 0
    ? (s == 3600 ? 'hour' : '${s ~/ 3600} hours')
    : '${(s / 60).round()} minutes';

class _Index extends ConsumerWidget {
  const _Index();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final st = ref.watch(indexStatusProvider);
    final v = st.value;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        const SectionHeader('The index'),
        if (v == null && st.hasError)
          ErrorPanel(error: st.error!, compact: true, onRetry: () => ref.invalidate(indexStatusProvider))
        else if (v == null)
          const Padding(padding: EdgeInsets.all(16), child: LinearProgressIndicator())
        else
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 16),
            child: LayoutBuilder(
              builder: (c, box) {
                final cols = box.maxWidth >= 600 ? 4 : 2;
                final w = (box.maxWidth - 10 * (cols - 1)) / cols;
                final tiles = [
                  StatTile(label: 'Files', value: count(v.files), icon: Icons.description_outlined),
                  StatTile(label: 'Size', value: bytes(v.bytes), icon: Icons.data_usage),
                  StatTile(
                    label: 'Hashed',
                    value: percent(v.files == 0 ? null : v.hashed / v.files),
                    icon: Icons.tag,
                    sub: '${count(v.hashed)} files',
                  ),
                  StatTile(
                    label: 'Versions kept',
                    value: count(v.versions),
                    icon: Icons.history,
                    sub: plural(v.scans, 'scan'),
                  ),
                ];
                // rows of tiles as tall as their tallest
                return Column(
                  children: [
                    for (var i = 0; i < tiles.length; i += cols) ...[
                      if (i > 0) const SizedBox(height: 10),
                      IntrinsicHeight(
                        child: Row(
                          crossAxisAlignment: CrossAxisAlignment.stretch,
                          children: [
                            for (var j = i; j < i + cols && j < tiles.length; j++) ...[
                              if (j > i) const SizedBox(width: 10),
                              SizedBox(width: w, child: tiles[j]),
                            ],
                          ],
                        ),
                      ),
                    ],
                  ],
                );
              },
            ),
          ),
        if (v != null && v.unreadable > 0)
          Padding(
            padding: const EdgeInsets.fromLTRB(16, 8, 16, 0),
            child: Muted('${plural(v.unreadable, 'file')} couldn\'t be read when last tried.'),
          ),
      ],
    );
  }
}

class _Updates extends ConsumerWidget {
  const _Updates();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final u = ref.watch(updateProvider);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        const SectionHeader('Updates'),
        LibraryBody(
          value: u,
          onRetry: () => ref.invalidate(updateProvider),
          data: (s) {
            final line = s.newer && s.latestVersion != null
                ? 'BackDown ${s.latestVersion} is available.'
                : s.checkedAt != null
                ? 'This is the newest ${s.channel == 'stable' ? 'release' : 'version, previews included'} (checked '
                      '${when(s.checkedAt)}).'
                : 'Not checked yet.';
            return Padding(
              padding: const EdgeInsets.symmetric(horizontal: 16),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text('This is BackDown ${s.current}', style: context.text.titleSmall),
                  const SizedBox(height: 2),
                  Text(
                    line,
                    style: context.text.bodyMedium?.copyWith(
                      color: s.newer ? context.colors.primary : null,
                      fontWeight: s.newer ? FontWeight.w600 : null,
                    ),
                  ),
                  if (s.error != null) ...[const SizedBox(height: 4), Muted(s.error!, color: context.bd.crit)],
                  if (s.installState == 'failed') ...[
                    const SizedBox(height: 4),
                    Muted('The last update didn\'t install: ${s.installMessage}', color: context.bd.crit),
                  ],
                  if (s.installState == 'running') ...[
                    const SizedBox(height: 4),
                    Muted('Updating: ${s.installMessage}…', color: context.bd.info),
                  ],
                  const SizedBox(height: 4),
                  Muted(
                    [
                      s.channel == 'stable' ? 'Offers releases only' : 'Offers previews too',
                      s.checkDaily ? 'looks once a day' : 'looks only when asked',
                    ].join(' · '),
                  ),
                  if (!s.canInstall && s.whyNot != null) ...[const SizedBox(height: 6), Muted(s.whyNot!)],
                  const SizedBox(height: 10),
                  Wrap(
                    spacing: 8,
                    runSpacing: 8,
                    children: [
                      if (s.newer && s.canInstall)
                        FilledButton.icon(
                          onPressed: () => runAction(
                            context,
                            ref,
                            (api) => api.post('update-install'),
                            done: 'Updating: BackDown\'s service restarts with it',
                          ),
                          icon: const Icon(Icons.system_update_alt, size: 18),
                          label: const Text('Update now'),
                        ),
                      if (s.notesUrl != null && s.newer)
                        OutlinedButton.icon(
                          onPressed: () => launchUrl(Uri.parse(s.notesUrl!), mode: LaunchMode.externalApplication),
                          icon: const Icon(Icons.open_in_new, size: 18),
                          label: const Text('Release notes'),
                        ),
                      OutlinedButton.icon(
                        onPressed: s.blockedUntil != null
                            ? null
                            : () async {
                                final r = await runAction(context, ref, (api) => api.post('update-check'));
                                if (r != null) ref.invalidate(updateProvider);
                              },
                        icon: const Icon(Icons.refresh, size: 18),
                        label: const Text('Check now'),
                      ),
                    ],
                  ),
                ],
              ),
            );
          },
        ),
      ],
    );
  }
}

class _Edition extends ConsumerWidget {
  const _Edition();

  static const _tiers = ['community', 'pro', 'team', 'forensic'];

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final features = ref.watch(featuresProvider);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        const SectionHeader('Edition and licence'),
        LibraryBody(
          value: features,
          onRetry: () => ref.invalidate(featuresProvider),
          data: (f) {
            final l = f.licence;
            (Color, String) state(Feature x) => !x.ready
                ? (context.bd.muted, 'coming')
                : x.tier == 'community'
                ? (context.bd.good, 'free')
                : !x.allowed
                ? (context.bd.warn, 'needs ${x.tierName}')
                : f.preview && f.tier == 'community'
                ? (context.bd.info, 'free in the preview')
                : (context.bd.good, 'unlocked');
            return Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                if (f.preview)
                  Container(
                    margin: const EdgeInsets.fromLTRB(16, 0, 16, 8),
                    padding: const EdgeInsets.all(12),
                    decoration: BoxDecoration(
                      color: context.bd.info.withValues(alpha: 0.12),
                      borderRadius: BorderRadius.circular(10),
                    ),
                    child: const Text.rich(
                      TextSpan(
                        children: [
                          TextSpan(
                            text: 'Pro features are free during the preview. ',
                            style: TextStyle(fontWeight: FontWeight.w700),
                          ),
                          TextSpan(
                            text:
                                'Nothing is locked while BackDown is a preview release; a licence key added now '
                                'carries over to the release.',
                          ),
                        ],
                      ),
                    ),
                  ),
                InfoRow(
                  'Edition',
                  'BackDown ${f.tierName}: ${f.preview
                      ? 'everything is unlocked.'
                      : f.tier == 'community'
                      ? 'Pro features need a licence key.'
                      : 'your licence unlocks ${f.tierName} features.'}',
                ),
                if (f.licenceProblem != null)
                  InfoRow('Licence', f.licenceProblem!, valueColor: context.bd.crit)
                else if (l == null)
                  const InfoRow('Licence', 'None added')
                else ...[
                  InfoRow('Licence', '${l.id} · ${_cap(l.product)}', mono: true, copyable: true),
                  InfoRow('For', l.name == null ? l.email : '${l.name} <${l.email}>'),
                  InfoRow(l.product == 'forensic' ? 'Sites' : 'Machines', '${l.seats}'),
                  InfoRow('Issued', l.issued),
                  InfoRow(
                    'Expires',
                    l.expires ??
                        (l.kind == 'preorder'
                            ? 'A pre-order: your first year starts with the release that ships Pro'
                            : 'Doesn\'t expire'),
                  ),
                ],
                if (f.licenceFile.isNotEmpty) InfoRow('Kept in', f.licenceFile, mono: true, copyable: true),
                Padding(
                  padding: const EdgeInsets.fromLTRB(16, 4, 16, 0),
                  child: Muted(
                    'A licence key is added on the computer (in Settings there, or with `backdown license add <key>`), '
                    'and checked there; nothing is sent anywhere.',
                  ),
                ),
                for (final t in _tiers)
                  if (f.features.where((x) => x.tier == t).toList() case final list when list.isNotEmpty) ...[
                    SectionHeader(
                      list.first.tierName,
                      padding: const EdgeInsets.fromLTRB(16, 16, 16, 2),
                      trailing: Text('${list.length}', style: context.text.labelMedium),
                    ),
                    for (final x in list)
                      ListTile(
                        dense: true,
                        title: Text(x.name, style: context.text.bodyMedium?.copyWith(fontWeight: FontWeight.w600)),
                        subtitle: Text(x.unlocks),
                        trailing: Tag(state(x).$2, color: state(x).$1),
                      ),
                  ],
                Padding(
                  padding: const EdgeInsets.fromLTRB(16, 8, 16, 0),
                  child: Align(
                    alignment: Alignment.centerLeft,
                    child: OutlinedButton.icon(
                      onPressed: () => launchUrl(Uri.parse(f.pricing), mode: LaunchMode.externalApplication),
                      icon: const Icon(Icons.open_in_new, size: 18),
                      label: const Text('Pricing'),
                    ),
                  ),
                ),
              ],
            );
          },
        ),
      ],
    );
  }

  static String _cap(String s) => s.isEmpty ? s : '${s[0].toUpperCase()}${s.substring(1)}';
}

class _ScanSettings extends ConsumerWidget {
  const _ScanSettings();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final v = ref.watch(scanSettingsProvider);
    final s = v.value;
    if (v.hasValue && s == null) return const SizedBox.shrink();
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        const SectionHeader('Fast rescans'),
        if (s == null)
          v.hasError
              ? ErrorPanel(error: v.error!, compact: true, onRetry: () => ref.invalidate(scanSettingsProvider))
              : const Padding(padding: EdgeInsets.all(16), child: LinearProgressIndicator())
        else
          ..._fields(context, s),
        const LookOnlyNote(what: 'Settings'),
      ],
    );
  }

  /// The settings as the endpoint describes them (labels and help come with it), read-only.
  List<Widget> _fields(BuildContext context, Json data) {
    final obj = data.objOrNull('settings') ?? data;
    final labels = data.obj('labels'), help = data.obj('help');
    String label(String k) =>
        labels.strOrNull(k) ?? '${k.replaceAll('_', ' ')[0].toUpperCase()}${k.replaceAll('_', ' ').substring(1)}';
    const info = ['why', 'note', 'error'];
    return [
      for (final k in info)
        if (obj[k] is String)
          Padding(padding: const EdgeInsets.fromLTRB(16, 0, 16, 8), child: Muted(obj[k]! as String)),
      for (final MapEntry(:key, :value) in obj.entries)
        if (!info.contains(key) &&
            !['labels', 'help', 'ok', 'available'].contains(key) &&
            (value is bool || value is num || value is String))
          ListTile(
            dense: true,
            title: Text(label(key)),
            subtitle: help.strOrNull(key) == null ? null : Text(help.str(key)),
            trailing: value is bool
                ? Icon(
                    value ? Icons.check_circle : Icons.cancel_outlined,
                    color: value ? context.bd.good : context.bd.muted,
                  )
                : Text('$value', style: context.text.titleSmall),
          ),
    ];
  }
}

/// The report, as the service writes it: every drive, what's on it, how much exists elsewhere,
/// and the duplicates. Shareable as the Markdown file.
class ReportScreen extends ConsumerWidget {
  const ReportScreen({super.key});

  Future<void> _share(BuildContext context, String md) async {
    try {
      final dir = Directory('${(await getTemporaryDirectory()).path}/documents');
      await dir.create(recursive: true);
      final f = File('${dir.path}/backdown-report.md');
      await f.writeAsString(md);
      await SharePlus.instance.share(ShareParams(files: [XFile(f.path)], title: 'BackDown report'));
    } on Object catch (e) {
      if (context.mounted) showSnack(context, 'Couldn\'t share the report: $e');
    }
  }

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final report = ref.watch(reportProvider);
    final md = report.value;
    return Scaffold(
      appBar: AppBar(
        title: const Text('Report'),
        actions: [
          if (md != null) ...[
            IconButton(
              tooltip: 'Copy as Markdown',
              icon: const Icon(Icons.copy),
              onPressed: () => copyText(context, md, 'Report'),
            ),
            IconButton(tooltip: 'Share', icon: const Icon(Icons.share_outlined), onPressed: () => _share(context, md)),
          ],
        ],
      ),
      body: RefreshIndicator(
        onRefresh: () => ref.refresh(reportProvider.future).catchError((_) => ''),
        child: LibraryBody(
          value: report,
          onRetry: () => ref.invalidate(reportProvider),
          loading: const _Making(),
          data: (md) => LayoutBuilder(
            builder: (c, box) => ReportView(
              markdown: md,
              padding: EdgeInsets.fromLTRB(16 + sidePad(box.maxWidth), 8, 16 + sidePad(box.maxWidth), 32),
            ),
          ),
        ),
      ),
    );
  }
}

class _Making extends StatelessWidget {
  const _Making();

  @override
  Widget build(BuildContext context) => const Center(
    child: Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        CircularProgressIndicator(),
        SizedBox(height: 16),
        Text('Making the report: on a big index this takes a while'),
      ],
    ),
  );
}
