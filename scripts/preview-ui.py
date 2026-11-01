#!/usr/bin/env python3
"""Live UI preview over a local BackDown server. Run: python3 scripts/preview-ui.py.
Serves crates/backdown-cli/ui from this checkout and reloads the page when a file there changes,
so UI edits show without rebuilding the binary. API requests go to a running BackDown (the
installed one's token, from `backdown token --links`), GETs only: the preview can't change anything.
Use --upstream-log with the output of a development `backdown serve` process to preview against it.
Open /#duplicates to land straight in the Duplicates view.
"""
import argparse
import hashlib
import http.client
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import mimetypes
from pathlib import Path
import re
import subprocess
from urllib.parse import urlsplit, parse_qs, unquote

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--port', type=int, default=8790)
parser.add_argument('--upstream-log', type=Path)
args = parser.parse_args()
ui = Path(__file__).resolve().parents[1] / 'crates/backdown-cli/ui'
if args.upstream_log:
    link = re.search(r'http://127\.0\.0\.1:\d+/\?t=[a-f0-9]+', args.upstream_log.read_text()).group()
else:
    link = subprocess.check_output(['backdown', 'token', '--links'], text=True).splitlines()[0]
up = urlsplit(link)
token = parse_qs(up.query)['t'][0]
reload_js = b"""let version; setInterval(async()=>{try {const v=await (await fetch('/__preview_version')).text(); if(version && v!==version) location.reload(); version=v;}catch{}},1000); if(location.hash==='#duplicates'){ const t=setInterval(()=>{const b=document.querySelector('#dupes-btn'); if(b && document.querySelector('#places .place')){clearInterval(t);b.click();}},300); setTimeout(()=>clearInterval(t),30000); }"""
class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_): pass
    def send(self, status, body, mime):
        self.send_response(status)
        self.send_header('Content-Type', mime)
        self.send_header('Cache-Control', 'no-store')
        self.send_header('X-Content-Type-Options', 'nosniff')
        self.end_headers()
        try: self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError): pass
    def do_GET(self):
        if self.headers.get('Host') not in {f'127.0.0.1:{args.port}', f'localhost:{args.port}'}:
            return self.send(403, b'Local preview only', 'text/plain')
        path = urlsplit(self.path).path
        if path == '/__preview_version':
            stamp = '|'.join(f'{p}:{p.stat().st_mtime_ns}' for p in sorted(ui.rglob('*')) if p.is_file())
            return self.send(200, hashlib.sha256(stamp.encode()).hexdigest().encode(), 'text/plain')
        if path == '/__preview.js': return self.send(200, reload_js, 'text/javascript')
        if path.startswith('/api/'):
            conn = http.client.HTTPConnection(up.hostname, up.port, timeout=180)
            try:
                conn.request('GET', self.path, headers={'Cookie': f'bd={token}'})
                res = conn.getresponse()
                self.send(res.status, res.read(), res.getheader('Content-Type', 'application/json'))
            except Exception as e: self.send(502, str(e).encode(), 'text/plain')
            finally: conn.close()
            return
        asset = (ui / unquote(path.lstrip('/') or 'index.html')).resolve()
        if not asset.is_relative_to(ui) or not asset.is_file(): return self.send(404, b'Not found', 'text/plain')
        body = asset.read_bytes()
        if asset.name == 'index.html': body = body.replace(b'</body>', b'<script src="/__preview.js" type="module"></script></body>')
        self.send(200, body, mimetypes.guess_type(asset)[0] or 'application/octet-stream')
    def do_POST(self): self.send(403, b'{"error":"Live preview is read-only. Use the installed app to apply changes."}', 'application/json')
print(f'Live preview: http://127.0.0.1:{args.port}/', flush=True)
ThreadingHTTPServer(('127.0.0.1', args.port), Handler).serve_forever()
