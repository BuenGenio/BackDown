#!/usr/bin/env python3
"""Build the localized landing pages: page.html + i18n/<lang>.json -> public/index.html (en) and public/<lang>/index.html,
plus public/sitemap.xml.

Placeholders in page.html:
  {{key}}       the string from i18n/<lang>.json, inserted as HTML (strings may carry <em>, <strong>, <code>)
  {{key|attr}}  the same string escaped for an attribute value
  {{@name}}     computed here (see `computed` below): language menu, slides, download cards, SEO tags

The build fails if any language is missing a key, has an extra one, or the template uses a key
English doesn't define, so a string added in English can't silently ship untranslated.
Standard library only; run from anywhere: python3 marketing/build.py
"""
import datetime
import html
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
SITE = 'https://backdown.io'
LANGS = ['en', 'de', 'pl', 'cs', 'sk', 'uk']  # language-menu order; English is served at /
FLAGS = {'en': 'gb', 'de': 'de', 'pl': 'pl', 'cs': 'cz', 'sk': 'sk', 'uk': 'ua'}  # public/flags/<code>.svg
TOKEN = re.compile(r'\{\{(@?[a-z0-9_.]+)(\|attr)?\}\}')

# slideshow, in display order: screenshot id (public/shots/<lang>/<id>-<theme>-<width>.webp, made by screenshots/) and the URL
# shown in its frame; captions are slides.<id>_title / _body / _alt
SLIDES = [('overview', 'localhost:8787/report'), ('verdicts', 'localhost:8787/report'), ('tree', 'localhost:8787/report'),
          ('duplicates', 'localhost:8787/report'), ('verification', 'localhost:8787/report'), ('dashboard', 'localhost:8787')]
SHOT_WIDTHS = (960, 1600, 2400)
SHOT_SIZES = '(min-width: 1340px) 1224px, calc(100vw - 32px)'
# click/tap halves over each screenshot with an arrow at the edge; mouse and touch only (aria-hidden, out of the tab
# order), since the arrow buttons under the slideshow and the arrow keys cover keyboard and screen-reader users
ZONES = ('<button class="zone prev" type="button" data-step="-1" tabindex="-1" aria-hidden="true"><span class="zarrow">'
         '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"><path d="m15 18-6-6 6-6"/></svg></span></button>'
         '<button class="zone next" type="button" data-step="1" tabindex="-1" aria-hidden="true"><span class="zarrow">'
         '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"><path d="m9 18 6-6-6-6"/></svg></span></button>')

# the early-access form appears twice (hero and closing CTA); site.js reads its messages from the data attributes
FORM = '''<form class="join" action="/api/waitlist" method="post" data-sending="{{form.sending|attr}}" data-ok="{{form.ok|attr}}" data-invalid="{{form.invalid|attr}}" data-error="{{form.error|attr}}" data-offline="{{form.offline|attr}}">
     <label class="hp" aria-hidden="true">Website <input name="website" tabindex="-1" autocomplete="off"></label>
     <input type="hidden" name="lang" value="{{@lang}}">
     <input type="email" name="email" required autocomplete="email" placeholder="{{form.placeholder|attr}}" aria-label="{{form.email_label|attr}}">
     <button class="btn btn-primary" type="submit">{{form.submit}}</button>
    </form>
    <p class="join-msg" role="status" aria-live="polite"></p>'''


def page_path(lang):
    return '/' if lang == 'en' else f'/{lang}/'


def esc(s):
    # attributes are always double-quoted, so apostrophes can stay readable
    return html.escape(s, quote=False).replace('"', '&quot;')


def flag(lang):
    return f'<img class="flag" src="/flags/{FLAGS[lang]}.svg" width="20" height="14" alt="">'


def slides(lang, t):
    out = []
    for i, (sid, url) in enumerate(SLIDES, 1):
        srcset = lambda theme: ', '.join(f'/shots/{lang}/{sid}-{theme}-{w}.webp {w}w' for w in SHOT_WIDTHS)
        load = 'fetchpriority="high"' if i == 1 else 'loading="lazy" decoding="async"'
        out.append(f'''    <figure class="slide" role="group" aria-roledescription="slide" aria-label="{i} / {len(SLIDES)}">
     <div class="frame card"><div class="frame-bar" aria-hidden="true"><span class="dots"><i></i><i></i><i></i></span><span class="mono">{url}</span></div>
      <picture><source media="(prefers-color-scheme: dark)" type="image/webp" srcset="{srcset('dark')}" sizes="{SHOT_SIZES}">
      <img src="/shots/{lang}/{sid}-light-1600.webp" srcset="{srcset('light')}" sizes="{SHOT_SIZES}" width="1600" height="1000" alt="{esc(t[f'slides.{sid}_alt'])}" {load}></picture>
      {ZONES}</div>
     <figcaption><b>{t[f'slides.{sid}_title']}</b> {t[f'slides.{sid}_body']}</figcaption>
    </figure>''')
    return '\n'.join(out)


def downloads(t, platforms):
    status = {'early': ('dev', 'download.status_early'), 'soon': ('plan', 'download.status_soon')}
    out = []
    for p in platforms:
        if p['status'] == 'available' and p['files']:
            tag = f'<span class="tag live">{esc(p["version"])}</span>'
            act = ''.join(f'<a class="btn btn-primary" href="{esc(f["url"])}" download>{t["download.cta_download"]} {esc(f["label"])}</a>'
                          f'<details class="sum"><summary>{t["download.checksum"]}</summary><code>{esc(f["sha256"])}</code></details>'
                          for f in p['files'])
        else:
            cls, key = status[p['status']]
            tag = f'<span class="tag {cls}">{t[key]}</span>'
            cta, style = ('download.cta_early', 'btn-primary') if p['status'] == 'early' else ('download.cta_notify', 'btn-ghost')
            act = f'<a class="btn {style}" href="#join">{t[cta]}</a>'
        out.append(f'''    <article class="dl card">
     <div class="dl-head"><span class="os-ic"><svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="{p['icon']}"/></svg></span>{tag}</div>
     <h3>{p['name']}</h3><p class="fmt mono">{p['formats']}</p><p>{t[f"download.{p['id']}_body"]}</p>
     <div class="dl-act">{act}</div>
    </article>''')
    return '\n'.join(out)


def jsonld(lang, t, platforms):
    url = SITE + page_path(lang)
    available = [p for p in platforms if p['status'] == 'available' and p['files']]
    data = {'@context': 'https://schema.org', '@graph': [
        {'@type': 'Organization', '@id': f'{SITE}/#org', 'name': 'BackDown', 'url': f'{SITE}/', 'logo': f'{SITE}/icon-512.png'},
        {'@type': 'WebSite', '@id': f'{SITE}/#website', 'url': f'{SITE}/', 'name': 'BackDown', 'publisher': {'@id': f'{SITE}/#org'},
         'inLanguage': LANGS},
        {'@type': 'WebPage', '@id': f'{url}#webpage', 'url': url, 'name': t['meta.title'], 'description': t['meta.description'],
         'inLanguage': lang, 'isPartOf': {'@id': f'{SITE}/#website'}, 'about': {'@id': f'{SITE}/#app'},
         'primaryImageOfPage': f'{SITE}/og/{lang}.png'},
        {'@type': 'SoftwareApplication', '@id': f'{SITE}/#app', 'name': 'BackDown', 'url': url,
         'description': t['meta.description'], 'applicationCategory': 'UtilitiesApplication',
         'operatingSystem': ', '.join(p['name'] for p in available) or 'Linux',
         'screenshot': [f'{SITE}/shots/{lang}/{sid}-light-1600.webp' for sid, _ in SLIDES],
         'offers': {'@type': 'Offer', 'price': '0', 'priceCurrency': 'USD',
                    'availability': 'https://schema.org/' + ('InStock' if available else 'PreOrder')},
         'publisher': {'@id': f'{SITE}/#org'}},
    ]}
    if available:  # a free preview is downloadable
        app = data['@graph'][-1]
        app['softwareVersion'] = available[0]['version'].lstrip('v')
        app['downloadUrl'] = [f['url'] for p in available for f in p['files']]
    # "</" can't appear inside a <script> block
    return json.dumps(data, ensure_ascii=False, separators=(',', ':')).replace('</', '<\\/')


def sitemap():
    today = datetime.date.today().isoformat()
    alts = ''.join(f'\n    <xhtml:link rel="alternate" hreflang="{l}" href="{SITE}{page_path(l)}"/>' for l in LANGS)
    alts += f'\n    <xhtml:link rel="alternate" hreflang="x-default" href="{SITE}/"/>'
    urls = ''.join(f'\n  <url>\n    <loc>{SITE}{page_path(l)}</loc>\n    <lastmod>{today}</lastmod>{alts}\n  </url>' for l in LANGS)
    return ('<?xml version="1.0" encoding="UTF-8"?>\n'
            '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9" xmlns:xhtml="http://www.w3.org/1999/xhtml">'
            f'{urls}\n</urlset>\n')


def main():
    template = (ROOT / 'page.html').read_text(encoding='utf-8').replace('{{@form}}', FORM)
    tables = {lang: json.loads((ROOT / 'i18n' / f'{lang}.json').read_text(encoding='utf-8')) for lang in LANGS}
    dl = json.loads((ROOT / 'downloads.json').read_text(encoding='utf-8'))
    platforms = dl['platforms']

    # keys the template uses directly, plus the ones the generators above read
    keys = set(tables['en'])
    used = {m[1] for m in TOKEN.finditer(template) if not m[1].startswith('@')}
    used |= {f'slides.{sid}_{f}' for sid, _ in SLIDES for f in ('title', 'body', 'alt')}
    used |= {'lang.name', 'meta.title', 'meta.description', 'slides.goto', 'download.status_early', 'download.status_soon',
             'download.cta_early', 'download.cta_notify', 'download.cta_download', 'download.checksum'}
    used |= {f"download.{p['id']}_body" for p in platforms}
    errors = [f'template uses undefined key {k}' for k in sorted(used - keys)]
    errors += [f'en.json key {k} is never used' for k in sorted(keys - used)]
    for lang, table in tables.items():
        errors += [f'{lang}.json is missing {k}' for k in sorted(keys - set(table))]
        errors += [f'{lang}.json has unknown key {k}' for k in sorted(set(table) - keys)]
    if errors:
        sys.exit('\n'.join(errors))

    alternates = '\n'.join(
        [f'<link rel="alternate" hreflang="{lang}" href="{SITE}{page_path(lang)}">' for lang in LANGS]
        + [f'<link rel="alternate" hreflang="x-default" href="{SITE}/">'])

    for lang, t in tables.items():
        menu = ''.join(
            '<li><a href="{}" hreflang="{}" lang="{}"{}>{}{}</a></li>'.format(
                page_path(other), other, other, ' aria-current="page"' if other == lang else '',
                flag(other), html.escape(tables[other]['lang.name']))
            for other in LANGS)
        computed = {
            '@lang': lang, '@code': lang.upper(), '@url': SITE + page_path(lang), '@home': page_path(lang),
            '@alternates': alternates, '@langmenu': menu, '@flag': flag(lang),
            '@og_image': f'{SITE}/og/{lang}.png',
            '@og_alternates': '\n'.join(f'<meta property="og:locale:alternate" content="{tables[o]["meta.og_locale"]}">'
                                        for o in LANGS if o != lang),
            '@jsonld': jsonld(lang, t, platforms),
            '@releases_url': dl['releases_url'],
            '@slides': slides(lang, t),
            '@dots': ''.join(f'<button type="button" data-i="{i}" aria-label="{esc(t["slides.goto"])} {i + 1}"></button>'
                             for i in range(len(SLIDES))),
            '@downloads': downloads(t, platforms),
        }

        def fill(m):
            key, attr = m[1], m[2]
            value = computed[key] if key.startswith('@') else t[key]
            return esc(value) if attr else value

        out = TOKEN.sub(fill, template)
        out = out.replace('<!doctype html>', f'<!doctype html>\n<!-- Generated by build.py from page.html + i18n/{lang}.json. Edit those, not this file. -->', 1)
        dest = ROOT / 'public' / page_path(lang).strip('/') / 'index.html'
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_text(out, encoding='utf-8')
        print(f'{lang}: {dest.relative_to(ROOT)}')

    (ROOT / 'public' / 'sitemap.xml').write_text(sitemap(), encoding='utf-8')
    print('sitemap: public/sitemap.xml')


if __name__ == '__main__':
    main()
