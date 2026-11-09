// backdown.io: static assets from ./public; this code runs only for /api/*, /demo/api/* (and the hourly status check).
//   /api/waitlist   early-access sign-ups (KV WAITLIST)
//   /api/status     the status page's live checks (status.js)
//   /api/preorder, /api/checkout, /api/stripe/webhook, /api/order   pre-orders of Pro, Team and Forensic (store.js)
//   /demo/api/*     the docs' live demo: the app's API answered from recorded responses (demo.js)
import { refresh, status } from './status.js';
import { checkout, order, preorder, webhook } from './store.js';
import { demo } from './demo.js';

const EMAIL = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;
const ROUTES = {
  '/api/waitlist': waitlist,
  '/api/status': status,
  '/api/preorder': preorder,
  '/api/checkout': checkout,
  '/api/stripe/webhook': webhook,
  '/api/order': order,
};

export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    const route = ROUTES[url.pathname];
    if (route) {
      try {
        return await route(request, env, ctx);
      } catch (e) {
        console.error(url.pathname, e.stack || e.message);
        return Response.json({ error: 'internal' }, { status: 500 });
      }
    }
    if (url.pathname.startsWith('/demo/api/')) return demo(request, env, url);
    if (url.pathname.startsWith('/api/')) return Response.json({ error: 'not found' }, { status: 404 });
    return env.ASSETS.fetch(request);
  },
  async scheduled(event, env, ctx) {
    ctx.waitUntil(refresh(env));
  },
};

async function waitlist(request, env) {
  if (request.method !== 'POST') return new Response('Method not allowed', { status: 405, headers: { Allow: 'POST' } });

  let form;
  try { form = await request.formData(); } catch { return new Response('Bad request', { status: 400 }); }
  // the page's language, so the launch email can go out in it; anything unexpected is treated as English
  const lang = /^[a-z]{2}(-[a-z]{2})?$/.test(form.get('lang') || '') ? form.get('lang') : 'en';

  // fetch() from site.js asks for JSON; a plain no-JS form post gets redirected back to its language page
  const json = (request.headers.get('Accept') || '').includes('application/json');
  const back = new URL(request.headers.get('Referer') || '/', request.url);
  const page = back.origin === new URL(request.url).origin ? back.pathname : (lang === 'en' ? '/' : `/${lang}/`);
  const done = (status, error) => json
    ? Response.json(error ? { error } : { ok: true }, { status })
    : Response.redirect(new URL(`${page}#${error ? 'join-error' : 'joined'}`, request.url), 303);

  // honeypot: hidden from people, filled in by bots; pretend it worked
  if (form.get('website')) return done(200);
  const email = String(form.get('email') || '').trim().toLowerCase();
  if (email.length > 254 || !EMAIL.test(email)) return done(422, "That email address doesn't look right.");

  const key = `email:${email}`;
  if (!(await env.WAITLIST.get(key))) await env.WAITLIST.put(key, JSON.stringify({ at: new Date().toISOString(), lang }));
  return done(200);
}
