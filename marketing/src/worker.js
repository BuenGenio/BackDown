// backdown.io: static assets from ./public, plus the early-access list at POST /api/waitlist (stored in KV).
const EMAIL = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (url.pathname === '/api/waitlist') return waitlist(request, env);
    return env.ASSETS.fetch(request);
  },
};

async function waitlist(request, env) {
  if (request.method !== 'POST') return new Response('Method not allowed', { status: 405, headers: { Allow: 'POST' } });

  let form;
  try { form = await request.formData(); } catch { return new Response('Bad request', { status: 400 }); }
  // the page's language, so the launch email can go out in it; anything unexpected is treated as English
  const lang = /^[a-z]{2}$/.test(form.get('lang') || '') ? form.get('lang') : 'en';

  // fetch() from site.js asks for JSON; a plain no-JS form post gets redirected back to its language page
  const json = (request.headers.get('Accept') || '').includes('application/json');
  const done = (status, error) => json
    ? Response.json(error ? { error } : { ok: true }, { status })
    : Response.redirect(new URL(`${lang === 'en' ? '/' : `/${lang}/`}#${error ? 'join-error' : 'joined'}`, request.url), 303);

  // honeypot: hidden from people, filled in by bots; pretend it worked
  if (form.get('website')) return done(200);
  const email = String(form.get('email') || '').trim().toLowerCase();
  if (email.length > 254 || !EMAIL.test(email)) return done(422, "That email address doesn't look right.");

  const key = `email:${email}`;
  if (!(await env.WAITLIST.get(key))) await env.WAITLIST.put(key, JSON.stringify({ at: new Date().toISOString(), lang }));
  return done(200);
}
