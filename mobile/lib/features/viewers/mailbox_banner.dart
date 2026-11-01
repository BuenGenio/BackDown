import 'package:flutter/material.dart';
import 'package:go_router/go_router.dart';

import '../../api/keys.dart';
import '../../models/browse.dart';
import '../../theme.dart';
import 'routes.dart';

/// The Maildir a folder listing shows: the folder itself when it holds cur/ and new/, or the one
/// whose cur/, new/ or tmp/ it is (its files named like "1698765432.M5P6Q7.host:2,S"), as the web
/// UI finds it. Null for any other folder, and for a folder seen at an earlier scan.
({String key, String name, bool inside})? maildirOf(Listing l) {
  if (l.isPast) return null;
  final dirs = {
    for (final e in l.entries)
      if (e.isDir) e.name,
  };
  if (dirs.contains('cur') && dirs.contains('new')) return (key: l.key, name: baseName(l.name), inside: false);
  if (l.crumbs.length >= 2 && const {'cur', 'new', 'tmp'}.contains(baseName(l.name))) {
    final file = RegExp(r'^\d{9,11}\.[^/]+$');
    if (l.entries.any((e) => !e.isDir && file.hasMatch(e.name))) {
      final up = l.crumbs[l.crumbs.length - 2];
      return (key: up.key, name: baseName(up.name), inside: true);
    }
  }
  return null;
}

/// "This folder is a mailbox (Maildir). Open as mailbox": its mail as threaded conversations
/// rather than folders of numbered files. Nothing for any other folder, so it can go in every
/// folder's header.
class MailboxBanner extends StatelessWidget {
  const MailboxBanner({super.key, required this.listing});
  final Listing listing;

  @override
  Widget build(BuildContext context) {
    final m = maildirOf(listing);
    if (m == null) return const SizedBox.shrink();
    return Container(
      margin: const EdgeInsets.fromLTRB(12, 4, 12, 8),
      padding: const EdgeInsets.fromLTRB(14, 8, 6, 8),
      decoration: BoxDecoration(
        color: context.bd.info.withValues(alpha: 0.12),
        borderRadius: BorderRadius.circular(10),
      ),
      child: Row(
        children: [
          Icon(Icons.mail_outline, color: context.bd.info, size: 20),
          const SizedBox(width: 10),
          Expanded(
            child: Text(
              m.inside
                  ? 'These files are the messages of the mailbox ${m.name} (Maildir).'
                  : 'This folder is a mailbox (Maildir).',
            ),
          ),
          TextButton(onPressed: () => context.push(mailboxLocation(m.key)), child: const Text('Open as mailbox')),
        ],
      ),
    );
  }
}
