// Capture the landing-page slideshow: the real dashboard/report UI (served with demo data by serve.py), one viewport
// per slide, light and dark, at 2x, in every site language. The app UI is English; for other languages the rendered
// page's text is swapped using ui-i18n/<lang>.json and its numbers reformatted for the locale before capture.
// Writes out/<lang>/<slide>-<theme>.png; encode.py turns them into public/shots/<lang>/*.webp.
//   node capture.mjs [lang,lang,...]      (default: every ui-i18n/*.json)
import { spawn } from 'node:child_process';
import { mkdirSync, readFileSync, readdirSync } from 'node:fs';
import puppeteer from 'puppeteer-core';

const PORT = 8790, BASE = `http://127.0.0.1:${PORT}`;
const FIREFOX = process.env.FIREFOX || '/usr/bin/firefox';
const VIEW = { width: 1280, height: 800, deviceScaleFactor: 2 };
const here = p => new URL(p, import.meta.url).pathname;
const LANGS = process.argv[2]?.split(',') ?? readdirSync(here('ui-i18n/')).filter(f => f.endsWith('.json')).map(f => f.slice(0, -5));

// slide id -> page, the element to scroll under the header (null = top), and tree rows to expand first (by label)
const SLIDES = [
  { id: 'overview', page: '/report', at: null },
  { id: 'verdicts', page: '/report', at: '#verdicts' },
  { id: 'tree', page: '/report', at: '#bytes', open: ['Backups', 'TimeMachine'] },
  { id: 'duplicates', page: '/report', at: '#dups' },
  { id: 'verification', page: '/report', at: '#verify' },
  { id: 'dashboard', page: '/', at: null },
];

// runs in the page: translate text nodes (exact phrases, then patterns) and localise numbers outside <code>
function localize({ exact, patterns, locale, suffixes = {} }) {
  for (let id = setInterval(() => {}, 1e6); id > 0; id--) clearInterval(id); // the dashboard re-renders every 5 s
  document.documentElement.lang = locale;
  const nfs = new Map();
  // 1,234.5 -> locale digits; the app's "2.11M" / "402 K" count suffixes -> the locale's abbreviation when one is given
  const num = s => s.replace(/(\d{1,3}(?:,\d{3})+(?:\.\d+)?|\d+\.\d+|\d+)( ?([KM])(?!\w))?/g, (m, n, _, suf) => {
    if (!suf && /^\d+$/.test(n)) return m; // years, times, counts without separators: leave alone
    const dec = (n.split('.')[1] || '').length;
    if (!nfs.has(dec)) nfs.set(dec, new Intl.NumberFormat(locale, { minimumFractionDigits: dec, maximumFractionDigits: dec }));
    const out = nfs.get(dec).format(Number(n.replace(/,/g, '')));
    return suf ? out + (suffixes[suf] ? '\u00a0' + suffixes[suf] : (m.includes(' ') ? ' ' : '') + suf) : out;
  });
  const res = patterns.map(([re, to]) => [new RegExp(re), to]);
  const tr = text => {
    const t = text.trim();
    if (!t) return text;
    const at = text.indexOf(t), wrap = s => text.slice(0, at) + s + text.slice(at + t.length);
    if (Object.hasOwn(exact, t)) return wrap(exact[t]);
    for (const [re, to] of res) if (re.test(t)) return wrap(num(t.replace(re, to)));
    return wrap(num(t));
  };
  const walk = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  for (let n; (n = walk.nextNode());) if (!n.parentElement.closest('script, style, code')) n.textContent = tr(n.textContent);
  document.querySelectorAll('[placeholder]').forEach(e => { e.placeholder = tr(e.placeholder); });
}

const server = spawn('python3', ['serve.py', '--port', String(PORT)], { cwd: import.meta.dirname, stdio: 'ignore' });
await new Promise(r => setTimeout(r, 800));

const browser = await puppeteer.launch({ browser: 'firefox', executablePath: FIREFOX, headless: true });
try {
  const page = await browser.newPage();
  await page.setViewport(VIEW);
  for (const lang of LANGS) {
    const dict = JSON.parse(readFileSync(here(`ui-i18n/${lang}.json`), 'utf8'));
    mkdirSync(here(`out/${lang}/`), { recursive: true });
    for (const theme of ['light', 'dark']) {
      for (const s of SLIDES) {
        await page.goto(`${BASE}${s.page}?theme=${theme}`, { waitUntil: 'networkidle0' });
        for (const label of s.open || []) await page.evaluate(l => [...document.querySelectorAll('#tree .row.has')].find(r => r.querySelector('.lbl')?.textContent === l)?.click(), label);
        if (lang !== 'en') await page.evaluate(localize, dict);
        await page.evaluate(() => document.fonts.ready);
        await page.evaluate(sel => {
          const header = document.querySelector('.hdr')?.offsetHeight || 60;
          const el = sel && document.querySelector(sel);
          window.scrollTo({ top: el ? el.getBoundingClientRect().top + window.scrollY - header - 20 : 0, behavior: 'instant' });
        }, s.at);
        await new Promise(r => setTimeout(r, 900)); // meters animate for .6s
        await page.screenshot({ path: here(`out/${lang}/${s.id}-${theme}.png`) });
      }
    }
    console.log(`out/${lang}/ (${SLIDES.length} slides × 2 themes)`);
  }
} finally {
  await browser.close();
  server.kill();
}
