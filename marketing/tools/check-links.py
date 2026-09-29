#!/usr/bin/env python3
"""Check the built site (public/) for broken internal links, before deploying.

    python3 marketing/tools/check-links.py

Every href, src, srcset and poster in every page, the canonical and hreflang URLs (https://backdown.io/... is
looked up in public/), the sitemap and the docs' search index must point at a file that exists, and a #fragment
at an id on that page. Also: no inline script the Content-Security-Policy would block (only JSON-LD and
speculation rules may be inline), and nothing that looks like a local path. Exits 1 on any problem.
Standard library only.
"""
import getpass
import html.parser
import json
import re
import sys
from pathlib import Path
from urllib.parse import unquote, urlsplit

PUBLIC = Path(__file__).resolve().parent.parent / 'public'
SITE = 'https://backdown.io'
INLINE_OK = {'application/ld+json', 'speculationrules'}
# this machine: the checkout, the home folder and the user name (the docs' examples use /home/you and the like)
LOCAL = re.compile('|'.join(re.escape(s) for s in {str(PUBLIC.parent.parent), str(Path.home()), f'/{getpass.getuser()}/'}
                            if len(s) > 3))


class Page(html.parser.HTMLParser):
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.ids, self.refs, self.problems = set(), [], []
        self.script = None

    def handle_starttag(self, tag, attrs):
        a = dict(attrs)
        if 'id' in a:
            self.ids.add(a['id'])
        if tag == 'a' and 'name' in a:
            self.ids.add(a['name'])
        for k in ('href', 'src', 'poster'):
            if a.get(k):
                self.refs.append((tag, a[k]))
        if a.get('srcset'):
            self.refs += [(tag, part.strip().split()[0]) for part in a['srcset'].split(',') if part.strip()]
        if tag == 'meta' and a.get('property') in ('og:url', 'og:image') or tag == 'meta' and a.get('name') == 'twitter:image':
            self.refs.append((tag, a.get('content', '')))
        if tag == 'script':
            self.script = a
            if 'src' not in a and a.get('type') not in INLINE_OK:
                self.problems.append(f'inline <script type="{a.get("type", "")}"> (the CSP blocks it)')
        if any(k.startswith('on') for k in a):
            self.problems.append(f'<{tag}> has an inline event handler (the CSP blocks it)')
        for k, v in a.items():
            if k in ('href', 'src') and v and v.strip().lower().startswith('javascript:'):
                self.problems.append(f'<{tag} {k}="javascript:…"> (the CSP blocks it)')


def target(url):
    """the file in public/ a URL points at, or None when it isn't ours"""
    if url.startswith(SITE + '/') or url == SITE:
        url = url[len(SITE):] or '/'
    parts = urlsplit(url)
    if parts.scheme or parts.netloc or not parts.path.startswith('/'):
        return None
    path = unquote(parts.path)
    f = PUBLIC / path.lstrip('/')
    if path.endswith('/'):
        f = f / 'index.html'
    elif not f.exists() and (f / 'index.html').exists():
        f = f / 'index.html'
    return f


def main():
    pages = {f: Page() for f in PUBLIC.rglob('*.html')}
    for f, p in pages.items():
        text = f.read_text(encoding='utf-8')
        p.feed(text)
        if LOCAL.search(text):
            p.problems.append(f'looks like a local path: {LOCAL.search(text)[0]!r}')
    problems = []
    checked = 0

    def check(src, url, where):
        nonlocal checked
        if not url or url.startswith(('mailto:', 'data:', 'tel:')):
            return
        base = src.relative_to(PUBLIC).as_posix()
        if url.startswith('#'):
            dest, frag = src, url[1:]
        else:
            if not url.startswith(('/', 'http:', 'https:')):
                url = '/' + (src.parent.relative_to(PUBLIC).as_posix() + '/' if src.parent != PUBLIC else '') + url
            dest = target(url)
            if dest is None:
                return
            frag = urlsplit(url).fragment
        checked += 1
        if not dest.exists():
            problems.append(f'{base}: {where} {url} -> nothing at {dest.relative_to(PUBLIC)}')
            return
        if frag and dest.suffix == '.html':
            if dest not in pages:
                return
            if unquote(frag) not in pages[dest].ids:
                problems.append(f'{base}: {where} {url} -> no id "{frag}" in {dest.relative_to(PUBLIC)}')

    for f, p in pages.items():
        problems += [f'{f.relative_to(PUBLIC)}: {x}' for x in p.problems]
        for tag, url in p.refs:
            check(f, url, f'<{tag}>')
    sitemap = (PUBLIC / 'sitemap.xml').read_text(encoding='utf-8')
    for url in re.findall(r'<loc>([^<]+)</loc>|href="([^"]+)"', sitemap):
        check(PUBLIC / 'sitemap.xml', next(u for u in url if u), 'sitemap')
    index = PUBLIC / 'docs' / 'search.json'
    if index.exists():
        for url, *_ in json.loads(index.read_text(encoding='utf-8')):
            check(index, url, 'search index')
    for p in problems:
        print(p)
    print(f'{len(pages)} pages, {checked} internal links checked, {len(problems)} problems')
    sys.exit(1 if problems else 0)


if __name__ == '__main__':
    main()
