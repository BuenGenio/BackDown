// Drive the demo BackDown's UI in headless Firefox and record every API response it gets, plus a crawl of
// every folder listing and the images in chosen folders. Writes OUT/d/<sha1(key)> and OUT/index.json.
//   node record.mjs <base url> <token> <out dir>        (FIREFOX=/path/to/firefox if it isn't /usr/bin/firefox)
import puppeteer from 'puppeteer-core';
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { demoKey } from '../../src/demo-keys.js';

const [, , BASE, TOKEN, OUT] = process.argv;
fs.mkdirSync(path.join(OUT, 'd'), { recursive: true });
const index = {};
const sha1 = s => crypto.createHash('sha1').update(s).digest('hex');
const stats = { saved: 0, replaced: 0, bytes: 0 };
function store(method, url, status, type, body) {
  const u = new URL(url);
  if (!u.pathname.startsWith('/api/')) return;
  const key = demoKey(method, u.pathname, u.search);
  const file = sha1(key);
  if (index[key]) stats.replaced++; else stats.saved++;
  stats.bytes += body.length;
  index[key] = { status, type: type || '', file, size: body.length, method, path: u.pathname + u.search };
  fs.writeFileSync(path.join(OUT, 'd', file), body);
}

// ---- node side: the session cookie, and a crawler that records what it fetches
const r0 = await fetch(`${BASE}/?t=${TOKEN}`, { redirect: 'manual' });
const cookie = (r0.headers.get('set-cookie') || '').split(';')[0];
if (!cookie) throw new Error('no session cookie: wrong token?');
async function get(pathq, { method = 'GET' } = {}) {
  const r = await fetch(BASE + pathq, { method, headers: { Cookie: cookie, 'X-BackDown': '1' } });
  const b = Buffer.from(await r.arrayBuffer());
  store(method, BASE + pathq, r.status, r.headers.get('content-type'), b);
  const ct = r.headers.get('content-type') || '';
  return ct.includes('json') ? JSON.parse(b.toString()) : null;
}
const q = o => new URLSearchParams(o).toString();

// ---- browser side
const browser = await puppeteer.launch({ browser: 'firefox', executablePath: process.env.FIREFOX || '/usr/bin/firefox', headless: true });
const page = await browser.newPage();
await page.setViewport({ width: 1400, height: 900 });
const errors = [];
page.on('pageerror', e => errors.push('pageerror: ' + e.message));
page.on('console', m => { if (m.type() === 'error') errors.push('console: ' + m.text()); });
page.on('response', async res => {
  try {
    const url = res.url();
    if (!url.startsWith(BASE + '/api/')) return;
    const status = res.status();
    if (status === 304 || status === 204 || (status >= 300 && status < 400)) return;
    let body;
    try { body = await res.buffer(); } catch { return; }
    store(res.request().method(), url, status, res.headers()['content-type'], body);
  } catch {}
});
const sleep = ms => new Promise(r => setTimeout(r, ms));
// the app keeps one long poll (/api/daemon?wait=…) in flight, so idle means one request at most
async function idle(ms = 600) { await sleep(ms); try { await page.waitForNetworkIdle({ idleTime: 400, concurrency: 1, timeout: 6000 }); } catch {} }
async function click(sel, wait = true) {
  try { await page.waitForSelector(sel, { timeout: 10000 }); await page.click(sel); } catch (e) { console.log('  (no ' + sel + ')'); return false; }
  if (wait) await idle(); return true;
}
async function clickEach(sel, max = 6, back = null) {
  const n = Math.min(max, await page.$$eval(sel, els => els.length).catch(() => 0));
  for (let i = 0; i < n; i++) {
    const els = await page.$$(sel); if (!els[i]) break;
    await els[i].click().catch(() => {}); await idle();
    if (back) await click(back);
  }
  return n;
}
async function openByName(name) {
  const ok = await page.evaluate(name => {
    const els = [...document.querySelectorAll('#view .tile[data-key], #view tr[data-key], #view .g-dir[data-key], #view figure.ph[data-key], #view .tm[data-key]')];
    const el = els.find(e => (e.getAttribute('title') || '').includes(name) || (e.querySelector('.name, figcaption span, .pn, td')?.textContent || '').trim().startsWith(name));
    if (!el) return false;
    el.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true, detail: 2 })); return true; // the app opens on a click with detail >= 2
  }, name);
  await idle(); if (!ok) console.log('  (not found: ' + name + ')'); return ok;
}
async function viewer(name) {
  if (!await openByName(name)) return;
  await sleep(1500); await idle(800);
  await page.evaluate(() => document.querySelector('#v-close')?.click()); await sleep(200);
}
const step = s => console.log('-- ' + s);

step('boot'); await page.goto(`${BASE}/?t=${TOKEN}`, { waitUntil: 'load' });
await page.waitForSelector('#places .place', { timeout: 30000 }); await idle(2000);

step('crawl: every folder of every drive, the time line of each, the roots');
const roots = await get('/api/roots');
await get('/api/daemon'); await get('/api/features'); await get('/api/volumes'); await get('/api/health'); await get('/api/mounts');
const queue = roots.roots.map(r => r.key), seen = new Set(queue), listings = {};
while (queue.length) {
  const k = queue.shift();
  const l = await get(`/api/ls?${q({ dir: k })}`); if (!l) continue;
  listings[k] = l;
  for (const e of l.entries || []) if (e.kind === 'dir' && !seen.has(e.key)) { seen.add(e.key); queue.push(e.key); }
}
console.log('  folders:', Object.keys(listings).length);
for (const r of roots.roots) { await get(`/api/timeline?${q({ root: r.id })}`).catch(() => {}); await get(`/api/scans?${q({ root: r.id })}`).catch(() => {}); }
step('crawl: image content in the photo folders (thumbnails), file details of everything small');
const atob64 = k => Buffer.from(k.replace(/-/g, '+').replace(/_/g, '/'), 'base64').toString();
const WANT_IMAGES = /\/(Pictures|DCIM|Photos|uploads|Paris 2019|Lisbon 2017|Kyoto 2023|Lviv 20\d\d|Panoramas|20\d\d)(\/|$)/;
let imgs = 0;
for (const [k, l] of Object.entries(listings)) {
  const dir = atob64(k);
  if (!WANT_IMAGES.test(dir) || /Backups\.backupdb|Nextcloud data|iPhone backup|Archive|Mirror|Laptop copy/.test(dir)) continue;
  for (const e of l.entries || []) if (e.kind === 'file' && /\.(jpe?g|png)$/i.test(e.name) && e.size < 3e6) { await get(`/api/content?${q({ id: e.id })}`); imgs++; }
}
console.log('  images:', imgs);

step('views: grid, list, gallery, size map; the details panel');
const laptop = roots.roots.find(r => r.name === 'Old Laptop');
await page.click(`#places .place[data-key="${laptop.key}"]`); await idle();
await openByName('home'); await openByName('alex');
for (const v of ['list', 'gallery', 'treemap', 'grid']) { await click(`#view-${v}`); }
await click('#info-btn'); await page.click('#view .tile[data-key]').catch(() => {}); await idle(); await click('#info-btn');
await openByName('Pictures'); await click('#view-gallery'); await openByName('Paris 2019'); await idle(1500);
await viewer('IMG_1052.JPG');
await click('#view-grid'); await click('#back'); await click('#back');

step('viewers: documents of every kind');
await openByName('Documents');
for (const n of ['Camera club constitution.docx', 'Trip budget 2019.xlsx', 'Flat renovation plan.odt', 'Club talk 2019.pptx']) await viewer(n);
const docs = listings[Object.keys(listings).find(k => atob64(k).endsWith('/Old Laptop/home/alex/Documents'))];
const pick = re => (docs?.entries || []).filter(e => e.kind === 'file' && re.test(e.name)).slice(0, 2).map(e => e.name);
for (const n of [...pick(/\.pdf$/), ...pick(/\.md$/), ...pick(/\.csv$/), ...pick(/\.json$/), ...pick(/\.html$/), ...pick(/\.txt$/)]) await viewer(n);
await click('#back');
await openByName('Mail'); await sleep(1500); await idle(); await openByName('cur'); await click('#back'); await click('#back');
await openByName('Projects'); await openByName('gallery-site'); await sleep(1200); await idle(); await viewer('README.md'); await click('#back'); await click('#back');
await openByName('.local'); await openByName('share'); await openByName('shotwell'); await openByName('data'); await viewer('photo.db');

step('time line: the earlier scan of the laptop');
await page.click(`#places .place[data-key="${laptop.key}"]`); await idle(); await openByName('home'); await openByName('alex'); await openByName('Documents');
await page.evaluate(() => { const t = document.querySelector('#time'); if (t) { t.value = 0; t.dispatchEvent(new Event('input', { bubbles: true })); t.dispatchEvent(new Event('change', { bubbles: true })); } }); await idle();
await click('#time-compare'); await idle(1200); await click('#time-now');

step('the backup drive: Time Machine, iPhone backup, Nextcloud, server, the mbox');
const backup = roots.roots.find(r => r.name === 'Backup 2021');
await page.click(`#places .place[data-key="${backup.key}"]`); await idle();
await viewer('mail-export-2018.mbox');
await openByName('Server'); await viewer('gallery.sqlite'); await openByName('dumps'); await viewer('photoclub-2019-01-01.sql'); await click('#back'); await click('#back');
await openByName('iPhone backup'); await idle(); await click('#back');

step('the archive and the mirror');
for (const name of ['Archive', 'Mirror', 'Camera cards']) { const r = roots.roots.find(x => x.name === name); if (r) { await page.click(`#places .place[data-key="${r.key}"]`); await idle(); } }

step('search');
await click('#search-btn');
for (const term of ['paris', 'invoice', 'IMG_1049', 'Lisbon walk', 'constitution', 'zzqx-nothing']) {
  await page.$eval('#q', el => { el.value = ''; }); await page.type('#q', term); await page.keyboard.press('Enter'); await idle(1200);
}
await click('#searchbox [data-scope="all"]'); await page.$eval('#q', el => { el.value = ''; }); await page.type('#q', 'paris'); await page.keyboard.press('Enter'); await idle(2500);
await click('#searchbox [data-scope="contents"]'); await page.$eval('#q', el => { el.value = ''; }); await page.type('#q', 'members'); await page.keyboard.press('Enter'); await idle(2500);
await click('#searchbox [data-scope="here"]'); await page.keyboard.press('Escape'); await idle();

step('duplicates'); await click('#dupes-btn'); await page.waitForSelector('.dp-row', { timeout: 30000 }).catch(() => {}); await idle(1500);
await clickEach('[data-dg-by]', 3); await clickEach('.dp-row td:nth-child(4)', 2);

step('recognised things'); await click('#entities-btn'); await idle(1500);
await clickEach('.ent-path[data-ent-open]', 3, '#entities-btn');

step('places'); await click('#places-btn'); await sleep(2500); await idle(1500);
await clickEach('[data-pl-facet]', 4); await clickEach('[data-pl-sort]', 3); await click('[data-pl-side="tags"]'); await click('[data-pl-side="refine"]');
await clickEach('.pl-chip button', 4);

step('swarm'); await click('#swarm-btn'); await sleep(3000); await idle(2000);
await clickEach('#view button.sw-m, #view [data-sw]', 4);

step('machines: the NAS'); await click('#machines .place[data-mc]'); await sleep(2500); await idle(1500);
await clickEach('#machines .place.sub[data-mc-dir]', 2); await idle(1500);
await page.evaluate(() => [...document.querySelectorAll('#view [data-key], #view tr')].slice(0, 1).forEach(e => e.dispatchEvent(new MouseEvent('click', { bubbles: true, detail: 2 })))); await idle(1500);
await click('#machines .place.add[data-mc-add]'); await idle();

step('plan'); await click('#plan-btn'); await idle(1500); await clickEach('#view [data-plan], #view .plan-row button', 2);

step('wipe'); await click('#wipe-btn'); await idle(1500);
const cards = roots.roots.find(r => r.name === 'Camera cards');
await page.click(`[data-wp-key="${cards.key}"]`).catch(() => {}); await idle(2500);
await page.click(`[data-wp-key="${backup.key}"]`).catch(() => {}); await idle(3000);
await clickEach('[data-wp-tw]', 2);

step('rescue'); await click('#rescue-btn'); await idle(1500);
await clickEach('input[name="rs-src"]', 4);

step('activity, health, custody'); await click('#activity-btn'); await idle(2000);
await clickEach('[data-health]', 4); await clickEach('.hpill', 4); await clickEach('#view .act-card button', 4, '#activity-btn');

step('settings'); await click('#settings-btn'); await idle(1500);
await clickEach('#view [role="tab"], #view .set-nav button, #view nav button', 12);
await get('/api/settings'); await get('/api/license'); await get('/api/notify-settings'); await get('/api/manifests'); await get('/api/schema'); await get('/api/update');
await get('/api/plans'); await get(`/api/plan?${q({ id: 1 })}`); await get('/api/machines'); await get('/api/health-machines'); await get('/api/custody'); await get('/api/mirrors'); await get('/api/notifications');
await get('/api/rescues'); await get('/api/swarm'); await get('/api/swarm-pool'); await get('/api/pool-plans'); await get('/api/entities'); await get('/api/dupes'); await get('/api/overlaps'); await get('/api/report'); await get('/api/report?format=md');
await get('/api/report?format=json');

fs.writeFileSync(path.join(OUT, 'index.json'), JSON.stringify({ base: BASE, recorded_at: new Date().toISOString(), entries: index }, null, 1));
console.log('recorded', stats.saved, 'responses (', stats.replaced, 'replaced),', (stats.bytes / 1e6).toFixed(1), 'MB');
console.log('page errors:', errors.length, errors.slice(0, 8));
await browser.close();
