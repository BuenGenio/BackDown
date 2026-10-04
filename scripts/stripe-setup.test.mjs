// Tests for scripts/stripe-setup.mjs:
//
//   node --test scripts/stripe-setup.test.mjs
//
// The script runs against a fake Stripe on 127.0.0.1 (marketing/tools/fake-stripe.mjs, through STRIPE_API), with
// HOME in a temporary folder holding a throwaway licence-signing key, and a fake `npx` first on PATH that only notes
// what it was asked (and keeps what `wrangler secret put` got on stdin). So nothing reaches Stripe or Cloudflare,
// and the real ~/.config/backdown/license-signing-key.pem is never read.
import { after, test } from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { generateKeyPairSync, timingSafeEqual } from 'node:crypto';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { fakeStripe, rand } from '../marketing/tools/fake-stripe.mjs';

if (!crypto.subtle.timingSafeEqual) crypto.subtle.timingSafeEqual = (a, b) => timingSafeEqual(a, b);
const { EDITIONS, preorderPrices } = await import('../marketing/src/store.js');

const scripts = dirname(fileURLToPath(import.meta.url));
const setupScript = join(scripts, 'stripe-setup.mjs');
const EVENTS = ['checkout.session.completed', 'checkout.session.async_payment_succeeded', 'checkout.session.async_payment_failed', 'charge.refunded'];

// a temporary HOME with a throwaway signing key, and a bin folder with the fake npx (all removed at the end)
const temps = [];
after(() => { for (const d of temps) rmSync(d, { recursive: true, force: true }); });
function sandbox() {
  const dir = mkdtempSync(join(tmpdir(), 'bd-stripe-setup-'));
  temps.push(dir);
  const home = join(dir, 'home'), bin = join(dir, 'bin'), npx = join(dir, 'npx');
  mkdirSync(join(home, '.config/backdown'), { recursive: true });
  mkdirSync(bin);
  mkdirSync(npx);
  const pem = generateKeyPairSync('ed25519').privateKey.export({ format: 'pem', type: 'pkcs8' });
  writeFileSync(join(home, '.config/backdown/license-signing-key.pem'), pem, { mode: 0o600 });
  writeFileSync(join(bin, 'npx'), `#!/bin/sh
echo "$*" >> "$FAKE_NPX_DIR/calls"
if [ "$1" = wrangler ] && [ "$2" = secret ] && [ "$3" = put ]; then cat > "$FAKE_NPX_DIR/secret-$4"; else cat > /dev/null; fi
exit 0
`);
  chmodSync(join(bin, 'npx'), 0o755);
  return {
    home, bin, npx, pem,
    calls: () => existsSync(join(npx, 'calls')) ? readFileSync(join(npx, 'calls'), 'utf8').trim().split('\n') : [],
    secret: name => existsSync(join(npx, `secret-${name}`)) ? readFileSync(join(npx, `secret-${name}`), 'utf8') : undefined,
  };
}

// run the script (asynchronously: the fake Stripe answers from this process)
function setup(box, stripe, args, env = {}) {
  return new Promise(resolve => {
    const child = spawn(process.execPath, [setupScript, ...args], {
      env: { PATH: `${box.bin}:${process.env.PATH}`, HOME: box.home, LANG: 'C', FAKE_NPX_DIR: box.npx,
        STRIPE_API: stripe?.url || 'http://127.0.0.1:9', STRIPE_SECRET_KEY: stripe?.key, ...env },
    });
    let out = '';
    child.stdout.on('data', d => { out += d; });
    child.stderr.on('data', d => { out += d; });
    child.on('close', code => resolve({ code, out }));
  });
}

const writes = stripe => stripe.state.requests.filter(r => r.method === 'POST');
const priceFor = (stripe, edition) => stripe.state.prices.find(p => p.lookup_key === EDITIONS[edition].lookupKey);

test('sets up the editions given, keeps every secret off the screen, and a second run changes nothing', async () => {
  const box = sandbox();
  const stripe = await fakeStripe({ key: `sk_test_${rand(24)}` });
  try {
    const first = await setup(box, stripe, ['--price', 'pro=1234', '--price', 'team=5678']);
    assert.equal(first.code, 0, first.out);
    assert.match(first.out, /Stripe test mode/);
    assert.match(first.out, /Pro: product prod_\w+ created/);
    assert.match(first.out, /Team: price price_\w+ at \$56\.78 per machine created/);

    // a product and a price for each edition given, and nothing for Forensic
    const byTag = Object.fromEntries(stripe.state.products.map(p => [p.metadata.backdown, p]));
    assert.deepEqual(Object.keys(byTag).sort(), ['pro_preorder', 'team_preorder']);
    assert.equal(byTag.pro_preorder.name, 'BackDown Pro (pre-order)');
    assert.equal(byTag.team_preorder.name, 'BackDown Team (pre-order)');
    assert.match(byTag.team_preorder.description, /for one machine, starting on the day Team is released\. Sold for 5 machines or more\./);
    for (const [edition, amount] of [['pro', 1234], ['team', 5678]]) {
      const p = priceFor(stripe, edition);
      assert.ok(p, edition);
      assert.deepEqual([p.unit_amount, p.currency, p.type, p.active, p.tax_behavior, p.product],
        [amount, 'usd', 'one_time', true, 'exclusive', byTag[`${edition}_preorder`].id]);
    }
    assert.equal(priceFor(stripe, 'forensic'), undefined);

    // one webhook endpoint, and the Worker's secrets went to wrangler on stdin
    assert.equal(stripe.state.webhooks.length, 1);
    assert.deepEqual(stripe.state.webhooks[0].enabled_events, EVENTS);
    assert.equal(box.secret('STRIPE_WEBHOOK_SECRET'), stripe.state.webhooks[0].secret);
    assert.equal(box.secret('STRIPE_SECRET_KEY'), stripe.key);
    assert.equal(box.secret('LICENSE_SIGNING_KEY'), box.pem);
    assert.deepEqual(box.calls(), ['wrangler secret put STRIPE_WEBHOOK_SECRET', 'wrangler secret put STRIPE_SECRET_KEY', 'wrangler secret put LICENSE_SIGNING_KEY',
      'wrangler kv key delete preorder --binding SITE --remote', 'wrangler kv key delete status --binding SITE --remote']);
    const pemBody = box.pem.split('\n')[1];
    for (const s of [stripe.key, stripe.state.webhooks[0].secret, pemBody]) assert.ok(!first.out.includes(s), 'a secret was printed');
    assert.match(first.out, /test mode: buy each edition with card 4242 4242 4242 4242, then run this again with the live key/);

    // the store now sells exactly these
    const env = { STRIPE_API: stripe.url, STRIPE_SECRET_KEY: stripe.key, SITE: { get: async () => null, put: async () => {} } };
    const sold = await preorderPrices(env, { fresh: true });
    assert.deepEqual([sold.pro?.amount, sold.team?.amount, sold.forensic], [1234, 5678, null]);

    // again with the same prices: nothing new in Stripe, and the webhook secret isn't replaced
    const before = writes(stripe).length;
    const second = await setup(box, stripe, ['--price', 'team=5678', '--price', 'pro=1234']);
    assert.equal(second.code, 0, second.out);
    assert.equal(writes(stripe).length, before, 'no POST to Stripe');
    assert.match(second.out, /Pro: product prod_\w+ \(exists\)/);
    assert.match(second.out, /Team: price price_\w+ \(\$56\.78 per machine, exists\)/);
    assert.match(second.out, /webhook: we_\w+ \(exists/);
    assert.equal(box.calls().filter(c => c === 'wrangler secret put STRIPE_WEBHOOK_SECRET').length, 1);
    assert.equal(stripe.state.products.length, 2);
    assert.equal(stripe.state.prices.length, 2);
  } finally { await stripe.close(); }
});

test('a new amount moves the lookup key to a new price; editions not given are left alone', async () => {
  const box = sandbox();
  const stripe = await fakeStripe({ key: `sk_test_${rand(24)}` });
  try {
    assert.equal((await setup(box, stripe, ['--price', 'pro=1234', '--price', 'team=5678'])).code, 0);
    const oldPro = priceFor(stripe, 'pro'), oldTeam = priceFor(stripe, 'team');

    const r = await setup(box, stripe, ['--price', 'team=9999', '--price', 'forensic=4321']);
    assert.equal(r.code, 0, r.out);
    const team = priceFor(stripe, 'team');
    assert.notEqual(team.id, oldTeam.id);
    assert.equal(team.unit_amount, 9999);
    assert.equal(oldTeam.active, false, 'the old Team price is archived');
    assert.equal(oldTeam.lookup_key, null);
    assert.match(r.out, new RegExp(`Team: price ${team.id} at \\$99\\.99 per machine created, replacing ${oldTeam.id}`));
    assert.equal(priceFor(stripe, 'pro'), oldPro, 'Pro untouched');
    assert.equal(oldPro.active, true);
    assert.equal(priceFor(stripe, 'forensic').unit_amount, 4321);
    assert.match(r.out, /Forensic: price price_\w+ at \$43\.21 per site created/);
    assert.equal(stripe.state.products.length, 3);
    assert.equal(stripe.state.webhooks.length, 1);
  } finally { await stripe.close(); }
});

test('refuses prices it can’t read, before asking Stripe anything', async () => {
  const box = sandbox();
  const stripe = await fakeStripe({ key: `sk_test_${rand(24)}` });
  try {
    for (const [args, message] of [
      [[], /pass --price <edition>=<US cents>/],
      [['--price', '5600'], /--price takes <edition>=<US cents>/],
      [['--price'], /--price takes <edition>=<US cents>/],
      [['--price', 'enterprise=5000'], /there's no edition "enterprise"/],
      [['--price', 'constructor=5000'], /there's no edition "constructor"/],
      [['--price', 'team=12.50'], /--price takes/],
      [['--price', 'team=-100'], /--price takes/],
      [['--price', 'team=99'], /under \$1/],
      [['--price', 'pro=1000', '--price', 'pro=2000'], /given twice/],
    ]) {
      const r = await setup(box, stripe, args);
      assert.equal(r.code, 1, `${args.join(' ')}: ${r.out}`);
      assert.match(r.out, message, args.join(' '));
    }
    const nokey = await setup(box, stripe, ['--price', 'pro=1000'], { STRIPE_SECRET_KEY: '' });
    assert.equal(nokey.code, 1);
    assert.match(nokey.out, /set STRIPE_SECRET_KEY/);
    assert.equal(stripe.state.requests.length, 0);
    assert.deepEqual(box.calls(), []);
  } finally { await stripe.close(); }
});

test('--worker-key goes to the Worker, --dry-run changes nothing, and a live key is told to try test mode first', async () => {
  const box = sandbox();
  const stripe = await fakeStripe({ key: `sk_test_${rand(24)}` });
  try {
    const worker = `rk_test_${rand(24)}`;
    const r = await setup(box, stripe, ['--price', 'forensic=4321', '--worker-key', worker]);
    assert.equal(r.code, 0, r.out);
    assert.equal(box.secret('STRIPE_SECRET_KEY'), worker);
    assert.ok(!r.out.includes(worker));
    assert.doesNotMatch(r.out, /the Worker got your full secret key/);
  } finally { await stripe.close(); }

  const dryBox = sandbox();
  const dry = await fakeStripe({ key: `sk_test_${rand(24)}` });
  try {
    const r = await setup(dryBox, dry, ['--price', 'team=5678', '--dry-run']);
    assert.equal(r.code, 0, r.out);
    assert.match(r.out, /dry run: nothing is changed/);
    assert.equal(writes(dry).length, 0);
    assert.deepEqual(dryBox.calls(), []);
  } finally { await dry.close(); }

  const liveBox = sandbox();
  const live = await fakeStripe({ key: `sk_live_${rand(24)}` });
  try {
    const r = await setup(liveBox, live, ['--price', 'pro=1234', '--dry-run']);
    assert.equal(r.code, 0, r.out);
    assert.match(r.out, /Stripe LIVE mode/);
    assert.match(r.out, /run this with a test key \(sk_test_…\) first/);
    assert.doesNotMatch(r.out, /This was test mode/);
  } finally { await live.close(); }
});
