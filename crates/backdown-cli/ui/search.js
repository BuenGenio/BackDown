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

// where to search and whether to include what's gone: remembered for the browser session
export const F = { scope: load('bd.findScope') === 'all' ? 'all' : 'here', earlier: load('bd.findEarlier') === '1', limit: STEP, seq: 0 };

function sync() {
  document.querySelectorAll('#searchbox [data-scope]').forEach(b => b.setAttribute('aria-pressed', String(b.dataset.scope === F.scope)));
  const box = $('#find-earlier');
  if (box) box.checked = F.earlier;
  const label = F.scope === 'all' ? 'Search every drive' : 'Search this folder';
  $('#q').placeholder = label;
  $('#q').setAttribute('aria-label', label);
}

export function setScope(scope) {
  F.scope = scope === 'all' ? 'all' : 'here';
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
      <button type="button" data-scope="all" title="Every indexed drive (Ctrl+Shift+F)">Everywhere</button></div>
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
  const res = await api('find', { q, dir: F.scope === 'here' ? dir : '', earlier: F.earlier ? 1 : '', limit: F.limit });
  return seq === F.seq ? res : null;
}

export function more() {
  F.limit = Math.min(MAX, F.limit + STEP);
}

export function foundHtml(res, q) {
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
