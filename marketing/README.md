# backdown.io

The landing page, in English (`/`), German (`/de/`), Polish (`/pl/`), Czech (`/cs/`), Slovak (`/sk/`) and Ukrainian (`/uk/`). A Cloudflare Worker serves `public/` as static assets; `src/worker.js` runs only for `/api/*` and stores early-access emails (with the page language) in the `WAITLIST` KV namespace. `www.backdown.io` is a separate Worker (`www-redirect/`) that 301s to the apex, so page views never invoke Worker code.

## Editing

The pages are generated: edit `page.html` (markup), `i18n/<lang>.json` (strings) and `downloads.json` (download cards), then run `python3 build.py`. It fails if any language is missing a string or has one English doesn't, so every language must be updated together. It also writes `public/sitemap.xml`. Standard library only; the generated `public/**/index.html` files are committed so the site deploys without a build step.

When real builds exist, set a platform in `downloads.json` to `"status": "available"` with its `version` and `files` (`label`, `url`, `sha256`); the card switches from "early access / coming in 1.0" to download buttons with checksums.

## Commands

Run from this directory unless noted:

| | |
|---|---|
| Rebuild pages + sitemap | `python3 build.py` |
| Preview locally | `npx wrangler dev` |
| Deploy the site | `python3 build.py && npx wrangler deploy` |
| Deploy the www redirect | `cd www-redirect && npx wrangler deploy` (only when it changes) |
| Read sign-ups | `npx wrangler kv key list --binding WAITLIST --remote` |
| Re-shoot the slideshow | `cd screenshots && npm install && npm run capture` |
| Re-render OG cards + icons | `cd screenshots && npm run assets` |

## Screenshots (`screenshots/`)

The slideshow shows the real dashboard and report UI (`../bd-dash.py`, `../assets/report.html`), served by `serve.py` with an invented demo dataset; nothing reads `index/` or any real drive. The report page hardcodes a few details of the founding dataset, which `serve.py` neutralises by pattern in its served copy. `capture.mjs` drives Firefox (via puppeteer-core) to shoot each slide in light and dark at 2x, per language; `encode.py` writes `public/shots/<lang>/<slide>-<theme>-{960,1600,2400}.webp`.

The app UI is English-only, so for other languages `capture.mjs` swaps the rendered text using `ui-i18n/<lang>.json` (exact phrases, plus regex patterns for generated lines) and reformats numbers for the locale. If the app's UI text changes, re-extract the visible strings and update every `ui-i18n` file.

`assets.mjs` renders `public/og/<lang>.png` from `og/og.html` and the icons, using the site's own CSS and fonts.

## Fonts

`bricolage-normal` / `instrument-*` are copies of `../assets/fonts/` (basic Latin only). The `*-latin-ext` subsets (for cs/sk/pl) come from the same families on Google Fonts; neither family has Cyrillic, so `inter-tight-cyrillic` / `inter-cyrillic` stand in for Ukrainian. `site.css` maps them all by `unicode-range`, so each page downloads only what its text needs. The tokens at the top of `site.css` mirror `../assets/shell.css`.

Keep the feature status tags (`Works today` / `In development` / `Planned`) honest as the roadmap moves, in every language.
