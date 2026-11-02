import 'package:backdown_mobile/features/activity/job_text.dart';
import 'package:backdown_mobile/models/daemon.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

Job _job(String kind, {String? path, String reason = 'requested'}) => Job.fromJson({
  'id': 1,
  'kind': kind,
  'path': path,
  'state': 'running',
  'reason': reason,
  'queued_at': 1_793_500_000,
});

void main() {
  test('removing a drive from the index is named as the web UI names it', () {
    final j = _job('forget', path: '/media/sam/Seagate');
    expect(jobTitle(j), 'Remove Seagate from the index');
    expect(jobTitle(j, driveName: (_) => 'Seagate 4TB'), 'Remove Seagate 4TB from the index');
    expect(jobTitle(_job('forget')), 'Remove a drive from the index');
    expect(jobKind('forget'), 'Removing a drive from the index');
    expect(jobIcon('forget'), isNot(Icons.settings_outlined));
  });

  test('a removal the service carries on after a restart says so', () {
    expect(jobReason('resumed'), 'carrying on');
  });

  test('indexing what\'s inside files isn\'t called hashing', () {
    expect(jobTitle(_job('content')), 'Index what\'s inside the files');
    expect(jobTitle(_job('hash')), 'Hash new and changed files');
  });

  test('a job the app doesn\'t know yet keeps its own name', () {
    expect(jobTitle(_job('something-new')), 'something-new');
    expect(jobKind('something-new'), 'something-new');
  });
}
