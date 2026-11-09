// The docs' live demo (public/demo/): BackDown's own web UI, built into the site, with its /api/ calls answered
// here from responses recorded against an invented set of drives (marketing/tools/demo/). Every request is a key
// (demo-keys.js) looked up in public/demo/d/index.json; the body is the recorded file beside it. Nothing is
// computed and nothing can be changed: a POST that wasn't recorded gets a polite refusal, an unknown GET a 404.
import { demoKey } from './demo-keys.js';

let indexPromise;
async function index(env, url) {
  indexPromise ||= env.ASSETS.fetch(new Request(new URL('/demo/d/index.json', url))).then(r => {
    if (!r.ok) throw new Error('demo index: ' + r.status);
    return r.json();
  }).catch(e => { indexPromise = null; throw e; });
  return indexPromise;
}

export async function demo(request, env, url) {
  const idx = await index(env, url);
  const pathname = url.pathname.slice('/demo'.length); // the key is the app's own path, /api/...
  const method = request.method.toUpperCase();
  const keys = [demoKey(method, pathname, url.search), demoKey(method, pathname, '')];
  let hit = keys.map(k => idx.entries[k]).find(Boolean);
  if (!hit && idx.fallback[pathname]) hit = idx.entries[idx.fallback[pathname]];
  if (!hit && method === 'GET' && pathname === '/api/content') {
    const r = await env.ASSETS.fetch(new Request(new URL('/demo/placeholder.svg', url)));
    return new Response(r.body, { status: 200, headers: { 'Content-Type': 'image/svg+xml', 'Cache-Control': 'public, max-age=86400' } });
  }
  if (!hit) {
    if (method !== 'GET') return Response.json({ error: 'This is a demo of BackDown on invented drives: it shows, but nothing here can be changed.' }, { status: 409 });
    return Response.json({ error: 'Not part of the demo. Install BackDown to try this on your own drives.' }, { status: 404 });
  }
  const [status, type, file] = hit;
  const r = await env.ASSETS.fetch(new Request(new URL(`/demo/d/${file}`, url)));
  if (!r.ok) return Response.json({ error: 'demo: recorded response missing' }, { status: 500 });
  return new Response(r.body, {
    status,
    headers: { 'Content-Type': type || 'application/octet-stream', 'Cache-Control': 'public, max-age=3600', 'X-Content-Type-Options': 'nosniff' },
  });
}
