#!/usr/bin/env python3
"""Build the localized site: templates/*.html + i18n/<lang>.json (+ site.json, downloads.json, status.json)
-> public/<page>/index.html (English) and public/<lang>/<page>/index.html, plus public/sitemap.xml.

templates/base.html is the shell (head, header, footer); each page's templates/<page>.html fills its {{@main}}.
Placeholders:
  {{key}}       the string from i18n/<lang>.json, inserted as HTML (strings may carry <em>, <strong>, <code>)
  {{key|attr}}  the same string escaped for an attribute value
  {{@name}}     computed here (see `computed` in render()): navigation, slides, download cards, SEO tags

The build fails if any language is missing a key, has an extra one, or a template uses a key English doesn't
define, so a string added in English can't silently ship untranslated. (status.json is the exception: its entries
fall back to English, marked lang="en", so the roadmap can be updated before it's translated.)
Standard library only; run from anywhere: python3 marketing/build.py
"""
import datetime
import hashlib
import html
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
T = ROOT / 'templates'
SITE = 'https://backdown.io'
BRAND_MARK = (ROOT / 'public/brand/backdown-mark.svg').read_text(encoding='utf-8').strip()
BRAND_VERSION = hashlib.sha256((BRAND_MARK + (ROOT / 'public/favicon.svg').read_text(encoding='utf-8')).encode()).hexdigest()[:10]
BRAND_INLINE = BRAND_MARK.replace('fill="#d8432b"', 'fill="currentColor"').replace('<svg ', '<svg aria-hidden="true" ', 1)
# page codes (also the URL folder and i18n/<code>.json), in language-menu order; English is served at /
LANGS = ['en', 'es', 'pt-br', 'de', 'pl', 'cs', 'sk', 'uk']
FLAGS = {'en': 'gb', 'es': 'es', 'pt-br': 'br', 'de': 'de', 'pl': 'pl', 'cs': 'cz', 'sk': 'sk', 'uk': 'ua'}  # public/flags/<code>.svg
TAGS = {'pt-br': 'pt-BR'}  # BCP 47 tag where it differs from the page code (hreflang, <html lang>)


def tag(lang):
    return TAGS.get(lang, lang)


TOKEN = re.compile(r'\{\{(@?[a-z0-9_.]+)(\|attr)?\}\}')

# The pages, in footer order. nav: in the header tabs. join: ends with the early-access form. og: has its own social
# card (public/og/<lang>-<id>.png, from screenshots/assets.mjs; the home page's is public/og/<lang>.png).
# only: built only when site.json enables it. langs: English-only pages.
PAGES = [
    dict(id='home', slug='', nav=False, join=True, og=True, priority='1.0', changefreq='weekly'),
    dict(id='features', slug='features', nav=True, join=True, og=True, priority='0.8', changefreq='monthly'),
    dict(id='pricing', slug='pricing', nav=True, join=True, og=True, priority='0.8', changefreq='monthly'),
    dict(id='download', slug='download', nav=True, join=True, og=True, priority='0.9', changefreq='weekly'),
    dict(id='status', slug='status', nav=True, join=True, og=True, priority='0.7', changefreq='daily'),
    dict(id='privacy', slug='privacy', nav=False, join=False, og=False, priority='0.3', changefreq='yearly'),
    dict(id='terms', slug='terms', nav=False, join=False, og=False, priority='0.3', changefreq='yearly', only='preorder', langs=['en']),
    dict(id='thanks', slug='thanks', nav=False, join=False, og=False, only='preorder', noindex=True),
]
OG_PAGES = [p['id'] for p in PAGES if p['og']]  # read by screenshots/assets.mjs via --list

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

# the early-access form appears in the hero and in the closing card; site.js reads its messages from the data attributes
FORM = '''<form class="join" action="/api/waitlist" method="post" data-sending="{{form.sending|attr}}" data-ok="{{form.ok|attr}}" data-invalid="{{form.invalid|attr}}" data-error="{{form.error|attr}}" data-offline="{{form.offline|attr}}">
     <label class="hp" aria-hidden="true">Website <input name="website" tabindex="-1" autocomplete="off"></label>
     <input type="hidden" name="lang" value="{{@page}}">
     <input type="email" name="email" required autocomplete="email" placeholder="{{form.placeholder|attr}}" aria-label="{{form.email_label|attr}}">
     <button class="btn btn-primary" type="submit">{{form.submit}}</button>
    </form>
    <p class="join-msg" role="status" aria-live="polite"></p>'''

JOIN = ''' <section id="join">
  <div class="wrap">
   <div class="cta card">
    <div><h2>{{cta.title}}</h2><p>{{cta.body}}</p></div>
    <div>
     {{@form}}
     <p class="nojs-msg ok" id="joined">{{form.ok}}</p>
     <p class="nojs-msg err" id="join-error">{{form.nojs_error}}</p>
     <p class="join-note">{{form.note}}</p>
    </div>
   </div>
  </div>
 </section>'''

FAQ = ['q1', 'q2', 'q3', 'q4', 'q5']
FAQ_PREORDER = ['pq1', 'pq2']
STATES = ['up', 'degraded', 'down', 'open', 'closed', 'unknown']


def url_path(lang, page):
    """/, /features/, /de/, /de/features/"""
    parts = [p for p in ('' if lang == 'en' else lang, page['slug']) if p]
    return '/' + ''.join(f'{p}/' for p in parts)


def esc(s):
    # attributes are always double-quoted, so apostrophes can stay readable
    return html.escape(s, quote=False).replace('"', '&quot;')


def text(s):
    """plain text from a string that may carry inline HTML (for meta tags and JSON-LD)"""
    return html.unescape(re.sub(r'<[^>]+>', '', s))


def flag(lang):
    return f'<img class="flag" src="/flags/{FLAGS[lang]}.svg" width="20" height="14" alt="">'


def human_date(t, iso):
    y, m, d = (int(x) for x in iso.split('-'))
    return t['status.date'].replace('{d}', str(d)).replace('{m}', t['status.months'].split(',')[m - 1]).replace('{y}', str(y))


def slides(lang, t):
    out = []
    for i, (sid, url) in enumerate(SLIDES, 1):
        srcset = lambda theme: ', '.join(f'/shots/{lang}/{sid}-{theme}-{w}.webp?v={BRAND_VERSION} {w}w' for w in SHOT_WIDTHS)
        load = 'fetchpriority="high"' if i == 1 else 'loading="lazy" decoding="async"'
        out.append(f'''    <figure class="slide" role="group" aria-roledescription="slide" aria-label="{i} / {len(SLIDES)}">
     <div class="frame card"><div class="frame-bar" aria-hidden="true"><span class="dots"><i></i><i></i><i></i></span><span class="mono">{url}</span></div>
      <picture><source media="(prefers-color-scheme: dark)" type="image/webp" srcset="{srcset('dark')}" sizes="{SHOT_SIZES}">
      <img src="/shots/{lang}/{sid}-light-1600.webp?v={BRAND_VERSION}" srcset="{srcset('light')}" sizes="{SHOT_SIZES}" width="1600" height="1000" alt="{esc(t[f'slides.{sid}_alt'])}" {load}></picture>
      {ZONES}</div>
     <figcaption><b>{t[f'slides.{sid}_title']}</b> {t[f'slides.{sid}_body']}</figcaption>
    </figure>''')
    return '\n'.join(out)


def downloads(t, platforms):
    status = {'early': ('dev', 'download.status_early'), 'soon': ('plan', 'download.status_soon')}
    out = []
    for p in platforms:
        if p['status'] == 'available' and p['files']:
            badge = f'<span class="tag live">{esc(p["version"])}</span>'
            act = ''.join(f'<a class="btn btn-primary" href="{esc(f["url"])}" download>{t["download.cta_download"]} {esc(f["label"])}</a>'
                          f'<details class="sum"><summary>{t["download.checksum"]}</summary><code>{esc(f["sha256"])}</code></details>'
                          for f in p['files'])
        else:
            cls, key = status[p['status']]
            badge = f'<span class="tag {cls}">{t[key]}</span>'
            cta, style = ('download.cta_early', 'btn-primary') if p['status'] == 'early' else ('download.cta_notify', 'btn-ghost')
            act = f'<a class="btn {style}" href="#join">{t[cta]}</a>'
        out.append(f'''    <article class="dl card">
     <div class="dl-head"><span class="os-ic"><svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="{p['icon']}"/></svg></span>{badge}</div>
     <h2 class="h3">{p['name']}</h2><p class="fmt mono">{p['formats']}</p><p>{t[f"download.{p['id']}_body"]}</p>
     <div class="dl-act">{act}</div>
    </article>''')
    return '\n'.join(out)


def faq_items(t, cfg):
    keys = FAQ + (FAQ_PREORDER if cfg['preorder']['enabled'] else [])
    return [(t[f'faq.{k}'], t[f'faq.{k.replace("q", "a", 1) if k.startswith("q") else "pa" + k[2:]}']) for k in keys]


def faq(t, cfg):
    return '\n'.join(f'    <details class="qa card"><summary><h3>{q}</h3></summary><p>{a}</p></details>' for q, a in faq_items(t, cfg))


def localized(field, lang):
    """a status.json text field: (html, fell back to English?)"""
    return (field[lang], False) if field.get(lang) else (field['en'], lang != 'en')


def status_now(lang, t, status):
    icons = {'done': '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m5 12.5 4.5 4.5L19 7.5"/></svg>',
             'doing': '<span class="spin" aria-hidden="true"></span>', 'next': '<span class="ring" aria-hidden="true"></span>'}
    out = []
    for item in status['now']:
        body, en = localized(item['text'], lang)
        out.append(f'    <li class="w-{item["state"]}"><span class="wi">{icons[item["state"]]}</span>'
                   f'<span class="wt"{" lang=en" if en else ""}>{body}</span><span class="tag {dict(done="live", doing="dev", next="plan")[item["state"]]}">{t["status.w_" + item["state"]]}</span></li>')
    return '\n'.join(out)


def status_milestones(lang, t, status):
    out = []
    for m in status['milestones']:
        title, en1 = localized(m['title'], lang)
        body, en2 = localized(m['body'], lang)
        year, q = m['when']
        when = t['status.quarter'].replace('{q}', str(q)).replace('{y}', str(year))
        cls = dict(done='live', now='dev', next='plan', later='plan')[m['state']]
        out.append(f'''    <li class="ms ms-{m["state"]}"><span class="ms-dot" aria-hidden="true"></span><div class="ms-body card">
     <div class="ms-top"><span class="mono">{m["version"]}</span><span class="tag {cls}">{t["status.m_" + m["state"]]}</span><span class="when">{when}</span></div>
     <h3{" lang=en" if en1 else ""}>{title}</h3><p{" lang=en" if en2 else ""}>{body}</p></div></li>''')
    return '\n'.join(out)


def status_shipped(lang, t, status):
    out = []
    for s in status['shipped']:
        body, en = localized(s['text'], lang)
        out.append(f'    <li><time datetime="{s["date"]}">{human_date(t, s["date"])}</time><span class="mono">{esc(s["version"])}</span>'
                   f'<p{" lang=en" if en else ""}>{body}</p></li>')
    return '\n'.join(out)


def jsonld(lang, page, t, meta, ctx):
    url = SITE + url_path(lang, page)
    home = SITE + url_path(lang, PAGES[0])
    graph = [
        {'@type': 'Organization', '@id': f'{SITE}/#org', 'name': 'BackDown', 'url': f'{SITE}/', 'logo': f'{SITE}/icon-512.png?v={BRAND_VERSION}'},
        {'@type': 'WebSite', '@id': f'{SITE}/#website', 'url': f'{SITE}/', 'name': 'BackDown', 'publisher': {'@id': f'{SITE}/#org'},
         'inLanguage': [tag(l) for l in LANGS]},
    ]
    webpage = {'@type': 'WebPage', '@id': f'{url}#webpage', 'url': url, 'name': meta['title'], 'description': meta['description'],
               'inLanguage': tag(lang), 'isPartOf': {'@id': f'{SITE}/#website'}, 'about': {'@id': f'{SITE}/#app'},
               'primaryImageOfPage': meta['og_image']}
    graph.append(webpage)
    if page['id'] != 'home':
        webpage['breadcrumb'] = {'@id': f'{url}#breadcrumb'}
        graph.append({'@type': 'BreadcrumbList', '@id': f'{url}#breadcrumb', 'itemListElement': [
            {'@type': 'ListItem', 'position': 1, 'name': 'BackDown', 'item': home},
            {'@type': 'ListItem', 'position': 2, 'name': meta['crumb'], 'item': url}]})
    if page['id'] == 'status':
        webpage['dateModified'] = ctx['status']['updated']
    if page['id'] == 'pricing':
        webpage['@type'] = ['WebPage', 'FAQPage']
        webpage['mainEntity'] = [{'@type': 'Question', 'name': text(q), 'acceptedAnswer': {'@type': 'Answer', 'text': text(a)}}
                                 for q, a in faq_items(t, ctx['cfg'])]
    if page['id'] in ('home', 'download', 'features'):
        available = [p for p in ctx['platforms'] if p['status'] == 'available' and p['files']]
        app = {'@type': 'SoftwareApplication', '@id': f'{SITE}/#app', 'name': 'BackDown', 'url': home,
               'description': text(t['meta.description']), 'applicationCategory': 'UtilitiesApplication',
               'operatingSystem': ', '.join(p['name'] for p in available) or 'Linux',
               'screenshot': [f'{SITE}/shots/{lang}/{sid}-light-1600.webp' for sid, _ in SLIDES],
               'offers': {'@type': 'Offer', 'price': '0', 'priceCurrency': 'USD',
                          'availability': 'https://schema.org/' + ('InStock' if available else 'PreOrder')},
               'publisher': {'@id': f'{SITE}/#org'}}
        if available:  # a free preview is downloadable
            app['softwareVersion'] = available[0]['version'].lstrip('v')
            app['downloadUrl'] = [f['url'] for p in available for f in p['files']]
        graph.append(app)
    # "</" can't appear inside a <script> block
    return json.dumps({'@context': 'https://schema.org', '@graph': graph}, ensure_ascii=False, separators=(',', ':')).replace('</', '<\\/')


def page_meta(lang, page, t):
    pid = page['id']
    og_image = f'{SITE}/og/{lang}.png' if pid == 'home' or not page['og'] else f'{SITE}/og/{lang}-{pid}.png'
    og_image += f'?v={BRAND_VERSION}'
    if pid == 'home':
        return dict(title=t['meta.title'], description=t['meta.description'], og_title=t['meta.og_title'],
                    og_description=t['meta.og_description'], og_alt=t['meta.og_image_alt'], og_image=og_image, crumb='BackDown')
    if pid == 'terms':
        title, desc = 'Pre-order terms · BackDown', 'What you get when you pre-order BackDown Pro, how refunds work, and who you are buying from.'
        return dict(title=title, description=desc, og_title=title, og_description=desc, og_alt=t['meta.og_image_alt'],
                    og_image=og_image, crumb='Pre-order terms')
    title, desc = t[f'meta.{pid}_title'], t[f'meta.{pid}_description']
    return dict(title=title, description=desc, og_title=title, og_description=desc, og_image=og_image,
                og_alt=text(t[f'{pid}.title']) if page['og'] else t['meta.og_image_alt'],
                crumb={'privacy': t['footer.privacy_link'], 'thanks': t['thanks.eyebrow']}.get(pid) or t[f'nav.{pid}'])


def load(name):
    return json.loads((ROOT / name).read_text(encoding='utf-8'))


def check_config(cfg):
    if cfg['preorder']['enabled']:
        missing = [k for k in ('name', 'address', 'email', 'law') if not cfg['seller'].get(k)]
        if missing:
            sys.exit(f'site.json: pre-orders are enabled but seller.{", seller.".join(missing)} is empty '
                     '(the terms page names who customers are buying from)')


def asset_version():
    h = hashlib.sha256()
    for f in ('site.css', 'site.js'):
        h.update((ROOT / 'public' / f).read_bytes())
    return h.hexdigest()[:10]


def pages_for(cfg):
    return [p for p in PAGES if not p.get('only') or cfg[p['only']]['enabled']]


def langs_of(page):
    return page.get('langs', LANGS)


def main():
    if sys.argv[1:] == ['--og-pages']:  # for screenshots/assets.mjs
        print(json.dumps({p['id']: p['id'] != 'home' and f'{p["id"]}.title' for p in PAGES if p['og']}))
        return
    cfg = load('site.json')
    check_config(cfg)
    tables = {lang: load(f'i18n/{lang}.json') for lang in LANGS}
    dl = load('downloads.json')
    status = load('status.json')
    platforms = dl['platforms']
    pages = pages_for(cfg)
    templates = {p['id']: (T / f'{p["id"]}.html').read_text(encoding='utf-8') for p in PAGES}
    base = (T / 'base.html').read_text(encoding='utf-8')
    preorder = (T / '_preorder.html').read_text(encoding='utf-8')
    ctx = dict(cfg=cfg, status=status, platforms=platforms)

    # every key any page could use (whether or not site.json switches that page on), so translations stay complete
    keys = set(tables['en'])
    everything = base + JOIN + FORM + preorder + ''.join(templates.values())
    used = {m[1] for m in TOKEN.finditer(everything) if not m[1].startswith('@')}
    used |= {f'slides.{sid}_{f}' for sid, _ in SLIDES for f in ('title', 'body', 'alt')}
    used |= {'lang.name', 'meta.title', 'meta.description', 'meta.og_title', 'meta.og_description', 'meta.og_image_alt',
             'slides.goto', 'download.status_early', 'download.status_soon', 'download.cta_early', 'download.cta_notify',
             'download.cta_download', 'download.checksum', 'footer.privacy', 'footer.privacy_analytics', 'footer.privacy_link',
             'footer.terms_link', 'footer.releases', 'policy.analytics_on', 'policy.analytics_off', 'policy.operator',
             'policy.orders_title', 'policy.orders_body', 'policy.contact', 'status.date', 'status.months', 'status.quarter',
             'preorder.cta'}
    used |= {f'nav.{p["id"]}' for p in PAGES if p['nav']}
    used |= {f'meta.{p["id"]}_{f}' for p in PAGES if p['id'] not in ('home', 'terms') for f in ('title', 'description')}
    used |= {f'status.w_{s}' for s in ('done', 'doing', 'next')} | {f'status.m_{s}' for s in ('done', 'now', 'next', 'later')}
    used |= {f'status.st_{s}' for s in STATES} | {f'faq.{k}' for k in FAQ + FAQ_PREORDER}
    used |= {f'faq.a{k[1:]}' for k in FAQ} | {f'faq.pa{k[2:]}' for k in FAQ_PREORDER}
    used |= {f'thanks.{s}' for s in ('pending', 'paid', 'paid_noemail', 'refunded', 'unknown', 'error')}
    used |= {f"download.{p['id']}_body" for p in platforms}
    errors = [f'template uses undefined key {k}' for k in sorted(used - keys)]
    errors += [f'en.json key {k} is never used' for k in sorted(keys - used)]
    for lang, table in tables.items():
        errors += [f'{lang}.json is missing {k}' for k in sorted(keys - set(table))]
        errors += [f'{lang}.json has unknown key {k}' for k in sorted(set(table) - keys)]
    if errors:
        sys.exit('\n'.join(errors))
    untranslated = sorted({l for item in status['now'] + status['milestones'] + status['shipped']
                           for field in ('text', 'title', 'body') if field in item for l in LANGS if not item[field].get(l)})
    if untranslated:
        print(f'note: status.json falls back to English for {", ".join(untranslated)}')

    version = asset_version()
    analytics = cfg['analytics']
    tracking = bool(analytics.get('site_id'))
    shipped_latest = next((p for p in platforms if p['status'] == 'available'), None)
    latest_version = shipped_latest['version'] if shipped_latest else status['shipped'][0]['version']
    latest_date = next((s['date'] for s in status['shipped'] if s['version'] == latest_version), status['shipped'][0]['date'])
    seller = cfg['seller']
    today = datetime.date.today().isoformat()

    for page in pages:
        pid = page['id']
        body = templates[pid].replace('{{@join}}', JOIN if page['join'] else '')
        body = body.replace('{{@preorder}}', preorder if cfg['preorder']['enabled'] else '')
        template = base.replace('{{@main}}', body.rstrip('\n')).replace('{{@form}}', FORM)
        variants = [l for l in langs_of(page)]
        alternates = '\n'.join(
            [f'<link rel="alternate" hreflang="{tag(l)}" href="{SITE}{url_path(l, page)}">' for l in variants]
            + [f'<link rel="alternate" hreflang="x-default" href="{SITE}{url_path("en", page)}">'])

        for lang in variants:
            t = tables[lang]
            meta = page_meta(lang, page, t)
            here = url_path(lang, page)
            href = {p['id']: url_path(lang, p) for p in PAGES}
            href['terms'] = url_path('en', next(p for p in PAGES if p['id'] == 'terms'))
            nav = ''.join(f'<a href="{href[p["id"]]}" data-p="{p["id"]}"{" aria-current=page" if p["id"] == pid else ""}>{t["nav." + p["id"]]}</a>'
                          for p in PAGES if p['nav'])
            foot = [(href[p['id']], t['nav.' + p['id']]) for p in PAGES if p['nav']]
            foot.append((href['privacy'], t['footer.privacy_link']))
            if cfg['preorder']['enabled']:
                foot.append((href['terms'], t['footer.terms_link']))
            foot.append((dl['releases_url'], t['footer.releases']))
            menu = ''.join(
                '<li><a href="{}" hreflang="{}" lang="{}"{}>{}{}</a></li>'.format(
                    url_path(other, page if other in variants else PAGES[0]), tag(other), tag(other),
                    ' aria-current="page"' if other == lang else '', flag(other), html.escape(tables[other]['lang.name']))
                for other in LANGS)
            email_link = lambda e: f'<a href="mailto:{esc(e)}">{esc(e)}</a>'
            computed = {
                '@lang': tag(lang), '@page': lang, '@page_id': pid, '@code': lang.split('-')[0].upper(),
                '@url': SITE + here, '@home': href['home'], '@asset_v': version,
                '@brand_v': BRAND_VERSION, '@brand_mark': BRAND_INLINE,
                '@title': esc(meta['title']), '@description': esc(text(meta['description'])),
                '@og_title': esc(text(meta['og_title'])), '@og_description': esc(text(meta['og_description'])),
                '@og_image': meta['og_image'], '@og_image_alt': esc(text(meta['og_alt'])),
                '@robots': 'noindex, follow' if page.get('noindex') else 'index, follow, max-image-preview:large, max-snippet:-1',
                '@alternates': alternates, '@langmenu': menu, '@flag': flag(lang), '@nav': nav,
                '@og_alternates': '\n'.join(f'<meta property="og:locale:alternate" content="{tables[o]["meta.og_locale"]}">'
                                            for o in variants if o != lang),
                '@footer_nav': ''.join(f'<a href="{esc(h)}">{label}</a>' for h, label in foot),
                '@footer_privacy': t['footer.privacy_analytics' if tracking else 'footer.privacy'],
                '@cta_href': '#join' if page['join'] else href['home'] + '#join',
                '@analytics': (f'<meta name="analytics" content="matomo" data-url="{esc(analytics["matomo_url"])}" data-site="{int(analytics["site_id"])}">\n'
                               f'<link rel="dns-prefetch" href="{esc(analytics["matomo_url"])}">') if tracking else '',
                '@jsonld': jsonld(lang, page, t, meta, ctx),
                '@releases_url': dl['releases_url'],
                **{f'@href.{k}': v for k, v in href.items()},
            }
            if pid == 'home':
                computed['@slides'] = slides(lang, t)
                computed['@dots'] = ''.join(f'<button type="button" data-i="{i}" aria-label="{esc(t["slides.goto"])} {i + 1}"></button>'
                                            for i in range(len(SLIDES)))
            if pid == 'download':
                computed['@downloads'] = downloads(t, platforms)
            if pid == 'pricing':
                computed['@faq'] = faq(t, cfg)
                computed['@preorder_link'] = (f'<a class="btn btn-primary po-link" href="#preorder" hidden>{t["preorder.cta"]}</a>'
                                              if cfg['preorder']['enabled'] else '')
            if pid == 'status':
                computed.update({
                    '@status_now': status_now(lang, t, status), '@status_milestones': status_milestones(lang, t, status),
                    '@status_shipped': status_shipped(lang, t, status),
                    '@status_updated': status['updated'], '@status_updated_h': human_date(t, status['updated']),
                    '@status_states': esc(json.dumps({s: t[f'status.st_{s}'] for s in STATES}, ensure_ascii=False)),
                    '@latest_version': esc(latest_version), '@latest_date': latest_date, '@latest_date_h': human_date(t, latest_date)})
            if pid == 'privacy':
                computed.update({
                    '@operator': f'<p>{t["policy.operator"].replace("{name}", esc(seller["name"])).replace("{address}", esc(seller["address"]))}</p>'
                                 if seller.get('name') else '',
                    '@analytics_policy': t['policy.analytics_on' if tracking else 'policy.analytics_off'],
                    '@orders_policy': f'<h2>{t["policy.orders_title"]}</h2>\n   <p>{t["policy.orders_body"]}</p>' if cfg['preorder']['enabled'] else '',
                    '@contact': f'<p>{t["policy.contact"].replace("{email}", email_link(cfg["contact_email"]))}</p>' if cfg.get('contact_email') else '',
                    '@policy_date': today, '@policy_date_h': human_date(t, today)})
            if pid == 'terms':
                computed.update({'@terms_lang_note': '', '@seller_name': esc(seller['name']), '@seller_address': esc(seller['address']),
                                 '@seller_email': esc(seller['email']), '@seller_law': esc(seller['law']),
                                 '@ship_by': human_date(t, cfg['preorder']['ship_by']), '@terms_date': today, '@terms_date_h': human_date(t, today)})
            if pid == 'thanks':
                computed['@thanks_states'] = esc(json.dumps({s: t[f'thanks.{s}'] for s in ('pending', 'paid', 'paid_noemail', 'refunded', 'unknown', 'error')},
                                                            ensure_ascii=False))

            def fill(m):
                key, attr = m[1], m[2]
                if key.startswith('@'):
                    if key not in computed:
                        sys.exit(f'{pid}: nothing computes {{{{{key}}}}}')
                    value = computed[key]
                else:
                    value = t[key]
                return esc(value) if attr else value

            out = TOKEN.sub(fill, template)
            out = out.replace('<!doctype html>', f'<!doctype html>\n<!-- Generated by build.py from templates/{pid}.html + i18n/{lang}.json. Edit those, not this file. -->', 1)
            dest = ROOT / 'public' / here.strip('/') / 'index.html'
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_text(out, encoding='utf-8')
        print(f'{pid}: {len(variants)} languages')

    # pages a site.json switch turned off since the last build
    for page in PAGES:
        if page not in pages:
            for lang in LANGS:
                stale = ROOT / 'public' / url_path(lang, page).strip('/') / 'index.html'
                if stale.exists():
                    stale.unlink()
                    stale.parent.rmdir()
                    print(f'removed {stale.relative_to(ROOT)}')

    notfound = ROOT / 'public' / '404.html'
    notfound_html = re.sub(r'/site\.css(\?v=[0-9a-f]+)?', f'/site.css?v={version}', notfound.read_text(encoding='utf-8'))
    notfound_html = re.sub(r'/favicon\.svg(\?v=[0-9a-f]+)?', f'/favicon.svg?v={BRAND_VERSION}', notfound_html)
    notfound_html = re.sub(r'<span class="mark">.*?</span>', f'<span class="mark">{BRAND_INLINE}</span>', notfound_html)
    notfound.write_text(notfound_html, encoding='utf-8')
    manifest_path = ROOT / 'public' / 'site.webmanifest'
    manifest = json.loads(manifest_path.read_text(encoding='utf-8'))
    for icon in manifest['icons']:
        icon['src'] = icon['src'].split('?')[0] + f'?v={BRAND_VERSION}'
    manifest_path.write_text(json.dumps(manifest, indent=2) + '\n', encoding='utf-8')
    (ROOT / 'public' / 'sitemap.xml').write_text(sitemap(pages, status), encoding='utf-8')
    print('sitemap: public/sitemap.xml')


def sitemap(pages, status):
    today = datetime.date.today().isoformat()
    urls = []
    for page in pages:
        if page.get('noindex'):
            continue
        variants = langs_of(page)
        alts = ''.join(f'\n    <xhtml:link rel="alternate" hreflang="{tag(l)}" href="{SITE}{url_path(l, page)}"/>' for l in variants)
        alts += f'\n    <xhtml:link rel="alternate" hreflang="x-default" href="{SITE}{url_path("en", page)}"/>'
        lastmod = status['updated'] if page['id'] == 'status' else today
        urls += [f'\n  <url>\n    <loc>{SITE}{url_path(l, page)}</loc>\n    <lastmod>{lastmod}</lastmod>'
                 f'\n    <changefreq>{page["changefreq"]}</changefreq>\n    <priority>{page["priority"]}</priority>{alts}\n  </url>'
                 for l in variants]
    return ('<?xml version="1.0" encoding="UTF-8"?>\n'
            '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9" xmlns:xhtml="http://www.w3.org/1999/xhtml">'
            f'{"".join(urls)}\n</urlset>\n')


if __name__ == '__main__':
    main()
