import 'package:backdown_mobile/api/api.dart';
import 'package:backdown_mobile/api/demo_api.dart';
import 'package:backdown_mobile/features/files/folder_screen.dart';
import 'package:backdown_mobile/features/library/common.dart';
import 'package:backdown_mobile/features/library/routes.dart';
import 'package:backdown_mobile/router.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import '../support/harness.dart';

/// The demo, with some answers replaced (a Community service, other kinds of rescue…).
class _Api extends DemoBackDownApi {
  _Api(this.answers);
  final Map<String, Object? Function(Map<String, Object?> params)> answers;

  @override
  Future<Object?> get(String endpoint, [Map<String, Object?> params = const {}, Duration? timeout]) async {
    final a = answers[endpoint];
    return a != null ? a(params) : super.get(endpoint, params, timeout);
  }
}

ApiException _locked(String name, String unlocks) => ApiException(
  ApiFailure.needsTier,
  '$name is part of BackDown Pro.',
  status: 402,
  detail: {
    'error': '$name is part of BackDown Pro.',
    'needs': 'pro',
    'needs_name': 'Pro',
    'name': name,
    'unlocks': unlocks,
    'pricing': 'https://backdown.io/pricing',
  },
);

Finder _scrollable() => find.byType(Scrollable).first;

void main() {
  group("What's here", () {
    testWidgets('groups what\'s recognised by kind, with each drive', (tester) async {
      final c = await pumpBackDown(tester);
      await goTo(tester, c, Routes.entities);
      expect(find.text('Everything  10'), findsOneWidget);
      expect(find.text('MAILBOX (MAILDIR)'), findsOneWidget);
      expect(find.text('sam@example.org: 7 messages'), findsOneWidget);
      // where it came from, and the drives with what's regenerable on them
      expect(find.textContaining('Copied from mail.example.org'), findsOneWidget);
      expect(find.text('9 KB regenerable'), findsOneWidget);
      expect(find.text('WD Passport'), findsOneWidget);
    });

    testWidgets('filters by kind and by what\'s typed', (tester) async {
      final c = await pumpBackDown(tester);
      await goTo(tester, c, Routes.entities);
      await tester.ensureVisible(find.text('Database dump  1'));
      await tester.tap(find.text('Database dump  1'));
      await tester.pumpAndSettle();
      expect(find.text('MySQL dump of shop, 31 Dec 2018'), findsOneWidget);
      expect(find.text('sam@example.org: 7 messages'), findsNothing);

      await tester.ensureVisible(find.text('Everything  10'));
      await tester.tap(find.text('Everything  10'));
      await tester.enterText(find.byType(TextField), 'itunes');
      await tester.pumpAndSettle();
      expect(find.text("Sam's iPhone (iPhone 8, iOS 12.4)"), findsOneWidget);
      expect(find.text('MySQL dump of shop, 31 Dec 2018'), findsNothing);

      await tester.enterText(find.byType(TextField), 'nothing like this');
      await tester.pumpAndSettle();
      expect(find.text('Nothing matches the filter'), findsOneWidget);
    });

    testWidgets('a drive narrows the list to what\'s on it', (tester) async {
      final c = await pumpBackDown(tester);
      await goTo(tester, c, Routes.entities);
      await tester.tap(find.text('sam'));
      await tester.pumpAndSettle();
      await tester.scrollUntilVisible(find.text('LINUX HOME FOLDER'), 300, scrollable: _scrollable());
      expect(find.text("sam's home folder"), findsOneWidget);
      expect(find.text('sam@example.org: 7 messages'), findsNothing);
    });

    testWidgets('a thing\'s path opens its folder', (tester) async {
      final c = await pumpBackDown(tester);
      await goTo(tester, c, Routes.entities);
      await tester.tap(find.textContaining('Mail/​mail.example.org'));
      await tester.pumpAndSettle();
      expect(find.byType(FolderScreen), findsOneWidget);
      expect(find.text('cur'), findsOneWidget);
    });

    testWidgets('a thing opens with its facts, where it came from and what\'s regenerable', (tester) async {
      final c = await pumpBackDown(tester);
      await goTo(tester, c, Routes.entities);
      await tester.tap(find.text('sam'));
      await tester.pumpAndSettle();
      await tester.scrollUntilVisible(find.text("sam's home folder"), 300, scrollable: _scrollable());
      await tester.tap(find.text("sam's home folder"));
      await tester.pumpAndSettle();
      expect(find.text('WHERE IT CAME FROM'), findsOneWidget);
      expect(find.text('Nothing on the drive says where it came from.'), findsOneWidget);
      final sheet = find.byType(Scrollable).last;
      await tester.scrollUntilVisible(find.text('thumbnail cache'), 200, scrollable: sheet);
      expect(find.text('INSIDE IT'), findsOneWidget);
      // a regenerable folder opens where it is
      await tester.drag(sheet, const Offset(0, -200));
      await tester.pumpAndSettle();
      await tester.tap(find.text('thumbnail cache'));
      await tester.pumpAndSettle();
      expect(find.byType(FolderScreen), findsOneWidget);
      expect(find.text('IMG_5100.JPG'), findsOneWidget);
    });

    testWidgets('"Recognise again" from a phone says it needs the computer', (tester) async {
      final c = await pumpBackDown(tester);
      await goTo(tester, c, Routes.entities);
      await tester.tap(find.byType(PopupMenuButton<String>));
      await tester.pumpAndSettle();
      await tester.tap(find.text('Recognise again'));
      await tester.pumpAndSettle();
      expect(find.text('Needs someone at the computer'), findsOneWidget);
    });

    testWidgets('on Community, where things came from is explained, not shown', (tester) async {
      final demo = DemoBackDownApi();
      final c = await pumpBackDown(
        tester,
        api: _Api({
          'entities': (p) async {
            final v = (await demo.get('entities'))! as Map<String, Object?>;
            for (final r in v['roots']! as List) {
              for (final e in (r as Map)['entities'] as List) {
                final m = e as Map<String, Object?>;
                if ((m['provenance']! as List).isNotEmpty) {
                  m['provenance'] = <String>[];
                  m['provenance_locked'] = true;
                }
              }
            }
            return v;
          },
          'overlaps': (p) => throw _locked('Dedup across every source', 'How much of each drive…'),
        }),
      );
      await goTo(tester, c, Routes.entities);
      expect(find.text('Where it came from is part of BackDown Pro.'), findsWidgets);
      expect(find.textContaining('Copied from'), findsNothing);
      expect(find.textContaining('exists elsewhere'), findsNothing);
    });
  });

  group('Duplicates', () {
    testWidgets('the groups that free the most first, and "Show more"', (tester) async {
      final c = await pumpBackDown(tester);
      await goTo(tester, c, Routes.dupes);
      expect(find.text('could be freed across 23 groups of identical files'), findsOneWidget);
      expect(find.text('IMG_3409.JPG'), findsOneWidget);
      await tester.scrollUntilVisible(find.text('Show 3 more'), 500, scrollable: _scrollable());
      expect(find.text('The 20 groups that free the most are here.'), findsOneWidget);
      await tester.tap(find.text('Show 3 more'));
      await tester.pumpAndSettle();
      expect(find.text('Show 3 more'), findsNothing);
      await tester.scrollUntilVisible(
        find.text('HERE ONCE, AND AGAIN ON ANOTHER MACHINE'),
        500,
        scrollable: _scrollable(),
      );
      expect(find.text('IMG_3411.JPG'), findsOneWidget);
    });

    testWidgets('a group opens to its copies, here and on other machines', (tester) async {
      final c = await pumpBackDown(tester);
      await goTo(tester, c, Routes.dupes);
      await tester.tap(find.text('IMG_2108.JPG'));
      await tester.pumpAndSettle();
      expect(find.textContaining('Seagate Expansion/​Photos/​2014'), findsOneWidget);
      expect(find.textContaining('WD Passport/​Photos backup/​2014'), findsOneWidget);
      expect(find.text('On nextcloud'), findsOneWidget);
      // a copy's folder, with it picked out
      await tester.tap(find.byTooltip('Show in folder').first);
      await tester.pumpAndSettle();
      expect(find.byType(FolderScreen), findsOneWidget);
      expect(find.text('IMG_2100.JPG'), findsOneWidget);
    });

    testWidgets('the filter asks the service for one type, of any size', (tester) async {
      final c = await pumpBackDown(tester);
      await goTo(tester, c, Routes.dupes);
      await tester.tap(find.byTooltip('Filter'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('Any size'));
      await tester.tap(find.text('Text'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('Show'));
      await tester.pumpAndSettle();
      // notes.md is the only text file there twice; few enough to open at once
      expect(find.text('could be freed across 1 group of identical files'), findsOneWidget);
      expect(find.text('notes.md'), findsNWidgets(3));
      expect(find.textContaining(RegExp(r'^of 25 \(76\.\d MB\) in all$')), findsOneWidget);
      await tester.tap(find.text('Clear filters'));
      await tester.pumpAndSettle();
      expect(find.text('could be freed across 25 groups of identical files'), findsOneWidget);
    });

    testWidgets('a filter that leaves nothing says so', (tester) async {
      final c = await pumpBackDown(tester);
      await goTo(tester, c, Routes.dupes);
      await tester.tap(find.byTooltip('Filter'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('Videos'));
      await tester.tap(find.text('Show'));
      await tester.pumpAndSettle();
      expect(find.text('No duplicates match'), findsOneWidget);
    });
  });

  group('Plans and the rest of the history', () {
    testWidgets('a plan, its drives and items, and a dry run', (tester) async {
      final c = await pumpBackDown(tester);
      await goTo(tester, c, Routes.plans);
      expect(find.text('Plan 1'), findsOneWidget);
      expect(find.text('sam: some stays only there'), findsOneWidget);
      await tester.tap(find.text('Plan 1'));
      await tester.pumpAndSettle();
      expect(find.text('Applying it copies nothing new.'), findsOneWidget);
      expect(find.textContaining('6 already in the archive'), findsOneWidget);
      expect(find.text('6.1 GB stays only here'), findsOneWidget);
      await tester.tap(find.text('Dry run'));
      await tester.pumpAndSettle();
      expect(
        find.text('Dry run: 0 files (0 B) would be copied; 6 are already in the archive. Nothing was written.'),
        findsOneWidget,
      );
      await tester.scrollUntilVisible(find.text('NOT COPIED'), 300, scrollable: _scrollable());
      expect(find.text('Downloads pile: 1 file, an installer'), findsOneWidget);
    });

    testWidgets('a plan that isn\'t done can\'t be applied from a phone', (tester) async {
      final demo = DemoBackDownApi();
      final c = await pumpBackDown(
        tester,
        api: _Api({
          'plan': (p) async => {
            ...(await demo.get('plan', {'id': 1}))! as Map<String, Object?>,
            'state': 'new',
          },
        }),
      );
      await goTo(tester, c, Routes.plan(1));
      await tester.tap(find.text('Apply: copy 0 B'));
      await tester.pumpAndSettle();
      expect(find.text('Needs someone at the computer'), findsOneWidget);
    });

    testWidgets('the freed-up drive, and what existed only there', (tester) async {
      final c = await pumpBackDown(tester);
      await goTo(tester, c, Routes.freeUp);
      expect(find.textContaining('Old Toshiba'), findsOneWidget);
      expect(find.text('done'), findsOneWidget);
      expect(find.textContaining('deleted 38 files'), findsOneWidget);
      expect(find.byTooltip('Chain of custody'), findsOneWidget);
    });

    testWidgets('a paid edition\'s history says what it is and how to unlock it', (tester) async {
      final c = await pumpBackDown(
        tester,
        api: _Api({
          'wipes': (p) => throw _locked('Reclaim simulator', 'Free up: see what freeing drives would delete.'),
        }),
      );
      await goTo(tester, c, Routes.freeUp);
      expect(find.text('Reclaim simulator'), findsOneWidget);
      expect(find.text('Free up: see what freeing drives would delete.'), findsOneWidget);
      expect(find.text('See pricing'), findsOneWidget);
    });

    testWidgets('the rescue, with the partitions it found', (tester) async {
      final c = await pumpBackDown(tester);
      await goTo(tester, c, Routes.rescues);
      expect(find.text('Look for partitions'), findsOneWidget);
      expect(find.text('ext4 "ubuntu"'), findsOneWidget);
      expect(find.text('the backup GPT at the end of the disk is damaged'), findsOneWidget);
    });

    testWidgets('a copy of a failing drive and files found on a card', (tester) async {
      final now = DateTime.now().millisecondsSinceEpoch ~/ 1000;
      final c = await pumpBackDown(
        tester,
        api: _Api({
          'rescues': (p) => [
            {
              'id': 3,
              'created_at': now - 600,
              'updated_at': now - 60,
              'mode': 'carve',
              'source': '/dev/sdc',
              'dest': '/home/sam/Pictures',
              'options': {'kinds': <String>[]},
              'state': 'done',
              'summary': 'found 4 files (12.0 MB), 1 of them cut off or damaged',
              'result': {
                'found': 4,
                'found_bytes': 12000000,
                'incomplete': 1,
                'by_group': {'photos': 4, 'videos': 0, 'audio': 0, 'documents': 0, 'archives': 0, 'other': 0},
                'unreadable': 0,
              },
            },
            {
              'id': 2,
              'created_at': now - 7200,
              'updated_at': now - 3600,
              'mode': 'image',
              'source': '/dev/sdb',
              'dest': '/media/sam/Seagate Expansion/Disk images/old-laptop.img',
              'options': <String, Object?>{},
              'state': 'stopped',
              'summary': 'stopped',
              'result': {
                'size': 1000,
                'rescued': 900,
                'bad': 40,
                'todo': 60,
                'bad_areas': 3,
                'bins': [
                  [100, 0, 0],
                  [60, 40, 0],
                  [0, 0, 100],
                ],
              },
            },
          ],
        }),
      );
      await goTo(tester, c, Routes.rescues);
      expect(find.text('Find files'), findsOneWidget);
      expect(find.textContaining('4 photos'), findsOneWidget);
      expect(find.text('Open the recovered files'), findsOneWidget);
      expect(find.text('Copy'), findsOneWidget);
      expect(find.textContaining('40 B unreadable in 3 places'), findsOneWidget);
      await tester.tap(find.text('Open the recovered files'));
      await tester.pumpAndSettle();
      expect(find.byType(FolderScreen), findsOneWidget);
    });

    testWidgets('the mirror, synced and verified', (tester) async {
      final c = await pumpBackDown(tester);
      await goTo(tester, c, Routes.mirrors);
      expect(find.text('both drives here'), findsOneWidget);
      expect(find.textContaining('all identical'), findsOneWidget);
      expect(find.textContaining('3 only on the mirror'), findsOneWidget);
    });

    testWidgets('the manifest, with its key', (tester) async {
      final c = await pumpBackDown(tester);
      await goTo(tester, c, Routes.manifests);
      expect(find.text('Seagate Expansion'), findsOneWidget);
      expect(find.text('key 3f9a-c021-7be4-d518'), findsOneWidget);
      expect(find.byTooltip('Share the signed document'), findsOneWidget);
    });
  });

  group('This BackDown', () {
    testWidgets('the version, the service, the index and updates', (tester) async {
      final c = await pumpBackDown(tester);
      await goTo(tester, c, Routes.serverInfo);
      expect(find.text('BackDown 1.0.0-beta.4'), findsOneWidget);
      expect(find.textContaining('process 4242'), findsOneWidget);
      expect(find.text('Files'), findsOneWidget);
      await tester.scrollUntilVisible(find.text('BackDown 1.0.0-rc.1 is available.'), 300, scrollable: _scrollable());
      await tester.tap(find.text('Update now'));
      await tester.pumpAndSettle();
      expect(find.text('Needs someone at the computer'), findsOneWidget);
    });

    testWidgets('the editions, what each feature unlocks, and how scans are made', (tester) async {
      final c = await pumpBackDown(tester);
      await goTo(tester, c, Routes.serverInfo);
      await tester.scrollUntilVisible(find.text('Reclaim simulator'), 400, scrollable: _scrollable());
      expect(find.text('free in the preview'), findsWidgets);
      await tester.scrollUntilVisible(
        find.text('Use the drive\'s change log and file table when possible'),
        400,
        scrollable: _scrollable(),
      );
      expect(find.text('FAST RESCANS'), findsOneWidget);
    });

    testWidgets('the report, read on the phone', (tester) async {
      final c = await pumpBackDown(tester);
      await goTo(tester, c, reportRoute);
      expect(find.text('BackDown report'), findsOneWidget);
      expect(find.textContaining('WD Passport (/media/sam/WD Passport)'), findsOneWidget);
      await tester.scrollUntilVisible(find.text('Duplicates'), 600, scrollable: _scrollable());
    });

    testWidgets('a licence, as the service has it', (tester) async {
      final demo = DemoBackDownApi();
      final c = await pumpBackDown(
        tester,
        api: _Api({
          'features': (p) async => {
            ...(await demo.get('features'))! as Map<String, Object?>,
            'tier': 'pro',
            'tier_name': 'Pro',
            'preview': false,
            'licence': {
              'id': 'TEST0-AAAAA-BBBBB-CCCCC',
              'product': 'pro',
              'kind': 'order',
              'seats': 2,
              'email': 'sam@example.org',
              'name': 'Sam',
              'issued': '2026-09-28',
              'expires': null,
            },
          },
        }),
      );
      await goTo(tester, c, Routes.serverInfo);
      await tester.scrollUntilVisible(find.text('Sam <sam@example.org>'), 400, scrollable: _scrollable());
      expect(find.text('TEST0-AAAAA-BBBBB-CCCCC · Pro'), findsOneWidget);
      expect(find.text('Doesn\'t expire'), findsOneWidget);
    });
  });

  test('a junk folder\'s parent is found from its key\'s bytes', () {
    expect(parentKey('L21lZGlhL3NhbS9QaWN0dXJlcy8udGh1bWJuYWlscw'), 'L21lZGlhL3NhbS9QaWN0dXJlcw');
    expect(parentKey('L21lZGlh'), 'Lw');
  });
}
