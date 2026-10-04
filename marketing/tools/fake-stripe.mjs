// A small fake of the parts of Stripe's API that backdown.io's store (src/store.js) and scripts/stripe-setup.mjs
// use, for their tests: prices by lookup key, Checkout Sessions, products, webhook endpoints, the account and
// Stripe Tax. It listens on 127.0.0.1 only, keeps everything in memory, and checks parameters roughly the way
// Stripe does (unknown prices, quantities outside the adjustable range, lookup keys taken without
// transfer_lookup_key), so a wrong request fails here too. Nothing in it talks to the network.
//
//   const stripe = await fakeStripe({ key: 'sk_test_…' });   // stripe.url is the API base (STRIPE_API)
//   stripe.addPrice({ lookup_key: 'backdown_team_preorder', unit_amount: 1234 });
//   stripe.pay(sessionId, { quantity: 7, email: 'alice@example.com' });
//   await stripe.close();
import { createServer } from 'node:http';
import { randomBytes } from 'node:crypto';

const ALNUM = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789';
export const rand = (n = 24) => [...randomBytes(n)].map(b => ALNUM[b % ALNUM.length]).join('');

// Stripe's form encoding back into objects: a[b][0][c]=v and a[]=v
export function decode(search) {
  const out = {};
  for (const [k, v] of new URLSearchParams(search)) {
    const path = k.replace(/\]/g, '').split('[');
    let o = out;
    for (let i = 0; i < path.length; i++) {
      let p = path[i];
      if (p === '') p = Array.isArray(o) ? o.length : Object.keys(o).length;
      if (i === path.length - 1) o[p] = v;
      else o = o[p] ??= /^\d*$/.test(path[i + 1]) ? [] : {};
    }
  }
  return out;
}

const list = (data, url) => ({ object: 'list', data, has_more: false, url });

export async function fakeStripe({ key = 'sk_test_fake', account = {}, tax = 'active', registrations = ['US'] } = {}) {
  const state = {
    prices: [], products: [], sessions: new Map(), webhooks: [], requests: [],
    account: { id: 'acct_test', country: 'GB', charges_enabled: true,
      business_profile: { name: 'Example Ltd', support_email: 'support@example.com', url: 'https://example.com' }, ...account },
  };
  const now = () => Math.floor(Date.now() / 1000);

  function addPrice({ lookup_key = null, unit_amount, currency = 'usd', type = 'one_time', active = true, product = 'prod_test' } = {}) {
    const p = { id: `price_${rand(14)}`, object: 'price', lookup_key, unit_amount, currency, type, active, product, metadata: {} };
    state.prices.push(p);
    return p;
  }

  // a Checkout Session as Stripe would have it after a POST /v1/checkout/sessions (or one made before a change)
  function addSession({ metadata = {}, price, quantity = 1, adjustable_quantity } = {}) {
    const p = state.prices.find(x => x.id === price);
    const id = `cs_test_${rand(30)}`;
    const s = {
      id, object: 'checkout.session', url: `https://checkout.stripe.test/c/pay/${id}`, mode: 'payment', status: 'open',
      payment_status: 'unpaid', metadata, created: now(), currency: p?.currency || 'usd', amount_total: (p?.unit_amount || 0) * quantity,
      total_details: { amount_tax: 0 }, customer: null, customer_details: null, payment_intent: null,
      _items: [{ price, quantity, adjustable_quantity }],
    };
    state.sessions.set(id, s);
    return s;
  }

  // the buyer pays (perhaps after changing the quantity on the Checkout page)
  function pay(id, { quantity, email = 'alice@example.com', name = 'Alice Example' } = {}) {
    const s = state.sessions.get(id);
    if (!s) throw new Error(`no session ${id}`);
    if (quantity) s._items[0].quantity = quantity;
    const p = state.prices.find(x => x.id === s._items[0].price);
    Object.assign(s, {
      status: 'complete', payment_status: 'paid', payment_intent: `pi_${rand(24)}`, customer: `cus_${rand(14)}`,
      customer_details: { email, name }, amount_total: (p?.unit_amount || 0) * s._items[0].quantity,
    });
    return s;
  }

  function view(s, expand) {
    const { _items, _params, ...out } = s;
    if (expand.includes('line_items')) {
      out.line_items = list(_items.map((li, i) => ({
        id: `li_${i}`, object: 'item', quantity: li.quantity, price: state.prices.find(x => x.id === li.price) || null,
      })), `/v1/checkout/sessions/${s.id}/line_items`);
    }
    return out;
  }

  const routes = [
    ['GET', /^\/v1\/account$/, () => state.account],
    ['GET', /^\/v1\/tax\/settings$/, () => ({ object: 'tax.settings', status: tax, head_office: { address: { country: state.account.country } } })],
    ['GET', /^\/v1\/tax\/registrations$/, () => list(registrations.map(country => ({ object: 'tax.registration', country, status: 'active', country_options: {} })))],

    ['GET', /^\/v1\/products$/, q => list(state.products.filter(p => q.active === undefined || String(p.active) === q.active))],
    ['POST', /^\/v1\/products$/, (q, b) => {
      if (!b.name) return [400, 'Missing required param: name.'];
      const p = { id: `prod_${rand(14)}`, object: 'product', active: true, name: b.name, description: b.description, tax_code: b.tax_code, url: b.url, metadata: b.metadata || {} };
      state.products.push(p);
      return p;
    }],

    ['GET', /^\/v1\/prices$/, q => {
      const keys = q.lookup_keys ? Object.values(q.lookup_keys) : null;
      const found = state.prices.filter(p => (!keys || keys.includes(p.lookup_key)) && (q.active === undefined || String(p.active) === q.active));
      return list(found.slice(0, Number(q.limit) || 10));
    }],
    ['POST', /^\/v1\/prices$/, (q, b) => {
      if (!state.products.some(p => p.id === b.product)) return [400, `No such product: '${b.product}'`];
      if (!/^\d+$/.test(b.unit_amount || '')) return [400, 'Invalid integer: unit_amount'];
      const holder = b.lookup_key && state.prices.find(p => p.lookup_key === b.lookup_key);
      if (holder && b.transfer_lookup_key !== 'true') return [400, `A price (${holder.id}) already uses that lookup key.`];
      if (holder) holder.lookup_key = null;
      const p = addPrice({ lookup_key: b.lookup_key || null, unit_amount: Number(b.unit_amount), currency: b.currency, product: b.product });
      Object.assign(p, { tax_behavior: b.tax_behavior, nickname: b.nickname, metadata: b.metadata || {} });
      return p;
    }],
    ['POST', /^\/v1\/prices\/(price_\w+)$/, (q, b, [, id]) => {
      const p = state.prices.find(x => x.id === id);
      if (!p) return [404, `No such price: '${id}'`];
      if (b.active !== undefined) p.active = b.active === 'true';
      return p;
    }],

    ['POST', /^\/v1\/checkout\/sessions$/, (q, b) => {
      const li = b.line_items?.[0];
      const p = li && state.prices.find(x => x.id === li.price);
      if (!p) return [400, `No such price: '${li?.price}'`];
      if (!p.active) return [400, `The price specified is inactive. This field only accepts active prices.`];
      const quantity = Number(li.quantity);
      const aq = li.adjustable_quantity;
      if (aq?.enabled === 'true') {
        const min = Number(aq.minimum ?? 0), max = Number(aq.maximum ?? 99);
        if (!(min >= 0 && max <= 999999 && min <= max)) return [400, 'Invalid adjustable_quantity range.'];
        if (quantity < min || quantity > max) return [400, 'The quantity must be within the adjustable_quantity range.'];
      }
      if (!b.success_url?.startsWith('http')) return [400, 'Not a valid URL: success_url'];
      const s = addSession({ metadata: b.metadata || {}, price: p.id, quantity, adjustable_quantity: aq });
      s._params = b;
      return view(s, []);
    }],
    ['GET', /^\/v1\/checkout\/sessions\/([\w]+)$/, (q, b, [, id]) => {
      const s = state.sessions.get(id);
      if (!s) return [404, `No such checkout.session: '${id}'`];
      return view(s, Object.values(q.expand || {}));
    }],

    ['GET', /^\/v1\/webhook_endpoints$/, () => list(state.webhooks.map(({ secret, ...h }) => h))],
    ['POST', /^\/v1\/webhook_endpoints$/, (q, b) => {
      const h = { id: `we_${rand(14)}`, object: 'webhook_endpoint', url: b.url, enabled_events: Object.values(b.enabled_events || {}),
        description: b.description, status: 'enabled', secret: `whsec_${rand(32)}` };
      state.webhooks.push(h);
      return h; // the only time Stripe shows the secret
    }],
    ['POST', /^\/v1\/webhook_endpoints\/(we_\w+)$/, (q, b, [, id]) => {
      const h = state.webhooks.find(x => x.id === id);
      if (!h) return [404, `No such webhook endpoint: '${id}'`];
      if (b.enabled_events) h.enabled_events = Object.values(b.enabled_events);
      const { secret, ...out } = h;
      return out;
    }],
  ];

  const server = createServer(async (req, res) => {
    const url = new URL(req.url, 'http://127.0.0.1');
    let raw = '';
    for await (const chunk of req) raw += chunk;
    const query = decode(url.search), body = decode(raw);
    state.requests.push({ method: req.method, path: url.pathname, query, body });
    const reply = (status, obj) => { res.writeHead(status, { 'Content-Type': 'application/json' }); res.end(JSON.stringify(obj)); };
    if (req.headers.authorization !== `Bearer ${key}`) return reply(401, { error: { type: 'invalid_request_error', message: 'Invalid API Key provided' } });
    for (const [method, re, handle] of routes) {
      const m = req.method === method && re.exec(url.pathname);
      if (!m) continue;
      const out = handle(query, body, m);
      if (Array.isArray(out)) return reply(out[0], { error: { type: 'invalid_request_error', message: out[1] } });
      return reply(200, out);
    }
    reply(404, { error: { type: 'invalid_request_error', message: `Unrecognized request URL (${req.method}: ${url.pathname})` } });
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));

  return {
    url: `http://127.0.0.1:${server.address().port}`, key, state, addPrice, addSession, pay,
    // the requests that changed something, or asked for one path
    calls: (method, path) => state.requests.filter(r => r.method === method && (path instanceof RegExp ? path.test(r.path) : r.path === path)),
    close: () => new Promise(resolve => server.close(resolve)),
  };
}
