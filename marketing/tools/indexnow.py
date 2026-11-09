#!/usr/bin/env python3
"""Tell search engines what changed: submit the sitemap's URLs through IndexNow (Bing, Yandex, Seznam, Naver and
others share one endpoint; Google doesn't take IndexNow and finds the sitemap on its own).

    python3 tools/indexnow.py            # every URL in public/sitemap.xml
    python3 tools/indexnow.py /docs/ /uk/docs/   # just these paths

Run it after `npx wrangler deploy`. The key is site.json search.indexnow_key, served as public/<key>.txt.
"""
import json
import re
import sys
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SITE = 'https://backdown.io'


def main():
    key = json.loads((ROOT / 'site.json').read_text(encoding='utf-8'))['search']['indexnow_key']
    if not key or not (ROOT / 'public' / f'{key}.txt').exists():
        sys.exit('site.json search.indexnow_key is empty, or public/<key>.txt is missing')
    if sys.argv[1:]:
        urls = [SITE + p if p.startswith('/') else p for p in sys.argv[1:]]
    else:
        urls = re.findall(r'<loc>([^<]+)</loc>', (ROOT / 'public/sitemap.xml').read_text(encoding='utf-8'))
    body = json.dumps({'host': 'backdown.io', 'key': key, 'keyLocation': f'{SITE}/{key}.txt', 'urlList': urls}).encode()
    req = urllib.request.Request('https://api.indexnow.org/indexnow', data=body, headers={'Content-Type': 'application/json; charset=utf-8'})
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            print(f'IndexNow: {r.status} for {len(urls)} URLs')
    except urllib.error.HTTPError as e:
        sys.exit(f'IndexNow: HTTP {e.code} {e.read().decode(errors="replace")[:200]}')


if __name__ == '__main__':
    main()
