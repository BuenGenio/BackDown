// A local stand-in for the Cloudflare Worker: marketing/public as static files, /demo/api/* through src/demo.js.
//   node preview.mjs [port]
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { demo } from '../../src/demo.js';
const [, , PORT = '8791'] = process.argv;
const PUB = path.resolve(path.dirname(new URL(import.meta.url).pathname), '../../public');
const TYPES = { '.html': 'text/html; charset=utf-8', '.css': 'text/css; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.mjs': 'text/javascript; charset=utf-8',
  '.json': 'application/json', '.svg': 'image/svg+xml', '.png': 'image/png', '.webp': 'image/webp', '.jpg': 'image/jpeg', '.xml': 'application/xml', '.txt': 'text/plain; charset=utf-8', '.zip': 'application/zip', '.md': 'text/markdown; charset=utf-8' };
function fileResponse(pathname) {
  let p = path.join(PUB, decodeURIComponent(pathname));
  if (fs.existsSync(p) && fs.statSync(p).isDirectory()) p = path.join(p, 'index.html');
  if (!fs.existsSync(p)) return new Response('not found', { status: 404 });
  return new Response(fs.readFileSync(p), { headers: { 'Content-Type': TYPES[path.extname(p)] || 'application/octet-stream' } });
}
const env = { ASSETS: { fetch: async req => fileResponse(new URL(req.url).pathname) } }; // a Promise, as the real binding returns
http.createServer(async (req, res) => {
  const url = new URL(req.url, `http://127.0.0.1:${PORT}`);
  let r;
  try { r = url.pathname.startsWith('/demo/api/') ? await demo(new Request(url, { method: req.method }), env, url) : fileResponse(url.pathname); }
  catch (e) { r = new Response('error: ' + e.message, { status: 500 }); }
  res.writeHead(r.status, Object.fromEntries(r.headers));
  res.end(Buffer.from(await r.arrayBuffer()));
}).listen(Number(PORT), '127.0.0.1', () => console.log(`preview on http://127.0.0.1:${PORT}/`));
