// Tests for scripts/sign-release.mjs and the signing step of scripts/publish-release.sh:
//
//   node --test scripts/sign-release.test.mjs
//
// Every key here is a throwaway one made by the test. HOME points at a temporary folder in every run, so
// the default key path (~/.config/backdown/release-signing-key.pem) can never be the real key, and
// publish-release.sh runs from a copy of the scripts in a temporary repository, with a fake `gh` that
// only notes what it was asked: nothing is published.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash, generateKeyPairSync, verify } from 'node:crypto';
import { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const scripts = dirname(fileURLToPath(import.meta.url));
const repo = join(scripts, '..');
const signer = join(scripts, 'sign-release.mjs');
const SUMS = 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  backdown-v9.9.9-linux-arm64.tar.gz\n';

const temp = () => mkdtempSync(join(tmpdir(), 'bd-sign-'));

/// A throwaway Ed25519 key: its PEM file, and its public half (hex, as the app writes it).
function throwaway(dir, name = 'throwaway.pem', type = 'ed25519') {
  const { privateKey, publicKey } = type === 'ec'
    ? generateKeyPairSync('ec', { namedCurve: 'P-256' })
    : generateKeyPairSync(type);
  const path = join(dir, name);
  writeFileSync(path, privateKey.export({ format: 'pem', type: 'pkcs8' }), { mode: 0o600 });
  const hex = type === 'ed25519' ? publicKey.export({ format: 'der', type: 'spki' }).subarray(-32).toString('hex') : '';
  return { path, hex, publicKey };
}

/// Run a command with a clean environment: PATH, a temporary HOME, and `env`.
function run(cmd, args, env, opts = {}) {
  const home = env.HOME || temp();
  const r = spawnSync(cmd, args, {
    env: { PATH: env.PATH || process.env.PATH, HOME: home, LANG: 'C', ...env },
    encoding: 'utf8',
    ...opts,
  });
  return { code: r.status, out: `${r.stdout}${r.stderr}` };
}

function sumsIn(dir) {
  const p = join(dir, 'SHA256SUMS.txt');
  writeFileSync(p, SUMS);
  return p;
}

test('signs SHA256SUMS.txt with a key whose public half is the expected one', () => {
  const dir = temp();
  const k = throwaway(dir);
  const sums = sumsIn(dir);
  const r = run('node', [signer, sums], { BACKDOWN_RELEASE_KEY: k.path, BACKDOWN_TEST_RELEASE_PUBKEY: k.hex });
  assert.equal(r.code, 0, r.out);
  assert.match(r.out, /BACKDOWN_TEST_RELEASE_PUBKEY is set/);
  const sig = readFileSync(`${sums}.sig`, 'utf8');
  assert.match(sig, /^[A-Za-z0-9_-]{86}\n$/, 'base64url, no padding, one line');
  assert.ok(verify(null, readFileSync(sums), k.publicKey, Buffer.from(sig.trim(), 'base64url')));
  // over the exact bytes: any change and it no longer checks
  assert.ok(!verify(null, Buffer.from(SUMS.replace('e3b0', 'e3b1')), k.publicKey, Buffer.from(sig.trim(), 'base64url')));
  assert.ok(!existsSync(`${sums}.sig.tmp`));
});

test('refuses a key that is not the release key built into the app', () => {
  const dir = temp();
  const k = throwaway(dir);
  const sums = sumsIn(dir);
  const app = readFileSync(join(repo, 'crates/backdown-cli/src/update/mod.rs'), 'utf8').match(/hex32\(\s*"([0-9a-f]{64})"\s*\)/)[1];
  const r = run('node', [signer, sums], { BACKDOWN_RELEASE_KEY: k.path });
  assert.equal(r.code, 1, r.out);
  assert.match(r.out, /isn't the release-signing key/);
  assert.ok(r.out.includes(app), 'names the key BackDown checks against');
  assert.ok(r.out.includes(k.hex), 'and the one it was given');
  assert.ok(!existsSync(`${sums}.sig`), 'nothing was signed');
});

test('refuses a key that does not match an overridden expected key', () => {
  const dir = temp();
  const k = throwaway(dir, 'a.pem');
  const other = throwaway(dir, 'b.pem');
  const sums = sumsIn(dir);
  const r = run('node', [signer, sums], { BACKDOWN_RELEASE_KEY: k.path, BACKDOWN_TEST_RELEASE_PUBKEY: other.hex });
  assert.equal(r.code, 1, r.out);
  assert.match(r.out, /isn't the release-signing key/);
  assert.ok(!existsSync(`${sums}.sig`));
  const bad = run('node', [signer, sums], { BACKDOWN_RELEASE_KEY: k.path, BACKDOWN_TEST_RELEASE_PUBKEY: 'not hex' });
  assert.equal(bad.code, 1, bad.out);
  assert.match(bad.out, /must be 64 lowercase hex digits/);
});

test('refuses a missing key, by default the one under HOME, and a key that is not Ed25519', () => {
  const dir = temp();
  const sums = sumsIn(dir);
  const missing = run('node', [signer, sums], { BACKDOWN_RELEASE_KEY: join(dir, 'nope.pem') });
  assert.equal(missing.code, 1, missing.out);
  assert.match(missing.out, /can't read the release-signing key .*nope\.pem \(ENOENT\)/);
  // no BACKDOWN_RELEASE_KEY: ~/.config/backdown/release-signing-key.pem, here a temporary HOME's
  const home = temp();
  const dflt = run('node', [signer, sums], { HOME: home });
  assert.equal(dflt.code, 1, dflt.out);
  assert.ok(dflt.out.includes(join(home, '.config/backdown/release-signing-key.pem')), dflt.out);
  const ec = throwaway(dir, 'ec.pem', 'ec');
  const notEd = run('node', [signer, sums], { BACKDOWN_RELEASE_KEY: ec.path });
  assert.equal(notEd.code, 1, notEd.out);
  assert.match(notEd.out, /is an ec key; releases are signed with Ed25519/);
  writeFileSync(join(dir, 'junk.pem'), 'not a key');
  const junk = run('node', [signer, sums], { BACKDOWN_RELEASE_KEY: join(dir, 'junk.pem') });
  assert.equal(junk.code, 1, junk.out);
  assert.ok(!existsSync(`${sums}.sig`));
});

test('--check only checks the key', () => {
  const dir = temp();
  const k = throwaway(dir);
  const before = readdirSync(dir).sort();
  const ok = run('node', [signer, '--check'], { BACKDOWN_RELEASE_KEY: k.path, BACKDOWN_TEST_RELEASE_PUBKEY: k.hex });
  assert.equal(ok.code, 0, ok.out);
  assert.match(ok.out, /is the release-signing key/);
  assert.deepEqual(readdirSync(dir).sort(), before, 'nothing written');
  const wrong = run('node', [signer, '--check'], { BACKDOWN_RELEASE_KEY: k.path });
  assert.equal(wrong.code, 1, wrong.out);
  assert.equal(run('node', [signer], { BACKDOWN_RELEASE_KEY: k.path }).code, 1, 'a file to sign is needed');
});

// ---------- publish-release.sh

const haveTools = ['bash', 'git', 'sha256sum'].every(t => spawnSync('sh', ['-c', `command -v ${t}`]).status === 0);
const TAG = 'v9.9.9';

/// A temporary repository laid out like ours, with a --local build of TAG in target/dist, and a fake `gh`
/// (and, if asked, a fake `node`) first on PATH. Returns its paths.
function fakeRepo({ fakeNode } = {}) {
  const root = temp();
  for (const d of ['scripts/release-notes', 'crates/backdown-cli/src', 'fakebin', `target/dist/${TAG}`]) mkdirSync(join(root, d), { recursive: true });
  for (const f of ['scripts/publish-release.sh', 'scripts/sign-release.mjs', 'crates/backdown-cli/src/update/mod.rs']) { mkdirSync(dirname(join(root, f)), { recursive: true }); copyFileSync(join(repo, f), join(root, f)); }
  chmodSync(join(root, 'scripts/publish-release.sh'), 0o755);
  writeFileSync(join(root, `scripts/release-notes/${TAG}.md`), 'notes\n');
  const git = (...a) => {
    const r = spawnSync('git', ['-C', root, ...a], {
      encoding: 'utf8',
      env: { PATH: process.env.PATH, HOME: root, GIT_CONFIG_NOSYSTEM: '1', GIT_AUTHOR_NAME: 'Test', GIT_AUTHOR_EMAIL: 'test@example.invalid', GIT_COMMITTER_NAME: 'Test', GIT_COMMITTER_EMAIL: 'test@example.invalid' },
    });
    assert.equal(r.status, 0, r.stderr);
    return r.stdout.trim();
  };
  git('init', '-q');
  git('commit', '-q', '--allow-empty', '-m', 'release');
  git('tag', TAG);
  const dist = join(root, 'target/dist', TAG);
  let sums = '';
  for (const p of ['linux-x86_64.tar.gz', 'linux-arm64.tar.gz', 'macos-universal.tar.gz', 'windows-x86_64.zip']) {
    const name = `backdown-${TAG}-${p}`;
    writeFileSync(join(dist, name), `archive ${p}`);
    sums += `${createHash('sha256').update(`archive ${p}`).digest('hex')}  ${name}\n`;
  }
  writeFileSync(join(dist, 'SHA256SUMS.txt'), sums);
  writeFileSync(`${dist}.built-from`, `${git('rev-parse', 'HEAD')}\n`);
  const ghLog = join(root, 'gh.log');
  writeFileSync(join(root, 'fakebin/gh'), `#!/bin/sh\necho "$*" >> '${ghLog}'\n[ "$1 $2" = "release create" ] && exit 1\nexit 0\n`, { mode: 0o755 });
  if (fakeNode) writeFileSync(join(root, 'fakebin/node'), fakeNode, { mode: 0o755 });
  return { root, dist, ghLog, upload: join(root, 'target/release-upload', TAG), PATH: `${join(root, 'fakebin')}:${process.env.PATH}` };
}

const publish = (r, env) => run('bash', [join(r.root, 'scripts/publish-release.sh'), TAG, '--local'], { PATH: r.PATH, HOME: r.root, ...env });
const ghCalls = r => (existsSync(r.ghLog) ? readFileSync(r.ghLog, 'utf8') : '');

test('publish-release.sh refuses to run with a test key set', { skip: !haveTools && 'needs bash, git and sha256sum' }, () => {
  const r = fakeRepo();
  const k = throwaway(r.root);
  const p = publish(r, { BACKDOWN_RELEASE_KEY: k.path, BACKDOWN_TEST_RELEASE_PUBKEY: k.hex });
  assert.notEqual(p.code, 0, p.out);
  assert.match(p.out, /BACKDOWN_TEST_RELEASE_PUBKEY is set, which is for tests only/);
  assert.equal(ghCalls(r), '', 'gh was never run');
});

test('publish-release.sh stops before anything when the key is not the release key', { skip: !haveTools && 'needs bash, git and sha256sum' }, () => {
  const r = fakeRepo();
  const k = throwaway(r.root);
  const p = publish(r, { BACKDOWN_RELEASE_KEY: k.path });
  assert.notEqual(p.code, 0, p.out);
  assert.match(p.out, /isn't the release-signing key/);
  assert.equal(ghCalls(r), '', 'gh was never run');
  assert.ok(!existsSync(r.upload), 'nothing was staged');
  const missing = publish(r, { BACKDOWN_RELEASE_KEY: join(r.root, 'nope.pem') });
  assert.notEqual(missing.code, 0, missing.out);
  assert.equal(ghCalls(r), '');
});

// a `node` that passes the key check: it fails signing, or writes a stand-in signature
const nodeThat = sign => `#!/bin/sh\n[ "$2" = --check ] && exit 0\n${sign ? 'printf \'stand-in\\n\' > "$2.sig"; exit 0' : 'echo "sign-release: simulated failure" >&2; exit 1'}\n`;

test('publish-release.sh uploads nothing if signing fails after the key check', { skip: !haveTools && 'needs bash, git and sha256sum' }, () => {
  const r = fakeRepo({ fakeNode: nodeThat(false) });
  const p = publish(r, { BACKDOWN_RELEASE_KEY: join(r.root, 'unused.pem') });
  assert.notEqual(p.code, 0, p.out);
  assert.match(p.out, /simulated failure/);
  assert.equal(ghCalls(r), '', 'gh was never run');
  assert.ok(!existsSync(join(r.upload, 'SHA256SUMS.txt.sig')));
  assert.ok(!existsSync(join(r.dist, 'SHA256SUMS.txt.sig')), 'the build folder is left as it was');
});

test('publish-release.sh uploads the signature with the release', { skip: !haveTools && 'needs bash, git and sha256sum' }, () => {
  const r = fakeRepo({ fakeNode: nodeThat(true) });
  const p = publish(r, { BACKDOWN_RELEASE_KEY: join(r.root, 'unused.pem') });
  // the fake gh fails `release create`, which stops the script right there
  assert.notEqual(p.code, 0, p.out);
  const calls = ghCalls(r);
  assert.match(calls, /^release create v9\.9\.9 /m, p.out);
  assert.ok(calls.includes(join(r.upload, 'SHA256SUMS.txt.sig')), calls);
  assert.ok(calls.includes(join(r.upload, 'SHA256SUMS.txt ')), calls);
  assert.deepEqual(readdirSync(r.dist).sort(), readdirSync(r.upload).filter(n => n !== 'SHA256SUMS.txt.sig').sort());
  assert.ok(!existsSync(join(r.dist, 'SHA256SUMS.txt.sig')), 'the signature is staged, not put in the build folder');
});
