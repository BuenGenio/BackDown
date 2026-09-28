// BackDown browser: folders as they were at any scan, with versions, copies and file viewers.
import { fileIcon, folderIcon, driveIcon, kindOf } from './icons.js';
import { openViewer, openDiff, openGit, previewInto } from './viewers.js';
import { squarify, compareListings, menu, closeMenu, menuOpen, toast } from './extras.js';

const $ = s => document.querySelector(s);
const esc = s => String(s ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
export const bytes = n => n >= 1e12 ? (n / 1e12).toFixed(2) + ' TB' : n >= 1e9 ? (n / 1e9).toFixed(1) + ' GB' : n >= 1e6 ? (n / 1e6).toFixed(1) + ' MB' : n >= 1e3 ? Math.round(n / 1e3) + ' KB' : n + ' B';
const count = n => n >= 1e6 ? (n / 1e6).toFixed(2) + ' M' : n >= 1e3 ? (n / 1e3).toFixed(1) + ' K' : String(n);
export const when = t => t ? new Date(t * 1000).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' }) : '—';
const day = t => t ? new Date(t * 1000).toLocaleDateString(undefined, { dateStyle: 'medium' }) : '—';
const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' });

export async function api(path, params = {}) {
  const q = new URLSearchParams(Object.entries(params).filter(([, v]) => v !== undefined && v !== null && v !== ''));
  const r = await fetch(`/api/${path}?${q}`, { credentials: 'same-origin' });
  const j = await r.json().catch(() => ({ error: r.statusText }));
  if (!r.ok || j.error) throw new Error(j.error || r.statusText);
  return j;
}

const S = {
  roots: [], root: null, dir: null, scan: null, timeline: [], listing: null, entries: [],
  view: localStorage.getItem('bd.view') || 'grid', sort: { key: 'name', asc: true },
  sel: new Set(), anchor: null, back: [], fwd: [], mode: 'browse', info: localStorage.getItem('bd.info') === '1',
  filter: { type: 'all', size: 'any', date: 'any' }, hidden: localStorage.getItem('bd.hidden') === '1', cmp: null,
};

// ---------- boot

async function boot() {
  let data;
  try { data = await api('roots'); } catch (e) { return fatal(e.message); }
  S.roots = data.roots;
  const st = data.status;
  $('#status-files').textContent = summary(st);
  renderPlaces();
  if (!S.roots.length) {
    $('#view').innerHTML = `<div class="empty"><div>${driveIcon()}<h2>Nothing indexed yet</h2><p>Index a drive first, then reload:<br><code>backdown index /media/my-drive</code></p></div></div>`;
    return;
  }
  const saved = sessionStorage.getItem('bd.dir');
  const start = saved && S.roots.some(r => atob64(saved).startsWith(atob64(r.key))) ? saved : S.roots[0].key;
  await go(start, { push: false });
}

const summary = st => `${count(st.files)} files · ${bytes(st.bytes)} · ${count(st.hashed)} hashed`;

function fatal(msg) {
  $('#view').innerHTML = `<div class="empty"><div><h2>Can't load the index</h2><p>${esc(msg)}</p></div></div>`;
}

// base64url -> raw bytes as a latin1 string, only to compare prefixes
function atob64(k) { return atob(k.replace(/-/g, '+').replace(/_/g, '/') + '==='.slice((k.length + 3) % 4)); }

function renderPlaces() {
  const busy = new Set([S.daemon?.current, ...(S.daemon?.queued || [])].filter(j => j?.kind === 'index').map(j => j.path));
  $('#places').innerHTML = S.roots.map(r => `
    <div class="place-row">
      <button class="place" type="button" data-key="${r.key}" aria-current="${S.root?.id === r.id && S.mode === 'browse'}" title="${esc(r.path)}">
        <span class="pi">${driveIcon()}</span><span class="pn">${esc(r.name)}</span>
        <span class="ps">${bytes(r.bytes)} · ${count(r.files)} files</span>
      </button>
      ${S.daemon?.running ? `<button class="rescan${busy.has(r.path) ? ' spin' : ''}" type="button" data-rescan="${r.id}" title="Rescan ${esc(r.name)} now" aria-label="Rescan ${esc(r.name)}"><svg viewBox="0 0 24 24"><path d="M20 11a8 8 0 1 0-2.3 5.7"/><path d="M20 5v6h-6"/></svg></button>` : ''}
    </div>`).join('');
  $('#dupes-btn').setAttribute('aria-current', String(S.mode === 'dupes'));
  $('#activity-btn').setAttribute('aria-current', String(S.mode === 'activity'));
  $('#entities-btn').setAttribute('aria-current', String(S.mode === 'entities'));
}

// ---------- navigation

async function go(dirKey, { push = true, select = null, scan = S.scan } = {}) {
  const root = S.roots.find(r => atob64(dirKey).startsWith(atob64(r.key)));
  if (!root) return;
  const rootChanged = S.root?.id !== root.id;
  if (rootChanged) scan = null;
  let listing;
  try { listing = await api('ls', { dir: dirKey, scan: scan ?? '' }); } catch (e) { return toastError(e); }
  if (push && S.dir && (S.dir !== dirKey || S.mode !== 'browse')) { S.back.push({ dir: S.dir, scan: S.scan }); S.fwd = []; }
  Object.assign(S, { root, dir: dirKey, scan, listing, mode: 'browse' });
  sessionStorage.setItem('bd.dir', dirKey);
  S.entries = listing.entries;
  S.sel = new Set(select ? [select] : []);
  S.anchor = select;
  closeSearch(false);
  if (rootChanged || !S.timeline.length) await loadTimeline();
  renderAll();
  if (select) document.querySelector(`[data-key="${CSS.escape(select)}"]`)?.scrollIntoView({ block: 'center' });
}

async function loadTimeline() {
  S.timeline = await api('timeline', { root: S.root.id }).catch(() => []);
  const t = $('#time');
  $('#timebar').hidden = S.timeline.length < 2;
  t.max = Math.max(0, S.timeline.length - 1);
  const span = S.timeline.length > 1 ? S.timeline.at(-1).finished_at - S.timeline[0].finished_at : 1;
  $('#ticks').innerHTML = S.timeline.map(p => `<i style="left:${S.timeline.length > 1 ? 100 * (p.finished_at - S.timeline[0].finished_at) / span : 0}%"></i>`).join('');
  syncSlider();
}

function syncSlider() {
  const i = S.scan == null ? S.timeline.length - 1 : S.timeline.findIndex(p => p.id === S.scan);
  $('#time').value = Math.max(0, i);
  const p = S.timeline[i];
  const now = S.scan == null || S.scan === S.listing?.latest_scan;
  $('#time-label').textContent = now ? `Now · scanned ${when(p?.finished_at)}` : when(p?.finished_at);
  $('#time-now').hidden = now;
}

async function setScan(idx) {
  const p = S.timeline[idx];
  const scan = !p || idx === S.timeline.length - 1 ? null : p.id;
  try {
    const listing = await api('ls', { dir: S.dir, scan: scan ?? '' });
    Object.assign(S, { scan, listing, entries: listing.entries });
  } catch {
    // this folder didn't exist then: climb to the closest ancestor that did
    for (const c of [...S.listing.crumbs].reverse().slice(1)) {
      try {
        const listing = await api('ls', { dir: c.key, scan: scan ?? '' });
        Object.assign(S, { scan, listing, dir: c.key, entries: listing.entries });
        break;
      } catch { /* keep climbing */ }
    }
  }
  S.sel = new Set([...S.sel].filter(k => S.entries.some(e => e.key === k)));
  renderAll();
}

function renderAll() {
  renderTabs();
  $('#timebar').hidden = S.mode !== 'browse' || S.timeline.length < 2;
  renderPlaces();
  renderPath();
  renderBanner();
  renderView();
  renderInfo();
  syncSlider();
  $('#back').disabled = !S.back.length;
  $('#fwd').disabled = !S.fwd.length;
}

function renderPath() {
  if (S.mode === 'compare' && S.cmp?.listing) {
    $('#pathbar').innerHTML = `<span class="crumb-tag">Compare</span>` + S.cmp.listing.crumbs.map((c, i) =>
      `${i ? '<span class="crumb-sep">›</span>' : ''}<button class="crumb" type="button" data-key="${c.key}" title="${esc(c.name)}">${esc(i === 0 ? S.root.name : c.name)}</button>`).join('');
    return;
  }
  if (S.mode !== 'browse') {
    $('#pathbar').innerHTML = `<span class="crumb" style="cursor:default">${{ dupes: 'Duplicates', activity: 'Activity', entities: 'What’s here' }[S.mode] || 'Search'}</span>`;
    return;
  }
  $('#pathbar').innerHTML = S.listing.crumbs.map((c, i) =>
    `${i ? '<span class="crumb-sep">›</span>' : ''}<button class="crumb" type="button" data-key="${c.key}" title="${esc(c.name)}">${esc(i === 0 ? S.root.name : c.name)}</button>`).join('');
  const bar = $('#pathbar');
  bar.scrollLeft = bar.scrollWidth;
}

function renderBanner() {
  const b = $('#banner');
  const L = S.listing;
  if (S.mode !== 'browse' || !L) { b.hidden = true; return; }
  if (L.needs_reindex) {
    b.className = 'banner warn'; b.hidden = false;
    b.innerHTML = `Folder sizes and history need one more <code>backdown index</code> of this drive (it was indexed by an older version).`;
    return;
  }
  if (S.scan == null || S.scan === L.latest_scan) {
    // a git repository: its history is one click away
    const repo = S.entries.some(e => e.kind === 'dir' && e.name === '.git');
    b.hidden = !repo;
    if (repo) { b.className = 'banner'; b.innerHTML = `This folder is a git repository. <button class="btn sm" type="button" id="banner-git">View history</button>`; }
    return;
  }
  const gone = S.entries.filter(e => e.later === 'deleted').length, changed = S.entries.filter(e => e.later === 'changed').length;
  b.className = 'banner'; b.hidden = false;
  b.innerHTML = `Showing <b>${esc(S.root.name)}</b> as it was on <b>${when(L.scanned_at)}</b>.
    ${gone ? `${gone} item${gone === 1 ? ' has' : 's have'} since been deleted (struck through).` : ''}
    ${changed ? `${changed} changed later.` : ''}
    <button class="btn sm" type="button" id="banner-now">Back to now</button>`;
}

// ---------- the folder view

function sorted() {
  const { key, asc } = S.sort;
  const dir = asc ? 1 : -1;
  const val = e => key === 'size' ? e.size : key === 'mtime' ? e.mtime : key === 'copies' ? e.copies : e.versions;
  const ext = e => e.kind === 'dir' || !e.name.includes('.') ? '' : e.name.slice(e.name.lastIndexOf('.') + 1).toLowerCase();
  return S.entries.filter(shown).sort((a, b) => (a.kind === b.kind ? 0 : a.kind === 'dir' ? -1 : 1)
    || (key === 'name' ? dir * collator.compare(a.name, b.name)
      : key === 'type' ? dir * collator.compare(ext(a), ext(b)) || collator.compare(a.name, b.name)
        : dir * (val(a) - val(b)) || collator.compare(a.name, b.name)));
}

// ---------- filters: type, size, modified, hidden files

const TYPES = [['all', 'Any'], ['dir', 'Folders'], ['image', 'Images'], ['video', 'Videos'], ['audio', 'Audio'], ['doc', 'Documents'],
  ['pdf', 'PDFs'], ['sheet', 'Spreadsheets'], ['slides', 'Presentations'], ['text', 'Text'], ['code', 'Code'],
  ['data', 'Data and databases'], ['archive', 'Archives'], ['disk', 'Disk images'], ['mail', 'Email'], ['other', 'Other files']];
const SIZES = [['any', 'Any', 0, Infinity], ['s', 'Under 1 MB', 0, 1e6], ['m', '1 MB – 100 MB', 1e6, 1e8], ['l', '100 MB – 1 GB', 1e8, 1e9], ['xl', 'Over 1 GB', 1e9, Infinity]];
const DATES = [['any', 'Any time'], ['7', 'In the past week'], ['30', 'In the past month'], ['365', 'In the past year'], ['old', 'More than a year ago']];
const filtering = () => S.filter.type !== 'all' || S.filter.size !== 'any' || S.filter.date !== 'any';
const isHidden = e => e.name.startsWith('.') || e.name.endsWith('~');

function shown(e) {
  if (!S.hidden && isHidden(e)) return false;
  const f = S.filter;
  if (f.type !== 'all') {
    if (f.type === 'dir') { if (e.kind !== 'dir') return false; }
    else {
      if (e.kind === 'dir') return false;
      const k = kindOf(e.name);
      const ok = f.type === 'data' ? ['data', 'sql', 'db'].includes(k) : f.type === 'text' ? ['text', 'diff'].includes(k)
        : f.type === 'other' ? k === 'file' : k === f.type;
      if (!ok) return false;
    }
  }
  if (f.size !== 'any') {
    const [, , lo, hi] = SIZES.find(x => x[0] === f.size);
    if (e.size < lo || e.size >= hi) return false;
  }
  if (f.date !== 'any') {
    const age = Date.now() / 1000 - e.mtime;
    if (f.date === 'old' ? age < 365 * 86400 : age > +f.date * 86400) return false;
  }
  return true;
}

function setupFilters() {
  const opts = list => list.map(([v, l]) => `<option value="${v}">${l}</option>`).join('');
  $('#f-type').innerHTML = opts(TYPES); $('#f-size').innerHTML = opts(SIZES); $('#f-date').innerHTML = opts(DATES);
  for (const [id, f] of [['#f-type', 'type'], ['#f-size', 'size'], ['#f-date', 'date']]) {
    $(id).addEventListener('change', e => { S.filter = { ...S.filter, [f]: e.target.value }; renderView(); });
  }
  $('#f-hidden').addEventListener('change', toggleHidden);
  $('#f-clear').onclick = clearFilters;
  syncFilterUI();
}
function syncFilterUI() {
  $('#f-type').value = S.filter.type; $('#f-size').value = S.filter.size; $('#f-date').value = S.filter.date;
  $('#f-hidden').checked = S.hidden;
  $('#filter-btn').classList.toggle('on', filtering());
}
function clearFilters() { S.filter = { type: 'all', size: 'any', date: 'any' }; syncFilterUI(); renderView(); }
function toggleFilters(show = $('#filterbar').hidden) {
  $('#filterbar').hidden = !show;
  $('#filter-btn').setAttribute('aria-pressed', String(show));
  if (show) $('#f-type').focus();
}
function toggleHidden() {
  S.hidden = !S.hidden;
  localStorage.setItem('bd.hidden', S.hidden ? '1' : '0');
  syncFilterUI();
  if (S.mode === 'browse') renderView();
  toast(S.hidden ? 'Showing hidden files' : 'Hiding hidden files');
}

function sortMenu() {
  const b = $('#sort-btn').getBoundingClientRect();
  const set = (key, asc) => { S.sort = { key, asc }; localStorage.setItem('bd.sort', JSON.stringify(S.sort)); if (S.mode === 'browse') renderView(); };
  menu(b.right - 200, b.bottom + 4, [
    ...[['name', 'Name'], ['size', 'Size'], ['mtime', 'Modified'], ['type', 'Type'], ['copies', 'Copies'], ['versions', 'Versions']].map(([k, l]) => ({
      label: l, checked: S.sort.key === k, act: () => set(k, S.sort.key === k ? S.sort.asc : k === 'name' || k === 'type'),
    })),
    { sep: 1 },
    { label: 'Ascending', checked: S.sort.asc, act: () => set(S.sort.key, true) },
    { label: 'Descending', checked: !S.sort.asc, act: () => set(S.sort.key, false) },
  ]);
}

// a real picture for images the browser can show (small enough to load for a thumbnail)
function thumb(e, extra = '') {
  if (kindOf(e.name) !== 'image' || e.size > 25e6 || /\.(tiff?|heic|heif|raw|cr2|nef|arw|dng|psd)$/i.test(e.name)) return '';
  return `<img class="thumb ${extra}" src="/api/content?id=${e.id}" alt="" loading="lazy" decoding="async" data-name="${esc(e.name)}">`;
}

function badges(e) {
  const out = [];
  if (e.new_here) out.push('<span class="bdg new" title="New or changed at this scan">new</span>');
  if (e.later === 'deleted') out.push('<span class="bdg deleted" title="Deleted after this scan">deleted later</span>');
  else if (e.later === 'changed') out.push('<span class="bdg changed" title="Changed after this scan">changed later</span>');
  if (e.copies) out.push(`<span class="bdg copies" title="${e.copies} identical cop${e.copies === 1 ? 'y' : 'ies'} elsewhere">×${e.copies + 1}</span>`);
  return out.join('');
}

function renderView() {
  const v = $('#view');
  if (S.mode !== 'browse' || !S.listing) return; // search, duplicates, activity and compare draw themselves
  const items = sorted();
  const hiddenBy = S.entries.length - items.length;
  $('#f-count').textContent = hiddenBy ? `${items.length} of ${S.entries.length} shown` : '';
  $('#filter-btn').classList.toggle('on', filtering());
  if (!items.length) {
    v.innerHTML = hiddenBy
      ? `<div class="empty"><div><h2>Nothing to show</h2><p>${hiddenBy} item${hiddenBy === 1 ? ' is' : 's are'} hidden by ${filtering() ? 'the filters' : 'the hidden-files setting'}.</p>
          <p>${filtering() ? '<button class="btn" type="button" id="clear-filters">Clear filters</button> ' : ''}${!S.hidden && S.entries.some(isHidden) ? '<button class="btn" type="button" id="show-hidden">Show hidden files</button>' : ''}</p></div></div>`
      : `<div class="empty"><div>${folderIcon(72)}<h2>Empty folder</h2><p>${S.scan != null ? 'Nothing was here at this scan.' : 'Nothing indexed in this folder.'}</p></div></div>`;
    return;
  }
  if (S.view === 'gallery') return renderGallery(v, items);
  if (S.view === 'treemap') return renderTreemap(v, items);
  if (S.view === 'grid') {
    v.innerHTML = `<div class="grid" role="listbox" aria-multiselectable="true">${items.map(e => `
      <div class="tile${S.sel.has(e.key) ? ' sel' : ''}${e.later === 'deleted' && S.scan != null ? ' ghost' : ''}" role="option" aria-selected="${S.sel.has(e.key)}" data-key="${e.key}" title="${esc(e.name)}">
        <div class="badges">${badges(e)}</div>
        ${e.kind === 'dir' ? folderIcon(56) : thumb(e) || fileIcon(e.name, 56)}
        <div class="nm">${esc(e.name)}</div>
        <div class="sub">${e.kind === 'dir' ? `${count(e.files ?? 0)} item${e.files === 1 ? '' : 's'}` : bytes(e.size)}</div>
      </div>`).join('')}</div>`;
  } else {
    const col = (k, label, n) => `<th data-sort="${k}" class="${n ? 'n' : ''}" ${S.sort.key === k ? `aria-sort="${S.sort.asc ? 'ascending' : 'descending'}"` : ''}>${label}</th>`;
    v.innerHTML = `<table class="list"><thead><tr>${col('name', 'Name')}${col('size', 'Size', 1)}${col('mtime', 'Modified')}${col('copies', 'Copies', 1)}${col('versions', 'Versions', 1)}<th></th></tr></thead><tbody>${items.map(e => `
      <tr class="${S.sel.has(e.key) ? 'sel' : ''}${e.later === 'deleted' && S.scan != null ? ' ghost' : ''}" data-key="${e.key}" aria-selected="${S.sel.has(e.key)}">
        <td><div class="nmc">${e.kind === 'dir' ? folderIcon(20) : fileIcon(e.name, 20)}<span class="nm">${esc(e.name)}</span></div></td>
        <td class="n">${e.kind === 'dir' ? `<span class="dim">${count(e.files ?? 0)} item${e.files === 1 ? '' : 's'} · </span>` : ''}${bytes(e.size)}</td>
        <td>${when(e.mtime)}</td>
        <td class="n">${e.kind === 'file' ? (e.copies ? e.copies + 1 : e.sha256 ? '1' : '<span class="dim">—</span>') : ''}</td>
        <td class="n">${e.versions > 1 ? e.versions : '<span class="dim">1</span>'}</td>
        <td>${badges(e)}</td>
      </tr>`).join('')}</tbody></table>`;
  }
}

// ---------- details pane

let infoSeq = 0;
async function renderInfo() {
  const seq = ++infoSeq; // a slower, older render must not overwrite a newer one
  const pane = $('#info');
  $('#info-btn').setAttribute('aria-pressed', String(S.info));
  pane.hidden = !S.info;
  if (!S.info) return;
  const keys = [...S.sel];
  const chosen = keys.map(k => S.entries.find(e => e.key === k)).filter(Boolean);
  if (chosen.length === 2 && chosen.every(e => e.kind === 'file')) {
    pane.innerHTML = `<div class="hero">${fileIcon(chosen[0].name, 48)}<h3>2 files selected</h3></div>
      <p class="note">${esc(chosen[0].name)}<br>${esc(chosen[1].name)}</p>
      <button class="btn primary" type="button" id="cmp-sel">Compare contents</button>`;
    $('#cmp-sel').onclick = () => openDiff(chosen[0].id, chosen[1].id);
    return;
  }
  if (chosen.length !== 1) {
    const L = S.listing;
    pane.innerHTML = L ? `<div class="hero">${folderIcon(64)}<h3>${esc(L.name)}</h3></div>
      <dl class="kv"><dt>Contains</dt><dd>${count(L.files)} files</dd><dt>Size</dt><dd>${bytes(L.bytes)}</dd>
      <dt>Scanned</dt><dd>${when(L.scanned_at)}</dd><dt>Location</dt><dd>${esc(L.path)}</dd></dl>
      ${chosen.length > 1 ? `<p class="note">${chosen.length} items selected</p>` : '<p class="note">Select a file to see its versions and every other place its content lives.</p>'}` : '';
    return;
  }
  const e = chosen[0];
  if (e.kind === 'dir') {
    pane.innerHTML = `<div class="hero">${folderIcon(64)}<h3>${esc(e.name)}</h3></div>
      <dl class="kv"><dt>Contains</dt><dd>${count(e.files)} files</dd><dt>Size</dt><dd>${bytes(e.size)}</dd><dt>Newest file</dt><dd>${when(e.mtime)}</dd>
      ${e.versions > 1 ? `<dt>Changed</dt><dd>${e.versions - 1} time${e.versions === 2 ? '' : 's'} across scans</dd>` : ''}</dl>
      <button class="btn" type="button" id="open-dir">Open</button>
      <button class="btn" type="button" id="git-dir" hidden>Git history</button>`;
    $('#open-dir').onclick = () => go(e.key);
    // a repository: offer its history when the folder holds a .git
    api('ls', { dir: e.key, scan: S.scan ?? '' }).then(l => {
      if (l.entries.some(x => x.kind === 'dir' && x.name === '.git')) {
        const b = $('#git-dir'); if (!b) return;
        b.hidden = false; b.onclick = () => openGit(e.key, e.name);
      }
    }).catch(() => {});
    return;
  }
  pane.innerHTML = `<div class="hero">${fileIcon(e.name, 64)}<h3>${esc(e.name)}</h3><div class="note">${bytes(e.size)}</div></div><p class="note">Loading…</p>`;
  let f;
  try { f = await api('file', { id: e.id }); } catch (err) { pane.innerHTML += `<p class="note">${esc(err.message)}</p>`; return; }
  if (seq !== infoSeq) return; // selection moved on while this loaded
  const vers = [...f.versions].reverse();
  pane.innerHTML = `<div class="preview" id="pv"></div><div class="hero">${fileIcon(f.name, 64)}<h3>${esc(f.name)}</h3>
      <div><button class="btn primary" type="button" id="open-file" ${f.content_from === null ? 'disabled' : ''}>Open</button></div></div>
    <dl class="kv"><dt>Size</dt><dd>${bytes(f.size)} <span class="note">(${f.size.toLocaleString()} bytes)</span></dd>
      <dt>Modified</dt><dd>${when(f.mtime)}</dd>
      <dt>SHA-256</dt><dd class="hash">${f.sha256 ? esc(f.sha256) : '<span class="note">not hashed: no other file shares its size, so it is unique</span>'}</dd>
      ${f.error ? `<dt>Read error</dt><dd>${esc(f.error)}</dd>` : ''}
      ${f.content_from ? `<dt>Read from</dt><dd>${esc(f.content_from)} <span class="note">(identical copy)</span></dd>` : ''}
      ${f.content_from === null ? `<dt>Content</dt><dd class="note">Not on any indexed drive any more. The index still remembers its size, hash and dates.</dd>` : ''}
    </dl>
    <h4>Other copies (${f.copies.length})</h4>
    ${f.copies.length ? f.copies.map(c => `<button class="rowlink" type="button" data-dir="${c.dir_key}" data-sel="${c.key}">${esc(c.path)}</button>`).join('') : '<p class="note">No identical copy anywhere else in the index.</p>'}
    <h4>Versions (${f.versions.length})</h4>
    ${vers.map(v => `<div class="ver"><span class="when">${day(v.from)}${v.to !== v.from ? ` – ${v.current ? 'now' : day(v.to)}` : ''}</span>
        <span class="meta">${bytes(v.size)}${v.sha256 ? ` · ${v.sha256.slice(0, 10)}…` : ''}${v.readable ? '' : ' · bytes gone'}</span>
        <span class="acts">${v.readable ? `<button class="btn sm" type="button" data-open-ver="${v.id}">Open</button>` : ''}
          ${v.readable && !v.current && f.versions.some(x => x.current && x.readable) ? `<button class="btn sm" type="button" data-cmp-ver="${v.id}">Diff</button>` : ''}</span></div>`).join('')}`;
  $('#open-file')?.addEventListener('click', () => openEntry(e));
  if (f.content_from !== null) previewInto($('#pv'), { id: f.id, name: f.name, size: f.size }, () => seq === infoSeq);
  pane.querySelectorAll('[data-dir]').forEach(b => b.onclick = () => go(b.dataset.dir, { select: b.dataset.sel, scan: null }));
  pane.querySelectorAll('[data-open-ver]').forEach(b => b.onclick = () => openViewer({ id: +b.dataset.openVer, name: f.name, size: f.versions.find(v => v.id === +b.dataset.openVer).size }, []));
  const cur = f.versions.find(x => x.current);
  pane.querySelectorAll('[data-cmp-ver]').forEach(b => b.onclick = () => openDiff(+b.dataset.cmpVer, cur.id));
}

// ---------- opening things

function openEntry(e) {
  if (e.kind === 'dir') return go(e.key);
  // in the gallery, the viewer steps through its photos and videos only
  const files = sorted().filter(x => x.kind === 'file' && (S.view !== 'gallery' || MEDIA.has(kindOf(x.name))));
  openViewer(e, files);
}

// ---------- photo gallery and size map

const MEDIA = new Set(['image', 'video']);
function renderGallery(v, items) {
  const media = items.filter(e => e.kind === 'file' && MEDIA.has(kindOf(e.name)));
  const dirs = items.filter(e => e.kind === 'dir');
  const sel = e => `${S.sel.has(e.key) ? ' sel' : ''}${e.later === 'deleted' && S.scan != null ? ' ghost' : ''}`;
  v.innerHTML = (dirs.length ? `<div class="g-dirs">${dirs.map(e => `<button class="g-dir${sel(e)}" type="button" data-key="${e.key}" title="${esc(e.name)}">${folderIcon(18)}<span>${esc(e.name)}</span></button>`).join('')}</div>` : '')
    + (media.length ? `<div class="gallery">${media.map(e => {
      const pic = kindOf(e.name) === 'video'
        ? `<video src="/api/content?id=${e.id}#t=0.5" preload="metadata" muted playsinline></video><span class="g-play" aria-hidden="true">▶</span>`
        : thumb(e, 'g-img') || `<div class="g-icon">${fileIcon(e.name, 64)}</div>`;
      return `<figure class="ph${sel(e)}" data-key="${e.key}" title="${esc(e.name)}">${pic}<figcaption><span>${esc(e.name)}</span><small>${day(e.mtime)}</small></figcaption></figure>`;
    }).join('')}</div>`
      : `<div class="empty"><div><h2>No photos or videos here</h2><p>${dirs.length ? 'Open a folder above, or switch' : 'Switch'} back to the grid with Ctrl+1.</p></div></div>`);
}

// colours per kind of file, as in the icons
const TM_COLOR = { text: '#8a8f98', code: '#5b6b8c', data: '#2e8b57', sql: '#b8800e', db: '#1f8a8a', image: '#8e5bd1', pdf: '#d8432b',
  doc: '#3b6fd8', sheet: '#2e8b57', slides: '#d97a1b', archive: '#8a6a3e', audio: '#c2458a', video: '#7a4fd6', disk: '#55606e',
  mail: '#3b8fd8', diff: '#2e8b57', file: '#8a8f98' };
function renderTreemap(v, items) {
  const sized = items.filter(e => e.size > 0);
  const total = sized.reduce((s, e) => s + e.size, 0);
  if (!total) { v.innerHTML = '<div class="empty"><div><h2>Nothing here takes up space</h2></div></div>'; return; }
  const W = Math.max(200, v.clientWidth - 28), H = Math.max(220, v.clientHeight - 28 - 34);
  // tiles too small to see or click are gathered into one "smaller items" tile
  const minArea = 40 * 26, big = [], small = [];
  for (const e of sized) (e.size / total * W * H >= minArea ? big : small).push(e);
  const rest = small.reduce((s, e) => s + e.size, 0);
  const cells = squarify([...big.map(e => ({ size: e.size, e })), ...(rest ? [{ size: rest, rest: small.length }] : [])], { x: 0, y: 0, w: W, h: H });
  const label = (c, name, size) => c.w > 60 && c.h > 34 ? `<span class="tm-n">${esc(name)}</span><span class="tm-s">${bytes(size)}</span>`
    : c.w > 38 && c.h > 18 ? `<span class="tm-s">${bytes(size)}</span>` : '';
  const empty = items.length - sized.length;
  v.innerHTML = `<div class="tm-head"><b>${bytes(total)}</b> in ${count(items.length)} item${items.length === 1 ? '' : 's'}${empty ? ` · ${empty} empty not shown` : ''}${small.length ? ` · ${small.length} small ones grouped` : ''} · double-click a folder to go in</div>
    <div class="treemap" style="width:${W}px;height:${H}px">${cells.map(c => {
      const at = `left:${c.x.toFixed(1)}px;top:${c.y.toFixed(1)}px;width:${c.w.toFixed(1)}px;height:${c.h.toFixed(1)}px`;
      if (c.item.rest) return `<div class="tm rest" style="${at}" title="${c.item.rest} smaller items · ${bytes(c.item.size)}">${label(c, `${c.item.rest} smaller items`, c.item.size)}</div>`;
      const e = c.item.e;
      const color = e.kind === 'dir' ? 'var(--folder)' : TM_COLOR[kindOf(e.name)] || TM_COLOR.file;
      const nest = e.kind === 'dir' && c.w > 110 && c.h > 70;
      return `<div class="tm ${e.kind}${S.sel.has(e.key) ? ' sel' : ''}${e.later === 'deleted' && S.scan != null ? ' ghost' : ''}" data-key="${e.key}" style="${at};--c:${color}" title="${esc(e.name)} · ${bytes(e.size)} · ${(100 * e.size / total).toFixed(1)}%"${nest ? ` data-nest="${e.key}" data-w="${c.w}" data-h="${c.h}"` : ''}>${label(c, e.name, e.size)}</div>`;
    }).join('')}</div>`;
  nestTreemap(v);
}

// a second level inside the bigger folder tiles, so a folder that is mostly one subfolder still shows
// where its bytes are (double-click an inner folder to go straight there)
let tmSeq = 0;
async function nestTreemap(v) {
  const seq = ++tmSeq, scan = S.scan;
  const tiles = [...v.querySelectorAll('.tm[data-nest]')].sort((a, b) => b.dataset.w * b.dataset.h - a.dataset.w * a.dataset.h).slice(0, 16);
  await Promise.all(tiles.map(async el => {
    const listing = await api('ls', { dir: el.dataset.nest, scan: scan ?? '' }).catch(() => null);
    if (!listing || seq !== tmSeq || !el.isConnected) return;
    const kids = listing.entries.filter(e => e.size > 0 && (S.hidden || !isHidden(e)));
    const W = +el.dataset.w - 12, H = +el.dataset.h - 34;
    if (!kids.length || W < 40 || H < 30) return;
    const total = kids.reduce((n, e) => n + e.size, 0), big = [], small = [];
    for (const e of kids) (e.size / total * W * H >= 18 * 14 ? big : small).push(e);
    const rest = small.reduce((n, e) => n + e.size, 0);
    const cells = squarify([...big.map(e => ({ size: e.size, e })), ...(rest ? [{ size: rest, rest: small.length }] : [])], { x: 0, y: 0, w: W, h: H });
    el.insertAdjacentHTML('beforeend', `<div class="tm-in" style="width:${W}px;height:${H}px">${cells.map(c => {
      const at = `left:${c.x.toFixed(1)}px;top:${c.y.toFixed(1)}px;width:${c.w.toFixed(1)}px;height:${c.h.toFixed(1)}px`;
      if (c.item.rest) return `<div class="tm2 rest" style="${at}" title="${c.item.rest} smaller items · ${bytes(c.item.size)}"></div>`;
      const e = c.item.e, color = e.kind === 'dir' ? 'var(--folder)' : TM_COLOR[kindOf(e.name)] || TM_COLOR.file;
      return `<div class="tm2 ${e.kind}" style="${at};--c:${color}" title="${esc(e.name)} · ${bytes(e.size)}"${e.kind === 'dir' ? ` data-go="${e.key}"` : ''}>${c.w > 56 && c.h > 18 ? `<span>${esc(e.name)}</span>` : ''}</div>`;
    }).join('')}</div>`);
  }));
}
let tmTimer;
new ResizeObserver(() => {
  if (S.mode === 'browse' && S.view === 'treemap') { clearTimeout(tmTimer); tmTimer = setTimeout(renderView, 120); }
}).observe($('#view'));

// ---------- compare a folder between two scans

const delta = r => (r.b?.size || 0) - (r.a?.size || 0);
const signed = n => n === 0 ? '±0' : `${n > 0 ? '+' : '−'}${bytes(Math.abs(n))}`;
const TEXTISH = new Set(['text', 'code', 'data', 'sql', 'diff']);

async function startCompare(dirKey = S.dir, a, b) {
  const tl = S.timeline;
  if (tl.length < 2) return toastError(new Error('This drive has only one scan so far: there is nothing to compare with yet.'));
  const latest = tl.at(-1).id;
  b = b ?? latest;
  a = a ?? (S.scan != null && S.scan !== latest ? S.scan : tl.at(-2).id);
  S.cmp = { dir: dirKey, a, b, same: false, listing: S.listing };
  S.mode = 'compare';
  closeSearch(false);
  await renderCompare();
}

async function renderCompare() {
  const c = S.cmp, v = $('#view');
  renderPlaces(); renderTabs();
  $('#banner').hidden = true; $('#timebar').hidden = true; $('#info').hidden = true;
  v.innerHTML = '<div class="empty"><p>Comparing…</p></div>';
  const ls = scan => api('ls', { dir: c.dir, scan }).catch(() => null);
  const [la, lb] = await Promise.all([ls(c.a), ls(c.b)]);
  if (S.mode !== 'compare' || S.cmp !== c) return;
  c.listing = lb || la || c.listing;
  renderPath(); renderTabs();
  const rows = compareListings(la?.entries || [], lb?.entries || []);
  const order = { changed: 0, added: 1, removed: 2, same: 3 };
  rows.sort((x, y) => order[x.status] - order[y.status] || Math.abs(delta(y)) - Math.abs(delta(x)) || collator.compare((x.b || x.a).name, (y.b || y.a).name));
  const of = st => rows.filter(r => r.status === st);
  const sum = (list, f) => list.reduce((n, r) => n + f(r), 0);
  const [added, removed, changed, same] = ['added', 'removed', 'changed', 'same'].map(of);
  const scans = sel => S.timeline.map(p => `<option value="${p.id}"${p.id === sel ? ' selected' : ''}>${esc(when(p.finished_at))}</option>`).join('');
  const list = rows.filter(r => c.same || r.status !== 'same');
  const pill = { added: 'ok', removed: 'off', changed: 'warn', same: '' };
  v.innerHTML = `<div class="cmp-bar">
      <label>From <select id="cmp-a">${scans(c.a)}</select></label>
      <button class="hb-btn" id="cmp-swap" type="button" title="Swap the scans" aria-label="Swap the scans"><svg viewBox="0 0 24 24"><path d="M7 7h13M16 3l4 4-4 4M17 17H4M8 13l-4 4 4 4"/></svg></button>
      <label>to <select id="cmp-b">${scans(c.b)}</select></label>
      <span class="cmp-sum"><span class="pill ok">${added.length} added · ${bytes(sum(added, r => r.b.size))}</span>
        <span class="pill off">${removed.length} removed · ${bytes(sum(removed, r => r.a.size))}</span>
        <span class="pill warn">${changed.length} changed · ${signed(sum(changed, delta))}</span></span>
      <label class="chk"><input type="checkbox" id="cmp-same"${c.same ? ' checked' : ''}> show ${same.length} unchanged</label>
      <button class="btn sm" id="cmp-close" type="button">Done</button></div>
    ${!la ? '<p class="note pad">This folder didn’t exist at the first scan: everything in it is new.</p>' : !lb ? '<p class="note pad">This folder is gone by the second scan.</p>' : ''}
    ${list.length ? `<table class="list cmp"><thead><tr><th></th><th>Name</th><th class="n">Before</th><th class="n">After</th><th class="n">Change</th><th>Modified</th></tr></thead><tbody>${list.map(r => {
      const e = r.b || r.a;
      return `<tr data-cmp="${e.key}" class="${r.status}" title="${e.kind === 'dir' ? 'Double-click to compare inside' : r.status === 'changed' && TEXTISH.has(kindOf(e.name)) ? 'Double-click to see the differences' : 'Double-click to open'}">
        <td><span class="pill ${pill[r.status]}">${r.status}</span></td>
        <td><div class="nmc">${e.kind === 'dir' ? folderIcon(20) : fileIcon(e.name, 20)}<span class="nm">${esc(e.name)}</span></div></td>
        <td class="n">${r.a ? bytes(r.a.size) : '—'}</td><td class="n">${r.b ? bytes(r.b.size) : '—'}</td>
        <td class="n ${delta(r) > 0 ? 'up' : delta(r) < 0 ? 'down' : ''}">${signed(delta(r))}</td><td>${when(e.mtime)}</td></tr>`;
    }).join('')}</tbody></table>`
      : `<div class="empty"><div><h2>No differences</h2><p>${same.length ? 'Everything in this folder is the same at both scans.' : 'This folder is empty at both scans.'}</p></div></div>`}`;
  $('#cmp-a').onchange = e => { c.a = +e.target.value; renderCompare(); };
  $('#cmp-b').onchange = e => { c.b = +e.target.value; renderCompare(); };
  $('#cmp-swap').onclick = () => { [c.a, c.b] = [c.b, c.a]; renderCompare(); };
  $('#cmp-same').onchange = e => { c.same = e.target.checked; renderCompare(); };
  $('#cmp-close').onclick = exitCompare;
  c.rows = rows;
}

function compareOpen(key) {
  const r = S.cmp.rows?.find(x => (x.b || x.a).key === key);
  if (!r) return;
  const e = r.b || r.a;
  if (e.kind === 'dir') { S.cmp = { ...S.cmp, dir: e.key }; return renderCompare(); }
  if (r.status === 'changed' && r.a.id && r.b.id && TEXTISH.has(kindOf(e.name))) return openDiff(r.a.id, r.b.id);
  openViewer(e, []);
}

function exitCompare() {
  const dir = S.cmp?.dir || S.dir;
  S.cmp = null; S.mode = 'browse';
  go(dir, { push: false, scan: null });
}

// ---------- actions: context menu, download, the desktop

const sepOf = p => p.includes('\\') && !p.includes('/') ? '\\' : '/';
const pathOf = e => `${S.listing.path.replace(/[\\/]+$/, '')}${sepOf(S.listing.path)}${e.name}`;

async function copyText(t) {
  try { await navigator.clipboard.writeText(t); }
  catch { const a = document.createElement('textarea'); a.value = t; document.body.append(a); a.select(); document.execCommand('copy'); a.remove(); }
  toast('Copied');
}
function download(e) {
  const a = document.createElement('a');
  a.href = `/api/content?id=${e.id}&download=1`; a.download = e.name;
  document.body.append(a); a.click(); a.remove();
}
async function openDesktop(params) {
  const r = await fetch(`/api/open?${new URLSearchParams(params)}`, { method: 'POST', headers: { 'X-BackDown': '1' }, credentials: 'same-origin' });
  const j = await r.json().catch(() => ({}));
  if (!r.ok || j.error) return toastError(new Error(j.error || r.statusText));
  toast(params.reveal || params.dir ? 'Shown in your file manager' : 'Opened with your system app');
}
function showDetails() { if (!S.info) $('#info-btn').click(); }
function selectAll() { S.sel = new Set(sorted().map(x => x.key)); paintSelection(); renderInfo(); }

function itemMenu(x, y) {
  const ents = [...S.sel].map(k => S.entries.find(e => e.key === k)).filter(Boolean);
  const one = ents.length === 1 ? ents[0] : null;
  let items;
  if (!ents.length) items = folderMenu();
  else if (one?.kind === 'dir') items = [
    { label: 'Open', hint: 'Enter', act: () => go(one.key) },
    { label: 'Open in new tab', hint: 'Ctrl+Enter', act: () => newTab(one.key) },
    { label: 'Show in file manager', act: () => openDesktop({ dir: one.key }) },
    { sep: 1 },
    { label: 'Size map', act: async () => { await go(one.key); setView('treemap'); } },
    { label: 'Compare with an earlier scan…', act: () => startCompare(one.key), disabled: S.timeline.length < 2 },
    { sep: 1 },
    { label: 'Copy path', act: () => copyText(pathOf(one)) },
    { label: 'Details', hint: 'Ctrl+I', act: showDetails },
  ];
  else if (one) items = [
    { label: 'Open', hint: 'Enter', act: () => openEntry(one) },
    { label: 'Open with system app', act: () => openDesktop({ id: one.id }), disabled: !one.id },
    { label: 'Show in file manager', act: () => openDesktop({ id: one.id, reveal: 1 }), disabled: !one.id },
    { label: 'Download a copy', act: () => download(one), disabled: !one.id },
    { sep: 1 },
    { label: 'Copy path', act: () => copyText(pathOf(one)) },
    { label: 'Details', hint: 'Ctrl+I', act: showDetails },
  ];
  else {
    const files = ents.filter(e => e.kind === 'file' && e.id);
    items = [
      { label: files.length > 20 ? 'Download copies (the first 20)' : `Download ${files.length} cop${files.length === 1 ? 'y' : 'ies'}`, disabled: !files.length,
        act: () => files.slice(0, 20).forEach((f, i) => setTimeout(() => download(f), i * 300)) },
      { label: 'Copy paths', act: () => copyText(ents.map(pathOf).join('\n')) },
      { sep: 1 },
      { label: `Details (${ents.length} selected)`, hint: 'Ctrl+I', act: showDetails },
    ];
  }
  menu(x, y, items);
}

function folderMenu() {
  return [
    { label: 'New tab', hint: 'Alt+N', act: () => newTab(S.dir) },
    { label: 'Select all', hint: 'Ctrl+A', act: selectAll },
    { sep: 1 },
    ...[['grid', 'Grid', 'Ctrl+1'], ['list', 'List', 'Ctrl+2'], ['gallery', 'Photo gallery', 'Ctrl+3'], ['treemap', 'Size map', 'Ctrl+4']]
      .map(([v, l, h]) => ({ label: l, hint: h, checked: S.view === v, act: () => setView(v) })),
    { sep: 1 },
    { label: 'Show hidden files', hint: 'Ctrl+H', checked: S.hidden, act: toggleHidden },
    { label: 'Filters…', act: () => toggleFilters(true) },
    { label: 'Sort by…', act: sortMenu },
    { sep: 1 },
    { label: 'Compare with an earlier scan…', act: () => startCompare(S.dir), disabled: S.timeline.length < 2 },
    { label: 'Show in file manager', act: () => openDesktop({ dir: S.dir }) },
    { label: 'Copy path', act: () => copyText(S.listing.path) },
    { sep: 1 },
    { label: 'Keyboard shortcuts', hint: '?', act: showHelp },
  ];
}

$('#view').addEventListener('contextmenu', e => {
  if (S.mode !== 'browse' || !S.listing) return;
  e.preventDefault();
  const item = e.target.closest('[data-key]');
  if (item && !S.sel.has(item.dataset.key)) select(item.dataset.key);
  if (!item && S.sel.size) { S.sel.clear(); paintSelection(); renderInfo(); }
  itemMenu(e.clientX, e.clientY);
});

// ---------- drag to select an area

let band = null, suppressClick = false;
$('#view').addEventListener('pointerdown', e => {
  if (e.button !== 0 || S.mode !== 'browse' || S.view === 'treemap' || e.pointerType === 'touch') return;
  if (e.target.closest('[data-key], button, input, select, a, th, video')) return;
  const v = $('#view'), r = v.getBoundingClientRect();
  if (e.clientX - r.left > v.clientWidth || e.clientY - r.top > v.clientHeight) return; // the scrollbar
  band = { x: e.clientX - r.left + v.scrollLeft, y: e.clientY - r.top + v.scrollTop, base: e.ctrlKey || e.metaKey || e.shiftKey ? new Set(S.sel) : new Set(), el: null, id: e.pointerId, frame: 0 };
});
$('#view').addEventListener('pointermove', e => {
  if (!band || e.pointerId !== band.id) return;
  const v = $('#view'), r = v.getBoundingClientRect();
  const x = e.clientX - r.left + v.scrollLeft, y = e.clientY - r.top + v.scrollTop;
  if (!band.el) {
    if (Math.hypot(x - band.x, y - band.y) < 5) return;
    band.el = document.createElement('div'); band.el.className = 'band'; v.append(band.el);
    v.setPointerCapture(e.pointerId);
  }
  // near an edge: scroll
  if (e.clientY > r.bottom - 28) v.scrollTop += 18; else if (e.clientY < r.top + 28) v.scrollTop -= 18;
  band.rect = { L: Math.min(x, band.x), T: Math.min(y, band.y), W: Math.abs(x - band.x), H: Math.abs(y - band.y) };
  Object.assign(band.el.style, { left: `${band.rect.L}px`, top: `${band.rect.T}px`, width: `${band.rect.W}px`, height: `${band.rect.H}px` });
  cancelAnimationFrame(band.frame);
  band.frame = requestAnimationFrame(bandSelect);
});
// select what the band touches (once a frame while dragging, and once more on release)
function bandSelect() {
  if (!band?.rect) return;
  const v = $('#view'), r = v.getBoundingClientRect(), { L, T, W, H } = band.rect;
  const sel = new Set(band.base);
  for (const it of v.querySelectorAll('[data-key]')) {
    const b = it.getBoundingClientRect(), ix = b.left - r.left + v.scrollLeft, iy = b.top - r.top + v.scrollTop;
    if (ix < L + W && ix + b.width > L && iy < T + H && iy + b.height > T) sel.add(it.dataset.key);
  }
  S.sel = sel; paintSelection();
}
const endBand = e => {
  if (!band || e.pointerId !== band.id) return;
  if (band.el) {
    cancelAnimationFrame(band.frame); bandSelect();
    band.el.remove(); suppressClick = true; S.anchor = [...S.sel].at(-1) ?? null; renderInfo();
  }
  band = null;
};
$('#view').addEventListener('pointerup', endBand);
$('#view').addEventListener('pointercancel', endBand);

// ---------- typing a location

function editPath() {
  if (S.mode !== 'browse' || !S.listing) return;
  closeSearch(false);
  $('#pathbar').innerHTML = `<input id="path-input" type="text" spellcheck="false" autocomplete="off" aria-label="Location: type a path and press Enter" value="${esc(S.listing.path)}">`;
  const inp = $('#path-input');
  inp.focus(); inp.select();
  inp.addEventListener('keydown', async e => {
    e.stopPropagation();
    if (e.key === 'Escape') { e.preventDefault(); renderPath(); $('#view').focus(); }
    else if (e.key === 'Enter') { e.preventDefault(); await goPath(inp.value); }
  });
  inp.addEventListener('blur', () => setTimeout(() => { if ($('#path-input') && document.activeElement !== $('#path-input')) renderPath(); }, 150));
}
async function goPath(text) {
  let t = text.trim();
  if (!t) return;
  // "Old-Laptop/home/alex": a drive's name stands for its path
  const first = t.split(/[\\/]/)[0], r = S.roots.find(x => x.name === first);
  if (r && !/^([\\/]|[A-Za-z]:)/.test(t)) t = r.path.replace(/[\\/]+$/, '') + t.slice(first.length);
  try {
    const res = await api('resolve', { path: t });
    await go(res.dir, { select: res.select || null, scan: null });
    $('#view').focus();
  } catch (e) { toastError(e); }
}

// ---------- tabs

// tabs saved by an earlier load of this page, read before anything renders (which would overwrite them)
const savedTabs = (() => { try { return JSON.parse(sessionStorage.getItem('bd.tabs')); } catch { return null; } })();
const TAB_FIELDS = ['root', 'dir', 'scan', 'listing', 'entries', 'timeline', 'mode', 'sel', 'anchor', 'back', 'fwd', 'view', 'sort', 'filter', 'cmp'];
let tabs = [], tabAt = 0;
const snap = () => Object.fromEntries(TAB_FIELDS.map(f => [f, S[f]]));
function tabTitle(t) {
  if (t.mode === 'activity') return 'Activity';
  if (t.mode === 'dupes') return 'Duplicates';
  if (t.mode === 'entities') return 'What’s here';
  if (t.mode === 'compare') return `Compare · ${t.cmp?.listing?.name || ''}`;
  if (!t.listing) return S.roots.find(r => t.dir && atob64(t.dir).startsWith(atob64(r.key)))?.name || '…';
  return t.listing.crumbs.length > 1 ? t.listing.name : t.root?.name || t.listing.name;
}
function renderTabs() {
  if (!tabs.length) tabs = [snap()];
  tabs[tabAt] = snap();
  const bar = $('#tabs');
  bar.hidden = tabs.length < 2;
  if (bar.hidden) { try { sessionStorage.removeItem('bd.tabs'); } catch {} return; }
  bar.innerHTML = tabs.map((t, i) => `<div class="tab" role="tab" aria-selected="${i === tabAt}" data-tab="${i}" title="${esc(t.listing?.path || tabTitle(t))}">
      <span class="tab-n">${esc(tabTitle(t))}</span><button class="tab-x" type="button" data-tab-close="${i}" aria-label="Close tab" title="Close tab (Alt+W)">×</button></div>`).join('');
  try { sessionStorage.setItem('bd.tabs', JSON.stringify({ at: tabAt, dirs: tabs.map(t => t.dir) })); } catch {}
}
async function newTab(dirKey = S.dir) {
  if (!dirKey) return;
  if (!tabs.length) tabs = [snap()];
  tabs[tabAt] = snap();
  tabs.splice(tabAt + 1, 0, { ...snap(), mode: 'browse', back: [], fwd: [], sel: new Set(), anchor: null, cmp: null, scan: null });
  tabAt++;
  Object.assign(S, tabs[tabAt]);
  await go(dirKey, { push: false, scan: null });
  renderTabs();
}
async function switchTab(i) {
  if (i === tabAt || !tabs[i]) return;
  tabs[tabAt] = snap();
  tabAt = i;
  Object.assign(S, tabs[i]);
  await showCurrent();
}
function closeTab(i = tabAt) {
  if (tabs.length < 2) return;
  tabs[tabAt] = snap();
  tabs.splice(i, 1);
  if (i < tabAt) tabAt--;
  tabAt = Math.min(tabAt, tabs.length - 1);
  Object.assign(S, tabs[tabAt]);
  showCurrent();
}
async function showCurrent() {
  setViewButtons(); syncFilterUI(); closeSearch(false);
  if (S.mode === 'activity') return showActivity();
  if (S.mode === 'dupes') return showDupes();
  if (S.mode === 'entities') return showEntities();
  if (S.mode === 'compare' && S.cmp) return renderCompare();
  S.mode = 'browse';
  if (!S.listing) return S.dir ? go(S.dir, { push: false, scan: null }) : showActivity();
  $('#timebar').hidden = S.timeline.length < 2;
  renderAll();
}

// ---------- keyboard shortcuts help

const HELP = [
  ['Moving around', [[['Enter'], 'Open'], [['Alt+↑'], 'Parent folder'], [['Alt+←', 'Backspace'], 'Back'], [['Alt+→'], 'Forward'], [['Ctrl+L'], 'Type a location'], [['Ctrl+F'], 'Search this folder']]],
  ['Selecting', [[['Arrows'], 'Move'], [['Shift+Arrows'], 'Extend the selection'], [['Ctrl+A'], 'Select all'], [['Esc'], 'Clear the selection'], [['Drag'], 'Select an area'], [['Shift+F10', 'Menu'], 'Actions for the selection']]],
  ['Views', [[['Ctrl+1'], 'Grid'], [['Ctrl+2'], 'List'], [['Ctrl+3'], 'Photo gallery'], [['Ctrl+4'], 'Size map'], [['Ctrl+H'], 'Show hidden files'], [['Ctrl+I'], 'Details pane']]],
  ['Tabs', [[['Alt+N'], 'New tab'], [['Alt+W'], 'Close tab'], [['Alt+PgDn'], 'Next tab'], [['Alt+PgUp'], 'Previous tab'], [['Ctrl+Enter'], 'Open a folder in a new tab'], [['Middle-click'], 'Open a folder in a new tab']]],
  ['File viewer', [[['←', '→'], 'Previous and next file'], [['Esc'], 'Close']]],
  ['Anywhere', [[['?'], 'This list']]],
];
function showHelp() {
  closeMenu();
  if (document.querySelector('.help-wrap')) return;
  const d = document.createElement('div');
  d.className = 'help-wrap';
  d.innerHTML = `<div class="help" role="dialog" aria-modal="true" aria-labelledby="help-t">
    <header><h2 id="help-t">Keyboard shortcuts</h2><button class="hb-btn" type="button" data-help-close aria-label="Close"><svg viewBox="0 0 24 24"><path d="M6 6l12 12M18 6 6 18"/></svg></button></header>
    <div class="help-cols">${HELP.map(([group, rows]) => `<section><h3>${group}</h3><dl>${rows.map(([keys, what]) =>
      `<dt>${keys.map(k => `<kbd>${k}</kbd>`).join(' or ')}</dt><dd>${what}</dd>`).join('')}</dl></section>`).join('')}</div></div>`;
  document.body.append(d);
  const prev = document.activeElement;
  d.querySelector('[data-help-close]').focus();
  const close = () => { d.remove(); document.removeEventListener('keydown', onKey, true); prev?.focus?.(); };
  const onKey = e => { if (e.key === 'Escape' || e.key === '?') { e.preventDefault(); e.stopPropagation(); close(); } };
  document.addEventListener('keydown', onKey, true);
  d.addEventListener('click', e => { if (e.target === d || e.target.closest('[data-help-close]')) close(); });
}

// ---------- search and duplicates

let searchTimer;
function openSearch() {
  $('#searchbox').hidden = false; $('#pathbar').hidden = true;
  $('#search-btn').setAttribute('aria-pressed', 'true');
  $('#q').focus(); $('#q').select();
}
function closeSearch(rerender = true) {
  if ($('#searchbox').hidden) return;
  $('#searchbox').hidden = true; $('#pathbar').hidden = false; $('#q').value = '';
  $('#search-btn').setAttribute('aria-pressed', 'false');
  if (rerender && S.mode === 'search') { S.mode = 'browse'; renderAll(); }
}
async function runSearch(q) {
  if (!q.trim()) { S.mode = 'browse'; renderAll(); return; }
  let hits;
  try { hits = await api('search', { dir: S.dir, q }); } catch (e) { return toastError(e); }
  S.mode = 'search';
  renderPath(); $('#banner').hidden = true;
  $('#view').innerHTML = hits.length ? `<table class="list"><thead><tr><th>Name</th><th>Folder</th><th class="n">Size</th><th>Modified</th></tr></thead><tbody>${hits.map(h => `
    <tr data-hit-dir="${h.kind === 'dir' ? h.key : h.dir_key}" data-hit-sel="${h.kind === 'dir' ? '' : h.key}">
      <td><div class="nmc">${h.kind === 'dir' ? folderIcon(20) : fileIcon(h.name, 20)}<span>${esc(h.name)}</span></div></td>
      <td class="path" title="${esc(h.path)}">${esc(h.path.slice(0, h.path.length - h.name.length))}</td>
      <td class="n">${bytes(h.size)}</td><td>${when(h.mtime)}</td></tr>`).join('')}</tbody></table>`
    : `<div class="empty"><div><h2>No matches</h2><p>Nothing named like “${esc(q)}” in this folder.</p></div></div>`;
}

let dupesMin = '1M';
async function showDupes() {
  S.mode = 'dupes';
  renderTabs(); renderPlaces(); renderPath(); $('#banner').hidden = true; $('#info').hidden = true; $('#timebar').hidden = true;
  $('#view').innerHTML = '<div class="empty"><p>Loading duplicates…</p></div>';
  let d;
  try { d = await api('dupes', { min: dupesMin }); } catch (e) { return toastError(e); }
  const sizes = [['1', 'any size'], ['100K', '100 KB and up'], ['1M', '1 MB and up'], ['100M', '100 MB and up']];
  const picker = `<select id="dupes-min" aria-label="Smallest file to count">${sizes.map(([v, l]) => `<option value="${v}" ${v === dupesMin ? 'selected' : ''}>${l}</option>`).join('')}</select>`;
  $('#view').innerHTML = d.largest.length ? `
    <div class="banner" style="margin:-14px -14px 14px">${bytes(d.reclaimable)} could be freed across ${count(d.groups)} groups of identical files (SHA-256 verified; hardlinks count once). ${picker}</div>
    <table class="list"><thead><tr><th>Copies</th><th class="n">Each</th><th class="n">Reclaimable</th><th>Locations</th></tr></thead><tbody>${d.largest.map(g => `
      <tr><td>${fileIcon(g.paths[0], 20)} ×${g.copies}</td><td class="n">${bytes(g.size)}</td><td class="n"><b>${bytes(g.reclaimable)}</b></td>
      <td>${g.paths.map(p => `<button class="rowlink" type="button" data-dupe="${esc(p)}">${esc(p)}</button>`).join('')}</td></tr>`).join('')}</tbody></table>`
    : `<div class="empty"><div><h2>No duplicates ${dupesMin === '1' ? 'found yet' : 'this size'}</h2><p>${picker}</p><p>Only hashed files count: <code>backdown hash</code> hashes everything that could be a duplicate.</p></div></div>`;
  $('#dupes-min').onchange = e => { dupesMin = e.target.value; showDupes(); };
  $('#view').querySelectorAll('[data-dupe]').forEach(b => b.onclick = async () => {
    const hits = await api('search', { dir: S.roots.find(r => b.dataset.dupe.startsWith(r.path))?.key || S.roots[0].key, q: b.dataset.dupe.split(/[\\/]/).pop() });
    const h = hits.find(x => x.path === b.dataset.dupe);
    if (h) { $('#timebar').hidden = S.timeline.length < 2; go(h.dir_key, { select: h.key, scan: null }); }
  });
}

// ---------- What's here: recognised things on every drive, with where they came from

const KIND_COLOR = { 'mail-store': '#3b8fd8', maildir: '#3b8fd8', mdbox: '#3b8fd8', thunderbird: '#3b8fd8', nextcloud: '#1f8a8a',
  'nextcloud-server': '#1f8a8a', timemachine: '#55606e', 'ios-backup': '#8e5bd1', 'android-backup': '#2e8b57', system: '#b8800e',
  'home-mac': '#d97a1b', 'home-linux': '#d97a1b', 'home-windows': '#d97a1b', website: '#c2458a', git: '#d8432b', 'sql-dump': '#b8800e',
  'disk-image': '#55606e', photos: '#8e5bd1', music: '#c2458a', videos: '#7a4fd6', export: '#3b6fd8', downloads: '#8a8f98', recovery: '#8a8f98' };
const ENT = { kind: 'all', q: '', data: null, overlaps: null };
const yearsOf = e => { if (!e.oldest || !e.newest) return ''; const a = new Date(e.oldest * 1000).getFullYear(), b = new Date(e.newest * 1000).getFullYear(); return a === b ? `${a}` : `${a}–${b}`; };

async function showEntities() {
  S.mode = 'entities';
  closeSearch(false);
  renderTabs(); renderPlaces(); renderPath();
  $('#banner').hidden = true; $('#info').hidden = true; $('#timebar').hidden = true;
  $('#view').innerHTML = '<div class="empty"><p>Loading…</p></div>';
  try { ENT.data = await api('entities'); } catch (e) { return toastError(e); }
  renderEntities();
  // how much of each thing exists elsewhere: slower, so it fills in afterwards
  api('overlaps').then(o => { ENT.overlaps = new Map(o.map(x => [x.unit.key + ':' + x.unit.level, x])); if (S.mode === 'entities') renderEntities(); }).catch(() => {});
}

function renderEntities() {
  const d = ENT.data, v = $('#view');
  if (S.mode !== 'entities' || !d) return;
  const roots = d.roots || [];
  const all = roots.flatMap(r => r.entities);
  const kinds = [...new Map(all.map(e => [e.kind, e.label])).entries()];
  const match = e => (ENT.kind === 'all' || e.kind === ENT.kind) && (!ENT.q || `${e.label} ${e.title} ${e.path} ${e.provenance.join(' ')} ${e.facts.map(f => f[1]).join(' ')}`.toLowerCase().includes(ENT.q));
  const keep = v.scrollTop;
  const bar = `<div class="ent-bar">
      <label>Show <select id="ent-kind"><option value="all">Everything (${all.length})</option>${kinds.map(([k, l]) => `<option value="${k}"${k === ENT.kind ? ' selected' : ''}>${esc(l)} (${all.filter(e => e.kind === k).length})</option>`).join('')}</select></label>
      <input id="ent-q" type="search" placeholder="Filter by name, path or origin" value="${esc(ENT.q)}" aria-label="Filter">
      <span class="grow"></span>
      ${d.daemon ? '<button class="btn sm" type="button" id="ent-refresh">Recognise again</button>' : ''}
      <a class="btn sm" href="/api/report" target="_blank" rel="noopener">Report</a>
      <a class="btn sm" href="/api/report?format=md" download="backdown-report.md">Markdown</a></div>`;
  if (!roots.length) {
    v.innerHTML = bar + `<div class="empty"><div><h2>Nothing recognised yet</h2><p>${d.daemon ? 'BackDown recognises what’s on a drive after it hashes it. Start it now with “Recognise again”.' : 'Run <code>backdown entities</code> in a terminal, then reload.'}</p></div></div>`;
  } else {
    v.innerHTML = bar + roots.map(r => {
      const list = r.entities.filter(match);
      const drive = ENT.overlaps?.get(`${S.roots.find(x => x.path === r.root)?.key}:drive`);
      const junk = r.junk.reduce((n, j) => n + j.bytes, 0) + r.entities.reduce((n, e) => n + e.junk.reduce((m, j) => m + j.bytes, 0), 0);
      return `<section class="ent-drive">
        <header><h2>${esc(rootName(r.root))}</h2><span class="muted">${esc(r.root)}</span>
          ${drive ? `<span class="pill ${drive.elsewhere >= drive.unit.bytes && drive.unit.bytes ? 'ok' : ''}">${Math.floor(100 * drive.elsewhere / Math.max(1, drive.unit.bytes))}% exists elsewhere</span>` : ''}
          ${junk ? `<span class="pill">${bytes(junk)} regenerable</span>` : ''}
          ${r.fresh ? '' : '<span class="pill warn" title="Scanned again since: recognise again to update">from an earlier scan</span>'}
          ${r.read_drive ? '' : '<span class="pill off" title="The drive wasn’t connected, so its files couldn’t be read for provenance">drive wasn’t connected</span>'}</header>
        ${list.length ? `<div class="ents">${list.map(e => entityCard(e, r)).join('')}</div>` : `<p class="muted pad">${r.entities.length ? 'Nothing matches the filter.' : 'Nothing recognised on this drive: no mail stores, backups, systems, repositories or libraries.'}</p>`}
      </section>`;
    }).join('');
  }
  v.scrollTop = keep;
  $('#ent-kind').onchange = e => { ENT.kind = e.target.value; renderEntities(); };
  const q = $('#ent-q');
  q.oninput = () => { clearTimeout(q._t); q._t = setTimeout(() => { ENT.q = q.value.trim().toLowerCase(); renderEntities(); $('#ent-q').focus(); const i = $('#ent-q'); i.setSelectionRange(i.value.length, i.value.length); }, 200); };
  $('#ent-refresh')?.addEventListener('click', async () => {
    const r = await fetch('/api/recognise', { method: 'POST', headers: { 'X-BackDown': '1' }, credentials: 'same-origin' });
    if (!r.ok) return toastError(new Error((await r.json().catch(() => ({}))).error || r.statusText));
    toast('Recognising: it shows in Activity, and here when it’s done');
    if (!polling) pollDaemon(true);
  });
}

function entityCard(e, r) {
  const rel = e.path.startsWith(r.root) ? e.path.slice(r.root.length).replace(/^[\\/]/, '') || '.' : e.path;
  const ov = ENT.overlaps?.get(`${e.key}:entity`);
  const junk = e.junk.reduce((n, j) => n + j.bytes, 0);
  const share = ov && ov.unit.bytes ? ov.elsewhere / ov.unit.bytes : null;
  return `<article class="ent${e.low_value ? ' low' : ''}" style="--c:${KIND_COLOR[e.kind] || '#8a8f98'}">
    <div class="ent-top"><span class="ent-kind">${esc(e.label)}</span><span class="ent-size">${bytes(e.bytes)}</span></div>
    <h3>${esc(e.title)}</h3>
    <button class="rowlink ent-path" type="button" data-ent-open="${e.key}" data-ent-file="${e.is_file ? 1 : ''}" title="Open ${esc(e.path)}">${esc(rel)}</button>
    <div class="ent-meta">${[yearsOf(e), e.is_file ? '' : `${count(e.files)} files`, e.confidence < 0.8 ? `${Math.round(e.confidence * 100)}% sure` : ''].filter(Boolean).map(esc).join(' · ')}</div>
    ${e.facts.length ? `<dl class="ent-facts">${e.facts.filter(([k]) => k !== 'Size').map(([k, val]) => `<dt>${esc(k)}</dt><dd>${esc(val)}</dd>`).join('')}</dl>` : ''}
    ${e.provenance.length ? `<ul class="ent-prov">${e.provenance.map(p => `<li>${esc(p)}</li>`).join('')}</ul>` : ''}
    ${share != null ? `<div class="ent-else"><div class="meter"><i style="width:${(share * 100).toFixed(1)}%"></i></div><span>${Math.floor(share * 100)}% exists elsewhere${ov.containers[0] ? `, mostly in <button class="rowlink" type="button" data-ent-open="${ov.containers[0].key}">${esc(ov.containers[0].label)}</button>` : ''}</span></div>` : ''}
    ${junk ? `<p class="ent-junk">${bytes(junk)} regenerable: ${esc([...new Set(e.junk.map(j => j.why))].slice(0, 3).join(', '))}</p>` : ''}
    ${e.low_value ? '<p class="ent-junk">Probably not worth keeping as a unit.</p>' : ''}
  </article>`;
}

// open an entity's folder (or the folder holding it, with it selected)
async function openEntityPath(key, isFile) {
  if (!isFile) {
    try { await go(key, { scan: null }); return; } catch {}
  }
  const r = await api('resolve', { key }).catch(e => { toastError(e); return null; });
  if (r) go(r.dir, { select: r.select || null, scan: null });
}

// ---------- the daemon: activity, rescans, live refresh
//
// Long polls /api/daemon: the daemon answers as soon as something changes (at most once a second while
// a job reports progress) or after 50 s, so an idle browser costs about one small request a minute.
// Nothing is asked while the tab is hidden; coming back asks at once.

let lastDone = null, shownFail = 0, polling = false, pollTimer, busyKey = '', clockSkew = 0;
async function pollDaemon(fresh = false) {
  clearTimeout(pollTimer);
  if (polling || document.hidden) return;
  polling = true;
  const t0 = Date.now();
  let d, failed = false;
  try { d = await api('daemon', S.daemon && !fresh ? { since: S.daemon.change, wait: 50 } : {}); }
  catch { d = { running: false }; failed = !!S.daemon; }
  polling = false;
  await applyDaemon(d);
  if (document.hidden) return;
  if (failed) { pollTimer = setTimeout(pollDaemon, 15000, true); return; } // the daemon restarted or stopped
  if (!d.running) return; // `backdown serve`: there's no daemon to wait for
  // a change can arrive every second while a job runs; the Activity view shows every one, the sidebar every other
  const gap = (S.mode === 'activity' ? 1000 : 2000) - (Date.now() - t0);
  pollTimer = setTimeout(pollDaemon, Math.max(0, gap));
}
document.addEventListener('visibilitychange', () => { if (!document.hidden) pollDaemon(true); });

const jobName = j => j.kind === 'index' ? `Scan ${rootName(j.path)}` : j.kind === 'entities' ? 'Recognise what’s on the drives' : 'Hash new and changed files';
const rootName = p => S.roots.find(r => r.path === p)?.name || (p || '').split(/[\\/]/).filter(Boolean).pop() || p;

async function applyDaemon(d) {
  const had = !!S.daemon;
  S.daemon = d.running ? d : null;
  if (d.running) clockSkew = d.now - Date.now() / 1000;
  const st = $('#status-act');
  if (S.daemon) {
    const cur = d.current, q = d.queued.length, c = cur?.counts;
    st.innerHTML = cur
      ? `<span class="act"><span class="pulse"></span>${cur.kind === 'index' ? `Scanning ${esc(rootName(cur.path))}` : 'Hashing'}${c ? ` · ${pct(cur) != null ? Math.round(pct(cur) * 100) + '%' : count(c.files) + ' files'}` : ''}</span>${q ? `<span>${q} more queued</span>` : ''}`
      : `<span class="act idle">Up to date</span><span>${d.rescan_every_secs ? `rescans every ${dur(d.rescan_every_secs)}` : 'rescans on request'}</span>`;
    // a job finished: fresh sizes, timeline and (unless looking at the past) the open folder
    const done = d.recent.find(j => j.state === 'done');
    if (lastDone !== null && done && done.id !== lastDone) {
      const data = await api('roots').catch(() => null);
      if (data) { S.roots = data.roots; $('#status-files').textContent = summary(data.status); renderPlaces(); }
      if (S.mode === 'browse' && done.kind === 'index' && S.root && done.path === S.root.path) {
        await loadTimeline();
        if (S.scan == null) await go(S.dir, { push: false, scan: null, select: S.anchor });
      }
    }
    if (done) lastDone = done.id;
    // recognition finished: refresh What's here if it's on screen
    if (done && done.kind === 'entities' && done.id === lastDone && S.mode === 'entities' && done.finished_at > d.now - 5) showEntities();
    const failed = d.recent.find(j => j.state === 'failed');
    if (failed && failed.id > shownFail && failed.finished_at > d.now - 60 && S.mode !== 'activity') toastError(new Error(`${jobName(failed)} failed: ${failed.error}`));
    if (failed) shownFail = Math.max(shownFail, failed.id);
  }
  // the sidebar only changes when the set of busy drives does
  const key = JSON.stringify([had, !!S.daemon, S.daemon?.current?.path, S.daemon?.queued.map(j => j.path)]);
  if (key !== busyKey) { busyKey = key; renderPlaces(); }
  if (S.mode === 'activity') renderActivity();
}

async function rescan(rootId) {
  const r = await fetch(`/api/rescan?root=${rootId}`, { method: 'POST', headers: { 'X-BackDown': '1' }, credentials: 'same-origin' });
  const j = await r.json().catch(() => ({}));
  if (!r.ok) return toastError(new Error(j.error || r.statusText));
  // a long poll in flight returns by itself with the new job; otherwise ask now
  if (!polling) pollDaemon(true);
}

// ---------- the Activity view: what the daemon is doing, what's next, and when each drive is due

const rtf = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' });
function rel(t) {
  if (!t) return '—';
  const s = t - (Date.now() / 1000 + clockSkew);
  for (const [unit, n] of [['day', 86400], ['hour', 3600], ['minute', 60]]) if (Math.abs(s) >= n) return rtf.format(Math.round(s / n), unit);
  return Math.abs(s) < 10 ? (s < 0 ? 'just now' : 'any moment') : rtf.format(Math.round(s), 'second');
}
function dur(s) {
  s = Math.max(0, Math.round(s));
  if (s >= 86400) { const d = Math.floor(s / 86400), h = Math.round((s % 86400) / 3600); return h ? `${d} d ${h} h` : `${d} d`; }
  if (s >= 3600) { const h = Math.floor(s / 3600), m = Math.round((s % 3600) / 60); return m ? `${h} h ${m} min` : `${h} h`; }
  if (s >= 60) return `${Math.round(s / 60)} min`;
  return `${s} s`;
}
// how far a running job is, 0..1, or null when there's nothing to compare with (a drive's first scan)
function pct(j) {
  const c = j.counts;
  if (!c) return null;
  // quick samples are small fixed-size reads, so count files there; the full pass is measured in bytes
  if (j.kind === 'hash') return c.stage !== 'sample' && c.total_bytes ? c.bytes / c.total_bytes : c.total_files ? c.files / c.total_files : null;
  // a scan is measured against the drive's previous one, so it can run past 100%: cap it
  return c.total_files ? Math.min(0.99, c.files / c.total_files) : null;
}
function eta(j, now) {
  const p = pct(j), took = now - (j.started_at || now);
  if (p == null || p < 0.02 || took < 15) return '';
  return `about ${dur(took * (1 - p) / p)} left`;
}
const REASON = { requested: 'you asked', scheduled: 'scheduled', returned: 'drive came back', 'after-scan': 'after a scan' };
const JOB_ICON = {
  index: '<svg viewBox="0 0 24 24"><path d="M20 11a8 8 0 1 0-2.3 5.7"/><path d="M20 5v6h-6"/></svg>',
  hash: '<svg viewBox="0 0 24 24"><path d="M4 9h16M4 15h16M10 3 8 21M16 3l-2 18"/></svg>',
  entities: '<svg viewBox="0 0 24 24"><path d="M20.6 13.4 13.4 20.6a2 2 0 0 1-2.8 0L3 13V3h10l7.6 7.6a2 2 0 0 1 0 2.8z"/><circle cx="7.5" cy="7.5" r="1.5"/></svg>',
};

let activityClock;
function showActivity() {
  S.mode = 'activity';
  closeSearch(false);
  renderTabs(); renderPlaces(); renderPath();
  $('#banner').hidden = true; $('#info').hidden = true; $('#timebar').hidden = true;
  renderActivity();
  pollDaemon(true);
}

function renderActivity() {
  clearInterval(activityClock);
  if (S.mode !== 'activity') return;
  // relative times ("5 min ago") age between changes: redraw them twice a minute while on screen
  activityClock = setInterval(() => { if (S.mode !== 'activity') clearInterval(activityClock); else if (!document.hidden) renderActivity(); }, 30000);
  const d = S.daemon, now = Date.now() / 1000 + clockSkew;
  const view = $('#view');
  const keepScroll = view.scrollTop;
  if (!d) {
    view.innerHTML = `<div class="activity">
      <section class="act-card"><div class="act-head"><span class="act-dot off"></span><h2>BackDown isn't running in the background</h2></div>
        <p>This window was opened with <code>backdown serve</code>, so nothing rescans your drives on its own. To keep them up to date,
        run BackDown as a service: <code>backdown service install</code> (Linux), then <code>backdown open</code>.</p></section>
      ${drivesTable(S.roots.map(r => ({ path: r.path, last_scan_at: r.scanned_at, files: r.files, bytes: r.bytes, state: 'on-request', present: null })), false)}
    </div>`;
    return;
  }
  const cur = d.current;
  let now_;
  if (cur) {
    const c = cur.counts || {}, p = pct(cur);
    const stage = cur.kind === 'hash' ? (c.stage === 'sample' ? ' · quick samples' : c.stage ? ' · full SHA-256' : '') : '';
    const detail = cur.kind === 'hash'
      ? `${count(c.files || 0)} of ${count(c.total_files || 0)} files · ${bytes(c.bytes || 0)} of ${bytes(c.total_bytes || 0)}`
      : `${count(c.files || 0)} files · ${bytes(c.bytes || 0)}${c.total_files ? ` <span class="muted">(last scan: ${count(c.total_files)} files)</span>` : ''}`;
    now_ = `<section class="act-card now">
      <div class="act-head"><span class="pulse"></span><h2>${esc(jobName(cur))}<span class="muted">${stage}</span></h2><span class="act-pct">${p != null ? Math.round(p * 100) + '%' : ''}</span></div>
      <div class="meter${p == null ? ' unknown' : ''}"><i style="width:${p != null ? (p * 100).toFixed(1) : 100}%"></i></div>
      <p>${detail}</p>
      <p class="muted">Started ${rel(cur.started_at)} · ${REASON[cur.reason] || ''}${eta(cur, now) ? ` · ${eta(cur, now)}` : ''}</p>
    </section>`;
  } else {
    const next = d.drives.filter(x => x.present && x.next_at).sort((a, b) => a.next_at - b.next_at)[0];
    now_ = `<section class="act-card idle">
      <div class="act-head"><span class="act-dot"></span><h2>Up to date</h2></div>
      <p>${next ? `Next: a scan of <b>${esc(rootName(next.path))}</b> ${rel(next.next_at)}.` : d.rescan_every_secs ? 'Nothing is due: drives that are away are scanned when they come back.' : 'Drives are scanned when you ask (rescan button, or <code>backdown index</code>).'}</p>
    </section>`;
  }
  const queue = d.queued.length ? `<ol class="act-list">${d.queued.map((j, i) => `
      <li><span class="ji">${JOB_ICON[j.kind] || ''}</span><span class="jn">${i + 1}. ${esc(jobName(j))}</span><span class="pill">${REASON[j.reason] || ''}</span><span class="muted">queued ${rel(j.queued_at)}</span></li>`).join('')}</ol>`
    : '<p class="muted pad">Nothing waiting.</p>';
  const recent = d.recent.length ? `<ol class="act-list">${d.recent.map(j => `
      <li class="${j.state}"><span class="ji ${j.state}">${j.state === 'failed' ? '<svg viewBox="0 0 24 24"><path d="M12 8v5M12 16h.01"/><circle cx="12" cy="12" r="9"/></svg>' : '<svg viewBox="0 0 24 24"><path d="m5 12.5 4.5 4.5L19 7.5"/></svg>'}</span>
        <span class="jn">${esc(jobName(j))}<small>${esc(j.state === 'failed' ? j.error || 'failed' : j.summary)}</small></span>
        <span class="pill">${REASON[j.reason] || ''}</span>
        <span class="muted" title="${esc(when(j.finished_at))}">${rel(j.finished_at)}${j.started_at && j.finished_at ? ` · took ${dur(j.finished_at - j.started_at)}` : ''}</span></li>`).join('')}</ol>`
    : '<p class="muted pad">Nothing has run since the daemon started.</p>';
  view.innerHTML = `<div class="activity">
    ${now_}
    <section><h3>Up next</h3>${queue}</section>
    <section><h3>Drives and schedule</h3>${drivesTable(d.drives, true)}</section>
    <section><h3>Recent</h3>${recent}</section>
    <p class="act-foot muted">BackDown ${esc(d.version)} · running since ${rel(d.started_at)} · ${d.rescan_every_secs ? `rescans each drive every ${dur(d.rescan_every_secs)}` : 'rescans on request'} ·
      drives checked ${rel(d.checked_at)} · index <code>${esc(d.db)}</code></p>
  </div>`;
  view.scrollTop = keepScroll;
}

function drivesTable(drives, live) {
  if (!drives.length) return '<p class="muted pad">No drives indexed yet: <code>backdown index /media/my-drive</code></p>';
  const label = x => ({
    scanning: '<span class="pill busy">Scanning now</span>', queued: '<span class="pill busy">Waiting its turn</span>',
    away: '<span class="pill off">Not connected</span>', due: '<span class="pill warn">Due now</span>',
    'on-request': '<span class="pill">On request</span>', scheduled: '<span class="pill ok">Connected</span>',
  }[x.state] || '');
  const next = x => x.state === 'away' ? 'when it’s plugged back in' : x.state === 'on-request' ? '—' : x.state === 'scanning' ? 'now' : x.state === 'queued' ? 'next in line' : x.next_at ? rel(x.next_at) : '—';
  return `<table class="list act-drives"><thead><tr><th>Drive</th><th>Status</th><th>Last scan</th>${live ? '<th>Next scan</th>' : ''}<th class="n">Files</th><th class="n">Size</th>${live ? '<th></th>' : ''}</tr></thead><tbody>
    ${drives.map(x => { const r = S.roots.find(r => r.path === x.path); return `<tr>
      <td><b>${esc(rootName(x.path))}</b><small class="muted">${esc(x.path)}</small></td>
      <td>${live ? label(x) : ''}</td>
      <td title="${esc(when(x.last_scan_at))}">${x.last_scan_at ? rel(x.last_scan_at) : 'never finished'}</td>
      ${live ? `<td>${next(x)}</td>` : ''}
      <td class="n">${count(x.files)}</td><td class="n">${bytes(x.bytes)}</td>
      ${live ? `<td>${r && x.present !== false ? `<button class="btn sm" type="button" data-rescan="${r.id}"${['scanning', 'queued'].includes(x.state) ? ' disabled' : ''}>Rescan now</button>` : ''}</td>` : ''}
    </tr>`; }).join('')}</tbody></table>`;
}

// ---------- selection & keyboard

function select(key, ev) {
  const order = sorted().map(e => e.key);
  if (ev?.shiftKey && S.anchor) {
    const [a, b] = [order.indexOf(S.anchor), order.indexOf(key)].sort((x, y) => x - y);
    S.sel = new Set(order.slice(a, b + 1));
  } else if (ev?.ctrlKey || ev?.metaKey) {
    S.sel.has(key) ? S.sel.delete(key) : S.sel.add(key);
    S.anchor = key;
  } else {
    S.sel = new Set([key]); S.anchor = key;
  }
  paintSelection(); renderInfo();
}

// Mark the selection on the items already on screen; never rebuild the view for it, or the second
// click of a double-click lands on a new element and the browser drops the double-click.
function paintSelection() {
  document.querySelectorAll('#view [data-key]').forEach(el => {
    const on = S.sel.has(el.dataset.key);
    el.classList.toggle('sel', on);
    el.setAttribute('aria-selected', String(on));
  });
}

function move(dx, dy, ev) {
  const order = sorted();
  if (!order.length) return;
  let i = order.findIndex(e => e.key === S.anchor);
  let cols = 1;
  if (S.view === 'grid') {
    const tiles = [...document.querySelectorAll('.tile')];
    const top = tiles[0]?.offsetTop;
    cols = Math.max(1, tiles.filter(t => t.offsetTop === top).length);
  }
  i = i < 0 ? 0 : Math.min(order.length - 1, Math.max(0, i + dx + dy * cols));
  select(order[i].key, ev?.shiftKey ? ev : null);
  document.querySelector(`[data-key="${CSS.escape(order[i].key)}"]`)?.scrollIntoView({ block: 'nearest' });
}

function goBack() { const b = S.back.pop(); if (b) { S.fwd.push({ dir: S.dir, scan: S.scan }); go(b.dir, { push: false, scan: b.scan }); } }
function goFwd() { const f = S.fwd.pop(); if (f) { S.back.push({ dir: S.dir, scan: S.scan }); go(f.dir, { push: false, scan: f.scan }); } }
function goUp() { const c = S.listing?.crumbs; if (c && c.length > 1) go(c.at(-2).key, { select: c.at(-1).key }); }

function toastError(e) {
  const b = $('#banner');
  b.className = 'banner warn'; b.hidden = false; b.textContent = e.message || String(e);
}

// ---------- wiring

const VIEWS = ['grid', 'list', 'gallery', 'treemap'];
function setView(v) {
  S.view = VIEWS.includes(v) ? v : 'grid';
  localStorage.setItem('bd.view', S.view);
  setViewButtons();
  if (S.mode === 'browse') renderView();
}
function setViewButtons() { for (const x of VIEWS) $(`#view-${x}`).setAttribute('aria-pressed', String(S.view === x)); }

document.addEventListener('click', e => {
  const t = e.target;
  if (suppressClick) { suppressClick = false; if (t.closest('#view')) return; }
  const tab = t.closest('[data-tab-close]');
  if (tab) return closeTab(+tab.dataset.tabClose);
  const tb = t.closest('[data-tab]');
  if (tb) return switchTab(+tb.dataset.tab);
  if (t.closest('#clear-filters')) return clearFilters();
  const eo = t.closest('[data-ent-open]');
  if (eo) return openEntityPath(eo.dataset.entOpen, !!eo.dataset.entFile);
  const inner = t.closest('[data-go]');
  if (inner && e.detail >= 2) return go(inner.dataset.go);
  if (t.closest('#show-hidden')) return toggleHidden();
  const cr = t.closest('[data-cmp]');
  if (cr) {
    document.querySelectorAll('tr.sel[data-cmp]').forEach(x => x.classList.remove('sel')); cr.classList.add('sel');
    if (e.detail >= 2) compareOpen(cr.dataset.cmp);
    return;
  }
  const rs = t.closest('[data-rescan]');
  if (rs) { rs.classList.add('spin'); return rescan(rs.dataset.rescan); }
  const place = t.closest('.place[data-key]');
  if (place) { $('#timebar').hidden = S.timeline.length < 2; return go(place.dataset.key, { scan: null }); }
  const crumb = t.closest('.crumb[data-key]');
  if (crumb) return S.mode === 'compare' ? (S.cmp = { ...S.cmp, dir: crumb.dataset.key }, renderCompare()) : go(crumb.dataset.key);
  if (t.closest('#pathbar') && !t.closest('.crumb') && e.detail >= 2) return editPath();
  if (t.closest('#banner-now')) return setScan(S.timeline.length - 1);
  if (t.closest('#banner-git')) return openGit(S.dir, S.listing.name);
  const th = t.closest('#view th[data-sort]');
  if (th) { const k = th.dataset.sort; S.sort = { key: k, asc: S.sort.key === k ? !S.sort.asc : k === 'name' }; return renderView(); }
  const hit = t.closest('[data-hit-dir]');
  if (hit) return go(hit.dataset.hitDir, { select: hit.dataset.hitSel || null, scan: null });
  const item = t.closest('#view [data-key]');
  if (item) {
    if (e.detail >= 2 && !e.ctrlKey && !e.metaKey && !e.shiftKey) {
      const en = S.entries.find(x => x.key === item.dataset.key);
      return en && openEntry(en);
    }
    return select(item.dataset.key, e);
  }
  if (t.closest('#tabs')) return;
  if (t.closest('#view') && S.mode === 'browse') { S.sel.clear(); paintSelection(); renderInfo(); }
});
$('#back').onclick = goBack; $('#fwd').onclick = goFwd;
for (const v of VIEWS) $(`#view-${v}`).onclick = () => setView(v);
$('#filter-btn').onclick = () => toggleFilters();
$('#sort-btn').onclick = sortMenu;
$('#time-compare').onclick = () => startCompare(S.dir);
// middle-click: a folder opens in a new tab, a tab closes
document.addEventListener('mousedown', e => { if (e.button === 1 && e.target.closest('#view [data-key], #tabs [data-tab]')) e.preventDefault(); });
document.addEventListener('auxclick', e => {
  if (e.button !== 1) return;
  const tb = e.target.closest('#tabs [data-tab]');
  if (tb) return closeTab(+tb.dataset.tab);
  const item = e.target.closest('#view [data-key]');
  const en = item && S.mode === 'browse' && S.entries.find(x => x.key === item.dataset.key);
  if (en?.kind === 'dir') newTab(en.key);
});
$('#info-btn').onclick = () => { S.info = !S.info; localStorage.setItem('bd.info', S.info ? '1' : '0'); renderInfo(); };
$('#search-btn').onclick = () => $('#searchbox').hidden ? openSearch() : closeSearch();
$('#q').addEventListener('input', e => { clearTimeout(searchTimer); searchTimer = setTimeout(() => runSearch(e.target.value), 220); });
$('#q').addEventListener('keydown', e => { if (e.key === 'Escape') { closeSearch(); $('#view').focus(); } });
$('#dupes-btn').onclick = showDupes;
$('#activity-btn').onclick = showActivity;
$('#entities-btn').onclick = showEntities;
$('#time').addEventListener('input', e => setScan(+e.target.value));
$('#time-now').onclick = () => setScan(S.timeline.length - 1);

document.addEventListener('keydown', e => {
  if (!$('#viewer').hidden || menuOpen() || document.querySelector('.help-wrap')) return; // they have their own keys
  const typing = e.target.closest('input, textarea, select');
  const ctrl = e.ctrlKey || e.metaKey, k = e.key;
  if (ctrl && k === 'f') { e.preventDefault(); return openSearch(); }
  if (ctrl && k === 'i') { e.preventDefault(); return $('#info-btn').click(); }
  if (ctrl && !e.shiftKey && ['1', '2', '3', '4'].includes(k)) { e.preventDefault(); return setView(VIEWS[+k - 1]); }
  if (ctrl && (k === 'l' || k === 'L')) { e.preventDefault(); return editPath(); }
  if (ctrl && (k === 'h' || k === 'H')) { e.preventDefault(); return toggleHidden(); }
  // tabs: Alt, since browsers keep Ctrl+T/W/Tab for their own tabs
  if (e.altKey && !ctrl && (k === 'n' || k === 'N')) { e.preventDefault(); return newTab(); }
  if (e.altKey && !ctrl && (k === 'w' || k === 'W')) { e.preventDefault(); return closeTab(); }
  if (e.altKey && !ctrl && (k === 'PageDown' || k === 'PageUp') && tabs.length > 1) {
    e.preventDefault(); return switchTab((tabAt + (k === 'PageDown' ? 1 : -1) + tabs.length) % tabs.length);
  }
  if (typing) return;
  if (k === '?' || k === 'F1') { e.preventDefault(); return showHelp(); }
  if (S.mode === 'compare') { if (k === 'Escape') exitCompare(); return; }
  if (e.altKey && e.key === 'ArrowLeft') { e.preventDefault(); return goBack(); }
  if (e.altKey && e.key === 'ArrowRight') { e.preventDefault(); return goFwd(); }
  if (e.altKey && e.key === 'ArrowUp') { e.preventDefault(); return goUp(); }
  if (S.mode !== 'browse') { if (e.key === 'Escape') { S.mode = 'browse'; renderAll(); } return; }
  if (k === 'ContextMenu' || (e.shiftKey && k === 'F10')) {
    e.preventDefault();
    const at = S.anchor && document.querySelector(`#view [data-key="${CSS.escape(S.anchor)}"]`);
    const b = (at || $('#view')).getBoundingClientRect();
    return itemMenu(b.left + Math.min(40, b.width / 2), b.top + Math.min(40, b.height / 2));
  }
  if (k === 'Backspace') { e.preventDefault(); return goBack(); }
  if (ctrl && k === 'a') { e.preventDefault(); return selectAll(); }
  if (k === 'Escape') { S.sel.clear(); paintSelection(); return renderInfo(); }
  const arrows = { ArrowRight: [1, 0], ArrowLeft: [-1, 0], ArrowDown: [0, 1], ArrowUp: [0, -1] };
  if (arrows[k]) { e.preventDefault(); const [dx, dy] = S.view === 'grid' ? arrows[k] : [0, arrows[k][0] + arrows[k][1]]; return move(dx, dy, e); }
  if ((k === 'Enter' || k === ' ') && S.anchor) {
    e.preventDefault();
    const en = S.entries.find(x => x.key === S.anchor);
    if (en && ctrl && en.kind === 'dir') return newTab(en.key);
    if (en) openEntry(en);
  }
});

// a thumbnail the browser can't decode (or whose bytes are gone) falls back to its file icon;
// listened for here because the page's CSP rightly forbids inline handlers
document.addEventListener('error', e => {
  const img = e.target;
  if (img instanceof HTMLImageElement && img.classList.contains('thumb')) {
    img.replaceWith(document.createRange().createContextualFragment(fileIcon(img.dataset.name || '', 56)));
  }
}, true);

try { const so = JSON.parse(localStorage.getItem('bd.sort')); if (so?.key) S.sort = so; } catch {}
setView(S.view);
setupFilters();
boot().then(restoreTabs).then(() => pollDaemon(true));

// tabs survive a reload of the page (not of the browser): reopened lazily, each loads when shown
function restoreTabs() {
  const saved = savedTabs;
  const dirs = (saved?.dirs || []).filter(d => d && S.roots.some(r => atob64(d).startsWith(atob64(r.key))));
  if (dirs.length < 2 || !S.listing) return;
  tabs = dirs.map(dir => ({ ...snap(), dir, listing: null, entries: [], mode: 'browse', back: [], fwd: [], sel: new Set(), anchor: null, cmp: null, scan: null }));
  tabAt = Math.min(saved.at || 0, tabs.length - 1);
  const here = tabs.findIndex(t => t.dir === S.dir);
  if (here >= 0) { tabs[here] = snap(); tabAt = here; renderTabs(); }
  else { tabs[tabAt] = { ...tabs[tabAt] }; Object.assign(S, tabs[tabAt]); showCurrent(); }
}
