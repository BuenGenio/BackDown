import 'package:flutter/material.dart';

import '../../api/keys.dart';
import '../../models/daemon.dart';

/// A job in words, as the web UI names it (ui/app.js jobName).
String jobTitle(Job j) {
  final p = j.path;
  final name = p == null || p.isEmpty ? '' : baseName(p);
  return switch (j.kind) {
    'index' => 'Scan ${name.isEmpty ? 'a drive' : name}',
    'entities' => 'Recognise what\'s on the drives',
    'apply' => 'Apply ${p ?? 'a plan'}',
    'mirror-sync' => 'Sync ${p ?? 'a mirror'}',
    'mirror-verify' => 'Verify ${p ?? 'a mirror'}',
    'wipe' => 'Free up (${p ?? 'a wipe'})',
    'rescue' => 'Rescue',
    'remote-sync' => 'Sync the machine ${p ?? ''}'.trim(),
    'dedup' => 'Tidy the archive (${p ?? 'dedup'})',
    'mail-merge' => 'Merge mailboxes (${p ?? 'merge'})',
    'indexes' || 'dupes-index' => 'Prepare the index for speed (once)',
    'attest' => 'Sign a manifest of ${name.isEmpty ? 'a drive' : name}',
    _ => 'Hash new and changed files',
  };
}

/// Why a job was queued, in words.
String jobReason(String reason) => switch (reason) {
  'requested' => 'you asked',
  'scheduled' => 'scheduled',
  'returned' => 'drive came back',
  'after-scan' => 'after a scan',
  'after-apply' => 'after copying',
  'after-wipe' => 'after wiping',
  'after-rescue' => 'recovered files',
  'after-dedup' => 'after tidying',
  'after-merge' => 'after merging mail',
  'after-sync' => 'after a machine\'s sync',
  'after-hash' => 'after hashing',
  _ => reason,
};

IconData jobIcon(String kind) => switch (kind) {
  'index' => Icons.refresh,
  'hash' => Icons.tag,
  'entities' => Icons.sell_outlined,
  'remote-sync' => Icons.dns_outlined,
  'dedup' => Icons.link,
  'mail-merge' => Icons.mail_outline,
  'apply' => Icons.copy_all_outlined,
  'mirror-sync' => Icons.sync_alt,
  'mirror-verify' => Icons.verified_user_outlined,
  'wipe' => Icons.delete_outline,
  'rescue' => Icons.healing_outlined,
  'attest' => Icons.workspace_premium_outlined,
  _ => Icons.settings_outlined,
};
