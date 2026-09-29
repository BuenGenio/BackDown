// Render the press kit's logo PNGs, with transparent backgrounds, from the SVGs build.py writes to
// public/press/logos/ (from the brand masters in public/brand/ and public/favicon.svg). Run after build.py, then
// build.py again so the zip picks them up:  python3 ../build.py && node press.mjs && python3 ../build.py
// Firefox's screenshots can't leave out the page background, so each SVG is drawn on a canvas and saved from there.
import { readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import puppeteer from 'puppeteer-core';

const LOGOS = new URL('../public/press/logos/', import.meta.url).pathname;
// width in pixels: large enough for print, small enough to mail
const WIDTH = { logo: 2400, wordmark: 2000, mark: 1200, icon: 1024 };

const browser = await puppeteer.launch({ browser: 'firefox', executablePath: process.env.FIREFOX || '/usr/bin/firefox', headless: true });
try {
  const page = await browser.newPage();
  await page.setContent('<!doctype html><title>logos</title>');
  for (const file of readdirSync(LOGOS).filter(f => f.endsWith('.svg')).sort()) {
    const svg = readFileSync(join(LOGOS, file), 'utf8');
    const [, , vw, vh] = svg.match(/viewBox="([^"]+)"/)[1].split(/\s+/).map(Number);
    const kind = file.replace(/^backdown-|(-dark)?\.svg$/g, '');
    const w = WIDTH[kind], h = Math.round(w * vh / vw);
    const sized = svg.replace(/ width="[^"]*"| height="[^"]*"/g, '').replace('<svg ', `<svg width="${w}" height="${h}" `);
    const png = await page.evaluate(async (svg, w, h) => {
      const img = new Image();
      img.src = 'data:image/svg+xml;charset=utf-8,' + encodeURIComponent(svg);
      await img.decode();
      const c = document.createElement('canvas');
      c.width = w; c.height = h;
      c.getContext('2d').drawImage(img, 0, 0, w, h);
      return c.toDataURL('image/png').split(',')[1];
    }, sized, w, h);
    writeFileSync(join(LOGOS, file.replace(/\.svg$/, '.png')), Buffer.from(png, 'base64'));
    console.log(`public/press/logos/${file.replace(/\.svg$/, '.png')}  ${w}×${h}`);
  }
} finally {
  await browser.close();
}
