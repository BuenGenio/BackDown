// Search from the header's box: this folder, or every indexed drive at once. /api/find (find.rs)
// ranks names (exact, starts with, a word starts with, contains; newer first) and can include
// files that were there at earlier scans; this draws the scope switch, the results and "show more".
import { api, bytes, when } from './app.js';
import { fileIcon, folderIcon } from './icons.js';

const $ = s => document.querySelector(s);
// names are shown composed (NFC), whichever form the drive stores them in (HFS+ decomposes them)
const esc = s => String(s ?? '').normalize('NFC').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const num = n => Number(n || 0).toLocaleString();
const STEP = 200, MAX = 2000;
const load = k => { try { return sessionStorage.getItem(k); } catch { return null; } };
const store = (k, v) => { try { sessionStorage.setItem(k, v); } catch {} };

// where to search and whether to include what's gone: remembered for the browser session.
// scope: 'here' (this folder's names), 'all' (every drive's names), 'contents' (inside mail and
// databases: /api/contents, the content index that content.rs fills)
const SCOPES = ['here', 'all', 'contents'];
export const F = { scope: SCOPES.includes(load('bd.findScope')) ? load('bd.findScope') : 'here', earlier: load('bd.findEarlier') === '1', limit: STEP, seq: 0 };

function sync() {
  document.querySelectorAll('#searchbox [data-scope]').forEach(b => b.setAttribute('aria-pressed', String(b.dataset.scope === F.scope)));
  const box = $('#find-earlier');
  if (box) box.checked = F.earlier;
  const label = F.scope === 'all' ? 'Search every drive' : F.scope === 'contents' ? 'Search inside mail and databases' : 'Search this folder';
  $('#q').placeholder = label;
  $('#q').setAttribute('aria-label', label);
  const earlier = document.querySelector('#searchbox .find-earlier');
  if (earlier) earlier.hidden = F.scope === 'contents';
}

export function setScope(scope) {
  F.scope = SCOPES.includes(scope) ? scope : 'here';
  F.limit = STEP;
  store('bd.findScope', F.scope);
  sync();
}

export function setEarlier(on) {
  F.earlier = !!on;
  F.limit = STEP;
  store('bd.findEarlier', F.earlier ? '1' : '0');
  sync();
}

// the scope switch and the "earlier versions" box, added to the search box once; `rerun()` searches again
export function mountFind(rerun) {
  $('#searchbox').insertAdjacentHTML('beforeend', `<div class="find-scope" role="group" aria-label="Where to search">
      <button type="button" data-scope="here" title="This folder and the folders in it">This folder</button>
      <button type="button" data-scope="all" title="Every indexed drive (Ctrl+Shift+F)">Everywhere</button>
      <button type="button" data-scope="contents" title="Inside things: mail by subject, sender and recipient; database dumps and SQLite files by table and text">Inside</button></div>
    <label class="find-earlier" title="Also files that were there at earlier scans but aren’t now"><input type="checkbox" id="find-earlier"><span>Earlier versions</span></label>`);
  $('#searchbox').addEventListener('click', e => {
    const b = e.target.closest('[data-scope]');
    if (b && b.dataset.scope !== F.scope) { setScope(b.dataset.scope); rerun(); }
  });
  $('#find-earlier').addEventListener('change', e => { setEarlier(e.target.checked); rerun(); });
  // from the box, ↓ goes to the results; in them, ↑ and ↓ move and ↑ from the first goes back
  document.addEventListener('keydown', e => {
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
    const opens = [...document.querySelectorAll('#view .hit-open')];
    if (!opens.length) return;
    if (e.target.id === 'q' && e.key === 'ArrowDown') { e.preventDefault(); opens[0].focus(); return; }
    const i = opens.indexOf(e.target);
    if (i < 0) return;
    e.preventDefault();
    if (e.key === 'ArrowUp' && i === 0) $('#q').focus();
    else opens[Math.max(0, Math.min(opens.length - 1, i + (e.key === 'ArrowDown' ? 1 : -1)))].focus();
  });
  sync();
}

// the results for `q` (null when a newer search has started since)
export async function find(q, dir) {
  const seq = ++F.seq;
  if (F.scope === 'contents') {
    const res = await api('contents', { q, limit: F.limit });
    if (seq !== F.seq) return null;
    res.contents = true;
    if (!res.total) { try { res.status = await api('contents-status'); } catch { /* shown without it */ } }
    return seq === F.seq ? res : null;
  }
  const res = await api('find', { q, dir: F.scope === 'here' ? dir : '', earlier: F.earlier ? 1 : '', limit: F.limit });
  return seq === F.seq ? res : null;
}

// a hit inside a thing: a message, a table, a chunk of rows, a SQLite full-text table
const KIND_WORD = { message: 'Message', table: 'Table', rows: 'Rows', 'sqlite-fts': 'Full-text table', 'sqlite-rows': 'Rows' };
const SOURCE_WORD = { 'dovecot-cache': 'from Dovecot’s index', 'message-file': 'from the message', 'mdbox-storage': 'from the store', mbox: 'from the mbox', dump: 'from the dump', sqlite: 'from the file', 'sqlite-fts': 'SQLite’s own index', mongodump: 'from the dump', 'mysql-data': 'from the data directory' };

function contentsHtml(res, q) {
  const hits = res.hits, left = res.total - hits.length;
  const secs = res.ms >= 100 ? ` · ${(res.ms / 1000).toFixed(res.ms < 1000 ? 2 : 1)} s` : '';
  const roots = new Map((res.roots || []).map(r => [r.path, r.name]));
  if (!hits.length) {
    const st = res.status, none = st && !st.docs;
    return `<div class="empty"><div><h2>${none ? 'Nothing indexed yet' : 'No matches'}</h2>
      <p>${none ? `Contents are indexed after each scan when the depth isn’t off (now: ${esc(st.depth)}). ` : `Nothing inside the indexed mail and databases matches “${esc(q)}”. `}${st && st.docs ? `${num(st.docs)} documents are indexed, depth ${esc(st.depth)}.` : ''}</p>
      <p class="find-alt"><button class="btn sm" type="button" data-find-build>${none ? 'Index contents now' : 'Index again'}</button><button class="btn sm" type="button" data-find-scope="all">Search names instead</button></p></div></div>`;
  }
  const bar = `<div class="find-bar" role="status"><span><b>${num(res.total)}</b> ${res.total === 1 ? 'match' : 'matches'} inside mail and databases${left > 0 ? `, the best ${num(hits.length)} shown` : ''}<span class="dim">${secs}</span></span>
    ${left > 0 && F.limit < MAX ? `<button class="btn sm" type="button" data-find-more>Show ${num(Math.min(STEP, left))} more</button>` : ''}</div>`;
  const rows = hits.map(h => {
    const rel = h.path.startsWith(h.root) ? h.path.slice(h.root.length).replace(/^[\\/]/, '') : h.path;
    const where = h.kind === 'message' ? rel.replace(/[\\/](cur|new)[\\/][^\\/]*$/, '') : rel;
    return `<tr data-hit-dir="${h.dir_key}" data-hit-sel="${h.key}">
      <td><div class="nmc"><button class="hit-open" type="button" title="${esc(h.path)}">${esc(h.title)}</button><span class="bdg">${esc(KIND_WORD[h.kind] || h.kind)}</span></div></td>
      <td class="path" title="${esc(SOURCE_WORD[h.source] || h.source)}">${esc(h.who)}${h.locator && h.kind !== 'message' ? ` <span class="dim">${esc(h.locator)}</span>` : ''}</td>
      <td class="drive">${esc(roots.get(h.root) || h.root)}</td>
      <td class="path" title="${esc(h.path)}">${esc(where) || '<span class="dim">top</span>'}</td>
      <td>${h.at ? when(h.at) : ''}</td></tr>`;
  }).join('');
  return `${bar}<table class="list find"><thead><tr><th>Found</th><th>Who / where in it</th><th>Drive</th><th>Folder</th><th>When</th></tr></thead><tbody>${rows}</tbody></table>`;
}

export function more() {
  F.limit = Math.min(MAX, F.limit + STEP);
}

export function foundHtml(res, q) {
  if (res.contents) return contentsHtml(res, q);
  const all = F.scope === 'all', hits = res.hits, left = res.total - hits.length;
  const where = all ? 'on every drive' : 'in this folder';
  const secs = res.ms >= 100 ? ` · ${(res.ms / 1000).toFixed(res.ms < 1000 ? 2 : 1)} s` : '';
  const switchTo = all ? '' : '<button class="btn sm" type="button" data-find-scope="all">Search every drive</button>';
  if (!hits.length) {
    return `<div class="empty"><div><h2>No matches</h2><p>Nothing named like “${esc(q)}” ${where}${F.earlier ? ', now or at earlier scans' : ''}.</p>
      <p class="find-alt">${switchTo}${F.earlier ? '' : '<button class="btn sm" type="button" data-find-earlier>Include earlier versions</button>'}</p></div></div>`;
  }
  const bar = `<div class="find-bar" role="status"><span><b>${num(res.total)}</b> ${res.total === 1 ? 'match' : 'matches'} ${where}${res.gone ? ` (${num(res.gone)} no longer there)` : ''}${left > 0 ? `, the best ${num(hits.length)} shown` : ''}<span class="dim">${secs}</span></span>
    ${left > 0 && F.limit < MAX ? `<button class="btn sm" type="button" data-find-more>Show ${num(Math.min(STEP, left))} more</button>` : left > 0 ? '<span class="dim">Narrow the search to see the rest.</span>' : ''}${switchTo}</div>`;
  const rows = hits.map(h => {
    const dir = h.kind === 'dir';
    const target = dir ? h.key : h.dir_key;
    const at = h.gone && h.scan ? ` data-hit-scan="${h.scan}"` : '';
    const folder = h.folder || (all ? '' : '·');
    return `<tr class="${h.gone ? 'gone' : ''}" data-hit-dir="${target}" data-hit-sel="${dir ? '' : h.key}"${at}>
      <td><div class="nmc">${dir ? folderIcon(20) : fileIcon(h.name, 20)}<button class="hit-open" type="button" title="${esc(h.path)}">${esc(h.name)}</button>${h.gone ? '<span class="bdg deleted">no longer there</span>' : ''}</div></td>
      ${all ? `<td class="drive">${esc(h.drive)}</td>` : ''}
      <td class="path" title="${esc(h.path)}">${esc(folder) || '<span class="dim">top</span>'}</td>
      <td class="n">${bytes(h.size)}</td><td>${h.gone ? `<span class="dim">last seen</span> ${when(h.seen)}` : when(h.mtime)}</td></tr>`;
  }).join('');
  return `${bar}<table class="list find"><thead><tr><th>Name</th>${all ? '<th>Drive</th>' : ''}<th>Folder</th><th class="n">Size</th><th>Modified</th></tr></thead><tbody>${rows}</tbody></table>`;
}
