// BackDown browser: folders as they were at any scan, with versions, copies and file viewers.
import { fileIcon, folderIcon, driveIcon } from './icons.js';
import { openViewer, openDiff, openGit } from './viewers.js';

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
};

// ---------- boot

async function boot() {
  let data;
  try { data = await api('roots'); } catch (e) { return fatal(e.message); }
  S.roots = data.roots;
  const st = data.status;
  $('#status').textContent = `${count(st.files)} files · ${bytes(st.bytes)} · ${count(st.hashed)} hashed`;
  renderPlaces();
  if (!S.roots.length) {
    $('#view').innerHTML = `<div class="empty"><div>${driveIcon()}<h2>Nothing indexed yet</h2><p>Index a drive first, then reload:<br><code>backdown index /media/my-drive</code></p></div></div>`;
    return;
  }
  const saved = sessionStorage.getItem('bd.dir');
  const start = saved && S.roots.some(r => atob64(saved).startsWith(atob64(r.key))) ? saved : S.roots[0].key;
  await go(start, { push: false });
}

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
  if (S.mode !== 'browse') {
    $('#pathbar').innerHTML = `<span class="crumb" style="cursor:default">${S.mode === 'dupes' ? 'Duplicates' : 'Search'}</span>`;
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
  const val = e => key === 'name' ? e.name : key === 'size' ? e.size : key === 'mtime' ? e.mtime : key === 'copies' ? e.copies : e.versions;
  return [...S.entries].sort((a, b) => (a.kind === b.kind ? 0 : a.kind === 'dir' ? -1 : 1)
    || (key === 'name' ? dir * collator.compare(a.name, b.name) : dir * (val(a) - val(b)) || collator.compare(a.name, b.name)));
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
  if (S.mode === 'search' || S.mode === 'dupes') return; // rendered by their own functions
  const items = sorted();
  if (!items.length) {
    v.innerHTML = `<div class="empty"><div>${folderIcon(72)}<h2>Empty folder</h2><p>${S.scan != null ? 'Nothing was here at this scan.' : 'Nothing indexed in this folder.'}</p></div></div>`;
    return;
  }
  if (S.view === 'grid') {
    v.innerHTML = `<div class="grid" role="listbox" aria-multiselectable="true">${items.map(e => `
      <div class="tile${S.sel.has(e.key) ? ' sel' : ''}${e.later === 'deleted' && S.scan != null ? ' ghost' : ''}" role="option" aria-selected="${S.sel.has(e.key)}" data-key="${e.key}" title="${esc(e.name)}">
        <div class="badges">${badges(e)}</div>
        ${e.kind === 'dir' ? folderIcon(56) : fileIcon(e.name, 56)}
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
  pane.innerHTML = `<div class="hero">${fileIcon(f.name, 64)}<h3>${esc(f.name)}</h3>
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
  pane.querySelectorAll('[data-dir]').forEach(b => b.onclick = () => go(b.dataset.dir, { select: b.dataset.sel, scan: null }));
  pane.querySelectorAll('[data-open-ver]').forEach(b => b.onclick = () => openViewer({ id: +b.dataset.openVer, name: f.name, size: f.versions.find(v => v.id === +b.dataset.openVer).size }, []));
  const cur = f.versions.find(x => x.current);
  pane.querySelectorAll('[data-cmp-ver]').forEach(b => b.onclick = () => openDiff(+b.dataset.cmpVer, cur.id));
}

// ---------- opening things

function openEntry(e) {
  if (e.kind === 'dir') return go(e.key);
  const files = sorted().filter(x => x.kind === 'file');
  openViewer(e, files);
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
  renderPlaces(); renderPath(); $('#banner').hidden = true; $('#info').hidden = true; $('#timebar').hidden = true;
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

// ---------- the daemon: activity, rescans, live refresh

let lastDone = null, pollTimer;
async function pollDaemon() {
  clearTimeout(pollTimer);
  let d;
  try { d = await api('daemon'); } catch { d = { running: false }; }
  const was = S.daemon;
  S.daemon = d.running ? d : null;
  const st = $('#status');
  if (S.daemon) {
    const cur = d.current, q = d.queued.length;
    const name = p => S.roots.find(r => r.path === p)?.name || (p || '').split(/[\\/]/).pop();
    st.innerHTML = cur
      ? `<span class="act"><span class="pulse"></span>${cur.kind === 'index' ? `Scanning ${esc(name(cur.path))}` : 'Hashing'}${cur.progress ? ` · ${esc(cur.progress)}` : ''}</span>${q ? `<span>${q} more queued</span>` : ''}`
      : `<span class="act idle">Up to date</span><span>${d.rescan_every_secs ? `rescans every ${Math.round(d.rescan_every_secs / 3600)} h` : 'rescans on request'}</span>`;
    // a scan finished: fresh sizes, timeline and (unless looking at the past) the open folder
    const done = d.recent.find(j => j.state === 'done');
    if (lastDone !== null && done && done.id !== lastDone && S.mode === 'browse') {
      const data = await api('roots').catch(() => null);
      if (data) S.roots = data.roots;
      if (done.kind === 'index' && S.root && done.path === S.root.path) {
        await loadTimeline();
        if (S.scan == null) await go(S.dir, { push: false, scan: null, select: S.anchor });
      }
    }
    if (done) lastDone = done.id;
    const failed = d.recent.find(j => j.state === 'failed' && j.finished_at > (Date.now() / 1000 - 10));
    if (failed) toastError(new Error(`${failed.kind} ${failed.path || ''}: ${failed.error}`));
  }
  if (!!was !== !!S.daemon || S.daemon) renderPlaces();
  pollTimer = setTimeout(pollDaemon, S.daemon?.current ? 1500 : 4000);
}

async function rescan(rootId) {
  const r = await fetch(`/api/rescan?root=${rootId}`, { method: 'POST', headers: { 'X-BackDown': '1' }, credentials: 'same-origin' });
  const j = await r.json().catch(() => ({}));
  if (!r.ok) return toastError(new Error(j.error || r.statusText));
  pollDaemon();
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
  renderView(); renderInfo();
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

function setView(v) {
  S.view = v; localStorage.setItem('bd.view', v);
  $('#view-grid').setAttribute('aria-pressed', String(v === 'grid'));
  $('#view-list').setAttribute('aria-pressed', String(v === 'list'));
  if (S.mode === 'browse') renderView();
}

document.addEventListener('click', e => {
  const t = e.target;
  const rs = t.closest('[data-rescan]');
  if (rs) { rs.classList.add('spin'); return rescan(rs.dataset.rescan); }
  const place = t.closest('.place[data-key]');
  if (place) { $('#timebar').hidden = S.timeline.length < 2; return go(place.dataset.key, { scan: null }); }
  const crumb = t.closest('.crumb[data-key]');
  if (crumb) return go(crumb.dataset.key);
  if (t.closest('#banner-now')) return setScan(S.timeline.length - 1);
  if (t.closest('#banner-git')) return openGit(S.dir, S.listing.name);
  const th = t.closest('th[data-sort]');
  if (th) { const k = th.dataset.sort; S.sort = { key: k, asc: S.sort.key === k ? !S.sort.asc : k === 'name' }; return renderView(); }
  const hit = t.closest('[data-hit-dir]');
  if (hit) return go(hit.dataset.hitDir, { select: hit.dataset.hitSel || null, scan: null });
  const item = t.closest('#view [data-key]');
  if (item) return select(item.dataset.key, e);
  if (t.closest('#view') && S.mode === 'browse') { S.sel.clear(); renderView(); renderInfo(); }
});
document.addEventListener('dblclick', e => {
  const item = e.target.closest('#view [data-key]');
  if (item) { const en = S.entries.find(x => x.key === item.dataset.key); if (en) openEntry(en); }
});
$('#back').onclick = goBack; $('#fwd').onclick = goFwd;
$('#view-grid').onclick = () => setView('grid');
$('#view-list').onclick = () => setView('list');
$('#info-btn').onclick = () => { S.info = !S.info; localStorage.setItem('bd.info', S.info ? '1' : '0'); renderInfo(); };
$('#search-btn').onclick = () => $('#searchbox').hidden ? openSearch() : closeSearch();
$('#q').addEventListener('input', e => { clearTimeout(searchTimer); searchTimer = setTimeout(() => runSearch(e.target.value), 220); });
$('#q').addEventListener('keydown', e => { if (e.key === 'Escape') { closeSearch(); $('#view').focus(); } });
$('#dupes-btn').onclick = showDupes;
$('#time').addEventListener('input', e => setScan(+e.target.value));
$('#time-now').onclick = () => setScan(S.timeline.length - 1);

document.addEventListener('keydown', e => {
  if (!$('#viewer').hidden) return; // the viewer has its own keys
  const typing = e.target.closest('input, textarea');
  if ((e.ctrlKey || e.metaKey) && e.key === 'f') { e.preventDefault(); return openSearch(); }
  if ((e.ctrlKey || e.metaKey) && e.key === 'i') { e.preventDefault(); return $('#info-btn').click(); }
  if ((e.ctrlKey || e.metaKey) && e.key === '1') { e.preventDefault(); return setView('grid'); }
  if ((e.ctrlKey || e.metaKey) && e.key === '2') { e.preventDefault(); return setView('list'); }
  if (typing) return;
  if (e.altKey && e.key === 'ArrowLeft') { e.preventDefault(); return goBack(); }
  if (e.altKey && e.key === 'ArrowRight') { e.preventDefault(); return goFwd(); }
  if (e.altKey && e.key === 'ArrowUp') { e.preventDefault(); return goUp(); }
  if (S.mode !== 'browse') { if (e.key === 'Escape') { S.mode = 'browse'; renderAll(); } return; }
  const k = e.key;
  if (k === 'Backspace') { e.preventDefault(); return goBack(); }
  if ((e.ctrlKey || e.metaKey) && k === 'a') { e.preventDefault(); S.sel = new Set(S.entries.map(x => x.key)); renderView(); return renderInfo(); }
  if (k === 'Escape') { S.sel.clear(); renderView(); return renderInfo(); }
  const arrows = { ArrowRight: [1, 0], ArrowLeft: [-1, 0], ArrowDown: [0, 1], ArrowUp: [0, -1] };
  if (arrows[k]) { e.preventDefault(); const [dx, dy] = S.view === 'grid' ? arrows[k] : [0, arrows[k][0] + arrows[k][1]]; return move(dx, dy, e); }
  if ((k === 'Enter' || k === ' ') && S.anchor) {
    e.preventDefault();
    const en = S.entries.find(x => x.key === S.anchor);
    if (en) openEntry(en);
  }
});

setView(S.view);
boot().then(pollDaemon);
