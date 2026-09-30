#!/usr/bin/env node
// Mint a BackDown licence key by hand (a complimentary key, a replacement, one for yourself), in the
// same format backdown.io issues on a paid order (marketing/src/license.js):
//   BD1.<base64url JSON claims>.<base64url Ed25519 signature over "BD1.<claims>">
//
//   node scripts/mint-licence.mjs --key signing.pem --email you@example.com [--name "Your Name"]
//        [--seats 1] [--product pro|team|forensic] [--kind order] [--expires YYYY-MM-DD] [--id XXXXX-…]
//   pass show backdown/licence-signing | node scripts/mint-licence.mjs --key - --email …
//
// --key is the licence-signing private key (PKCS#8 PEM, the site's LICENSE_SIGNING_KEY): a file, or
// `-` for standard input so it can come straight from a password manager. It's read, used and
// dropped; nothing is written. Before signing, the key's public half is checked against the one
// built into the app (crates/backdown-cli/src/license.rs), so a wrong key can't mint a key that
// BackDown would refuse. Prints the licence key alone on stdout; add it with `backdown license add`.
import { createPrivateKey, createPublicKey, sign, randomBytes } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const die = m => { process.stderr.write(`mint-licence: ${m}\n`); process.exit(1); };
const args = new Map();
const argv = process.argv.slice(2);
for (let i = 0; i < argv.length; i++) {
  const a = argv[i];
  if (a === '-h' || a === '--help') {
    process.stdout.write(readFileSync(fileURLToPath(import.meta.url), 'utf8').split('\n').slice(1, 15).map(l => l.replace(/^\/\/ ?/, '')).join('\n') + '\n');
    process.exit(0);
  }
  if (!a.startsWith('--') || i + 1 >= argv.length) die(`unexpected ${a} (see --help)`);
  args.set(a.slice(2), argv[++i]);
}
const known = new Set(['key', 'email', 'name', 'seats', 'product', 'kind', 'expires', 'id']);
for (const k of args.keys()) if (!known.has(k)) die(`unknown option --${k} (see --help)`);
if (!args.get('key')) die('--key is required: the licence-signing private key (a PEM file, or - for stdin)');
const email = args.get('email');
if (!email || !email.includes('@')) die('--email is required');
const product = args.get('product') || 'pro';
if (!['pro', 'team', 'forensic'].includes(product)) die('--product must be pro, team or forensic');
const seats = Number(args.get('seats') || 1);
if (!Number.isInteger(seats) || seats < 1) die('--seats must be a whole number, 1 or more');
const expires = args.get('expires');
if (expires && !/^\d{4}-\d{2}-\d{2}$/.test(expires)) die('--expires must be YYYY-MM-DD');

// the signing key, and its public half against the app's
const pem = args.get('key') === '-' ? readFileSync(0, 'utf8') : readFileSync(args.get('key'), 'utf8');
let priv;
try { priv = createPrivateKey(pem); } catch { die('--key isn\'t a readable private key (expected PKCS#8 PEM)'); }
if (priv.asymmetricKeyType !== 'ed25519') die(`--key is an ${priv.asymmetricKeyType} key; licences are signed with Ed25519`);
const pub = createPublicKey(priv).export({ format: 'der', type: 'spki' }).subarray(-32).toString('hex');
const rs = readFileSync(join(dirname(fileURLToPath(import.meta.url)), '..', 'crates/backdown-cli/src/license.rs'), 'utf8');
const built = (rs.match(/hex32\("([0-9a-f]{64})"\)/) || [])[1];
if (!built) die('can\'t find the public key in crates/backdown-cli/src/license.rs');
if (pub !== built) die('this isn\'t the licence-signing key: its public half doesn\'t match the one BackDown checks keys against');

// 20 Crockford base32 characters, in groups of 5 (like the site's order-derived ids)
const CROCKFORD = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';
const id = args.get('id') || [...randomBytes(20)].map(b => CROCKFORD[b & 31]).join('').match(/.{5}/g).join('-');
const payload = {
  v: 1, id, product, kind: args.get('kind') || 'order', seats, email,
  name: args.get('name') || undefined, issued: new Date().toISOString().slice(0, 10), expires: expires || undefined,
};
const b64url = b => Buffer.from(b).toString('base64url');
const body = `BD1.${b64url(JSON.stringify(payload))}`;
const sig = sign(null, Buffer.from(body), priv);
process.stdout.write(`${body}.${b64url(sig)}\n`);
