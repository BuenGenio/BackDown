import 'package:flutter/material.dart';

import '../../api/keys.dart';
import '../../core/format.dart';
import '../../models/daemon.dart';

/// A job in words, as the web UI names it (ui/app.js jobName). [driveName] names a drive by its
/// path (the index's name for it); [rescueName] is what a rescue job is ("Copy: sdb"), when known.
String jobTitle(Job j, {String Function(String path)? driveName, String? rescueName}) {
  final p = j.path;
  final name = p == null || p.isEmpty ? '' : (driveName?.call(p) ?? baseName(p));
  return switch (j.kind) {
    'index' => 'Scan ${name.isEmpty ? 'a drive' : name}',
    'entities' => 'Recognise what\'s on the drives',
    'apply' => 'Apply ${p ?? 'a plan'}',
    'mirror-sync' => 'Sync ${p ?? 'a mirror'}',
    'mirror-verify' => 'Verify ${p ?? 'a mirror'}',
    'wipe' => 'Free up (${p ?? 'a wipe'})',
    'rescue' => rescueName ?? 'Rescue',
    'remote-sync' => 'Sync the machine ${p ?? ''}'.trim(),
    'pool-send' => 'Send to the pool${p == null ? '' : ' ($p)'}',
    'dedup' => 'Tidy the archive (${p ?? 'dedup'})',
    'mail-merge' => 'Merge mailboxes (${p ?? 'merge'})',
    'indexes' || 'dupes-index' => 'Prepare the index for speed (once)',
    'attest' => 'Sign a manifest of ${name.isEmpty ? 'a drive' : name}',
    'forget' => 'Remove ${name.isEmpty ? 'a drive' : name} from the index',
    'content' => 'Index what\'s inside the files',
    'hash' => 'Hash new and changed files',
    // a kind newer than this app: its own name rather than a wrong one
    _ => j.kind,
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
  'resumed' => 'carrying on',
  _ => reason,
};

IconData jobIcon(String kind) => switch (kind) {
  'index' => Icons.refresh,
  'hash' => Icons.tag,
  'entities' => Icons.sell_outlined,
  'remote-sync' => Icons.dns_outlined,
  'pool-send' => Icons.hub_outlined,
  'dedup' => Icons.link,
  'mail-merge' => Icons.mail_outline,
  'apply' => Icons.copy_all_outlined,
  'mirror-sync' => Icons.sync_alt,
  'mirror-verify' => Icons.verified_user_outlined,
  'wipe' => Icons.delete_outline,
  'rescue' => Icons.healing_outlined,
  'attest' => Icons.workspace_premium_outlined,
  'forget' => Icons.remove_circle_outline,
  'content' => Icons.manage_search,
  _ => Icons.settings_outlined,
};

/// What kind of job it is, as a noun, for a job's details.
String jobKind(String kind) => switch (kind) {
  'index' => 'Scan',
  'hash' => 'Hashing',
  'entities' => 'Recognition',
  'remote-sync' => 'Another machine\'s sync',
  'pool-send' => 'Pool',
  'dedup' => 'Tidying the archive',
  'mail-merge' => 'Merging mailboxes',
  'apply' => 'Copying (a plan)',
  'mirror-sync' => 'Mirror sync',
  'mirror-verify' => 'Mirror check',
  'wipe' => 'Freeing up',
  'rescue' => 'Rescue',
  'attest' => 'Manifest',
  'forget' => 'Removing a drive from the index',
  'content' => 'Indexing what\'s inside files',
  'indexes' || 'dupes-index' => 'Preparing the index',
  _ => kind,
};

/// A job's state in words: "Running", "Waiting", "Done", "Failed", "Stopped".
String jobState(String state) => switch (state) {
  'running' => 'Running',
  'queued' => 'Waiting',
  'done' => 'Done',
  'failed' => 'Failed',
  'stopped' => 'Stopped',
  _ => state,
};

/// Hashing's stage after its name: quick samples first, then the full SHA-256.
String jobStage(Job j) {
  if (j.kind != 'hash') return '';
  return switch (j.counts?.stage) {
    'sample' => 'quick samples',
    null || '' => '',
    _ => 'full SHA-256',
  };
}

/// A running job's counts, as Activity shows them: hashing against its exact totals, a scan
/// against the drive's last scan; for other jobs, what they say about themselves.
String jobDetail(Job j) {
  final c = j.counts;
  if (j.kind == 'hash' && c != null) {
    return '${count(c.files)} of ${count(c.totalFiles)} files · ${bytes(c.bytes)} of ${bytes(c.totalBytes)}';
  }
  if (j.kind == 'index' && c != null) {
    final last = c.totalFiles > 0 ? ' (last scan: ${count(c.totalFiles)} files)' : '';
    return '${count(c.files)} files · ${bytes(c.bytes)}$last';
  }
  if (j.progress.isNotEmpty) return j.progress;
  return j.running ? 'starting…' : '';
}

/// "about 12 min left", or null when there isn't enough to go on yet.
String? jobEta(Job j, int now) {
  final s = j.secondsLeft(now);
  return s == null ? null : 'about ${dur(s)} left';
}

/// How long a finished job took, or null.
int? jobTook(Job j) {
  final a = j.startedAt, b = j.finishedAt;
  return a == null || b == null ? null : b - a;
}

/// The jobs that take long enough for a phone to be told when they're done: scans, hashing,
/// copying, freeing up, rescues, syncs and removing a drive from the index.
const longJobKinds = {'index', 'hash', 'apply', 'wipe', 'rescue', 'remote-sync', 'mirror-sync', 'pool-send', 'forget'};

/// A time that's already past ("3 minutes ago", "just now"), as [rel] says it but never "any
/// moment": the service's seconds and the phone's whole seconds can make "now" look a moment ahead.
String ago(int? t, {int skew = 0}) {
  if (t == null || t == 0) return '—';
  final now = nowSeconds(skew);
  return rel(t >= now ? now - 1 : t, skew: skew);
}
