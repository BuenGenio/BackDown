// BackDown Pro pre-orders, sold with Stripe Checkout and fulfilled with an offline licence key.
//
//   GET  /api/preorder            is the pre-order open, and at what price (cached from Stripe for an hour)
//   POST /api/checkout            form post from /pricing/ -> 303 to a Stripe Checkout page (works without JS)
//   POST /api/stripe/webhook      Stripe events, signature-checked: payment -> licence (+ email); full refund -> revoked
//   GET  /api/order?session_id=   the /thanks/ page: payment state and, once paid, the licence key
//
// The price is whatever active one-time Stripe price has the lookup key below, so opening, closing and repricing
// happen in Stripe; scripts/stripe-setup.mjs creates it. Secrets: STRIPE_SECRET_KEY, STRIPE_WEBHOOK_SECRET,
// LICENSE_SIGNING_KEY. Optional: an EMAIL (send_email) binding, which also emails the key; without it the key is
// shown on the thanks page and Stripe's receipt is the only email.
// KV ORDERS: order:<checkout session> {status, email, seats, licence…}, licence:<id> {session, status}, pi:<payment intent> -> session.

import { issue } from './license.js';

export const LOOKUP_KEY = 'backdown_pro_preorder';
const PRODUCT = 'pro_preorder';
const MAX_SEATS = 50;
const LANGS = ['en', 'es', 'pt-br', 'de', 'pl', 'cs', 'sk', 'uk'];
// Checkout's own language; it has no Ukrainian, so Stripe picks from the browser there
const LOCALES = { en: 'en-GB', es: 'es', 'pt-br': 'pt-BR', de: 'de', pl: 'pl', cs: 'cs', sk: 'sk', uk: 'auto' };
const page = (lang, slug) => `${lang === 'en' ? '' : `/${lang}`}/${slug}/`;

class StripeError extends Error {
  constructor(message, status) { super(message); this.status = status; }
}

// Stripe's form encoding: nested objects and arrays become a[b][0][c]=v
function encode(obj, prefix, out = new URLSearchParams()) {
  for (const [k, v] of Object.entries(obj)) {
    if (v === undefined || v === null) continue;
    const key = prefix ? `${prefix}[${k}]` : k;
    if (typeof v === 'object') encode(v, key, out);
    else out.append(key, String(v));
  }
  return out;
}

async function stripe(env, method, path, params, idempotencyKey) {
  // STRIPE_API lets tests point this at stripe-mock
  let url = `${env.STRIPE_API || 'https://api.stripe.com'}/v1/${path}`;
  const init = { method, headers: { Authorization: `Bearer ${env.STRIPE_SECRET_KEY}` } };
  if (params && method === 'GET') url += `?${encode(params)}`;
  else if (params) {
    init.body = encode(params);
    init.headers['Content-Type'] = 'application/x-www-form-urlencoded';
  }
  if (idempotencyKey) init.headers['Idempotency-Key'] = idempotencyKey;
  const r = await fetch(url, init);
  const data = await r.json().catch(() => ({}));
  if (!r.ok) throw new StripeError(data.error?.message || `Stripe ${r.status}`, r.status);
  return data;
}

export async function preorderPrice(env, { fresh = false } = {}) {
  if (!env.STRIPE_SECRET_KEY) return null;
  if (!fresh) {
    const cached = await env.SITE.get('preorder', 'json');
    if (cached && Date.now() - cached.at < 3600e3) return cached.price;
  }
  const list = await stripe(env, 'GET', 'prices', { 'lookup_keys[]': LOOKUP_KEY, active: 'true', limit: 1 });
  const p = list.data?.[0];
  const price = p && p.type === 'one_time' ? { id: p.id, amount: p.unit_amount, currency: p.currency } : null;
  await env.SITE.put('preorder', JSON.stringify({ at: Date.now(), price }));
  return price;
}

export async function preorder(request, env) {
  let price = null;
  try { price = await preorderPrice(env); } catch (e) { console.error('preorder price', e.message); }
  return Response.json(price ? { open: true, amount: price.amount, currency: price.currency } : { open: false },
    { headers: { 'Cache-Control': 'public, max-age=300' } });
}

export async function checkout(request, env) {
  if (request.method !== 'POST') return new Response('Method not allowed', { status: 405, headers: { Allow: 'POST' } });
  const form = await request.formData().catch(() => null);
  const lang = LANGS.includes(form?.get('lang')) ? form.get('lang') : 'en';
  const here = new URL(request.url);
  // Stripe sends buyers back here; always over https except on a local dev server
  const origin = /^(localhost|127\.0\.0\.1)$/.test(here.hostname) ? here.origin : `https://${here.host}`;
  const json = (request.headers.get('Accept') || '').includes('application/json');
  const fail = (status, error) => json ? Response.json({ error }, { status })
    : Response.redirect(`${origin}${page(lang, 'pricing')}?checkout=${error}#preorder`, 303);

  let price;
  try { price = await preorderPrice(env); } catch (e) { console.error('checkout price', e.message); return fail(502, 'unavailable'); }
  if (!price) return fail(409, 'closed');
  let session;
  try {
    session = await stripe(env, 'POST', 'checkout/sessions', {
      mode: 'payment',
      line_items: [{ price: price.id, quantity: 1, adjustable_quantity: { enabled: true, minimum: 1, maximum: MAX_SEATS } }],
      automatic_tax: { enabled: true },
      tax_id_collection: { enabled: true },
      billing_address_collection: 'required',
      customer_creation: 'always',
      invoice_creation: { enabled: true, invoice_data: { description: 'BackDown Pro pre-order: first year per machine, starting on release day', metadata: { product: PRODUCT } } },
      locale: LOCALES[lang],
      success_url: `${origin}${page(lang, 'thanks')}?session_id={CHECKOUT_SESSION_ID}`,
      cancel_url: `${origin}${page(lang, 'pricing')}#preorder`,
      metadata: { product: PRODUCT, lang },
      payment_intent_data: { metadata: { product: PRODUCT, lang } },
    });
  } catch (e) {
    console.error('checkout session', e.message);
    return fail(502, 'unavailable');
  }
  return json ? Response.json({ url: session.url }) : Response.redirect(session.url, 303);
}

// Stripe-Signature: t=<unix>,v1=<hex HMAC-SHA256 of "<t>.<body>">[,v1=…]; the secret is used as-is (whsec_…)
export async function verifySignature(body, header, secret, now = Date.now() / 1000, tolerance = 300) {
  if (!header || !secret) return false;
  const pairs = header.split(',').map(kv => kv.trim().split('='));
  const t = Number(pairs.find(([k]) => k === 't')?.[1]);
  const sigs = pairs.filter(([k]) => k === 'v1').map(([, v]) => v);
  if (!Number.isFinite(t) || !sigs.length || Math.abs(now - t) > tolerance) return false;
  const enc = new TextEncoder();
  const key = await crypto.subtle.importKey('raw', enc.encode(secret), { name: 'HMAC', hash: 'SHA-256' }, false, ['sign']);
  const mac = new Uint8Array(await crypto.subtle.sign('HMAC', key, enc.encode(`${t}.${body}`)));
  const expected = enc.encode([...mac].map(b => b.toString(16).padStart(2, '0')).join(''));
  return sigs.some(s => {
    const got = enc.encode(s);
    return got.byteLength === expected.byteLength && crypto.subtle.timingSafeEqual(got, expected);
  });
}

// Idempotent: the licence id and signature are derived from the order, so a webhook retry, or the thanks page
// fulfilling before the webhook arrives, reissues exactly the same key.
async function fulfil(env, sessionId) {
  const existing = await env.ORDERS.get(`order:${sessionId}`, 'json');
  if (existing?.licence) return existing;
  const s = await stripe(env, 'GET', `checkout/sessions/${sessionId}`, { 'expand[]': 'line_items' });
  if (s.metadata?.product !== PRODUCT) return null;
  if (s.payment_status !== 'paid') return { status: s.status === 'expired' ? 'expired' : 'pending' };
  const seats = (s.line_items?.data || []).reduce((n, li) => n + (li.quantity || 0), 0) || 1;
  const email = s.customer_details?.email || '';
  const name = s.customer_details?.name || '';
  const issued = new Date(s.created * 1000).toISOString().slice(0, 10);
  const licence = await issue(env, { orderId: s.id, seats, email, name, issued });
  const order = {
    status: 'paid', session: s.id, payment_intent: s.payment_intent, customer: s.customer, email, name, seats,
    lang: s.metadata?.lang || 'en', amount: s.amount_total, currency: s.currency, tax: s.total_details?.amount_tax ?? null,
    created: new Date(s.created * 1000).toISOString(), licence: licence.key, licence_id: licence.id, emailed: false,
  };
  await env.ORDERS.put(`order:${s.id}`, JSON.stringify(order));
  await env.ORDERS.put(`licence:${licence.id}`, JSON.stringify({ session: s.id, status: 'active', seats, email }));
  if (s.payment_intent) await env.ORDERS.put(`pi:${s.payment_intent}`, s.id);
  return order;
}

async function emailKey(env, order) {
  if (!env.EMAIL || !order?.licence || order.emailed || !order.email) return;
  const seats = order.seats === 1 ? 'one machine' : `${order.seats} machines`;
  const text = `Thank you for pre-ordering BackDown Pro.

Your licence key (${seats}):

${order.licence}

When Pro ships, add it with:

  backdown license add ${order.licence}

Your first year starts on release day. Until then you can get a full refund at any time: just reply to your Stripe receipt.
Pre-order terms: https://backdown.io/terms/

BackDown · https://backdown.io
`;
  const html = `<p>Thank you for pre-ordering BackDown Pro.</p><p>Your licence key (${seats}):</p>
<pre style="white-space:pre-wrap;word-break:break-all;font-size:13px;background:#f5f3ec;padding:12px;border-radius:8px">${order.licence}</pre>
<p>When Pro ships, add it with <code>backdown license add &lt;key&gt;</code>.</p>
<p>Your first year starts on release day. Until then you can get a full refund at any time: just reply to your Stripe receipt. <a href="https://backdown.io/terms/">Pre-order terms</a>.</p>
<p>BackDown · <a href="https://backdown.io">backdown.io</a></p>`;
  try {
    await env.EMAIL.send({ to: order.email, from: env.MAIL_FROM || 'orders@backdown.io', subject: 'Your BackDown Pro licence key', text, html });
    order.emailed = true;
    await env.ORDERS.put(`order:${order.session}`, JSON.stringify(order));
  } catch (e) {
    console.error('licence email', order.session, e.message);
  }
}

async function refunded(env, charge) {
  if (!charge.refunded || !charge.payment_intent) return; // a partial refund keeps the key
  const sessionId = await env.ORDERS.get(`pi:${charge.payment_intent}`);
  const order = sessionId && await env.ORDERS.get(`order:${sessionId}`, 'json');
  if (!order) return;
  order.status = 'refunded';
  order.refunded = new Date().toISOString();
  await env.ORDERS.put(`order:${sessionId}`, JSON.stringify(order));
  if (order.licence_id) await env.ORDERS.put(`licence:${order.licence_id}`, JSON.stringify({ session: sessionId, status: 'revoked', seats: order.seats, email: order.email }));
}

export async function webhook(request, env) {
  if (request.method !== 'POST') return new Response('Method not allowed', { status: 405, headers: { Allow: 'POST' } });
  const body = await request.text();
  if (!await verifySignature(body, request.headers.get('Stripe-Signature'), env.STRIPE_WEBHOOK_SECRET)) {
    return new Response('Bad signature', { status: 400 });
  }
  const event = JSON.parse(body);
  const obj = event.data?.object || {};
  switch (event.type) {
    case 'checkout.session.completed':
    case 'checkout.session.async_payment_succeeded':
      if (obj.metadata?.product === PRODUCT) await emailKey(env, await fulfil(env, obj.id));
      break;
    case 'charge.refunded':
      await refunded(env, obj);
      break;
  }
  // anything else is acknowledged, so Stripe doesn't retry it
  return Response.json({ received: true });
}

export async function order(request, env) {
  const id = new URL(request.url).searchParams.get('session_id') || '';
  const reply = (body, status = 200) => Response.json(body, { status, headers: { 'Cache-Control': 'no-store' } });
  if (!/^cs_(test|live)_[A-Za-z0-9]{10,250}$/.test(id)) return reply({ status: 'unknown' }, 404);
  let o = await env.ORDERS.get(`order:${id}`, 'json');
  if (!o && env.STRIPE_SECRET_KEY) {
    try {
      o = await fulfil(env, id);
    } catch (e) {
      if (e.status === 404) return reply({ status: 'unknown' }, 404);
      console.error('order lookup', id, e.message);
      return reply({ status: 'error' }, 502);
    }
  }
  if (!o) return reply({ status: 'unknown' }, 404);
  const out = { status: o.status, seats: o.seats, email: o.email, emailed: !!o.emailed };
  if (o.status === 'paid') out.licence = o.licence;
  return reply(out);
}
