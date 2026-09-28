// Render the site's raster assets with the site's own CSS and fonts:
//   public/og/<lang>.png          social cards (1200×630) from og/og.html + i18n/<lang>.json (hero.title, meta.og_description),
//   public/og/<lang>-<page>.png   and one per sub-page (its headline and meta description)
//   public/apple-touch-icon.png   180×180, public/icon-192.png, public/icon-512.png (full-bleed mark, maskable-safe)
import { execFileSync } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdirSync, readFileSync, readdirSync } from 'node:fs';
import { extname, join, normalize } from 'node:path';
import puppeteer from 'puppeteer-core';

const SITE = new URL('..', import.meta.url).pathname;
const PUBLIC = join(SITE, 'public');
const TYPES = { '.html': 'text/html', '.css': 'text/css', '.js': 'text/javascript', '.woff2': 'font/woff2', '.svg': 'image/svg+xml', '.png': 'image/png', '.webp': 'image/webp' };

// public/ at /, the OG template at /__og
const server = createServer((req, res) => {
  const path = decodeURIComponent(new URL(req.url, 'http://x').pathname);
  const file = path === '/__og' ? join(SITE, 'og', 'og.html') : join(PUBLIC, normalize(path).replace(/^(\.\.[/\\])+/, ''));
  let body;
  try { body = readFileSync(file); } catch { return res.writeHead(404).end(); }
  res.writeHead(200, { 'Content-Type': TYPES[extname(file)] || 'application/octet-stream' }).end(body);
}).listen(0, '127.0.0.1');
await new Promise(r => server.once('listening', r));
const base = `http://127.0.0.1:${server.address().port}`;

const browser = await puppeteer.launch({ browser: 'firefox', executablePath: process.env.FIREFOX || '/usr/bin/firefox', headless: true });
try {
  const page = await browser.newPage();

  mkdirSync(join(PUBLIC, 'og'), { recursive: true });
  await page.setViewport({ width: 1200, height: 630, deviceScaleFactor: 1 });
  // one card per page that has one (build.py --og-pages: page id -> the i18n key of its headline; the home page uses
  // hero.title); the text under the headline is the page's meta description
  const pages = JSON.parse(execFileSync('python3', [join(SITE, 'build.py'), '--og-pages'], { encoding: 'utf8' }));
  for (const file of readdirSync(join(SITE, 'i18n')).filter(f => f.endsWith('.json'))) {
    const lang = file.slice(0, -5), t = JSON.parse(readFileSync(join(SITE, 'i18n', file), 'utf8'));
    for (const [id, titleKey] of Object.entries(pages)) {
      const title = titleKey ? t[titleKey] : t['hero.title'];
      const desc = id === 'home' ? t['meta.og_description'] : t[`meta.${id}_description`];
      await page.goto(`${base}/__og`, { waitUntil: 'networkidle0' });
      await page.evaluate((lang, title, desc) => {
        document.documentElement.lang = lang;
        document.getElementById('t').innerHTML = title;
        document.getElementById('d').textContent = desc;
      }, lang, title, desc);
      await page.evaluate(() => document.fonts.ready);
      // sub-page headlines are longer than the home page's: shrink until the card holds headline and description
      await page.evaluate(() => {
        const og = document.querySelector('.og'), h1 = document.getElementById('t');
        for (let size = 112; og.scrollHeight > og.clientHeight && size > 52; size -= 4) h1.style.fontSize = `${size - 4}px`;
      });
      const out = id === 'home' ? `${lang}.png` : `${lang}-${id}.png`;
      await page.screenshot({ path: join(PUBLIC, 'og', out) });
      console.log(`public/og/${out}`);
    }
  }

  // the mark from favicon.svg on a full-bleed accent square; the lines stay inside the central 60% (maskable safe zone)
  const icon = size => `<html><body style="margin:0"><svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 32 32">
    <rect width="32" height="32" fill="#d8432b"/><path d="M10 12.5h12M10 16h12M10 19.5h7.5" fill="none" stroke="#fff" stroke-width="2.2" stroke-linecap="round"/></svg></body></html>`;
  for (const [name, size] of [['apple-touch-icon.png', 180], ['icon-192.png', 192], ['icon-512.png', 512]]) {
    await page.setViewport({ width: size, height: size, deviceScaleFactor: 1 });
    await page.setContent(icon(size));
    await page.screenshot({ path: join(PUBLIC, name) });
    console.log(`public/${name}`);
  }
} finally {
  await browser.close();
  server.close();
}
