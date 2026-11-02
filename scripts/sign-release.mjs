#!/usr/bin/env node
// Sign a release's SHA256SUMS.txt with BackDown's release-signing key, so `backdown update` (online, or
// --from a folder carried to a machine without a network) can tell the files are ours: writes
// SHA256SUMS.txt.sig beside it, a base64url Ed25519 signature over the file's exact bytes.
// scripts/publish-release.sh runs it before it uploads anything.
//
//   node scripts/sign-release.mjs target/release-upload/v1.0.0/SHA256SUMS.txt
//   node scripts/sign-release.mjs --check     # only check that the key is the release key; signs nothing
//
// The key is the release-signing private key (PKCS#8 PEM) in the file BACKDOWN_RELEASE_KEY names, by
// default ~/.config/backdown/release-signing-key.pem. It's read, used and dropped; only the .sig is written.
// Before signing, its public half is checked against the one built into the app (RELEASE_KEY in
// crates/backdown-cli/src/update/mod.rs), so a wrong key can't sign a release that BackDown would refuse, and
// the new signature is checked before it's written.
//
// For tests only: BACKDOWN_TEST_RELEASE_PUBKEY (64 hex digits) stands in for the app's public key in that
// check, so a throwaway key can sign. It's announced on stderr, and publish-release.sh refuses to run while
// it's set.
import { createPrivateKey, createPublicKey, sign, verify } from 'node:crypto';
import { readFileSync, renameSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const die = m => { process.stderr.write(`sign-release: ${m}\n`); process.exit(1); };
const here = fileURLToPath(import.meta.url);
const argv = process.argv.slice(2);
if (argv.includes('-h') || argv.includes('--help')) {
  process.stdout.write(readFileSync(here, 'utf8').split('\n').slice(1, 18).map(l => l.replace(/^\/\/ ?/, '')).join('\n') + '\n');
  process.exit(0);
}
const check = argv.includes('--check');
const files = argv.filter(a => a !== '--check');
for (const a of files) if (a.startsWith('-')) die(`unknown option ${a} (see --help)`);
if (check ? files.length !== 0 : files.length !== 1) die('usage: sign-release.mjs SHA256SUMS.txt, or --check (see --help)');

// the public key BackDown checks releases against
let expected;
const testKey = process.env.BACKDOWN_TEST_RELEASE_PUBKEY;
if (testKey) {
  if (!/^[0-9a-f]{64}$/.test(testKey)) die('BACKDOWN_TEST_RELEASE_PUBKEY must be 64 lowercase hex digits');
  process.stderr.write('sign-release: BACKDOWN_TEST_RELEASE_PUBKEY is set: checking the key against that test key, not the app\'s (for tests only)\n');
  expected = testKey;
} else {
  const rs = readFileSync(join(dirname(here), '..', 'crates/backdown-cli/src/update/mod.rs'), 'utf8');
  expected = (rs.match(/RELEASE_KEY\s*:\s*\[u8;\s*32\]\s*=\s*(?:crate::license::)?hex32\(\s*"([0-9a-f]{64})"\s*\)/) || [])[1];
  if (!expected) die('can\'t find RELEASE_KEY in crates/backdown-cli/src/update/mod.rs');
}

// the signing key, and its public half against that
const keyFile = process.env.BACKDOWN_RELEASE_KEY || join(homedir(), '.config/backdown/release-signing-key.pem');
let pem;
try { pem = readFileSync(keyFile, 'utf8'); } catch (e) {
  die(`can't read the release-signing key ${keyFile} (${e.code || e.message}); BACKDOWN_RELEASE_KEY names another file`);
}
let priv;
try { priv = createPrivateKey(pem); } catch { die(`${keyFile} isn't a readable private key (expected PKCS#8 PEM)`); }
pem = undefined;
if (priv.asymmetricKeyType !== 'ed25519') die(`${keyFile} is an ${priv.asymmetricKeyType} key; releases are signed with Ed25519`);
const pub = createPublicKey(priv);
const pubHex = pub.export({ format: 'der', type: 'spki' }).subarray(-32).toString('hex');
if (pubHex !== expected) {
  die(`${keyFile} isn't the release-signing key: its public half is ${pubHex}, and BackDown checks releases against ${expected}. Nothing was signed`);
}
if (check) {
  process.stderr.write(`sign-release: ${keyFile} is the release-signing key\n`);
  process.exit(0);
}

// the signature, checked, then written in one step
const sums = files[0];
let data;
try { data = readFileSync(sums); } catch (e) { die(`can't read ${sums} (${e.code || e.message})`); }
if (data.length === 0) die(`${sums} is empty`);
const sig = sign(null, data, priv);
if (!verify(null, data, pub, sig)) die('the new signature doesn\'t check out; nothing was written');
const out = `${sums}.sig`;
writeFileSync(`${out}.tmp`, `${sig.toString('base64url')}\n`);
renameSync(`${out}.tmp`, out);
process.stderr.write(`sign-release: signed ${sums} (${data.length} bytes) → ${out}\n`);
