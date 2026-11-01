import 'package:backdown_mobile/api/demo_api.dart';
import 'package:backdown_mobile/api/json.dart';
import 'package:backdown_mobile/api/keys.dart';
import 'package:backdown_mobile/demo/demo_fs.dart';
import 'package:backdown_mobile/features/viewers/disk_view.dart';
import 'package:backdown_mobile/features/viewers/inner_file.dart';
import 'package:backdown_mobile/features/viewers/mailbox_banner.dart';
import 'package:backdown_mobile/features/viewers/mailbox_view.dart';
import 'package:backdown_mobile/features/viewers/routes.dart';
import 'package:backdown_mobile/features/viewers/sqlite_view.dart';
import 'package:backdown_mobile/models/browse.dart';
import 'package:backdown_mobile/router.dart';
import 'package:backdown_mobile/theme.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:go_router/go_router.dart';

import '../support/harness.dart';
import 'support.dart';

void main() {
  testWidgets('a photo opens with its place among the folder\'s files, and swipes to the next', (tester) async {
    final c = await pumpBackDown(tester);
    await openInViewer(tester, c, 'IMG_2100.JPG', withSiblings: true);
    expect(find.text('IMG_2100.JPG'), findsOneWidget);
    expect(find.textContaining('1 of 9'), findsOneWidget);
    await tester.fling(find.byType(PageView), const Offset(-400, 0), 1500);
    await tester.pumpAndSettle();
    expect(find.text('IMG_2101.JPG'), findsOneWidget);
    expect(find.textContaining('2 of 9'), findsOneWidget);
  });

  testWidgets('opened by a link, the viewer asks the service what the file is', (tester) async {
    final c = await pumpBackDown(tester);
    await goTo(tester, c, Routes.view(demoFile('sort.py').id));
    expect(find.text('sort.py'), findsOneWidget);
    expect(find.textContaining('lines · Python'), findsOneWidget);
  });

  testWidgets('code shows with line numbers and its language, and wraps on demand', (tester) async {
    final c = await pumpBackDown(tester);
    await openInViewer(tester, c, 'sort.py');
    expect(find.textContaining('def taken(path: Path) -> str:'), findsOneWidget);
    expect(find.text('12'), findsWidgets); // a line number
    await tester.tap(find.byTooltip('Don\'t wrap long lines'));
    await tester.pumpAndSettle();
    expect(find.byTooltip('Wrap long lines'), findsOneWidget);
    expect(find.textContaining('def taken(path: Path) -> str:'), findsOneWidget);
  });

  testWidgets('notes in Markdown show as text', (tester) async {
    final c = await pumpBackDown(tester);
    await openInViewer(tester, c, 'notes.md');
    expect(find.textContaining('lines · Markdown'), findsOneWidget);
    expect(find.textContaining('old-laptop.img is the ThinkPad'), findsOneWidget);
  });

  testWidgets('a PDF here, without a PDF reader, offers another app', (tester) async {
    final c = await pumpBackDown(tester);
    await openInViewer(tester, c, 'invoice-2019-01.pdf');
    expect(find.text('PDFs show in the app on Android and iOS'), findsOneWidget);
    expect(find.text('Open in another app'), findsWidgets);
  });

  testWidgets('music in the demo says there\'s nothing to play', (tester) async {
    final c = await pumpBackDown(tester);
    await openInViewer(tester, c, '02 Says.mp3');
    expect(find.text('Nothing to play in the demo'), findsOneWidget);
  });

  testWidgets('a file the demo has no bytes for says so', (tester) async {
    final c = await pumpBackDown(tester);
    await openInViewer(tester, c, 'Info.plist');
    expect(find.text('Can\'t show this file'), findsOneWidget);
    expect(find.text('The demo has no content for Info.plist.'), findsOneWidget);
  });

  testWidgets('an mbox reads as conversations, reads on, and searches', (tester) async {
    final c = await pumpBackDown(tester);
    await openInViewer(tester, c, 'archive-2012.mbox');
    expect(find.textContaining('Partial list'), findsOneWidget);
    expect(find.text('Year-end reminder'), findsNothing);
    await tester.tap(find.text('Keep reading'));
    await tester.pumpAndSettle();
    expect(find.textContaining('Partial list'), findsNothing);
    expect(find.text('Year-end reminder'), findsOneWidget);
    await tester.enterText(find.byType(TextField), 'lisbon');
    await tester.pump(const Duration(milliseconds: 400));
    await tester.pumpAndSettle();
    expect(find.text('Weekend in Lisbon'), findsOneWidget);
    expect(find.text('Backup drive'), findsNothing);
    await tester.tap(find.text('Weekend in Lisbon'));
    await tester.pumpAndSettle();
    expect(find.byType(ThreadScreen), findsOneWidget);
    // the search's hits start open
    expect(find.textContaining('Flights are cheap'), findsOneWidget);
    expect(find.text('Expand all'), findsOneWidget);
  });

  testWidgets('a conversation opens its newest message, and the rest on a tap', (tester) async {
    final c = await pumpBackDown(tester);
    await openInViewer(tester, c, 'archive-2012.mbox');
    await tester.tap(find.text('Conference talk: slides draft'));
    await tester.pumpAndSettle();
    expect(find.text('The 10th is good.'), findsOneWidget);
    expect(find.textContaining('Here is the outline'), findsNothing);
    await tester.tap(find.text('Expand all'));
    await tester.pumpAndSettle();
    expect(find.textContaining('Here is the outline'), findsOneWidget);
  });

  testWidgets('messages one by one, rather than threads', (tester) async {
    final c = await pumpBackDown(tester);
    await openInViewer(tester, c, 'archive-2012.mbox');
    await tester.tap(find.text('Messages'));
    await tester.pumpAndSettle();
    expect(find.text('Re: Conference talk: slides draft'), findsWidgets);
  });

  testWidgets('a Maildir message opens as mail, its attachments one tap away', (tester) async {
    final c = await pumpBackDown(tester);
    final cur = DemoFs.instance.byKey[keyOfPath('/media/sam/Seagate Expansion/Mail/mail.example.org/cur')]!;
    await openInViewer(tester, c, cur.children![4].name);
    expect(find.text('Photos for the album'), findsOneWidget);
    expect(find.text('ATTACHMENTS'), findsOneWidget);
    await tester.tap(find.text('IMG_2017.JPG'));
    await tester.pumpAndSettle();
    expect(find.byType(InnerFileScreen), findsOneWidget);
    expect(find.textContaining('attachment'), findsWidgets);
  });

  testWidgets('a Maildir folder opens as a mailbox', (tester) async {
    final c = await pumpBackDown(tester);
    await goTo(tester, c, mailboxLocation(keyOfPath('/media/sam/Seagate Expansion/Mail/mail.example.org')));
    expect(find.text('mail.example.org'), findsOneWidget);
    expect(find.text('Maildir · 7 messages'), findsOneWidget);
    await tester.tap(find.text('Moving your mail'));
    await tester.pumpAndSettle();
    expect(find.text('Done: both copied, and the script runs nightly.'), findsOneWidget);
  });

  testWidgets('the mailbox banner shows on a Maildir and inside its cur/, not elsewhere', (tester) async {
    final api = DemoBackDownApi();
    Future<Listing> ls(String path) async => Listing.fromJson(asJson(await api.get('ls', {'dir': keyOfPath(path)})));
    const box = '/media/sam/Seagate Expansion/Mail/mail.example.org';
    expect(maildirOf(await ls(box))?.key, keyOfPath(box));
    expect(maildirOf(await ls('$box/cur'))?.inside, isTrue);
    expect(maildirOf(await ls('$box/cur'))?.key, keyOfPath(box));
    expect(maildirOf(await ls('/media/sam/Seagate Expansion/Mail')), isNull);

    final listing = await ls(box);
    await tester.pumpWidget(
      MaterialApp.router(
        theme: buildTheme(Brightness.light),
        routerConfig: GoRouter(
          routes: [
            GoRoute(
              path: '/',
              builder: (c, s) => Scaffold(body: MailboxBanner(listing: listing)),
            ),
            GoRoute(path: '/mailbox/:key', builder: (c, s) => Text('mailbox ${s.pathParameters['key']}')),
          ],
        ),
      ),
    );
    expect(find.text('This folder is a mailbox (Maildir).'), findsOneWidget);
    await tester.tap(find.text('Open as mailbox'));
    await tester.pumpAndSettle();
    expect(find.text('mailbox ${keyOfPath(box)}'), findsOneWidget);
  });

  testWidgets('an archive browses as folders and opens a file inside', (tester) async {
    final c = await pumpBackDown(tester);
    await openInViewer(tester, c, 'site-backup.zip');
    expect(find.textContaining('zip · 33 entries'), findsOneWidget);
    await tester.tap(find.text('site'));
    await tester.pumpAndSettle();
    expect(find.text('gallery'), findsOneWidget);
    await tester.tap(find.text('data'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('orders-2018.csv'));
    await tester.pumpAndSettle();
    expect(find.byType(InnerFileScreen), findsOneWidget);
    expect(find.text('9 rows × 5 columns'), findsOneWidget);
    expect(find.text('Hillside School'), findsWidgets);
    await tester.pageBack();
    await tester.pumpAndSettle();
    // back in the archive, at the folder it was in; back again goes up a folder, not out
    expect(find.text('orders-2018.csv'), findsOneWidget);
    await tester.pageBack();
    await tester.pumpAndSettle();
    expect(find.text('gallery'), findsOneWidget);
  });

  testWidgets('a disk image shows its partitions and browses a filesystem in it', (tester) async {
    final c = await pumpBackDown(tester);
    await openInViewer(tester, c, 'old-laptop.img');
    expect(find.text('GPT'), findsOneWidget);
    expect(find.text('EFI system'), findsOneWidget);
    expect(find.text('Linux filesystem'), findsOneWidget);
    expect(find.text('Browse'), findsNWidgets(2)); // not the swap
    await tester.tap(find.text('Browse').last);
    await tester.pumpAndSettle();
    expect(find.byType(FsBrowseScreen), findsOneWidget);
    expect(find.textContaining('wasn\'t unmounted cleanly'), findsOneWidget);
    await tester.tap(find.text('home'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('sam'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('notes.txt'));
    await tester.pumpAndSettle();
    expect(find.textContaining('Things to move off the ThinkPad'), findsOneWidget);
  });

  testWidgets('deleted files show with how likely they come back', (tester) async {
    final c = await pumpBackDown(tester);
    await goTo(tester, c, fsLocation(demoFile('old-laptop.img').id, '1', 'EFI System Partition'));
    expect(find.text('OLDBOOT.EFI'), findsNothing);
    await tester.tap(find.byType(PopupMenuButton<String>));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Show deleted files'));
    await tester.pumpAndSettle();
    expect(find.text('OLDBOOT.EFI'), findsOneWidget);
    expect(find.text('recoverable'), findsOneWidget);
    expect(find.text('partly overwritten'), findsOneWidget);
  });

  testWidgets('a disc image lists its files', (tester) async {
    final c = await pumpBackDown(tester);
    await openInViewer(tester, c, 'ubuntu-24.04-desktop-amd64.iso');
    expect(find.textContaining('ISO 9660 disc image'), findsWidgets);
    await tester.tap(find.text('Browse the disc'));
    await tester.pumpAndSettle();
    expect(find.text('casper'), findsOneWidget);
    expect(find.text('md5sum.txt'), findsOneWidget);
  });

  testWidgets('a SQLite database lists its tables and pages through rows', (tester) async {
    final c = await pumpBackDown(tester);
    await openInViewer(tester, c, 'contacts.sqlite');
    expect(find.textContaining('SQLite · 5 tables'), findsOneWidget);
    expect(find.text('214 rows'), findsOneWidget);
    await tester.tap(find.text('contacts'));
    await tester.pumpAndSettle();
    expect(find.byType(SqliteTableScreen), findsOneWidget);
    expect(find.text('1–200 of 214'), findsOneWidget);
    expect(find.text('first_name'), findsOneWidget);
    await tester.tap(find.byTooltip('Next'));
    await tester.pumpAndSettle();
    expect(find.text('201–214 of 214'), findsOneWidget);
  });

  testWidgets('a SQL dump reads to the end, and a table shows its keys', (tester) async {
    final c = await pumpBackDown(tester);
    await openInViewer(tester, c, 'shop-2018.sql');
    expect(find.text('MySQL dump'), findsOneWidget);
    expect(find.textContaining('Read to the end'), findsOneWidget);
    expect(find.textContaining('10 tables'), findsOneWidget); // the second pass found the last one
    await tester.tap(find.text('orders'));
    await tester.pumpAndSettle();
    expect(find.text('customer_id'), findsWidgets);
    await tester.scrollUntilVisible(find.textContaining('customer_id → shop.customers'), 300);
    await tester.scrollUntilVisible(find.text('REFERENCED BY'), 300);
    expect(find.text('shop.order_items'), findsOneWidget);
  });

  testWidgets('a spreadsheet shows its sheets as tabs', (tester) async {
    final c = await pumpBackDown(tester);
    await openInViewer(tester, c, 'Taxes 2019.xlsx');
    expect(find.textContaining('5 sheets'), findsOneWidget);
    expect(find.text('Taxable profit'), findsOneWidget);
    await tester.tap(find.widgetWithText(ChoiceChip, 'Income'));
    await tester.pumpAndSettle();
    expect(find.text('Northwind Studio'), findsWidgets);
    expect(find.text('TRUE'), findsWidgets);
  });

  testWidgets('a presentation shows each slide\'s title, text and notes', (tester) async {
    final c = await pumpBackDown(tester, api: SlidesDemo());
    await openRef(tester, c, deck);
    expect(find.text('3 slides · PPTX'), findsOneWidget);
    expect(find.text('Keeping what matters'), findsOneWidget);
    expect(find.text('One copy is no copy'), findsOneWidget);
    expect(find.text('Slide 3'), findsOneWidget);
    expect(find.text('Ask about the drawer of old drives.'), findsOneWidget);
  });

  testWidgets('on a wide screen a mailbox shows a conversation beside its list', (tester) async {
    final c = await pumpBackDown(tester, size: const Size(1280, 800));
    await openInViewer(tester, c, 'archive-2012.mbox');
    expect(find.byType(ThreadView), findsOneWidget);
    await tester.tap(find.text('Photos from the lake').first);
    await tester.pumpAndSettle();
    expect(find.text('Of course. I\'ll send you the full size.'), findsOneWidget);
  });
}
