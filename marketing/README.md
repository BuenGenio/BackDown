# backdown.io

The landing page, in English (`/`), German (`/de/`), Polish (`/pl/`), Czech (`/cs/`), Slovak (`/sk/`) and Ukrainian (`/uk/`). A Cloudflare Worker serves `public/` as static assets; `src/worker.js` runs only for `/api/*` and stores early-access emails (with the page language) in the `WAITLIST` KV namespace.

The pages are generated: edit `page.html` (markup) and `i18n/<lang>.json` (strings), then run `python3 build.py`. It fails if any language is missing a string or has one English doesn't, so every language must be updated together. Standard library only. The generated `public/**/index.html` files are committed so the site deploys without a build step.

Run everything from this directory:

| | |
|---|---|
| Rebuild pages | `python3 build.py` |
| Preview locally | `npx wrangler dev` |
| Deploy | `python3 build.py && npx wrangler deploy` (attaches `backdown.io` and `www.backdown.io` as custom domains) |
| Read sign-ups | `npx wrangler kv key list --binding WAITLIST --remote` |
| Regenerate `og.png` | `firefox --headless --window-size=1200,630 --screenshot "$PWD/public/og.png" "file://$PWD/og/og.html"` |

Fonts: `bricolage-normal` / `instrument-*` are copies of `../assets/fonts/` (basic Latin only). The `*-latin-ext` subsets (for cs/sk/pl) come from the same families on Google Fonts; neither family has Cyrillic, so `inter-tight-cyrillic` / `inter-cyrillic` stand in for Ukrainian. `site.css` maps them all by `unicode-range`, so each page downloads only what its text needs. The tokens at the top of `site.css` mirror `../assets/shell.css`.

Keep the feature status tags (`Works today` / `In development` / `Planned`) honest as the roadmap moves, in every language.
