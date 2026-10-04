// /api/status: a few cheap checks, run hourly by the cron trigger and cached in KV, so a page view costs one KV read
// and nothing polls anything in real time. The status page asks once on load and again only while it stays visible.

import { preorderPrices } from './store.js';
import offered from '../downloads.json';

const RELEASES = 'https://github.com/BuenGenio/backdown-releases';
const STALE = 2 * 3600e3; // the cron runs hourly; past this, recheck inline so a stopped cron can't show old news
// the release this deploy of the site offers (publish-release.sh writes downloads.json and the site is
// deployed right after): a cached check that names another is rechecked then, not at the next hourly
// run, but at most every five minutes, as GitHub's feed can take that long to show a new release
const OFFERED = offered.platforms?.find(p => p.version)?.version;
const RECHECK = 5 * 60e3;

async function downloads() {
  // the releases Atom feed isn't subject to the API's per-IP rate limit (shared by every Worker)
  try {
    const feed = await fetch(`${RELEASES}/releases.atom`, { headers: { 'User-Agent': 'backdown.io status' }, cf: { cacheTtl: 300 } });
    if (!feed.ok) return { state: 'down' };
    const entry = (await feed.text()).split('<entry>')[1] || '';
    const tag = (entry.match(/\/releases\/tag\/([^"<]+)/) || [])[1];
    const updated = (entry.match(/<updated>([^<]+)<\/updated>/) || [])[1];
    if (!tag) return { state: 'degraded' };
    // and one file from it actually downloads (GitHub redirects to its storage host)
    const file = await fetch(`${RELEASES}/releases/download/${tag}/SHA256SUMS.txt`, { method: 'HEAD', redirect: 'follow' });
    return { state: file.ok ? 'up' : 'degraded', release: { tag: decodeURIComponent(tag), date: updated, url: `${RELEASES}/releases/tag/${tag}` } };
  } catch {
    return { state: 'down' };
  }
}

async function waitlist(env) {
  try { await env.WAITLIST.get('probe'); return 'up'; } catch { return 'down'; }
}

// pre-orders are open while any edition (Pro, Team, Forensic) has a price on sale
async function checkout(env) {
  if (!env.STRIPE_SECRET_KEY) return 'closed';
  try { return Object.values(await preorderPrices(env, { fresh: true }) || {}).some(Boolean) ? 'open' : 'closed'; } catch { return 'down'; }
}

export async function refresh(env) {
  const [dl, wl, co] = await Promise.all([downloads(), waitlist(env), checkout(env)]);
  const status = { checked: new Date().toISOString(), services: { site: 'up', downloads: dl.state, waitlist: wl, checkout: co }, release: dl.release || null };
  await env.SITE.put('status', JSON.stringify(status));
  return status;
}

export async function status(request, env) {
  let s = await env.SITE.get('status', 'json');
  const age = s ? Date.now() - Date.parse(s.checked) : Infinity;
  const behind = OFFERED && s?.release?.tag !== OFFERED && age > RECHECK;
  if (!s || age > STALE || behind) s = await refresh(env);
  return Response.json(s, { headers: { 'Cache-Control': 'public, max-age=300' } });
}
