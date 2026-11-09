# backdown.io

The website, in English (`/`), Spanish (`/es/`), Brazilian Portuguese (`/pt-br/`), German (`/de/`), Polish (`/pl/`), Czech (`/cs/`), Slovak (`/sk/`) and Ukrainian (`/uk/`). Each language has the same pages: home, `features/`, `pricing/`, `download/`, `status/` and `privacy/`, plus `terms/` (English) and `thanks/` once pre-orders are switched on. The docs (`/docs/`) and the press kit (`/press/`) are in English only for now; every language links to them from the header or footer, and on those pages the language menu says so and sends the other languages to their home page.

A Cloudflare Worker serves `public/` as static assets. `src/` runs only for `/api/*` and an hourly cron:

| | |
|---|---|
| `POST /api/waitlist` | early-access sign-ups → KV `WAITLIST` |
| `GET /api/status` | the status page's live checks (website, downloads, sign-ups, pre-orders), refreshed hourly by the cron and cached in KV `SITE` |
| `GET /api/preorder`, `POST /api/checkout`, `POST /api/stripe/webhook`, `GET /api/order` | pre-orders of Pro, Team and Forensic: Stripe Checkout → signed licence key for the edition bought (KV `ORDERS`); see `src/store.js` |

`www.backdown.io` is a separate Worker (`www-redirect/`) that 301s to the apex.

## Editing

The pages are generated: edit `templates/` (markup: `base.html` is the shell, one file per page), `i18n/<lang>.json` (strings), `downloads.json` (download cards), `status.json` (the status page's work list, roadmap and changelog) and `site.json` (analytics, pre-orders, seller details), then run `python3 build.py`. It fails if any language is missing a string or has one English doesn't, so every language must be updated together; `status.json` entries fall back to English (marked `lang="en"`) so the roadmap can be updated first and translated after. It also writes `public/sitemap.xml` (every page × language, with hreflang alternates). Standard library only; the generated `public/**/index.html` files are committed so the site deploys without a build step.

`site.css` and `site.js` are served immutable with a `?v=<hash>` the build stamps into every page, so edit them, rebuild, deploy.

Moving between pages fades: cross-document view transitions in Chrome, Edge and Safari (`@view-transition` in `site.css`), and a short fade-out in `site.js` elsewhere; the header stays put. Links are prefetched on hover (speculation rules).

### Docs (`docs/`)

`docs/**/*.md` become `/docs/…/` (`docs/guides/rescue.md` → `/docs/guides/rescue/`, `index.md` → its folder). Each file starts with front matter:

```
---
title: Rescue a failing drive
description: One sentence: the page's lede, meta description and card text.
order: 4
nav: Short sidebar label (optional)
group: start | help (top-level files only; files in guides/ and cli/ belong to that folder's group)
preview: yes (optional: marks the whole page "next preview")
---
```

The Markdown is a small, strict subset rendered by `mdlite.py` (standard library only; its docstring lists what it supports), plus `{preview}` for the "next preview" badge, `> [!NOTE]` / `[!TIP]` / `[!WARNING]` / `[!PREVIEW]` callouts, definition lists, and `<!-- cards -->` for a group's page list. `build.py` adds the sidebar (groups in `DOC_GROUPS`), breadcrumbs, "on this page" for pages with three or more sections, previous/next links, JSON-LD (TechArticle, BreadcrumbList), sitemap entries (English only, so hreflang lists `en` and `x-default`), and `public/docs/search.json`, the small index the docs' search box filters in the page (`/` focuses it). Code blocks get a copy button from `site.js`.

Keep the docs true to the code: things built but not in the current download are marked **next preview** (`{preview}`, or `preview: yes` for a whole page). At a release, remove those marks for what shipped.

`docs/cli/` is generated from the real binary; don't edit it by hand:

```sh
cargo build -p backdown-cli            # from the repository root
python3 tools/cli-reference.py         # --check exits 1 if docs/cli is out of date
python3 build.py
```

It runs `backdown --help` and `backdown <command> [<subcommand>] --help` in a clean environment (no local path or `$BACKDOWN_DB` can leak in; it refuses if one does), and marks commands and options the current download (the version in `downloads.json`, or `--released <tag>`) doesn't have. Examples, argument descriptions and notes that clap doesn't know live at the top of the script.

### Press kit (`/press/`)

`templates/press.html`, filled by `press_kit()` in `build.py`: boilerplates in 25, 50 and 100 words (the build checks the counts), a fact sheet from `downloads.json`, `status.json` and `site.json` (a press contact appears once `contact_email` is set), logos and screenshots. The build writes `public/press/logos/*.svg` from the brand masters (`public/brand/backdown-mark.svg`, `backdown-wordmark.svg`, `backdown-logo.svg`, `public/favicon.svg`, with dark variants by colour) and zips everything into `public/press/backdown-press-kit.zip` (fixed entry dates and order, so it only changes when its contents do). The transparent PNGs come from `cd screenshots && npm run press` after a build; build again afterwards so the zip includes them.

### Checking before a deploy

`python3 tools/check-links.py` checks every internal link, anchor, canonical/hreflang URL, sitemap entry and search-index entry in `public/`, that no page has an inline script or event handler the CSP would block, and that nothing mentions this machine's paths.

`python3 tools/i18n-add.py KEY --after OTHER_KEY en="…" es="…" …` adds or changes a string in every language at once, keeping the files' order and layout.

### Status page

Update `status.json` when something ships (`now`, `milestones`, `shipped`; keep it public-safe: no real paths, hosts or drive names), rebuild, deploy. The "Right now" box is live: the page asks `/api/status` once on load and every 10 minutes while visible; the Worker rechecks hourly.

### Analytics

Matomo at `matomo.matumi.anthill.hk`, cookieless, skipped entirely for Do Not Track / Global Privacy Control. It's off until `site.json` → `analytics.site_id` is set to backdown.io's id in Matomo (Administration → Websites → Manage → Add a website); the footer and privacy page switch their wording with it. The thanks page reports its URL without `?session_id=`.

### Pre-orders (Stripe)

Off by default. Each paid edition is sold as a pre-order of its first year, paid once with Stripe Checkout and fulfilled with an offline licence key whose `product` claim is that edition, so the app unlocks that tier:

| Edition | Stripe price lookup key | Sold per | Quantity |
|---|---|---|---|
| Pro | `backdown_pro_preorder` | machine | 1–50 |
| Team | `backdown_team_preorder` | machine | 5–500 |
| Forensic | `backdown_forensic_preorder` | site | 1–10 |

That table is `EDITIONS` in `src/store.js` (the setup script reads it too). Each edition's price is whatever active one-time Stripe price holds its lookup key; the amounts are set in Stripe, not here. To open them:

1. Test mode first: `STRIPE_SECRET_KEY=sk_test_… node ../scripts/stripe-setup.mjs --price pro=<cents> --price team=<cents> --price forensic=<cents>`, in US cents per machine or site, tax exclusive; give only the editions you want to sell. It checks the account and Stripe Tax, creates or reuses each edition's product and price and the webhook, and stores the Worker's secrets (`STRIPE_SECRET_KEY` or the restricted `--worker-key`, `STRIPE_WEBHOOK_SECRET`, `LICENSE_SIGNING_KEY` from `~/.config/backdown/license-signing-key.pem`) without printing them. Running it again with the same prices changes nothing; a new amount moves that edition's lookup key to a new price and archives the old one; editions left out are left alone. `--dry-run` shows what it would do.
2. In `site.json`, set `preorder.enabled` and every `seller` field (the terms page names who customers buy from); have the terms (`templates/terms.html`) reviewed, including what a Forensic "site" is and the one `preorder.ship_by` date that applies to every edition.
3. `python3 build.py && npx wrangler deploy`, buy each edition with card 4242 4242 4242 4242, check each key on the thanks page with `backdown license add` and `backdown license show`, then repeat step 1 with the live key (`sk_live_…`, and an `rk_live_…` restricted key as `--worker-key`).

Each pricing card shows its pre-order button, and the pre-order box offers that edition, only while `/api/preorder` reports an active Stripe price for it (`{open, amount, currency, editions: {pro, team, forensic}}`; the top-level fields are Pro's, as they were before), so archiving a price in Stripe closes that edition within the hour without a deploy. `POST /api/checkout` takes `edition` (Pro by default) and an optional `quantity` within the edition's limits; Checkout lets the buyer change the quantity within them too. Orders from before there were three editions have no edition in their metadata and are Pro. Licence keys are emailed only with an `EMAIL` binding (Cloudflare Email Service, see `wrangler.jsonc`); otherwise the thanks page shows the key and Stripe's receipt is the only email.

`node --test tools/store.test.mjs ../scripts/stripe-setup.test.mjs` tests the store and the setup script against a fake Stripe (`tools/fake-stripe.mjs`) with throwaway keys; nothing in them reaches Stripe or Cloudflare.

## Commands

Run from this directory unless noted:

| | |
|---|---|
| Rebuild pages + sitemap | `python3 build.py` |
| Preview locally | `npx wrangler dev` (put test secrets in `.dev.vars`, which git ignores) |
| Deploy the site | `python3 build.py && npx wrangler deploy` |
| Deploy the www redirect | `cd www-redirect && npx wrangler deploy` (only when it changes) |
| Read sign-ups | `npx wrangler kv key list --binding WAITLIST --remote` |
| Read orders | `npx wrangler kv key list --binding ORDERS --remote --prefix order:` |
| Re-shoot the slideshow | `cd screenshots && npm install && npm run capture` |
| Re-render OG cards + icons | `cd screenshots && npm run assets` (one card per page per language, plus the English-only `en-docs`/`en-press`; `node assets.mjs en-docs.png` renders just one) |
| Press kit logo PNGs | `python3 build.py && cd screenshots && npm run press && cd .. && python3 build.py` |
| Regenerate the CLI reference | `python3 tools/cli-reference.py` (after `cargo build -p backdown-cli`) |
| Check links before deploying | `python3 tools/check-links.py` |
| Test the store and the Stripe setup | `node --test tools/store.test.mjs ../scripts/stripe-setup.test.mjs` |

## The live demo (`tools/demo/`, `public/demo/`)

The guides embed BackDown's own web UI on an invented set of drives: `public/demo/` is the app's UI with its
`/api/` calls answered by the Worker (`src/demo.js`) from responses recorded against a real BackDown fed with
generated data. A guide embeds it with one raw block, `<figure class="demo" data-view="places" data-caption="…">
</figure>` (`data-dir` opens a folder, `data-mode` a layout, `data-tab` a tab inside the view). Regenerate it with
`tools/demo/run.sh` after a UI or API change; `tools/demo/README.md` has the pipeline and what's in the demo.
Deploy as usual: it's static files plus one Worker route (`/demo/api/*`, in `wrangler.jsonc`).

## Screenshots (`screenshots/`)

The slideshow shows the real dashboard and report UI (`../legacy/bd-dash.py`, `../assets/report.html`), served by `serve.py` with an invented demo dataset; nothing reads `index/` or any real drive. The report page hardcodes a few details of the founding dataset, which `serve.py` neutralises by pattern in its served copy. `capture.mjs` drives Firefox (via puppeteer-core) to shoot each slide in light and dark at 2x, per language; `encode.py` writes `public/shots/<lang>/<slide>-<theme>-{960,1600,2400}.webp`.

The app UI is English-only, so for other languages `capture.mjs` swaps the rendered text using `ui-i18n/<lang>.json` (exact phrases, plus regex patterns for generated lines) and reformats numbers for the locale. If the app's UI text changes, re-extract the visible strings and update every `ui-i18n` file.

`assets.mjs` renders `public/og/<lang>.png` (home) and `public/og/<lang>-<page>.png` from `og/og.html`, shrinking long headlines to fit, plus the icons, using the site's own CSS and fonts.

## Fonts

`bricolage-normal` / `instrument-*` are copies of `../assets/fonts/` (basic Latin only). The `*-latin-ext` subsets (for cs/sk/pl/es/pt-br) come from the same families on Google Fonts; neither family has Cyrillic, so `inter-tight-cyrillic` / `inter-cyrillic` stand in for Ukrainian. `site.css` maps them all by `unicode-range`, so each page downloads only what its text needs. The tokens at the top of `site.css` mirror `../assets/shell.css`.

Keep the feature status tags (`Works today` / `In development` / `Planned`) and `status.json` honest as the roadmap moves, in every language.
