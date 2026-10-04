// Duplicates: what's stored more than once, in groups that open to show where each copy is.
//
// The server sends the groups that free the most (report.rs dupes_listed), already narrowed by the
// filter bar: a group comes when one of its copies passes, and each copy says whether it does
// (`listed`). They're shown two ways:
//  - by content: a row, card or size-map tile for each set of identical files, opening to its copies;
//  - by folder: where those copies are, cut where a group's copies part ways, so folders that are
//    copies of one another (a project cloned thirty times, a backup of a backup) stand out.
// Ticked copies go to Free up (app.js showWipe), which proves each still has a copy that stays before
// it deletes anything; here one path of every group always stays unticked.
import { api, bytes, when } from './app.js';
import { fileIcon, folderIcon, kindOf, extsOf } from './icons.js';
import { openViewer } from './viewers.js';
import { menu, squarify, toast } from './extras.js';
import { dupesRemoteHtml, acrossHtml } from './machines.js';

const $ = s => document.querySelector(s);
const esc = s => String(s ?? '').normalize('NFC').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const num = n => Number(n || 0).toLocaleString();
const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' });
const sepOf = p => p.includes('\\') && !p.includes('/') ? '\\' : '/';
const leafOf = p => p.replace(/[\\/]+$/, '').split(/[\\/]/).pop();
const folderOf = p => p.slice(0, Math.max(1, p.replace(/[\\/]+$/, '').search(/[\\/][^\\/]*$/))) || p;
const plural = (n, one, many = one + 's') => `${num(n)} ${n === 1 ? one : many}`;
const SVG = {
  chev: '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="m9 6 6 6-6 6"/></svg>',
  show: '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3 7.5A1.5 1.5 0 0 1 4.5 6H9l2 2h8.5A1.5 1.5 0 0 1 21 9.5v8A1.5 1.5 0 0 1 19.5 19h-15A1.5 1.5 0 0 1 3 17.5z"/><path d="m11 13.5 2 2 3.5-4"/></svg>',
  copy: '<svg viewBox="0 0 24 24" aria-hidden="true"><rect x="8" y="8" width="12" height="12" rx="2"/><path d="M16 8V5.5A1.5 1.5 0 0 0 14.5 4h-9A1.5 1.5 0 0 0 4 5.5v9A1.5 1.5 0 0 0 5.5 16H8"/></svg>',
  check: '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="m5 12.5 4.5 4.5L19 7"/></svg>',
};

let H = {}; // app.js: its state, and the views this one hands off to (mountDupes)
const DP = {
  min: '1M', limit: 200, d: null, sel: new Map(), seq: 0, sort: { key: 'reclaimable', asc: false },
  by: localStorage.getItem('bd.dupes.by') === 'folder' ? 'folder' : 'content',
  open: new Set(), // groups and folders opened in the list
  focus: null,     // the one opened in the grid or the size map
  q: '',           // "Find": names and folders among what's listed
  m: null,         // the model of `d` (model())
};
const MIN = [['1', 'any size'], ['100K', '100 KB and up'], ['1M', '1 MB and up'], ['100M', '100 MB and up']];
const SORT = [['reclaimable', 'Space to free'], ['size', 'Size'], ['copies', 'Copies'], ['name', 'Name'], ['mtime', 'Newest copy']];

export function mountDupes(hooks) {
  H = hooks;
  const v = $('#view');
  v.addEventListener('change', onChange);
  v.addEventListener('input', onInput);
  v.addEventListener('click', onClick);
  v.addEventListener('dblclick', onDblClick);
  v.addEventListener('keydown', onKey);
  v.addEventListener('contextmenu', onMenu);
}

// ---------- asking the server

// the filter bar as the server takes it: a type's extensions (icons.js), sizes, dates, hidden
function query() {
  const f = H.S.filter, q = { min: DP.min, limit: DP.limit };
  if (f.type === 'other') { q.ext = extsOf().join(','); q.except = 1; }
  else if (f.type !== 'all') q.ext = extsOf(H.typeKinds[f.type] || [f.type]).join(',');
  if (f.size !== 'any') { const [, , lo, hi] = H.sizes.find(x => x[0] === f.size); q.from = lo; if (hi < Infinity) q.below = hi; }
  if (f.date !== 'any') {
    const now = Math.floor(Date.now() / 1000);
    if (f.date === 'old') q.before = now - 365 * 86400; else q.after = now - +f.date * 86400;
  }
  if (!H.S.hidden) q.nohidden = 1;
  return q;
}

export async function showDupes() {
  const fresh = H.S.mode !== 'dupes';
  H.enter('dupes');
  H.syncFilterUI();
  // folders are never duplicates here: only files are compared
  if (H.S.filter.type === 'dir') { DP.d = null; DP.seq++; return renderDupes(); }
  if (fresh || !DP.d) $('#view').innerHTML = '<div class="empty"><p>Loading duplicates…</p></div>';
  else $('#view').classList.add('stale');
  const seq = ++DP.seq;
  let d;
  try { d = await api('dupes', query()); }
  catch (e) { if (seq === DP.seq) $('#view').classList.remove('stale'); return H.toastError(e); }
  if (seq !== DP.seq) return; // a newer request
  $('#view').classList.remove('stale');
  if (H.S.mode !== 'dupes') return;
  DP.d = d; DP.m = null;
  // few enough to read at once: open them
  const m = model();
  if (m.groups.length <= 3) for (const g of m.groups) DP.open.add(g.sha256);
  renderDupes();
}

// ---------- the model: groups, the folders their copies are in, and every path by key

// how many leading and trailing names some folders all share: the rest is where they part ways
function sharedParts(folders) {
  const ps = folders.map(f => f.split(/[\\/]/)), min = Math.min(...ps.map(p => p.length));
  let pre = 0, post = 0;
  while (pre < min && ps.every(p => p[pre] === ps[0][pre])) pre++;
  while (post < min - pre && ps.every(p => p.at(-1 - post) === ps[0].at(-1 - post))) post++;
  return [pre, post];
}
const midOf = (folder, [pre, post]) => { const p = folder.split(/[\\/]/); return p.slice(pre, p.length - post).join(sepOf(folder)); };
// the folder a copy is in, cut where its group's copies part ways: …/workspace-ask for
// …/workspace-ask/data/marinas.json when the others are in …/workspace-chart/data
function placeOf(g, f) {
  const folder = folderOf(f.path), parts = folder.split(/[\\/]/);
  return { path: parts.slice(0, parts.length - g.shared[1]).join(sepOf(folder)) || sepOf(folder), depth: parts.length - g.shared[1] };
}

function model() {
  if (DP.m?.d === DP.d) return DP.m;
  const groups = [], byKey = new Map();
  for (const g0 of DP.d?.largest || []) {
    const shown = g0.files.filter(f => f.listed);
    if (!shown.length) continue;
    const g = { ...g0, shown, shared: sharedParts(g0.files.map(f => folderOf(f.path))), names: [...new Set(shown.map(f => leafOf(f.path)))] };
    for (const f of g.files) byKey.set(f.key, { g, f });
    groups.push(g);
  }
  // the folders, from the listed copies
  const places = new Map();
  for (const g of groups) for (const f of g.shown) {
    const { path, depth } = placeOf(g, f);
    let p = places.get(path);
    if (!p) places.set(path, p = { path, key: ancestorKey(f.dir, depth), copies: [], groups: new Set() });
    p.copies.push({ g, f }); p.groups.add(g);
  }
  for (const p of places.values()) {
    // what wiping the folder would free: the distinct copies (an inode, whatever its names) that are
    // only here, all of them when the group has a copy elsewhere, else all but one
    p.free = 0;
    const others = new Set();
    for (const g of p.groups) {
      const away = g.files.filter(f => placeOf(g, f).path !== p.path);
      const kept = new Set(away.map(f => f.copy));
      const onlyHere = new Set(g.files.filter(f => !kept.has(f.copy)).map(f => f.copy)).size;
      p.free += g.size * (kept.size ? onlyHere : Math.max(0, onlyHere - 1));
      for (const f of away) others.add(placeOf(g, f).path);
    }
    p.others = others.size;
  }
  nameFolders([...places.values()]);
  DP.m = { d: DP.d, groups, places: [...places.values()], byKey };
  return DP.m;
}

// each folder by the fewest last names that tell it from the others: deps, or workspace-legacy/…/deps
// when every project has a deps
function nameFolders(places) {
  const parts = places.map(p => p.path.split(/[\\/]/).filter(Boolean));
  places.forEach((p, i) => {
    const mine = parts[i];
    let k = 1;
    while (k < mine.length && parts.some((o, j) => j !== i && o.slice(-k).join('/') === mine.slice(-k).join('/'))) k++;
    p.name = (k > 2 ? [mine.at(-k), '…', mine.at(-1)] : mine.slice(-k)).join(sepOf(p.path)) || p.path;
  });
}

// an ancestor's key: the folder key's bytes up to its `n`th name, so a name that isn't UTF-8 survives
function ancestorKey(key, n) {
  const bin = atob(key.replace(/-/g, '+').replace(/_/g, '/') + '==='.slice((key.length + 3) % 4));
  let seen = 0, end = bin.length;
  for (let i = 0; i < bin.length; i++) if ((bin[i] === '/' || bin[i] === '\\') && ++seen === n) { end = i || 1; break; }
  return btoa(bin.slice(0, end)).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

// what's shown: a group of identical files, or a folder holding copies, with the same few facts
function items() {
  const m = model(), q = DP.q.trim().toLowerCase();
  const list = DP.by === 'folder'
    ? m.places.map(p => ({ id: p.path, kind: 'place', p, copies: p.copies, free: p.free, size: p.copies.reduce((n, c) => n + c.g.size, 0),
      count: p.copies.length, name: p.name, newest: Math.max(...p.copies.map(c => c.f.mtime)) }))
    : m.groups.map(g => ({ id: g.sha256, kind: 'group', g, copies: g.shown.map(f => ({ g, f })), free: g.reclaimable, size: g.size,
      count: g.copies, name: g.names[0], newest: Math.max(...g.shown.map(f => f.mtime)) }));
  const found = q ? list.filter(it => it.name.toLowerCase().includes(q) || it.copies.some(c => c.f.path.toLowerCase().includes(q))) : list;
  const { key, asc } = DP.sort, dir = asc ? 1 : -1;
  const val = it => key === 'size' ? it.size : key === 'copies' ? it.count : key === 'mtime' ? it.newest : it.free;
  return found.sort((a, b) => key === 'name' ? dir * collator.compare(a.name, b.name) : dir * (val(a) - val(b)) || b.free - a.free);
}

// ---------- ticking: what to let go of

const canTick = (g, key) => g.files.some(f => f.key !== key && !DP.sel.has(f.key));
function tick(g, f, on) {
  if (on && !canTick(g, f.key)) return false;
  if (on) DP.sel.set(f.key, { key: f.key, path: f.path, label: leafOf(f.path) });
  else DP.sel.delete(f.key);
  return true;
}
// keep `keep` (listed copies of `g`), tick its other listed copies
function keepOnly(g, keep) { for (const f of g.shown) tick(g, f, !keep.includes(f)); }
// the copy kept when a group is ticked as a whole: the oldest, the newest, or the shortest path
const keeper = (g, by = 'oldest') => [...g.shown].sort((a, b) =>
  (by === 'newest' ? b.mtime - a.mtime : by === 'shortest' ? 0 : a.mtime - b.mtime) || a.path.length - b.path.length || collator.compare(a.path, b.path))[0];
// what the ticks free: a distinct copy (hardlinks of one inode are one) whose every path is ticked
function freed() {
  let n = 0;
  for (const g of model().groups) {
    const copies = new Map();
    for (const f of g.files) copies.set(f.copy, (copies.get(f.copy) ?? true) && DP.sel.has(f.key));
    for (const all of copies.values()) if (all) n += g.size;
  }
  return n;
}
// an item's own tick: on when some of it is ticked
function itemTick(it) {
  const n = it.copies.filter(c => DP.sel.has(c.f.key)).length;
  const label = it.kind === 'group' ? (n ? 'Untick this group' : 'Tick every copy but the oldest') : (n ? 'Untick the copies here' : 'Tick the copies here');
  return `<input type="checkbox" data-dgt="${esc(it.id)}" ${n ? 'checked' : ''} ${n && n < it.copies.length - (it.kind === 'group') ? 'data-part' : ''} aria-label="${label}" title="${label}">`;
}
function onItemTick(it) {
  const any = it.copies.some(c => DP.sel.has(c.f.key));
  if (any) for (const c of it.copies) DP.sel.delete(c.f.key);
  else if (it.kind === 'group') keepOnly(it.g, [keeper(it.g)]);
  else for (const c of it.copies) tick(c.g, c.f, true);
}

// ---------- drawing

export function renderDupes() {
  if (H.S.mode !== 'dupes') return;
  const v = $('#view'), d = DP.d;
  $('#filter-btn').classList.toggle('on', H.filtering());
  const picker = `<label class="dp-min">Files of <select id="dupes-min" aria-label="Smallest file to count">${MIN.map(([x, l]) => `<option value="${x}" ${x === DP.min ? 'selected' : ''}>${l}</option>`).join('')}</select></label>`;
  const clear = H.filtering() ? '<button class="btn sm" type="button" id="clear-filters">Clear filters</button>' : '';
  if (H.S.filter.type === 'dir') {
    $('#f-count').textContent = '';
    v.innerHTML = `<div class="empty"><div><h2>Folders aren’t compared</h2><p>Duplicates are files with identical content. Pick another type to see them.</p><p>${clear}</p></div></div>`;
    return;
  }
  if (!d) return;
  const all = d.unfiltered || d, narrowed = all.groups !== d.groups;
  $('#f-count').textContent = narrowed ? `${num(d.groups)} of ${num(all.groups)} groups shown` : '';
  // ticks only on what's listed: a wipe or a new filter can take copies away
  const m = model();
  for (const k of DP.sel.keys()) if (!m.byKey.get(k)?.f.listed) DP.sel.delete(k);
  const hidden = d.only_hidden ? `${plural(d.only_hidden, 'group is', 'groups are')} only in hidden folders <button class="btn sm" type="button" id="show-hidden">Show hidden files</button>` : '';
  if (!m.groups.length) {
    v.innerHTML = all.groups || d.across?.files
      ? `<div class="empty"><div><h2>No duplicates match</h2><p>${plural(all.groups, 'group')} of identical files ${narrowed ? 'left out by the filters' : 'found'}. ${hidden}</p><p>${clear} ${picker}</p></div></div>${acrossHtml(d)}`
      : `<div class="empty"><div><h2>No duplicates ${DP.min === '1' ? 'found yet' : 'this size'}</h2><p>${picker}</p><p>Only hashed files count: <code>backdown hash</code> hashes everything that could be a duplicate.</p></div></div>`;
    return;
  }
  const more = d.largest.length < d.groups
    ? `<p class="dp-more muted">The ${num(d.largest.length)} groups that free the most are here.
        ${DP.limit < 5000 ? `<button class="btn sm" type="button" id="dp-more">Load ${num(Math.min(200, d.groups - d.largest.length))} more</button>` : ''}</p>` : '';
  v.innerHTML = `<header class="dg-top">
      <div class="dg-title"><h2>Duplicates</h2>
        <p><b class="dg-big">${bytes(d.reclaimable)}</b> could be freed across ${plural(d.groups, 'group')} of identical files${narrowed ? ` <span class="muted">of ${num(all.groups)} (${bytes(all.reclaimable)}) in all</span>` : ''}</p>
        ${hidden || clear ? `<p class="dg-notes">${hidden} ${clear}</p>` : ''}</div>
      <span class="dg-proof" title="Copies are compared by their full SHA-256, not by name or size; hardlinks of one file count once">${SVG.check}Content verified</span>
    </header>
    <div class="dg-bar">
      <div class="seg dg-by" role="group" aria-label="Group by">
        <button type="button" data-dg-by="content" aria-pressed="${DP.by === 'content'}" title="One entry for each set of identical files">Identical files</button>
        <button type="button" data-dg-by="folder" aria-pressed="${DP.by === 'folder'}" title="The folders the copies are in, where they part ways">Folders</button>
      </div>
      <input type="search" id="dg-q" class="dg-q" placeholder="Find by name or folder…" value="${esc(DP.q)}" aria-label="Find among the groups listed" spellcheck="false" autocomplete="off">
      ${picker}
      <button class="btn sm" type="button" id="dg-all"></button>
      <button class="btn sm" type="button" id="dp-select" aria-haspopup="menu">Select ▾</button>
    </div>
    <p class="dg-hint muted" id="dg-hint"></p>
    <div id="dg-list"></div>${more}${acrossHtml(d)}<div id="dg-sel"></div>`;
  wire();
  renderList();
}

// the part that changes as things are ticked, opened or found: the list and the bar under it
function renderList() {
  const el = $('#dg-list');
  if (H.S.mode !== 'dupes' || !el) return;
  const list = items(), view = H.S.view, all = DP.by === 'folder' ? model().places.length : model().groups.length;
  const what = DP.by === 'folder' ? 'folder' : 'group';
  $('#dg-hint').innerHTML = (DP.q.trim() ? `${plural(list.length, what)} of ${num(all)} match “${esc(DP.q.trim())}”. ` : '')
    + (DP.by === 'folder'
      ? 'Where the copies are, cut where each group’s copies part ways: folders holding the same things sit side by side. Open one to tick its copies, or keep them and tick the others.'
      : view === 'treemap' ? 'Each tile is a set of identical files, as big as the space it would free. Click one to see its copies.'
        : 'Open a group to compare where its copies are. <b>Keep</b> one, and the others are ticked; then <b>Free up</b>.');
  const allBtn = $('#dg-all');
  allBtn.hidden = view !== 'list';
  allBtn.textContent = list.length && list.every(it => DP.open.has(it.id)) ? 'Collapse all' : 'Expand all';
  if (!list.length) el.innerHTML = `<div class="dg-none muted">Nothing listed matches “${esc(DP.q.trim())}”.</div>`;
  else if (view === 'treemap') el.innerHTML = mapHtml(list);
  else if (view === 'grid' || view === 'gallery') el.innerHTML = cardsHtml(list, view === 'gallery');
  else el.innerHTML = rowsHtml(list);
  for (const x of el.querySelectorAll('[data-part]')) x.indeterminate = true;
  $('#dg-sel').innerHTML = selBarHtml();
}

// a picture for images the browser can show (sized by where it sits), else the file type's icon
const thumbOr = (f, size, px) => H.thumb({ id: f.id, name: leafOf(f.path), size }) || fileIcon(leafOf(f.path), px);
function itemIcon(it, px) {
  if (it.kind === 'place') return folderIcon(px);
  const f = it.g.shown[0];
  return thumbOr(f, it.g.size, px);
}

// where a group's copies are, on one line: what their folders share, and in braces where they part
// ways: /home/me/Projects/workspace-{ask, chart, deploy, +35 more}
function placesHtml(g, max = 3) {
  const folders = [...new Set(g.files.map(f => folderOf(f.path)))];
  if (folders.length === 1) return `in ${esc(folders[0])}`;
  const mids = [...new Set(folders.map(f => midOf(f, g.shared)))];
  if (mids.some(x => !x)) return `${esc(folders[0])} <span class="dim">and ${plural(folders.length - 1, 'folder')} below it</span>`;
  const [pre, post] = g.shared, sep = sepOf(folders[0]), parts = folders[0].split(/[\\/]/);
  const a = commonEdge(mids), z = commonEdge(mids.map(x => x.slice(a.length)), true);
  const inner = mids.map(x => x.slice(a.length, x.length - z.length) || '·');
  const head = (pre ? parts.slice(0, pre).join(sep) + sep : '') + a, tail = z + (post ? sep + parts.slice(-post).join(sep) : '');
  return `<span class="dim">${esc(head)}</span>{<b>${inner.slice(0, max).map(esc).join(', ')}</b>${inner.length > max ? `, <span class="dim">+${num(inner.length - max)} more</span>` : ''}}<span class="dim">${esc(tail)}</span>`;
}
// a start (or end) all the names share, cut where a word ends: the "workspace-" of workspace-ask and workspace-chart
function commonEdge(names, end = false) {
  const flip = s => end ? [...s].reverse().join('') : s, ns = names.map(flip);
  let i = 0;
  while (ns.every(n => i < n.length && n[i] === ns[0][i])) i++;
  const edge = ns[0].slice(0, i), cut = Math.max(...['-', '_', '.', ' ', '/', '\\'].map(c => edge.lastIndexOf(c)));
  return cut < 0 ? '' : flip(edge.slice(0, cut + 1));
}
// a copy's folder in full, with what it shares with its group's others dimmed
function folderHtml(g, f) {
  const folder = folderOf(f.path), [pre, post] = g.shared, sep = sepOf(folder), parts = folder.split(/[\\/]/);
  const mid = parts.slice(pre, parts.length - post).join(sep);
  if (!mid) return esc(folder);
  return `${pre ? esc(parts.slice(0, pre).join(sep) + sep) : ''}<em>${esc(mid)}</em>${post ? esc(sep + parts.slice(-post).join(sep)) : ''}`;
}

// what an item says about itself, in the row, the card and the tile
function facts(it) {
  if (it.kind === 'group') {
    const g = it.g, ticked = it.copies.filter(c => DP.sel.has(c.f.key)).length;
    return {
      title: `${esc(it.name)}${g.names.length > 1 ? ` <span class="dim">· also named ${esc(g.names.slice(1, 3).join(', '))}${g.names.length > 3 ? '…' : ''}</span>` : ''}`,
      sub: placesHtml(g),
      count: plural(g.copies, 'copy', 'copies'),
      countSub: [g.files.length > g.copies ? `${num(g.files.length)} names` : '', g.files.length > g.shown.length ? `${num(g.files.length - g.shown.length)} not listed` : '',
        g.remote?.length ? `+${num(g.remote.length)} on other machines` : ''].filter(Boolean).join(' · '),
      each: bytes(g.size), eachSub: 'each',
      free: bytes(g.reclaimable), freeSub: ticked ? `${num(ticked)} ticked` : 'to free',
    };
  }
  const p = it.p, ticked = it.copies.filter(c => DP.sel.has(c.f.key)).length;
  return {
    title: esc(it.name), sub: `<span class="dim">${esc(p.path)}</span>`,
    count: plural(it.count, 'copy', 'copies'), countSub: `also in ${plural(p.others, 'other folder')}`,
    each: bytes(it.size), eachSub: 'here',
    free: bytes(p.free), freeSub: ticked ? `${num(ticked)} ticked` : 'frees if wiped',
  };
}

function rowsHtml(list) {
  const folder = DP.by === 'folder';
  return `<div class="dg-list" role="list">
    <div class="dg-row dg-headrow" aria-hidden="true"><span></span><span></span><span></span>
      <span>${folder ? 'Folder' : 'Identical files'}</span><span class="dg-c">${folder ? 'Copies here' : 'Copies'}</span><span class="dg-e">${folder ? 'Size' : 'Size each'}</span><span class="dg-f">${folder ? 'Frees' : 'To free'}</span></div>
    ${list.map(it => {
      const open = DP.open.has(it.id), x = facts(it);
      return `<div class="dg${open ? ' open' : ''}" role="listitem">
        <div class="dg-row" role="button" tabindex="0" aria-expanded="${open}" data-dg="${esc(it.id)}">
          <span class="dg-chev">${SVG.chev}</span>${itemTick(it)}<span class="dg-ic">${itemIcon(it, 32)}</span>
          <span class="dg-main"><span class="dg-t">${x.title}</span><span class="dg-s">${x.sub}</span></span>
          <span class="n dg-c">${x.count}<small>${x.countSub}</small></span>
          <span class="n dg-e">${x.each}<small>${x.eachSub}</small></span>
          <span class="n dg-f">${x.free}<small>${x.freeSub}</small></span>
        </div>
        ${open ? `<div class="dg-body">${copiesHtml(it)}</div>` : ''}
      </div>`;
    }).join('')}</div>`;
}

function cardsHtml(list, big) {
  const focus = list.find(it => it.id === DP.focus);
  return `<div class="dg-cards${big ? ' big' : ''}">${list.map(it => {
    const x = facts(it), on = it === focus;
    return `<div class="dg-card${on ? ' open' : ''}${it.copies.some(c => DP.sel.has(c.f.key)) ? ' ticked' : ''}" role="button" tabindex="0" aria-expanded="${on}" data-dg="${esc(it.id)}">
        ${itemTick(it)}<span class="bdg dg-n">×${num(it.kind === 'group' ? it.g.copies : it.count)}</span>
        <div class="dg-stack${it.count > 1 ? ' many' : ''}">${itemIcon(it, big ? 96 : 64)}</div>
        <div class="dg-t">${esc(it.name)}</div>
        <div class="dg-meta">${x.each} ${x.eachSub} · <b>${x.free}</b> ${it.kind === 'group' ? 'to free' : 'frees'}</div>
        <div class="dg-s">${x.sub}</div>
      </div>${on ? `<div class="dg-panel">${panelHead(it)}${copiesHtml(it)}</div>` : ''}`;
  }).join('')}</div>`;
}

// the size map: each tile as big as what it would free
function mapHtml(list) {
  const v = $('#view'), W = Math.max(240, v.clientWidth - 28), H2 = Math.max(260, Math.min(560, v.clientHeight - 200));
  const sized = list.filter(it => it.free > 0), total = sized.reduce((n, it) => n + it.free, 0);
  if (!total) return '<div class="dg-none muted">Nothing here frees any space.</div>';
  const minArea = 46 * 28, big = [], small = [];
  for (const it of sized) (it.free / total * W * H2 >= minArea ? big : small).push(it);
  const rest = small.reduce((n, it) => n + it.free, 0);
  const cells = squarify([...big.map(it => ({ size: it.free, it })), ...(rest ? [{ size: rest, rest: small.length }] : [])], { x: 0, y: 0, w: W, h: H2 });
  const focus = list.find(it => it.id === DP.focus);
  return `<div class="treemap dg-map" style="width:${W}px;height:${H2}px">${cells.map(c => {
    const at = `left:${c.x.toFixed(1)}px;top:${c.y.toFixed(1)}px;width:${c.w.toFixed(1)}px;height:${c.h.toFixed(1)}px`;
    if (c.item.rest) return `<div class="tm rest" style="${at}" title="${num(c.item.rest)} smaller · ${bytes(c.item.size)}">${c.w > 70 && c.h > 30 ? `<span class="tm-s">${num(c.item.rest)} smaller · ${bytes(c.item.size)}</span>` : ''}</div>`;
    const it = c.item.it, ticked = it.copies.some(x => DP.sel.has(x.f.key));
    const color = it.kind === 'place' ? 'var(--folder)' : H.tmColor[kindOf(it.name)] || H.tmColor.file;
    const label = c.w > 70 && c.h > 36 ? `<span class="tm-n">${ticked ? SVG.check : ''}${esc(it.name)}</span><span class="tm-s">${bytes(it.free)} · ×${num(it.count)}</span>`
      : c.w > 40 && c.h > 18 ? `<span class="tm-s">${bytes(it.free)}</span>` : '';
    return `<div class="tm${it === focus ? ' sel' : ''}${ticked ? ' dg-ticked' : ''}" data-dg="${esc(it.id)}" style="${at};--c:${color}" title="${esc(it.name)} · ${bytes(it.free)} to free · ${plural(it.count, 'copy', 'copies')}">${label}</div>`;
  }).join('')}</div>${focus ? `<div class="dg-panel">${panelHead(focus)}${copiesHtml(focus)}</div>` : ''}`;
}

// the title over a card's or tile's copies
function panelHead(it) {
  const x = facts(it);
  return `<div class="dg-ph">${itemTick(it)}<span class="dg-ic">${itemIcon(it, 32)}</span><span class="dg-main"><span class="dg-t">${x.title}</span><span class="dg-s">${x.sub}</span></span>
    <span class="dg-pf">${x.count} · ${x.each} ${x.eachSub} · <b>${x.free}</b> ${x.freeSub}</span><button class="dg-x" type="button" data-dg-close aria-label="Close">×</button></div>`;
}

// an item's copies: ticks, names, folders, dates, which stays
function copiesHtml(it) {
  const place = it.kind === 'place';
  const rows = it.copies.map(({ g, f }) => {
    const on = DP.sel.has(f.key), locked = !on && !canTick(g, f.key);
    const where = place
      ? `<span class="dim">${esc(it.p.path)}</span>${esc(folderOf(f.path).slice(it.p.path.length))} <span class="dim">· ${plural(g.copies - 1, 'more copy', 'more copies')} elsewhere</span>`
      : folderHtml(g, f);
    return `<tr class="dp-row${on ? ' sel' : ''}" data-dk="${f.key}">
      <td class="ck"><input type="checkbox" data-dck="${f.key}" ${on ? 'checked' : ''} ${locked ? 'disabled title="One copy always stays"' : 'title="Free up this copy"'} aria-label="Free up ${esc(f.path)}"></td>
      <td><div class="dg-file">${thumbOr(f, g.size, 22)}<div class="dg-fm"><button class="dp-nm" type="button" data-dview="${f.key}" title="Open ${esc(leafOf(f.path))}">${esc(leafOf(f.path))}</button><span class="dp-dir">${where}</span></div></div></td>
      <td class="n c-size">${bytes(g.size)}</td>
      <td class="c-date">${when(f.mtime)}</td>
      <td class="c-keep">${keepHtml(g, f)}</td>
      <td class="c-act"><button class="dg-act" type="button" data-dopen="${f.key}" title="Show it in its folder" aria-label="Show it in its folder">${SVG.show}</button><button class="dg-act" type="button" data-dcopy="${f.key}" title="Copy its path" aria-label="Copy its path">${SVG.copy}</button></td>
    </tr>`;
  }).join('');
  const extra = place
    ? `<div class="dg-acts"><button class="btn sm" type="button" data-dplace="tick">Tick the copies here</button>
        <button class="btn sm" type="button" data-dplace="keep">Keep these, tick their copies elsewhere</button>
        <button class="btn sm" type="button" data-dplace="show">Show this folder</button>
        <button class="btn sm" type="button" data-dplace="wipe">Free up this folder…</button></div>`
    : `${it.g.files.length > it.g.shown.length ? `<p class="dg-off muted">${plural(it.g.files.length - it.g.shown.length, 'more copy is', 'more copies are')} here but not listed (filtered out or hidden); ${it.g.files.length - it.g.shown.length === 1 ? 'it stays' : 'they stay'}.</p>` : ''}${dupesRemoteHtml(it.g) ? `<div class="dg-remote">${dupesRemoteHtml(it.g)}</div>` : ''}`;
  return `<div data-dg-of="${esc(it.id)}"><table class="list dg-copies"><thead><tr><th class="ck"></th><th>File and its folder</th><th class="n c-size">Size</th><th class="c-date">Modified</th><th class="c-keep">Keep</th><th class="c-act"></th></tr></thead>
    <tbody>${rows}</tbody></table>${extra}</div>`;
}

// which copy stays: Keep ticks the group's others; once some are ticked, each says whether it goes
function keepHtml(g, f) {
  const on = DP.sel.has(f.key), any = g.files.some(x => DP.sel.has(x.key));
  const sole = any && !on && g.files.filter(x => !DP.sel.has(x.key)).length === 1;
  if (sole) return `<span class="dg-kept">${SVG.check}Kept</span>`;
  return `${on ? '<span class="bdg dp-frees">frees</span>' : any ? '<span class="bdg dp-keeps">keeps</span>' : ''}<button class="btn sm dg-keep" type="button" data-dkeep="${f.key}" title="Keep this copy, and tick the group’s others">Keep</button>`;
}

function selBarHtml() {
  const n = DP.sel.size, over = n > H.wipeMax;
  if (!n) return '';
  return `<div class="dp-bar" role="status"><span><b>${plural(n, 'copy', 'copies')}</b> ticked · frees up to <b>${bytes(freed())}</b>
      <span class="${over ? 'warn-t' : 'muted'}">${over ? `Free up takes ${H.wipeMax} at a time: untick some, or free a whole folder (group by Folders)` : 'once Free up has checked each still has a copy that stays'}</span></span>
    <button class="btn sm" type="button" id="dp-clear">Untick all</button>
    <button class="btn sm primary" type="button" id="dp-free"${over ? ' disabled' : ''}>Free up…</button></div>`;
}

// ---------- doing

// what's drawn once per load: the picker, and the "Here once, and again elsewhere" links
function wire() {
  const m = $('#dupes-min');
  if (m) m.onchange = e => { DP.min = e.target.value; DP.limit = 200; showDupes(); };
  $('#view').querySelectorAll('[data-dupe]').forEach(b => b.onclick = async () => {
    const S = H.S;
    const hits = await api('search', { dir: S.roots.find(r => b.dataset.dupe.startsWith(r.path))?.key || S.roots[0].key, q: leafOf(b.dataset.dupe) });
    const h = hits.find(x => x.path === b.dataset.dupe);
    if (h) H.openLocal(h.dir_key, h.key);
  });
}

const itemOf = id => items().find(it => it.id === id) || null;
const fileOf = key => model().byKey.get(key) || null;
const showIn = ({ f }) => H.openLocal(f.dir, f.key);
const view = ({ g, f }) => openViewer({ kind: 'file', id: f.id, key: f.key, name: leafOf(f.path), size: g.size, mtime: f.mtime, later: 'kept' });
function toggle(id) {
  if (H.S.view === 'list') { if (!DP.open.delete(id)) DP.open.add(id); }
  else DP.focus = DP.focus === id ? null : id;
  renderList();
}
function placeAct(it, act) {
  const p = it.p;
  if (act === 'tick') for (const c of p.copies) tick(c.g, c.f, true);
  if (act === 'keep') for (const g of p.groups) keepOnly(g, p.copies.filter(c => c.g === g).map(c => c.f));
  if (act === 'show') return H.openLocal(p.key, null);
  if (act === 'wipe') return H.showWipe([{ key: p.key, path: p.path, label: leafOf(p.path) || p.path }]);
  renderList();
}

function onChange(e) {
  if (H.S.mode !== 'dupes') return;
  const t = e.target;
  if (t.dataset.dck) {
    const it = fileOf(t.dataset.dck);
    if (it && !tick(it.g, it.f, t.checked)) toast('One copy always stays');
    return renderList();
  }
  if (t.dataset.dgt) {
    const it = itemOf(t.dataset.dgt);
    if (it) onItemTick(it);
    renderList();
  }
}

let findTimer;
function onInput(e) {
  if (H.S.mode !== 'dupes' || e.target.id !== 'dg-q') return;
  clearTimeout(findTimer);
  findTimer = setTimeout(() => { DP.q = e.target.value; renderList(); }, 120);
}

function onClick(e) {
  if (H.S.mode !== 'dupes') return;
  const t = e.target, at = s => t.closest(s);
  if (at('#dp-select')) { const b = at('#dp-select').getBoundingClientRect(); return selectMenu(b.left, b.bottom + 4); }
  if (at('#dp-clear')) { DP.sel.clear(); return renderList(); }
  if (at('#dp-free')) return H.showWipe([...DP.sel.values()]);
  if (at('#dp-more')) { DP.limit = Math.min(5000, DP.limit + 200); return showDupes(); }
  if (at('#dg-all')) {
    const list = items();
    if (list.every(it => DP.open.has(it.id))) DP.open.clear(); else for (const it of list) DP.open.add(it.id);
    return renderList();
  }
  const by = at('[data-dg-by]');
  if (by) { DP.by = by.dataset.dgBy; DP.focus = null; localStorage.setItem('bd.dupes.by', DP.by); return renderDupes(); }
  if (at('[data-dg-close]')) { DP.focus = null; return renderList(); }
  const fileBtn = at('[data-dopen], [data-dview], [data-dcopy], [data-dkeep]');
  if (fileBtn) {
    const ds = fileBtn.dataset, it = fileOf(ds.dopen || ds.dview || ds.dcopy || ds.dkeep);
    if (!it) return;
    if (ds.dopen) return showIn(it);
    if (ds.dview) return view(it);
    if (ds.dcopy) return H.copyText(it.f.path);
    keepOnly(it.g, [it.f]);
    return renderList();
  }
  const pa = at('[data-dplace]');
  if (pa) { const it = itemOf(at('[data-dg-of]')?.dataset.dgOf); return it && placeAct(it, pa.dataset.dplace); }
  // a click on a copy's row ticks it, like its checkbox; the second click of a double-click opens it
  const row = at('[data-dk]');
  if (row && !at('input, button, a')) {
    if (e.detail >= 2) return;
    const it = fileOf(row.dataset.dk);
    if (it && !tick(it.g, it.f, !DP.sel.has(it.f.key))) toast('One copy always stays');
    return renderList();
  }
  const item = at('[data-dg]');
  if (item && !at('input, button, a')) return toggle(item.dataset.dg);
}

function onDblClick(e) {
  const row = H.S.mode === 'dupes' && !e.target.closest('input, button') && e.target.closest('[data-dk]');
  const it = row && fileOf(row.dataset.dk);
  if (it) { tick(it.g, it.f, !DP.sel.has(it.f.key)); renderList(); view(it); } // undo its first click's tick
}

function onKey(e) {
  if (H.S.mode !== 'dupes' || (e.key !== 'Enter' && e.key !== ' ')) return;
  const item = e.target.matches?.('[data-dg]') && e.target;
  if (!item) return;
  e.preventDefault();
  toggle(item.dataset.dg);
  $(`[data-dg="${CSS.escape(item.dataset.dg)}"]`)?.focus();
}

function onMenu(e) {
  if (H.S.mode !== 'dupes') return;
  const row = e.target.closest('[data-dk]'), item = !row && e.target.closest('[data-dg]');
  if (!row && !item) return;
  e.preventDefault();
  if (row) copyMenu(e.clientX, e.clientY, row.dataset.dk);
  else itemMenu(e.clientX, e.clientY, item.dataset.dg);
}

function selectMenu(x, y) {
  const each = by => () => { for (const g of model().groups) keepOnly(g, [keeper(g, by)]); renderList(); };
  menu(x, y, [
    { label: 'Keep the oldest copy of each', act: each('oldest') },
    { label: 'Keep the newest copy of each', act: each('newest') },
    { label: 'Keep the shortest path of each', act: each('shortest') },
    { sep: 1 },
    { label: 'Untick all', act: () => { DP.sel.clear(); renderList(); }, disabled: !DP.sel.size },
  ]);
}

export function dupesSortItems() {
  const set = (key, asc) => { DP.sort = { key, asc }; renderList(); };
  return [
    ...SORT.map(([k, l]) => ({ label: l, checked: DP.sort.key === k, act: () => set(k, DP.sort.key === k ? DP.sort.asc : k === 'name') })),
    { sep: 1 },
    { label: 'Ascending', checked: DP.sort.asc, act: () => set(DP.sort.key, true) },
    { label: 'Descending', checked: !DP.sort.asc, act: () => set(DP.sort.key, false) },
  ];
}

function itemMenu(x, y, id) {
  const it = itemOf(id);
  if (!it) return;
  const open = H.S.view === 'list' ? DP.open.has(id) : DP.focus === id;
  const items = [{ label: open ? 'Close' : 'Open', act: () => toggle(id) }];
  if (it.kind === 'group') items.push({ sep: 1 },
    { label: 'Keep the oldest copy, tick the others', act: () => { keepOnly(it.g, [keeper(it.g)]); renderList(); } },
    { label: 'Keep the newest copy, tick the others', act: () => { keepOnly(it.g, [keeper(it.g, 'newest')]); renderList(); } },
    { label: 'Untick this group', act: () => { for (const c of it.copies) DP.sel.delete(c.f.key); renderList(); } });
  else items.push({ sep: 1 },
    { label: 'Tick the copies here', act: () => placeAct(it, 'tick') },
    { label: 'Keep these, tick their copies elsewhere', act: () => placeAct(it, 'keep') },
    { label: 'Show this folder', act: () => placeAct(it, 'show') },
    { label: 'Free up this folder…', act: () => placeAct(it, 'wipe') });
  menu(x, y, items);
}

function copyMenu(x, y, key) {
  const it = fileOf(key);
  if (!it) return;
  const { g, f } = it, folder = folderOf(f.path), name = esc(leafOf(folder) || folder);
  const here = model().groups.flatMap(o => o.shown.filter(c => folderOf(c.path) === folder).map(c => ({ g: o, f: c })));
  menu(x, y, [
    { label: 'Open', act: () => view(it) },
    { label: 'Show in its folder', act: () => showIn(it) },
    { label: 'Show in file manager', act: () => H.openDesktop({ id: f.id, reveal: 1 }) },
    { label: 'Copy path', act: () => H.copyText(f.path) },
    { sep: 1 },
    { label: 'Keep only this copy', act: () => { keepOnly(g, [f]); renderList(); } },
    { label: `Tick every copy in ${name} (${num(here.length)})`, act: () => { for (const c of here) tick(c.g, c.f, true); renderList(); } },
    { label: `Keep the copies in ${name}, tick the others`, act: () => {
      for (const c of here) keepOnly(c.g, c.g.shown.filter(o => folderOf(o.path) === folder));
      renderList();
    } },
    { sep: 1 },
    { label: `Free up ${name}: wipe what has a copy…`, act: () => H.showWipe([{ key: f.dir, path: folder, label: leafOf(folder) || folder }]) },
  ]);
}
