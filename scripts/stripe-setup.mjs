#!/usr/bin/env node
// One-time (and repeatable) Stripe setup for BackDown pre-orders of Pro, Team and Forensic, run on your own machine:
//
//   STRIPE_SECRET_KEY=sk_test_… node scripts/stripe-setup.mjs --price pro=<cents> [--price team=<cents>]
//       [--price forensic=<cents>] [--worker-key rk_test_…] [--tax-code txcd_…] [--dry-run]
//
// Run it with a test key (sk_test_…) first and buy each edition with card 4242 4242 4242 4242; then run it again
// with the live key (sk_live_…, and an rk_live_… --worker-key).
//
// --price <edition>=<cents> is that edition's founding-member pre-order price in US cents, tax exclusive (so $12.34
// is 1234): per machine for Pro and Team (Team sells 5 machines or more), per site for Forensic. Give it once for each
// edition to set up or reprice; the editions not given are left as they are.
//
// It checks the account and Stripe Tax, then for each edition given creates or reuses:
//   - the product "BackDown <Edition> (pre-order)"
//   - a one-time USD price with the edition's lookup key (backdown_pro_preorder, backdown_team_preorder,
//     backdown_forensic_preorder; marketing/src/store.js has the table), moving the key onto a new price when the
//     amount changes and deactivating the old one, since the site sells whatever active price holds the key
// and the webhook endpoint https://backdown.io/api/stripe/webhook, and stores the Worker's secrets with wrangler:
// STRIPE_SECRET_KEY (the --worker-key restricted key if given, which is better: Checkout Sessions write + Prices read
// is all the Worker needs), STRIPE_WEBHOOK_SECRET (only when the endpoint is created, since Stripe shows it once), and
// LICENSE_SIGNING_KEY (~/.config/backdown/license-signing-key.pem).
// Secrets go straight from this process to wrangler's stdin; nothing is printed or written to disk. Running it again
// with the same prices changes nothing in Stripe.
import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { EDITIONS } from '../marketing/src/store.js';

const args = process.argv.slice(2);
const flag = name => { const i = args.indexOf(name); return i >= 0 ? args[i + 1] : undefined; };
const dry = args.includes('--dry-run');
const key = process.env.STRIPE_SECRET_KEY;
const workerKey = flag('--worker-key') || process.env.STRIPE_WORKER_KEY;
const taxCode = flag('--tax-code') || 'txcd_10000000'; // General - Electronically Supplied Services
const API = process.env.STRIPE_API || 'https://api.stripe.com'; // scripts/stripe-setup.test.mjs points it at a fake Stripe
const WEBHOOK = 'https://backdown.io/api/stripe/webhook';
const EVENTS = ['checkout.session.completed', 'checkout.session.async_payment_succeeded', 'checkout.session.async_payment_failed', 'charge.refunded'];
const SITE = new URL('../marketing/', import.meta.url).pathname;
const names = Object.keys(EDITIONS).join(', ');

function die(msg) { console.error(`stripe-setup: ${msg}`); process.exit(1); }
const say = (...m) => console.log(...m);

// --price <edition>=<US cents>, once per edition
const prices = {};
args.forEach((a, i) => {
  if (a !== '--price') return;
  const m = /^([a-z]+)=(\d+)$/.exec(args[i + 1] || '');
  if (!m) die(`--price takes <edition>=<US cents> (so $12.34 is 1234), for an edition out of ${names}`);
  const [, edition, cents] = m;
  if (!Object.hasOwn(EDITIONS, edition)) die(`there's no edition "${edition}": it's one of ${names}`);
  if (edition in prices) die(`--price ${edition}= is given twice`);
  if (Number(cents) < 100) die(`--price ${edition}=${cents} is under $1: the amount is in US cents (so $12.34 is 1234)`);
  prices[edition] = Number(cents);
});
if (!key || !/^(sk|rk)_(live|test)_/.test(key)) die('set STRIPE_SECRET_KEY to your secret key (sk_test_… first, sk_live_… when that works) in this shell');
if (!Object.keys(prices).length) die(`pass --price <edition>=<US cents> for each edition to sell (${names}), e.g. --price team=<cents>`);
const live = key.includes('_live_');
const dollars = cents => `$${(cents / 100).toFixed(2)}`;

function encode(obj, prefix, out = new URLSearchParams()) {
  for (const [k, v] of Object.entries(obj)) {
    if (v === undefined) continue;
    const name = prefix ? `${prefix}[${k}]` : k;
    if (v && typeof v === 'object') encode(v, name, out); else out.append(name, String(v));
  }
  return out;
}
async function stripe(method, path, params) {
  let url = `${API}/v1/${path}`;
  const init = { method, headers: { Authorization: `Bearer ${key}` } };
  if (params && method === 'GET') url += `?${encode(params)}`;
  else if (params) { init.body = encode(params); init.headers['Content-Type'] = 'application/x-www-form-urlencoded'; }
  if (dry && method !== 'GET') { say(`  (dry run) ${method} /v1/${path} ${init.body || ''}`); return {}; }
  const r = await fetch(url, init);
  const j = await r.json();
  if (!r.ok) throw new Error(`${method} /v1/${path}: ${j.error?.message || r.status}`);
  return j;
}
function secret(name, value) {
  if (dry) return say(`  (dry run) wrangler secret put ${name}`);
  const r = spawnSync('npx', ['wrangler', 'secret', 'put', name], { cwd: SITE, input: value, stdio: ['pipe', 'inherit', 'inherit'] });
  if (r.status !== 0) die(`wrangler secret put ${name} failed`);
}

say(`Stripe ${live ? 'LIVE' : 'test'} mode${dry ? ' (dry run: nothing is changed)' : ''}`);
if (live) say('  Live mode takes real money. If you haven’t yet, run this with a test key (sk_test_…) first and buy each edition with card 4242 4242 4242 4242.');

// account
const acct = await stripe('GET', 'account');
say(`account: ${acct.settings?.dashboard?.display_name || acct.business_profile?.name || acct.id} (${acct.country}), charges ${acct.charges_enabled ? 'enabled' : 'NOT enabled'}`);
if (live && !acct.charges_enabled) die('this account can’t take live payments yet: finish activation in the Stripe dashboard');
const warnings = [];
if (!acct.business_profile?.support_email) warnings.push('set a support email in Settings → Business → Public details (it goes on receipts)');
if (!acct.business_profile?.url) warnings.push('set the business website (https://backdown.io) in Settings → Business → Public details');

// Stripe Tax: automatic_tax needs it active, and it only collects where you're registered
const tax = await stripe('GET', 'tax/settings');
say(`Stripe Tax: ${tax.status}${tax.head_office?.address?.country ? `, head office ${tax.head_office.address.country}` : ''}`);
if (tax.status !== 'active') die('Stripe Tax isn’t active: set your head office address and a default tax behaviour in Settings → Tax, then run this again');
const regs = await stripe('GET', 'tax/registrations', { status: 'active', limit: 100 });
say(`tax registrations: ${regs.data.map(r => r.country + (r.country_options?.[r.country.toLowerCase()]?.state ? `-${r.country_options[r.country.toLowerCase()].state}` : '')).join(', ') || 'none'}`);
if (!regs.data.length) warnings.push('no tax registrations: checkout will add no tax anywhere until you add the places you’re registered in (Settings → Tax → Registrations)');

// each edition given: its product, and its price under its lookup key
const products = await stripe('GET', 'products', { limit: 100, active: 'true' });
for (const [edition, amount] of Object.entries(prices)) {
  const ed = EDITIONS[edition], tag = `${edition}_preorder`, short = ed.name.replace(/^BackDown /, '');
  let product = products.data.find(p => p.metadata?.backdown === tag);
  if (product) say(`${short}: product ${product.id} (exists)`);
  else {
    product = await stripe('POST', 'products', {
      name: `${ed.name} (pre-order)`,
      description: `First year of ${ed.name} for one ${ed.unit}, starting on the day ${short} is released.`
        + `${ed.min > 1 ? ` Sold for ${ed.min} ${ed.unit}s or more.` : ''} Includes a licence key by email.`,
      tax_code: taxCode, url: 'https://backdown.io/pricing/', metadata: { backdown: tag },
    });
    say(`${short}: product ${product.id || '(new)'} created`);
  }

  // reuse the price when the amount matches, otherwise a new one takes over the lookup key
  const listed = await stripe('GET', 'prices', { 'lookup_keys[]': ed.lookupKey, active: 'true', limit: 1 });
  const current = listed.data?.[0];
  if (current && current.unit_amount === amount && current.currency === 'usd' && current.type === 'one_time') {
    say(`${short}: price ${current.id} (${dollars(amount)} per ${ed.unit}, exists)`);
  } else {
    const p = await stripe('POST', 'prices', {
      product: product.id, currency: 'usd', unit_amount: amount, tax_behavior: 'exclusive',
      lookup_key: ed.lookupKey, transfer_lookup_key: 'true', nickname: 'Founding-member pre-order', metadata: { backdown: tag },
    });
    if (current && !dry) await stripe('POST', `prices/${current.id}`, { active: 'false' });
    say(`${short}: price ${p.id || '(new)'} at ${dollars(amount)} per ${ed.unit} created${current ? `, replacing ${current.id}` : ''}`);
  }
}

// webhook endpoint
const hooks = await stripe('GET', 'webhook_endpoints', { limit: 100 });
const hook = hooks.data.find(h => h.url === WEBHOOK);
if (hook) {
  const missing = EVENTS.filter(e => !hook.enabled_events.includes(e) && !hook.enabled_events.includes('*'));
  if (missing.length) { await stripe('POST', `webhook_endpoints/${hook.id}`, { enabled_events: EVENTS }); say(`webhook: ${hook.id} updated (+${missing.join(', ')})`); }
  else say(`webhook: ${hook.id} (exists; its signing secret was stored when it was created — to replace it, delete the endpoint in the dashboard and run this again)`);
  if (hook.status !== 'enabled') warnings.push(`the webhook endpoint is ${hook.status}: enable it in Developers → Webhooks`);
} else {
  const h = await stripe('POST', 'webhook_endpoints', { url: WEBHOOK, enabled_events: EVENTS, description: 'backdown.io: issue licence keys for pre-orders' });
  say(`webhook: ${h.id || '(new)'} created`);
  if (h.secret) secret('STRIPE_WEBHOOK_SECRET', h.secret);
}

// the Worker's own secrets
secret('STRIPE_SECRET_KEY', workerKey || key);
if (!workerKey) warnings.push('the Worker got your full secret key; a restricted key (Checkout Sessions: write, Prices: read) passed as --worker-key is safer');
let pem;
try { pem = readFileSync(join(homedir(), '.config/backdown/license-signing-key.pem'), 'utf8'); } catch { die('~/.config/backdown/license-signing-key.pem is missing: licence keys can’t be signed'); }
secret('LICENSE_SIGNING_KEY', pem);

// drop the site's cached "closed" answer so the change shows within minutes, not an hour
if (!dry) for (const k of ['preorder', 'status']) spawnSync('npx', ['wrangler', 'kv', 'key', 'delete', k, '--binding', 'SITE', '--remote'], { cwd: SITE, stdio: 'ignore' });

say('\nDone.' + (warnings.length ? '\n\nStill to do in Stripe:\n' + warnings.map(w => `  - ${w}`).join('\n') : ''));
say(`
To open pre-orders on the site: in marketing/site.json set preorder.enabled to true and fill in seller (name, address,
email, law), then: cd marketing && python3 build.py && npx wrangler deploy
Each edition's pre-order button shows while its price is active; to close one, archive its price in the Stripe
dashboard (the site notices within the hour).
${live ? '' : 'This was test mode: buy each edition with card 4242 4242 4242 4242, then run this again with the live key.\n'}`);
