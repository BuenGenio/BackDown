// Capture the landing-page slideshow: the real dashboard/report UI (served with demo data by serve.py),
// one viewport per slide, light and dark, at 2x. Writes PNGs to out/; encode.py turns them into public/shots/*.webp.
import { spawn } from 'node:child_process';
import { mkdirSync } from 'node:fs';
import puppeteer from 'puppeteer-core';

const PORT = 8790, BASE = `http://127.0.0.1:${PORT}`;
const FIREFOX = process.env.FIREFOX || '/usr/bin/firefox';
const VIEW = { width: 1280, height: 800, deviceScaleFactor: 2 };

// slide id -> page, the element to scroll under the header (null = top), and tree rows to expand first (by label)
export const SLIDES = [
  { id: 'verdicts', page: '/report', at: '#verdicts' },
  { id: 'overview', page: '/report', at: null },
  { id: 'tree', page: '/report', at: '#bytes', open: ['Backups', 'TimeMachine'] },
  { id: 'duplicates', page: '/report', at: '#dups' },
  { id: 'verification', page: '/report', at: '#verify' },
  { id: 'dashboard', page: '/', at: null },
];

const server = spawn('python3', ['serve.py', '--port', String(PORT)], { cwd: import.meta.dirname, stdio: 'ignore' });
await new Promise(r => setTimeout(r, 800));
mkdirSync(new URL('out/', import.meta.url), { recursive: true });

const browser = await puppeteer.launch({ browser: 'firefox', executablePath: FIREFOX, headless: true });
try {
  const page = await browser.newPage();
  await page.setViewport(VIEW);
  for (const theme of ['light', 'dark']) {
    for (const s of SLIDES) {
      await page.goto(`${BASE}${s.page}?theme=${theme}`, { waitUntil: 'networkidle0' });
      await page.evaluate(() => document.fonts.ready);
      for (const label of s.open || []) await page.evaluate(l => [...document.querySelectorAll('#tree .row.has')].find(r => r.querySelector('.lbl')?.textContent === l)?.click(), label);
      await page.evaluate(sel => {
        const header = document.querySelector('.hdr')?.offsetHeight || 60;
        const el = sel && document.querySelector(sel);
        window.scrollTo(0, el ? el.getBoundingClientRect().top + window.scrollY - header - 20 : 0);
      }, s.at);
      await new Promise(r => setTimeout(r, 900)); // meters animate for .6s
      const path = `out/${s.id}-${theme}.png`;
      await page.screenshot({ path: new URL(path, import.meta.url).pathname });
      console.log(path);
    }
  }
} finally {
  await browser.close();
  server.kill();
}
