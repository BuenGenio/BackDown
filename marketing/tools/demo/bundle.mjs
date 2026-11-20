// Turn a recording (record.mjs) and the app's own UI into the static demo under marketing/public/demo/:
// the UI's files with their /api/ and /vendor/ paths moved under /demo/, demo.js in front of the app, and the
// recorded responses, with every path from the demo's build machine rewritten to the fictional machine's.
//   node bundle.mjs <base url> <token> <recording dir>      (D, DEMO_RUN and BD from env.sh; writes ../../public/demo)
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import os from 'node:os';
import { demoKey } from '../../src/demo-keys.js';

const [, , BASE, TOKEN, REC] = process.argv;
const HERE = path.dirname(new URL(import.meta.url).pathname);
const MK = path.resolve(HERE, '../..');
const D = process.env.D, RUN = process.env.DEMO_RUN || '/tmp/bd-demo-run', BD = process.env.BD;
if (!D || !BD) throw new Error('source tools/demo/env.sh first');
const OUT = path.join(MK, 'public/demo');
fs.rmSync(OUT, { recursive: true, force: true }); fs.mkdirSync(path.join(OUT, 'd'), { recursive: true }); fs.mkdirSync(path.join(OUT, 'vendor'));
const sha1 = s => crypto.createHash('sha1').update(s).digest('hex');

// ---- paths: the build machine's -> the fictional machine's (longest first)
const MAP = [
  [`${D}/drives`, '/media/alex'],
  [`${D}/nas-xdg/data/backdown`, '/volume1/@appdata/backdown'],
  [`${D}/xdg/data/backdown`, '/home/alex/.local/share/backdown'],
  [`${RUN}/backdown`, '/run/user/1000/backdown'],
  [`${D}/nas`, ''],
  [D, '/home/alex/.config/backdown-demo'],
  [BD, '/usr/bin/backdown'],
  [os.homedir(), '/home/alex'],
  [`u:${os.userInfo().username}:`, 'u:alex:'], // setfacl hints in rescue-access
  [os.hostname(), 'alex-desktop'],
].filter(([a]) => a).sort((a, b) => b[0].length - a[0].length);
const PREFIX_RE = new RegExp(MAP.map(([a]) => a.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('|'), 'g');
const mapPlain = s => s.replace(PREFIX_RE, m => MAP.find(([a]) => a === m)[1]);
const b64d = k => { try { return Buffer.from(k.replace(/-/g, '+').replace(/_/g, '/'), 'base64').toString('utf8'); } catch { return ''; } };
const b64e = s => Buffer.from(s, 'utf8').toString('base64').replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
const TOKEN_RE = /[A-Za-z0-9_-]{24,}/g; // base64url keys of paths (the app's folder and file keys)
function rewriteText(t) {
  t = t.replace(/"volumes read from [^"]*\(BACKDOWN_FAKE_VOLUMES\)",?/g, '').replace(/,\s*\]/g, ']').replace(/1\.0\.0-beta\.10/g, '1.0.0-beta.9');
  t = t.replace(TOKEN_RE, k => { const d = b64d(k); return d.startsWith(D) || d.startsWith(os.homedir()) ? b64e(mapPlain(d)) : k; });
  return mapPlain(t);
}
function rewriteKey(key) {
  const [method, rest] = key.split(' ', 2); const i = rest.indexOf('?');
  const pathname = i < 0 ? rest : rest.slice(0, i), search = i < 0 ? '' : rest.slice(i + 1);
  const pairs = [...new URLSearchParams(search)].map(([k, v]) => [k, rewriteText(v)]);
  return demoKey(method, pathname, new URLSearchParams(pairs).toString());
}

// ---- the recording
const rec = JSON.parse(fs.readFileSync(path.join(REC, 'index.json'), 'utf8'));
const entries = {}; let bytes = 0;
const TEXT = /json|text|javascript|xml|markdown|svg/;
for (const [key, e] of Object.entries(rec.entries)) {
  let body = fs.readFileSync(path.join(REC, 'd', e.file));
  if (TEXT.test(e.type)) {
    let t = rewriteText(body.toString('utf8'));
    if (/html/.test(e.type)) t = t.replace(/(["'(])\/api\//g, '$1/demo/api/');
    body = Buffer.from(t, 'utf8');
  }
  const nk = rewriteKey(key); const file = sha1(nk);
  fs.writeFileSync(path.join(OUT, 'd', file), body); bytes += body.length;
  entries[nk] = [e.status, e.type, file];
}
// where the demo falls short of the live app, the nearest recorded answer
const fallback = {};
const none = Object.keys(entries).find(k => /^GET \/api\/(find|search)\?/.test(k) && k.includes('zzqx-nothing'));
if (none) { fallback['/api/search'] = none; fallback['/api/find'] = none; }
// the map asks for labels and tiles by the bounding box it happens to show: any recorded answer is better than none
for (const path of ['/api/places-labels', '/api/places-basemap', '/api/places']) {
  const first = Object.keys(entries).find(k => k.startsWith(`GET ${path}?`));
  if (first && !entries[`GET ${path}`]) fallback[path] = first;
}
fs.writeFileSync(path.join(OUT, 'd', 'index.json'), JSON.stringify({ recorded_at: rec.recorded_at, entries, fallback }));
console.log('responses:', Object.keys(entries).length, (bytes / 1e6).toFixed(1), 'MB');

// ---- the UI, from the running app (so it matches the API that was recorded)
const r0 = await fetch(`${BASE}/?t=${TOKEN}`, { redirect: 'manual' });
const cookie = (r0.headers.get('set-cookie') || '').split(';')[0];
const UI = ['index.html', 'app.css', 'dashboard.css', 'settings.css', 'app.js', 'dashboard.js', 'viewers.js', 'icons.js', 'extras.js', 'mailbox.js', 'sqldump.js', 'search.js', 'settings.js', 'tidy.js',
  'health.js', 'machines.js', 'swarm.js', 'pool.js', 'fsbrowse.js', 'drives.js', 'dupes.js', 'places.js', 'geomap.js', 'favicon.svg',
  'vendor/pdf.min.mjs', 'vendor/pdf.worker.min.mjs', 'vendor/docx-preview.min.js', 'vendor/jszip.min.js', 'vendor/highlight.min.js',
  'vendor/hljs-light.css', 'vendor/hljs-dark.css'];
for (const name of UI) {
  const r = await fetch(`${BASE}/${name}`, { headers: { Cookie: cookie } });
  if (!r.ok) { console.log('missing', name, r.status); continue; }
  let body = Buffer.from(await r.arrayBuffer());
  if (/\.(js|mjs)$/.test(name) && !name.startsWith('vendor/')) {
    // string literals, and the regex literal the busy indicator uses to leave the daemon's long poll alone (/^\/api\/daemon\?/)
    body = Buffer.from(body.toString('utf8').replace(/(["'`])\/api\//g, '$1/demo/api/').replace(/\\\/api\\\//g, '\\/demo\\/api\\/').replace(/(["'`(])\/vendor\//g, '$1/demo/vendor/').replace(/(["'`])\/favicon\.svg/g, '$1/demo/favicon.svg'));
  } else if (name === 'index.html') {
    let t = body.toString('utf8')
      .replace(/(href|src)="\/(app\.css|dashboard\.css|settings\.css|app\.js|favicon\.svg|vendor\/[^"]+)"/g, '$1="/demo/$2"')
      .replace('<script type="module" src="/demo/app.js"', '<script src="/demo/demo.js"></script>\n<script type="module" src="/demo/app.js"')
      .replace(/<title>[^<]*<\/title>/, '<title>BackDown demo</title>')
      .replace('<meta charset="utf-8">', '<meta charset="utf-8">\n<meta name="robots" content="noindex">'); // an app shell, not a page to index
    if (!t.includes('/demo/demo.js')) throw new Error("index.html: couldn't place demo.js");
    body = Buffer.from(t);
  }
  fs.writeFileSync(path.join(OUT, name), body);
}
fs.copyFileSync(path.join(HERE, 'demo.js'), path.join(OUT, 'demo.js'));
fs.copyFileSync(path.join(HERE, 'placeholder.svg'), path.join(OUT, 'placeholder.svg'));
console.log('ui files:', UI.length, '->', OUT);
