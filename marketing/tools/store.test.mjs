// Tests for the store (src/store.js, src/license.js): pre-orders of Pro, Team and Forensic, from the price on the
// pricing page to the licence key and its refund.
//
//   node --test marketing/tools/store.test.mjs
//
// The Worker's own handlers run here in Node against a fake Stripe (fake-stripe.mjs, on 127.0.0.1) and in-memory
// KV namespaces. The licence-signing key is a throwaway one made by the test, and keys are checked the way the app
// checks them (crates/backdown-cli/src/license.rs). Nothing here talks to the network or to the real Stripe.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createHmac, generateKeyPairSync, timingSafeEqual, verify } from 'node:crypto';
import { fakeStripe, rand } from './fake-stripe.mjs';

// Workers' crypto.subtle has timingSafeEqual (store.js uses it on webhook signatures); Node's doesn't
if (!crypto.subtle.timingSafeEqual) crypto.subtle.timingSafeEqual = (a, b) => timingSafeEqual(a, b);

const { checkout, EDITIONS, editionOf, order, preorder, preorderPrices, webhook } = await import('../src/store.js');
const { issue, licenceId } = await import('../src/license.js');

// one throwaway key for the whole file (license.js imports it once per isolate, as a Worker would)
const { privateKey, publicKey } = generateKeyPairSync('ed25519');
const PEM = privateKey.export({ format: 'pem', type: 'pkcs8' });
const WEBHOOK_SECRET = `whsec_test_${rand(32)}`;

function kv() {
  const m = new Map();
  return {
    m,
    async get(k, type) { const v = m.get(k); return v === undefined ? null : type === 'json' ? JSON.parse(v) : v; },
    async put(k, v) { m.set(k, String(v)); },
    async delete(k) { m.delete(k); },
  };
}

// a fake Stripe with the given editions on sale ({team: 1234}: amounts in cents), and a Worker env pointing at it
async function setup(open = {}, { email = true } = {}) {
  const stripe = await fakeStripe({ key: `sk_test_${rand(24)}` });
  const prices = {};
  for (const [edition, amount] of Object.entries(open)) prices[edition] = stripe.addPrice({ lookup_key: EDITIONS[edition].lookupKey, unit_amount: amount });
  const sent = [];
  const env = {
    STRIPE_API: stripe.url, STRIPE_SECRET_KEY: stripe.key, STRIPE_WEBHOOK_SECRET: WEBHOOK_SECRET, LICENSE_SIGNING_KEY: PEM,
    SITE: kv(), ORDERS: kv(),
    ...(email ? { EMAIL: { async send(m) { sent.push(m); } } } : {}),
  };
  return { stripe, prices, env, sent };
}

const getPreorder = async env => (await preorder(new Request('https://backdown.io/api/preorder'), env)).json();

function postCheckout(env, fields, { json = true } = {}) {
  return checkout(new Request('https://backdown.io/api/checkout', {
    method: 'POST', body: new URLSearchParams(fields), headers: json ? { Accept: 'application/json' } : {},
  }), env);
}

// a Stripe event, signed the way Stripe signs them (Stripe-Signature: t=…,v1=HMAC-SHA256("<t>.<body>"))
function sendEvent(env, type, object, { secret = WEBHOOK_SECRET, t = Math.floor(Date.now() / 1000) } = {}) {
  const body = JSON.stringify({ id: `evt_${rand(20)}`, object: 'event', type, data: { object } });
  const sig = createHmac('sha256', secret).update(`${t}.${body}`).digest('hex');
  return webhook(new Request('https://backdown.io/api/stripe/webhook', {
    method: 'POST', body, headers: { 'Stripe-Signature': `t=${t},v1=${sig}`, 'Content-Type': 'application/json' },
  }), env);
}

const getOrder = async (env, id) => {
  const r = await order(new Request(`https://backdown.io/api/order?session_id=${encodeURIComponent(id)}`), env);
  return { status: r.status, body: await r.json() };
};

// the app's check: BD1.<base64url claims>.<base64url Ed25519 signature over "BD1.<claims>">, no padding
function verifyLicence(key) {
  assert.match(key, /^BD1\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$/);
  const [, claims, sig] = key.split('.');
  assert.ok(verify(null, Buffer.from(`BD1.${claims}`), publicKey, Buffer.from(sig, 'base64url')), 'the signature verifies');
  return JSON.parse(Buffer.from(claims, 'base64url').toString('utf8'));
}

// check out an edition through the API and pay for it on the fake Checkout page
async function buy(t, edition, { quantity, paidQuantity, email, name } = {}) {
  const r = await postCheckout(t.env, { edition, lang: 'en', ...(quantity ? { quantity: String(quantity) } : {}) });
  assert.equal(r.status, 200, `checkout ${edition}`);
  const { url } = await r.json();
  const id = url.split('/').pop();
  t.stripe.pay(id, { quantity: paidQuantity, email, name });
  return t.stripe.state.sessions.get(id);
}

test('/api/preorder reports each edition open or closed, and Pro at the top level', async () => {
  const none = await setup();
  try {
    const d = await getPreorder(none.env);
    assert.deepEqual(d, {
      open: false,
      editions: {
        pro: { open: false, unit: 'machine', min: 1, max: 50 },
        team: { open: false, unit: 'machine', min: 5, max: 500 },
        forensic: { open: false, unit: 'site', min: 1, max: 10 },
      },
    });
  } finally { await none.stripe.close(); }

  const team = await setup({ team: 2345 });
  try {
    const d = await getPreorder(team.env);
    assert.equal(d.open, false, 'the top level is Pro, which is closed');
    assert.equal(d.amount, undefined);
    assert.deepEqual(d.editions.team, { open: true, amount: 2345, currency: 'usd', unit: 'machine', min: 5, max: 500 });
    assert.equal(d.editions.pro.open, false);
    assert.equal(d.editions.forensic.open, false);
  } finally { await team.stripe.close(); }

  const all = await setup({ pro: 1111, team: 2222, forensic: 3333 });
  try {
    const r = await preorder(new Request('https://backdown.io/api/preorder'), all.env);
    assert.equal(r.headers.get('Cache-Control'), 'public, max-age=300');
    const d = await r.json();
    assert.equal(d.open, true);
    assert.equal(d.amount, 1111);
    assert.equal(d.currency, 'usd');
    assert.deepEqual(Object.fromEntries(Object.entries(d.editions).map(([e, v]) => [e, v.open && v.amount])), { pro: 1111, team: 2222, forensic: 3333 });
    // one Stripe call for all three, then the hour-long cache
    assert.equal(all.stripe.calls('GET', '/v1/prices').length, 1);
    await getPreorder(all.env);
    assert.equal(all.stripe.calls('GET', '/v1/prices').length, 1, 'the second ask comes from KV');
    assert.deepEqual(Object.values(all.stripe.calls('GET', '/v1/prices')[0].query.lookup_keys).sort(),
      ['backdown_forensic_preorder', 'backdown_pro_preorder', 'backdown_team_preorder']);
  } finally { await all.stripe.close(); }
});

test('a price that is inactive, recurring or under another lookup key is closed; an old cache entry is refreshed', async () => {
  const t = await setup();
  try {
    t.stripe.addPrice({ lookup_key: EDITIONS.pro.lookupKey, unit_amount: 1000, active: false });
    t.stripe.addPrice({ lookup_key: EDITIONS.team.lookupKey, unit_amount: 1000, type: 'recurring' });
    t.stripe.addPrice({ lookup_key: 'something_else', unit_amount: 1000 });
    // what the site cached before there were three editions: {at, price}
    await t.env.SITE.put('preorder', JSON.stringify({ at: Date.now(), price: { id: 'price_old', amount: 999, currency: 'usd' } }));
    const d = await getPreorder(t.env);
    assert.deepEqual(Object.values(d.editions).map(e => e.open), [false, false, false]);
    assert.equal(d.open, false);
    assert.equal(t.stripe.calls('GET', '/v1/prices').length, 1, 'the old entry was not used');

    // the status page asks for fresh prices, and counts pre-orders open when any edition is
    t.stripe.addPrice({ lookup_key: EDITIONS.forensic.lookupKey, unit_amount: 5000 });
    const fresh = await preorderPrices(t.env, { fresh: true });
    assert.equal(fresh.pro, null);
    assert.equal(fresh.forensic.amount, 5000);
    assert.ok(Object.values(fresh).some(Boolean));
  } finally { await t.stripe.close(); }

  // without Stripe set up, everything is closed and nothing is asked
  const off = await setup({ pro: 1000 });
  try {
    delete off.env.STRIPE_SECRET_KEY;
    assert.equal((await getPreorder(off.env)).open, false);
    assert.equal(await preorderPrices(off.env), null);
    assert.equal(off.stripe.state.requests.length, 0);
  } finally { await off.stripe.close(); }
});

test('checkout sells each edition at its own price, starting at its minimum, within its limits', async () => {
  const t = await setup({ pro: 1111, team: 2222, forensic: 3333 });
  try {
    for (const [edition, ed] of Object.entries(EDITIONS)) {
      const r = await postCheckout(t.env, { edition, lang: 'de' });
      assert.equal(r.status, 200, edition);
      const { url } = await r.json();
      const s = t.stripe.state.sessions.get(url.split('/').pop());
      const p = s._params;
      assert.equal(p.line_items[0].price, t.prices[edition].id, `${edition}: its own price`);
      assert.equal(p.line_items[0].quantity, String(ed.min), `${edition}: starts at the minimum`);
      assert.deepEqual(p.line_items[0].adjustable_quantity, { enabled: 'true', minimum: String(ed.min), maximum: String(ed.max) });
      assert.deepEqual(p.metadata, { product: `${edition}_preorder`, edition, lang: 'de' });
      assert.deepEqual(p.payment_intent_data.metadata, { product: `${edition}_preorder`, edition, lang: 'de' });
      assert.deepEqual(p.invoice_creation.invoice_data.metadata, { product: `${edition}_preorder`, edition });
      assert.equal(p.invoice_creation.invoice_data.description, `${ed.name} pre-order: first year per ${ed.unit}, starting on release day`);
      assert.equal(p.mode, 'payment');
      assert.equal(p.locale, 'de');
      assert.equal(p.success_url, 'https://backdown.io/de/thanks/?session_id={CHECKOUT_SESSION_ID}');
      assert.equal(p.cancel_url, 'https://backdown.io/de/pricing/#preorder');
      assert.equal(editionOf(s.metadata), edition);
    }
    assert.deepEqual(EDITIONS.pro, { lookupKey: 'backdown_pro_preorder', unit: 'machine', min: 1, max: 50, name: 'BackDown Pro' });
    assert.deepEqual(EDITIONS.team, { lookupKey: 'backdown_team_preorder', unit: 'machine', min: 5, max: 500, name: 'BackDown Team' });
    assert.deepEqual(EDITIONS.forensic, { lookupKey: 'backdown_forensic_preorder', unit: 'site', min: 1, max: 10, name: 'BackDown Forensic' });

    // no edition is Pro, as the form was before; a quantity inside the limits is where Checkout starts
    const plain = await (await postCheckout(t.env, {})).json();
    assert.equal(t.stripe.state.sessions.get(plain.url.split('/').pop())._params.line_items[0].price, t.prices.pro.id);
    for (const [edition, quantity] of [['team', 5], ['team', 500], ['team', 37], ['pro', 50], ['forensic', 10]]) {
      const { url } = await (await postCheckout(t.env, { edition, quantity: String(quantity) })).json();
      assert.equal(t.stripe.state.sessions.get(url.split('/').pop())._params.line_items[0].quantity, String(quantity), `${edition} × ${quantity}`);
    }

    // a form post without JS is sent on to Stripe
    const r = await postCheckout(t.env, { edition: 'forensic', lang: 'en' }, { json: false });
    assert.equal(r.status, 303);
    assert.match(r.headers.get('Location'), /^https:\/\/checkout\.stripe\.test\/c\/pay\/cs_test_/);
  } finally { await t.stripe.close(); }
});

test('checkout refuses unknown editions, quantities outside the limits, and closed editions', async () => {
  const t = await setup({ pro: 1111, team: 2222 });
  try {
    const refused = async (fields, status, error) => {
      const r = await postCheckout(t.env, fields);
      assert.equal(r.status, status, JSON.stringify(fields));
      assert.deepEqual(await r.json(), { error }, JSON.stringify(fields));
    };
    for (const edition of ['enterprise', 'Pro', 'supporter', 'constructor', '__proto__', 'toString', 'hasOwnProperty', ' pro']) {
      await refused({ edition }, 400, 'edition');
    }
    for (const [edition, quantity] of [['pro', '0'], ['pro', '51'], ['team', '4'], ['team', '1'], ['team', '501'], ['forensic', '11'],
      ['forensic', '0'], ['team', '-5'], ['team', '5.5'], ['team', 'ten'], ['team', '1e2'], ['team', '99999'], ['team', ' 6']]) {
      await refused({ edition, quantity }, 400, 'quantity');
    }
    await refused({ edition: 'forensic' }, 409, 'closed');
    assert.equal(t.stripe.calls('POST', '/v1/checkout/sessions').length, 0, 'no session was made for any of them');

    // without JS, back to the pricing page in the form's language, saying why
    const r = await postCheckout(t.env, { edition: 'enterprise', lang: 'pl' }, { json: false });
    assert.equal(r.status, 303);
    assert.equal(r.headers.get('Location'), 'https://backdown.io/pl/pricing/?checkout=edition#preorder');
    const q = await postCheckout(t.env, { edition: 'team', quantity: '2', lang: 'en' }, { json: false });
    assert.equal(q.headers.get('Location'), 'https://backdown.io/pricing/?checkout=quantity#preorder');

    // Stripe down: unavailable, not closed
    t.env.STRIPE_SECRET_KEY = 'sk_test_wrong';
    await t.env.SITE.delete('preorder');
    const logged = [], error = console.error;
    console.error = (...m) => logged.push(m.join(' '));
    try { await refused({ edition: 'team' }, 502, 'unavailable'); } finally { console.error = error; }
    assert.deepEqual(logged, ['checkout price Invalid API Key provided']);
  } finally { await t.stripe.close(); }
});

test('the webhook issues a Team licence with the paid quantity, and emails it once', async () => {
  const t = await setup({ team: 2222 });
  try {
    // the buyer starts at 5 machines and makes it 7 on the Checkout page
    const s = await buy(t, 'team', { paidQuantity: 7, email: 'alice@example.com', name: 'Alice Example' });
    const r = await sendEvent(t.env, 'checkout.session.completed', { id: s.id, object: 'checkout.session', metadata: s.metadata });
    assert.equal(r.status, 200);
    assert.deepEqual(await r.json(), { received: true });

    const stored = JSON.parse(t.env.ORDERS.m.get(`order:${s.id}`));
    assert.equal(stored.status, 'paid');
    assert.equal(stored.edition, 'team');
    assert.equal(stored.seats, 7);
    assert.equal(stored.email, 'alice@example.com');
    assert.equal(stored.amount, 2222 * 7);

    const claims = verifyLicence(stored.licence);
    assert.deepEqual(claims, {
      v: 1, id: await licenceId(s.id), product: 'team', kind: 'preorder', seats: 7,
      email: 'alice@example.com', name: 'Alice Example', issued: new Date(s.created * 1000).toISOString().slice(0, 10),
    });
    assert.deepEqual(Object.keys(claims), ['v', 'id', 'product', 'kind', 'seats', 'email', 'name', 'issued'], 'claims in the order keys have always been signed in');
    assert.match(claims.id, /^[0-9A-HJKMNP-TV-Z]{5}(-[0-9A-HJKMNP-TV-Z]{5}){3}$/);
    assert.equal(stored.licence_id, claims.id);
    assert.deepEqual(JSON.parse(t.env.ORDERS.m.get(`licence:${claims.id}`)), { session: s.id, status: 'active', edition: 'team', seats: 7, email: 'alice@example.com' });
    assert.equal(t.env.ORDERS.m.get(`pi:${s.payment_intent}`), s.id);

    assert.equal(t.sent.length, 1);
    assert.equal(t.sent[0].to, 'alice@example.com');
    assert.equal(t.sent[0].subject, 'Your BackDown Team licence key');
    assert.match(t.sent[0].text, /pre-ordering BackDown Team\.\n\nYour licence key \(7 machines\):/);
    assert.match(t.sent[0].text, /When BackDown Team ships/);
    assert.ok(t.sent[0].text.includes(stored.licence));
    assert.equal(JSON.parse(t.env.ORDERS.m.get(`order:${s.id}`)).emailed, true);

    // the thanks page names the edition and shows the same key
    const o = await getOrder(t.env, s.id);
    assert.equal(o.status, 200);
    assert.deepEqual(o.body, { status: 'paid', edition: 'team', seats: 7, email: 'alice@example.com', emailed: true, licence: stored.licence });
  } finally { await t.stripe.close(); }
});

test('Forensic licences count sites, and say so in the email', async () => {
  const t = await setup({ forensic: 3333 });
  try {
    const s = await buy(t, 'forensic', { quantity: 2 });
    await sendEvent(t.env, 'checkout.session.async_payment_succeeded', { id: s.id, metadata: s.metadata });
    const stored = JSON.parse(t.env.ORDERS.m.get(`order:${s.id}`));
    const claims = verifyLicence(stored.licence);
    assert.equal(claims.product, 'forensic');
    assert.equal(claims.seats, 2);
    assert.match(t.sent[0].text, /Your licence key \(2 sites\):/);
    assert.equal(t.sent[0].subject, 'Your BackDown Forensic licence key');
  } finally { await t.stripe.close(); }
});

test('a repeated event, or the thanks page getting there first, issues the key only once', async () => {
  const t = await setup({ team: 2222 });
  try {
    const s = await buy(t, 'team');
    // the thanks page asks before the webhook arrives: it fulfils the order itself
    const first = await getOrder(t.env, s.id);
    assert.equal(first.body.status, 'paid');
    const key = first.body.licence;
    const snapshot = new Map(t.env.ORDERS.m);
    const sessionGets = () => t.stripe.calls('GET', `/v1/checkout/sessions/${s.id}`).length;
    assert.equal(sessionGets(), 1);

    for (const type of ['checkout.session.completed', 'checkout.session.completed', 'checkout.session.async_payment_succeeded']) {
      assert.equal((await sendEvent(t.env, type, { id: s.id, metadata: s.metadata })).status, 200);
    }
    assert.equal(sessionGets(), 1, 'Stripe was not asked again');
    assert.equal(t.sent.length, 1, 'one email');
    const stored = JSON.parse(t.env.ORDERS.m.get(`order:${s.id}`));
    assert.equal(stored.licence, key, 'the same key');
    assert.deepEqual([...t.env.ORDERS.m.keys()].sort(), [...snapshot.keys()].sort(), 'no second licence');
    assert.equal([...t.env.ORDERS.m.keys()].filter(k => k.startsWith('licence:')).length, 1);

    // the key is a function of the order: issuing it again signs the same bytes
    const again = await issue(t.env, { orderId: s.id, product: 'team', seats: stored.seats, email: stored.email, name: stored.name, issued: stored.created.slice(0, 10) });
    assert.equal(again.key, key);
    assert.equal(again.id, await licenceId(s.id));
  } finally { await t.stripe.close(); }
});

test('an unpaid order waits; a bad or stale signature is refused; other events and other products are ignored', async () => {
  const t = await setup({ team: 2222 });
  try {
    const r = await postCheckout(t.env, { edition: 'team' });
    const id = (await r.json()).url.split('/').pop();
    assert.deepEqual((await getOrder(t.env, id)).body, { status: 'pending', edition: 'team', emailed: false });
    assert.equal((await getOrder(t.env, 'cs_test_doesnotexist123')).status, 404);
    assert.equal((await getOrder(t.env, 'not-a-session')).status, 404);

    t.stripe.pay(id);
    const s = t.stripe.state.sessions.get(id);
    assert.equal((await sendEvent(t.env, 'checkout.session.completed', { id, metadata: s.metadata }, { secret: 'whsec_wrong' })).status, 400);
    assert.equal((await sendEvent(t.env, 'checkout.session.completed', { id, metadata: s.metadata }, { t: Math.floor(Date.now() / 1000) - 3600 })).status, 400);
    assert.equal(t.env.ORDERS.m.size, 0, 'nothing issued');

    // someone else's sale on the same Stripe account
    const lookups = t.stripe.calls('GET', /^\/v1\/checkout\/sessions\//).length;
    const other = t.stripe.addSession({ metadata: { product: 'mug' }, price: t.prices.team.id });
    t.stripe.pay(other.id);
    assert.equal((await sendEvent(t.env, 'checkout.session.completed', { id: other.id, metadata: other.metadata })).status, 200);
    // an edition with the wrong product tag isn't ours either
    const odd = t.stripe.addSession({ metadata: { product: 'pro_preorder', edition: 'team' }, price: t.prices.team.id });
    t.stripe.pay(odd.id);
    assert.equal((await sendEvent(t.env, 'checkout.session.completed', { id: odd.id, metadata: odd.metadata })).status, 200);
    assert.equal((await sendEvent(t.env, 'invoice.paid', { id: 'in_123' })).status, 200);
    assert.equal(t.env.ORDERS.m.size, 0);
    assert.equal(t.stripe.calls('GET', /^\/v1\/checkout\/sessions\//).length, lookups, 'not even looked up');
    assert.equal((await getOrder(t.env, other.id)).status, 404);

    assert.equal(editionOf({ product: 'team_preorder' }), null, 'a Team tag without the edition');
    assert.equal(editionOf(undefined), null);
    assert.equal(editionOf({ product: 'forensic_preorder', edition: 'forensic' }), 'forensic');
  } finally { await t.stripe.close(); }
});

test('a full refund revokes the licence; a partial one keeps it', async () => {
  const t = await setup({ team: 2222 });
  try {
    const s = await buy(t, 'team', { paidQuantity: 6 });
    await sendEvent(t.env, 'checkout.session.completed', { id: s.id, metadata: s.metadata });
    const { licence_id: lid, licence: key } = JSON.parse(t.env.ORDERS.m.get(`order:${s.id}`));

    await sendEvent(t.env, 'charge.refunded', { id: 'ch_1', object: 'charge', refunded: false, amount_refunded: 100, payment_intent: s.payment_intent });
    assert.equal(JSON.parse(t.env.ORDERS.m.get(`licence:${lid}`)).status, 'active', 'a partial refund keeps the key');
    assert.equal((await getOrder(t.env, s.id)).body.licence, key);

    await sendEvent(t.env, 'charge.refunded', { id: 'ch_1', object: 'charge', refunded: true, payment_intent: s.payment_intent });
    assert.deepEqual(JSON.parse(t.env.ORDERS.m.get(`licence:${lid}`)), { session: s.id, status: 'revoked', edition: 'team', seats: 6, email: 'alice@example.com' });
    const o = JSON.parse(t.env.ORDERS.m.get(`order:${s.id}`));
    assert.equal(o.status, 'refunded');
    assert.ok(o.refunded);
    assert.deepEqual((await getOrder(t.env, s.id)).body, { status: 'refunded', edition: 'team', seats: 6, email: 'alice@example.com', emailed: true });

    // a late repeat of the payment event doesn't bring it back
    await sendEvent(t.env, 'checkout.session.completed', { id: s.id, metadata: s.metadata });
    assert.equal(JSON.parse(t.env.ORDERS.m.get(`licence:${lid}`)).status, 'revoked');
    assert.equal(t.sent.length, 1);
    // a refund for a payment that isn't ours changes nothing
    const before = new Map(t.env.ORDERS.m);
    await sendEvent(t.env, 'charge.refunded', { id: 'ch_2', refunded: true, payment_intent: 'pi_unknown' });
    assert.deepEqual(t.env.ORDERS.m, before);
  } finally { await t.stripe.close(); }
});

test('an order from before editions (no edition in its metadata) is Pro', async () => {
  const t = await setup({ pro: 1111 }, { email: false });
  try {
    // a session the old checkout made: metadata {product: 'pro_preorder', lang}
    const s = t.stripe.addSession({ metadata: { product: 'pro_preorder', lang: 'cs' }, price: t.prices.pro.id, quantity: 1 });
    t.stripe.pay(s.id, { quantity: 3, email: 'bob@example.com', name: '' });
    assert.equal((await sendEvent(t.env, 'checkout.session.completed', { id: s.id, metadata: s.metadata })).status, 200);
    const stored = JSON.parse(t.env.ORDERS.m.get(`order:${s.id}`));
    assert.equal(stored.edition, 'pro');
    assert.equal(stored.lang, 'cs');
    const claims = verifyLicence(stored.licence);
    assert.deepEqual(claims, { v: 1, id: await licenceId(s.id), product: 'pro', kind: 'preorder', seats: 3, email: 'bob@example.com', issued: new Date(s.created * 1000).toISOString().slice(0, 10) });
    assert.equal(stored.emailed, false, 'no EMAIL binding: the thanks page shows the key');
    assert.deepEqual((await getOrder(t.env, s.id)).body, { status: 'paid', edition: 'pro', seats: 3, email: 'bob@example.com', emailed: false, licence: stored.licence });

    // and one already stored before editions, with no edition field: still Pro, and its refund too
    const old = `cs_test_${rand(30)}`;
    await t.env.ORDERS.put(`order:${old}`, JSON.stringify({ status: 'paid', session: old, payment_intent: 'pi_old', email: 'carol@example.com', seats: 2, licence: 'BD1.x.y', licence_id: 'AAAAA-BBBBB-CCCCC-DDDDD', emailed: true }));
    await t.env.ORDERS.put('pi:pi_old', old);
    assert.deepEqual((await getOrder(t.env, old)).body, { status: 'paid', edition: 'pro', seats: 2, email: 'carol@example.com', emailed: true, licence: 'BD1.x.y' });
    await sendEvent(t.env, 'charge.refunded', { refunded: true, payment_intent: 'pi_old' });
    assert.deepEqual(JSON.parse(t.env.ORDERS.m.get('licence:AAAAA-BBBBB-CCCCC-DDDDD')), { session: old, status: 'revoked', edition: 'pro', seats: 2, email: 'carol@example.com' });
  } finally { await t.stripe.close(); }
});

test('issue() needs the product, and only signs the ones the app knows', async () => {
  const env = { LICENSE_SIGNING_KEY: PEM };
  const base = { orderId: 'cs_test_x', seats: 1, email: 'alice@example.com', issued: '2026-10-04' };
  for (const product of ['pro', 'team', 'forensic']) assert.equal(verifyLicence((await issue(env, { ...base, product })).key).product, product);
  await assert.rejects(issue(env, base), /no such product/);
  await assert.rejects(issue(env, { ...base, product: 'enterprise' }), /no such product/);
  await assert.rejects(issue({}, { ...base, product: 'pro' }), /LICENSE_SIGNING_KEY/);
  // the id is the order's, whichever edition
  assert.equal((await issue(env, { ...base, product: 'team' })).id, (await issue(env, { ...base, product: 'pro' })).id);
});
