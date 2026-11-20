// Focused checks for the dashboard's live job state and rendering. No daemon or DOM required.
//   node --test scripts/dashboard.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

const source = await readFile(new URL('../crates/backdown-cli/ui/dashboard.js', import.meta.url), 'utf8');
const { serviceState, jobMessage, dashboardHtml } = await import(`data:text/javascript;base64,${Buffer.from(source).toString('base64')}`);
const daemon = changes => ({ running: true, current: null, side: null, queued: [], recent: [], ...changes });
const state = changes => ({ checked: true, daemon: daemon(), ...changes });
const job = changes => ({ id: 1, kind: 'index', state: 'done', queued_at: 90, started_at: 95, finished_at: 100, summary: 'Scan complete.', ...changes });

test('connection, view-only and request failures remain distinct', () => {
  assert.equal(serviceState({}).label, 'Connecting');
  assert.equal(serviceState({ checked: true, daemon: null }).label, 'View only');
  const offline = serviceState(state({ error: 'Connection refused' }));
  assert.equal(offline.label, 'Unavailable');
  assert.equal(offline.message, 'Connection refused');
  // A read-only server can explicitly return running:false, including after a restart.
  const stopped = serviceState(state({ daemon: daemon({ running: false, current: job({ state: 'running' }) }) }));
  assert.equal(stopped.label, 'View only');
  assert.deepEqual(stopped.jobs, []);
});

test('a rescue running beside another job is live in its own service tile', () => {
  const hashing = job({ id: 11, kind: 'hash', state: 'running', finished_at: null, progress: 'Hashing 40 files' });
  const rescue = job({ id: 12, kind: 'rescue', state: 'running', finished_at: null, progress: 'Recovering the next block' });
  const live = state({ daemon: daemon({ current: hashing, side: rescue }) });
  const protection = serviceState(live, ['attest', 'mirror-sync', 'rescue']);
  assert.equal(protection.label, 'Running');
  assert.equal(protection.latest.id, rescue.id);
  assert.equal(protection.message, rescue.progress);
  assert.deepEqual(protection.jobs.map(j => j.id), [12]);
  assert.equal(serviceState(live, ['hash']).latest.id, 11);
  assert.deepEqual(serviceState(live).jobs.map(j => j.id), [11, 12]);
});

test('queued work takes priority while preserving its prior failed history', () => {
  const failed = job({ id: 10, state: 'failed', error: 'Drive disconnected' });
  const queued = job({ id: 11, state: 'queued', finished_at: null, started_at: null, summary: '', queued_at: 110 });
  const s = serviceState(state({ daemon: daemon({ queued: [queued], recent: [failed] }) }), ['index']);
  assert.equal(s.label, 'Queued');
  assert.equal(s.message, 'Waiting in the queue.');
  assert.deepEqual(s.jobs.map(j => j.id), [11, 10]);
  assert.equal(s.jobs[1].error, 'Drive disconnected');
});

test('latest completion time wins when a long job finishes after a newer job', () => {
  const newer = job({ id: 21, kind: 'mirror-sync', finished_at: 200 });
  const older = job({ id: 20, kind: 'rescue', state: 'stopped', finished_at: 300, summary: '' });
  const history = [newer, older];
  const s = serviceState(state({ daemon: daemon({ recent: history }) }), ['rescue', 'mirror-sync']);
  assert.equal(s.label, 'Stopped');
  assert.equal(s.latest.id, 20);
  assert.equal(s.message, 'Stopped before completion.');
  assert.deepEqual(s.jobs.map(j => j.id), [20, 21]);
  assert.deepEqual(history.map(j => j.id), [21, 20], 'the snapshot remains unchanged');
});

test('failed and stopped jobs never appear completed, and missing messages remain useful', () => {
  for (const [terminal, label] of [['failed', 'Failed'], ['stopped', 'Stopped']]) {
    const s = serviceState(state({ daemon: daemon({ recent: [job({ state: terminal, summary: '', error: '' })] }) }), ['index']);
    assert.equal(s.label, label);
    assert.notEqual(s.tone, 'ok');
    assert.ok(s.message.length > 0);
    assert.doesNotMatch(s.message, /^Completed/);
  }
  assert.equal(jobMessage(job({ state: 'failed', error: 'Access denied' })), 'Access denied');
  assert.equal(jobMessage(job({ state: 'done', summary: '' })), 'Completed.');
  assert.equal(jobMessage(job({ state: 'running', progress: '' })), 'Working…');
});

test('unrun services stay idle and duplicate snapshot jobs appear once', () => {
  const scan = job({ id: 30 });
  const live = state({ daemon: daemon({ recent: [scan] }) });
  assert.equal(serviceState(live).label, 'Online');
  assert.equal(serviceState(live, ['index']).label, 'Completed');
  assert.equal(serviceState(live, ['hash']).label, 'Idle');
  assert.equal(serviceState(state(), ['index']).label, 'Idle', 'a fresh daemon has no remembered completions');
  const running = job({ id: 31, state: 'running', finished_at: null });
  const s = serviceState(state({ daemon: daemon({ current: running, queued: [running], recent: [running, scan] }) }), ['index']);
  assert.deepEqual(s.jobs.map(j => j.id), [31, 30]);
});

test('dashboard treats server text as text in summaries, history and drive buttons', () => {
  const hostile = '<img src=x onerror="attack()">&\'';
  const encoded = '&lt;img src=x onerror=&quot;attack()&quot;&gt;&amp;&#39;';
  const unknown = job({ kind: 'future-task', state: 'failed', error: hostile });
  const html = dashboardHtml(state({
    daemon: daemon({ recent: [unknown], version: hostile, started_at: 80 }),
    roots: [{ key: hostile, name: hostile, path: '/test', files: 2, bytes: 30 }],
    index: { files: 2, bytes: 30 }, health: { error: hostile, drives: [] }, notes: { unread: 1 },
  }), { bytes: () => hostile, when: () => hostile, jobName: () => hostile, pct: () => null });
  assert.ok(!html.includes(hostile));
  assert.ok(html.includes(encoded));
  assert.ok(html.includes(`data-dashboard-root="${encoded}"`));
  assert.ok(html.includes(`Drive health is unavailable: ${encoded}`));
  const services = html.split('<section aria-labelledby="dash-services">')[1].split('</section>')[0];
  const scanTile = services.split('data-service="scans"')[1].split('</details>')[0];
  assert.match(scanTile, /Idle/);
  assert.ok(!scanTile.includes(encoded), 'an unknown job only belongs to the background overview');
});
