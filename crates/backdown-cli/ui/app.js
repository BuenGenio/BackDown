// BackDown browser: folders as they were at any scan, with versions, copies and file viewers.
import { fileIcon, folderIcon, driveIcon, kindOf } from './icons.js';
import { openViewer, openDiff, openGit, openMaildir, previewInto } from './viewers.js';
import { squarify, compareListings, menu, closeMenu, menuOpen, toast } from './extras.js';
import { mountFind, find, foundHtml, setScope, setEarlier, more as findMore } from './search.js';
import { showSettings, proGate, loadFeatures, checkUpdates } from './settings.js';
import { tidyCard, wireTidy, mergeButton, mergeSources } from './tidy.js';
import { mountHealth, daemonChanged, healthOfDisk, healthOfPath, pill, dot, mirrorNote, healthHead, notesSection } from './health.js';
import { mountMachines, renderMachinesNav, machinesDaemon, machinePath, showMachine, remoteCopiesHtml, dupesRemoteHtml, acrossHtml, wipeRemoteLine } from './machines.js';
import { mountCell, mountNotice, loadMounts, allowButton, browseFoundBtn, filesBody } from './drives.js';

const $ = s => document.querySelector(s);
// names are shown composed (NFC), whichever form the drive stores them in (HFS+ decomposes them)
const esc = s => String(s ?? '').normalize('NFC').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
export const bytes = n => n >= 1e12 ? (n / 1e12).toFixed(2) + ' TB' : n >= 1e9 ? (n / 1e9).toFixed(1) + ' GB' : n >= 1e6 ? (n / 1e6).toFixed(1) + ' MB' : n >= 1e3 ? Math.round(n / 1e3) + ' KB' : n + ' B';
const count = n => n >= 1e6 ? (n / 1e6).toFixed(2) + ' M' : n >= 1e3 ? (n / 1e3).toFixed(1) + ' K' : String(n);
export const when = t => t ? new Date(t * 1000).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' }) : '—';
const day = t => t ? new Date(t * 1000).toLocaleDateString(undefined, { dateStyle: 'medium' }) : '—';
const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' });

// ---------- feedback while the server works: every request (from any module) that takes more than a
// moment shows a bar along the top, and a note if it's still going after a few seconds. Not the
// Activity view's long-poll, which waits on purpose.
const BUSY = { n: 0, shown: 0, slow: 0 };
const waitsOnPurpose = url => /^\/api\/daemon\?/.test(url) && /[?&]wait=/.test(url);
const plainFetch = window.fetch.bind(window);
window.fetch = (input, init) => {
  const url = typeof input === 'string' ? input : input?.url || '';
  if (!url.startsWith('/api/') || waitsOnPurpose(url)) return plainFetch(input, init);
  busy(1);
  return plainFetch(input, init).finally(() => busy(-1));
};
function busy(d) {
  BUSY.n = Math.max(0, BUSY.n + d);
  const bar = document.getElementById('busy');
  if (!bar) return;
  if (BUSY.n > 0) {
    BUSY.shown ||= setTimeout(() => { bar.hidden = false; document.documentElement.classList.add('is-busy'); }, 200);
    BUSY.slow ||= setTimeout(() => { bar.dataset.slow = ''; }, 4000);
    return;
  }
  clearTimeout(BUSY.shown); clearTimeout(BUSY.slow); BUSY.shown = BUSY.slow = 0;
  bar.hidden = true; delete bar.dataset.slow; document.documentElement.classList.remove('is-busy');
}

export async function api(path, params = {}) {
  const q = new URLSearchParams(Object.entries(params).filter(([, v]) => v !== undefined && v !== null && v !== ''));
  const r = await fetch(`/api/${path}?${q}`, { credentials: 'same-origin' });
  const j = await r.json().catch(() => ({ error: r.statusText }));
  if (!r.ok || j.error) { const e = new Error(j.error || r.statusText); e.refusal = j.needs ? j : null; throw e; }
  return j;
}

// a view of its own (Activity, Settings, …): the sidebar and header follow
export function enterView(mode) {
  S.mode = mode;
  closeSearch(false);
  renderTabs(); renderPlaces(); renderPath();
  $('#banner').hidden = true; $('#info').hidden = true; $('#timebar').hidden = true;
}
export const daemonState = () => S.daemon;

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
  showSummary(data.status);
  renderPlaces();
  if (!S.roots.length) return showActivity();
  const saved = sessionStorage.getItem('bd.dir');
  const start = saved && S.roots.some(r => atob64(saved).startsWith(atob64(r.key))) ? saved : S.roots[0].key;
  await go(start, { push: false });
}

const summary = st => `${count(st.files)} files · ${bytes(st.bytes)} · ${count(st.hashed)} hashed`;
// the index's counts: at once when the server has them, else asked for separately, since on a
// big index they take a full read of it, and the drives shouldn't wait for that
function showSummary(st) {
  const el = $('#status-files');
  if (st) { el.textContent = summary(st); return; }
  if (!el.textContent) el.textContent = 'Counting files…';
  api('index-status').then(s => { el.textContent = summary(s); }).catch(() => {});
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
        <span class="ps">${dot(healthOfPath(r.path))}${bytes(r.bytes)} · ${count(r.files)} files</span>
      </button>
      ${S.daemon?.running ? `<button class="rescan${busy.has(r.path) ? ' spin' : ''}" type="button" data-rescan="${r.id}" title="Rescan ${esc(r.name)} now" aria-label="Rescan ${esc(r.name)}"><svg viewBox="0 0 24 24"><path d="M20 11a8 8 0 1 0-2.3 5.7"/><path d="M20 5v6h-6"/></svg></button>` : ''}
    </div>`).join('');
  $('#dupes-btn').setAttribute('aria-current', String(S.mode === 'dupes'));
  $('#activity-btn').setAttribute('aria-current', String(S.mode === 'activity'));
  $('#entities-btn').setAttribute('aria-current', String(S.mode === 'entities'));
  $('#plan-btn').setAttribute('aria-current', String(S.mode === 'plan'));
  $('#wipe-btn').setAttribute('aria-current', String(S.mode === 'wipe'));
  $('#rescue-btn').setAttribute('aria-current', String(S.mode === 'rescue'));
  renderMachinesNav(); // other machines and their drives (machines.js)
  $('#settings-btn').setAttribute('aria-current', String(S.mode === 'settings'));
}

// ---------- navigation

async function go(dirKey, { push = true, select = null, scan = S.scan } = {}) {
  const root = S.roots.find(r => atob64(dirKey).startsWith(atob64(r.key)));
  if (!root) return;
  const rootChanged = S.root?.id !== root.id;
  if (rootChanged && scan === S.scan) scan = null; // the old drive's scan; one asked for (a search hit's) stays
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
  if (S.mode === 'machine') { $('#pathbar').innerHTML = machinePath(); return; }
  if (S.mode !== 'browse') {
    $('#pathbar').innerHTML = `<span class="crumb" style="cursor:default">${{ dupes: 'Duplicates', activity: 'Activity', entities: 'What’s here', plan: 'Plan', wipe: 'Free up', rescue: 'Rescue', settings: 'Settings' }[S.mode] || 'Search'}</span>`;
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
    const mail = !repo && maildirHere();
    b.hidden = !repo && !mail;
    if (repo) { b.className = 'banner'; b.innerHTML = `This folder is a git repository. <button class="btn sm" type="button" id="banner-git">View history</button>`; }
    // a Maildir: its mail as threaded conversations, rather than folders of numbered files
    else if (mail) {
      b.className = 'banner';
      b.innerHTML = `${mail.inside ? `These files are the messages of the mailbox <b>${esc(mail.name)}</b> (Maildir).` : 'This folder is a mailbox (Maildir).'}
        <button class="btn sm" type="button" id="banner-mail">Open as mailbox</button>`;
    }
    return;
  }
  const gone = S.entries.filter(e => e.later === 'deleted').length, changed = S.entries.filter(e => e.later === 'changed').length;
  b.className = 'banner'; b.hidden = false;
  b.innerHTML = `Showing <b>${esc(S.root.name)}</b> as it was on <b>${when(L.scanned_at)}</b>.
    ${gone ? `${gone} item${gone === 1 ? ' has' : 's have'} since been deleted (struck through).` : ''}
    ${changed ? `${changed} changed later.` : ''}
    <button class="btn sm" type="button" id="banner-now">Back to now</button>`;
}

// the Maildir shown here: this folder when it holds cur/ and new/, or the one whose cur/, new/ or
// tmp/ this is (its files named like "1698765432.M5P6Q7.host:2,S")
const MAILDIR_FILE = /^\d{9,11}\.[^/]+$/;
function maildirHere() {
  const dirs = new Set(S.entries.filter(e => e.kind === 'dir').map(e => e.name));
  if (dirs.has('cur') && dirs.has('new')) return { key: S.dir, name: S.listing.name, inside: false };
  const up = S.listing.crumbs.at(-2);
  if (up && ['cur', 'new', 'tmp'].includes(S.listing.name) && S.entries.some(e => e.kind === 'file' && MAILDIR_FILE.test(e.name))) {
    return { key: up.key, name: S.listing.crumbs.length === 2 ? S.root.name : up.name, inside: true };
  }
  return null;
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
      <button class="btn" type="button" id="git-dir" hidden>Git history</button>
      <button class="btn" type="button" id="mail-dir" hidden>Open as mailbox</button>`;
    $('#open-dir').onclick = () => go(e.key);
    // a repository: offer its history when the folder holds a .git
    api('ls', { dir: e.key, scan: S.scan ?? '' }).then(l => {
      if (l.entries.some(x => x.kind === 'dir' && x.name === '.git')) {
        const b = $('#git-dir'); if (!b) return;
        b.hidden = false; b.onclick = () => openGit(e.key, e.name);
      }
      // a Maildir (cur/ and new/ inside): open its mail as conversations
      const sub = new Set(l.entries.filter(x => x.kind === 'dir').map(x => x.name));
      if (sub.has('cur') && sub.has('new')) {
        const b = $('#mail-dir'); if (!b) return;
        b.hidden = false; b.onclick = () => openMaildir(e.key, e.name);
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
      ${f.streams ? `<dt>Alternate data streams</dt><dd>${f.streams.count} (${bytes(f.streams.bytes)}): ${f.streams.names.map(([n, b]) =>
        `${esc(n)} <span class="note">${bytes(b)}${/^(zone\.identifier|smartscreen)$/i.test(n) ? ', a “downloaded from the internet” mark' : ''}</span>`).join(', ')}</dd>` : ''}
    </dl>
    <h4>Other copies (${f.copies.length})</h4>
    ${f.copies.length ? f.copies.map(c => `<button class="rowlink" type="button" data-dir="${c.dir_key}" data-sel="${c.key}">${esc(c.path)}</button>`).join('') : '<p class="note">No identical copy anywhere else in the index.</p>'}
    ${remoteCopiesHtml(f)}
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
    { label: 'Free up: wipe what has a copy…', act: () => wipeThese([one]) },
    { sep: 1 },
    { label: 'Copy path', act: () => copyText(pathOf(one)) },
    { label: 'Details', hint: 'Ctrl+I', act: showDetails },
  ];
  else if (one) items = [
    { label: 'Open', hint: 'Enter', act: () => openEntry(one) },
    { label: 'Open with system app', act: () => openDesktop({ id: one.id }), disabled: !one.id },
    { label: 'Show in file manager', act: () => openDesktop({ id: one.id, reveal: 1 }), disabled: !one.id },
    { label: 'Download a copy', act: () => download(one), disabled: !one.id },
    { label: 'Free up: wipe if it has a copy…', act: () => wipeThese([one]) },
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
      { label: `Free up: wipe what has a copy (${ents.length})…`, act: () => wipeThese(ents) },
      { sep: 1 },
      { label: `Details (${ents.length} selected)`, hint: 'Ctrl+I', act: showDetails },
    ];
  }
  menu(x, y, items);
}

const wipeThese = ents => showWipe(ents.map(e => ({ key: e.key, path: pathOf(e), label: e.name })));

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
    { label: 'Free up this folder…', act: () => showWipe([{ key: S.dir, path: S.listing.path, label: S.listing.crumbs.length > 1 ? S.listing.name : S.root.name }]) },
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
  if (t.mode === 'plan') return 'Plan';
  if (t.mode === 'wipe') return 'Free up';
  if (t.mode === 'rescue') return 'Rescue';
  if (t.mode === 'machine') return 'Machines';
  if (t.mode === 'settings') return 'Settings';
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
  if (S.mode === 'plan') return showPlan();
  if (S.mode === 'wipe') return showWipe();
  if (S.mode === 'rescue') return showRescue();
  if (S.mode === 'machine') return showMachine();
  if (S.mode === 'settings') return showSettings();
  if (S.mode === 'compare' && S.cmp) return renderCompare();
  S.mode = 'browse';
  if (!S.listing) return S.dir ? go(S.dir, { push: false, scan: null }) : showActivity();
  $('#timebar').hidden = S.timeline.length < 2;
  renderAll();
}

// ---------- keyboard shortcuts help

const HELP = [
  ['Moving around', [[['Enter'], 'Open'], [['Alt+↑'], 'Parent folder'], [['Alt+←', 'Backspace'], 'Back'], [['Alt+→'], 'Forward'], [['Ctrl+L'], 'Type a location'], [['Ctrl+F'], 'Search'], [['Ctrl+Shift+F'], 'Search every drive']]],
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
// this folder or every drive, ranked (search.js, find.rs)
async function runSearch(q) {
  if (!q.trim()) { S.mode = 'browse'; renderAll(); return; }
  let res;
  try { res = await find(q, S.dir); } catch (e) { return toastError(e); }
  if (!res || $('#searchbox').hidden || $('#q').value !== q) return; // a newer search, or none any more
  const keep = S.mode === 'search' ? $('#view').scrollTop : 0;
  S.mode = 'search';
  renderPath(); $('#banner').hidden = true;
  $('#view').innerHTML = foundHtml(res, q);
  $('#view').scrollTop = keep;
}
const rerunSearch = () => { if (!$('#searchbox').hidden && $('#q').value.trim()) runSearch($('#q').value); };

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
      <td>${g.paths.map(p => `<button class="rowlink" type="button" data-dupe="${esc(p)}">${esc(p)}</button>`).join('')}${dupesRemoteHtml(g)}</td></tr>`).join('')}</tbody></table>${acrossHtml(d)}`
    : d.across?.files ? `<div class="banner" style="margin:-14px -14px 14px">No duplicates on this computer ${dupesMin === '1' ? 'yet' : 'this size'}. ${picker}</div>${acrossHtml(d)}`
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
    ${e.provenance.length ? `<ul class="ent-prov">${e.provenance.map(p => `<li>${esc(p)}</li>`).join('')}</ul>` : e.provenance_locked ? '<p class="ent-junk">Where it came from is part of BackDown Pro. <button class="rowlink" type="button" data-set-go="licence">Add a licence key</button></p>' : ''}
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

// ---------- Plan: file the old drives into one archive (propose, read, dry-run, apply)

const PL = { id: null, data: null, list: [], dry: null, fresh: false, to: '', ents: null, junk: false };

async function showPlan(id = PL.id) {
  S.mode = 'plan';
  closeSearch(false);
  renderTabs(); renderPlaces(); renderPath();
  $('#banner').hidden = true; $('#info').hidden = true; $('#timebar').hidden = true;
  if (proGate('plan')) return;
  $('#view').innerHTML = '<div class="empty"><p>Loading…</p></div>';
  PL.list = await api('plans').catch(() => []);
  // what's regenerable on each drive (caches, previews, build output), for "leave them out"
  if (!PL.ents) api('entities').then(d => { PL.ents = d; if (S.mode === 'plan') junkSize(); }).catch(() => {});
  if (id == null && PL.list.length) id = PL.list[0].id;
  PL.id = id; PL.dry = null;
  PL.data = id != null ? await api('plan', { id }).catch(() => null) : null;
  renderPlan();
}

function renderPlan() {
  const v = $('#view');
  if (S.mode !== 'plan') return;
  const guess = S.roots.length ? `${S.roots.at(-1).path.replace(/[\\/]+$/, '')}/Archive` : '';
  const form = `<section class="pl-new card-ish"${PL.data && !PL.fresh ? ' hidden' : ''} id="pl-form">
      <h3>New plan</h3>
      <label class="pl-to">Archive folder, on the drive that will keep everything
        <input id="pl-to" list="pl-roots" spellcheck="false" placeholder="${esc(guess)}" value="${esc(PL.to)}"></label>
      <datalist id="pl-roots">${S.roots.map(r => `<option value="${esc(r.path.replace(/[\\/]+$/, ''))}/Archive">`).join('')}</datalist>
      <div class="pl-from"><span class="muted">File these drives:</span>${S.roots.map(r => `<label class="chk"><input type="checkbox" class="pl-src" value="${r.id}" checked> ${esc(r.name)}</label>`).join('')}</div>
      <label class="chk pl-junk"><input type="checkbox" id="pl-junk"${PL.junk ? ' checked' : ''}> Leave out caches, previews and build output <span class="muted" id="pl-junk-size"></span></label>
      <p class="note">The archive's own drive is never a source. A plan only proposes: nothing is copied until you apply it, and nothing is ever deleted. Caches and build output can be made again by the programs that made them; left out, the drive they're on can't be freed entirely, because they have no copy.</p>
      <button class="btn primary" type="button" id="pl-make">Make plan</button></section>`;
  const picker = PL.list.length > 1 ? `<select id="pl-pick" aria-label="Plans">${PL.list.map(p => `<option value="${p.id}"${p.id === PL.id ? ' selected' : ''}>Plan ${p.id} · ${esc(day(p.created_at))} · ${esc(p.state)}</option>`).join('')}</select>` : '';
  if (!PL.data) {
    v.innerHTML = `<div class="plan">${form}<div class="empty"><div><h2>No plan yet</h2><p>A plan proposes where everything on your old drives goes in one archive: mail with mail, phones with phones, each thing as a unit, with the reason for every placement.</p></div></div>${tidyCard(guess)}</div>`;
  } else {
    const { plan: p, state, journal } = PL.data;
    const j = Object.fromEntries(journal.map(([s, n, b]) => [s, { n, b }]));
    const groups = [['copy', 'Copy'], ['copy-unique', 'Copy only what’s unique'], ['merge', 'Merge snapshots of a mailbox'], ['skip', 'Not copied']];
    v.innerHTML = `<div class="plan">
      <div class="pl-head">
        <div><h2>Plan ${PL.id}</h2><p class="muted">into <code>${esc(p.target)}</code> · made ${esc(when(p.created_at))}${/caches left out/.test(p.schema || '') ? ' · caches left out' : ''} · <span class="pill ${state === 'done' ? 'ok' : state === 'partial' ? 'warn' : ''}">${esc(state)}</span></p></div>
        <div class="pl-acts">${picker}<button class="btn" type="button" id="pl-newbtn">New plan</button>
          <button class="btn" type="button" id="pl-dry">Dry run</button>
          <button class="btn primary" type="button" id="pl-go"${S.daemon ? '' : ' disabled title="Copying runs in the BackDown service: backdown apply ' + PL.id + ' --go"'}>Apply: copy ${bytes(p.copy_bytes)}</button></div>
      </div>
      ${form}
      ${PL.dry ? `<div class="pl-dry banner">${PL.dry.error ? esc(PL.dry.error) : `Dry run: ${count(PL.dry.would_copy)} files (${bytes(PL.dry.would_copy_bytes)}) would be copied${PL.dry.would_merge ? `; ${count(PL.dry.would_merge)} mailbox${PL.dry.would_merge === 1 ? '' : 'es'} would be merged from ${PL.dry.would_merge === 1 ? 'its' : 'their'} snapshots, message by message` : ''}${PL.dry.resumed ? `; ${count(PL.dry.resumed)} are done already` : ''}${PL.dry.duplicates ? `; ${count(PL.dry.duplicates)} are already in the archive` : ''}${PL.dry.conflicts ? `; ${count(PL.dry.conflicts)} destinations exist and will be compared, never overwritten` : ''}. Nothing was written.`}</div>` : ''}
      ${journal.length ? `<div class="pl-journal"><b>Done so far:</b> ${j.copied ? `${count(j.copied.n)} files copied and verified (${bytes(j.copied.b)})` : j.merged ? '' : 'nothing copied yet'}${j.merged ? `${j.copied ? ' · ' : ''}${count(j.merged.n)} mailbox${j.merged.n === 1 ? '' : 'es'} merged (${bytes(j.merged.b)})` : ''}${j['already-there'] ? ` · ${count(j['already-there'].n)} already there` : ''}${j.duplicate ? ` · ${count(j.duplicate.n)} already in the archive` : ''}${j.conflict ? ` · <span class="warn-t">${count(j.conflict.n)} conflicts</span>` : ''}${j.failed ? ` · <span class="warn-t">${count(j.failed.n)} failed</span>` : ''}</div>` : ''}
      <h3>Drives</h3>
      <table class="list pl-drives"><thead><tr><th>Drive</th><th class="n">Size</th><th class="n">To copy</th><th class="n">Already archived</th><th>After applying</th></tr></thead><tbody>
      ${p.drives.map(d => `<tr><td><b>${esc(d.drive)}</b><small class="muted">${esc(d.path)}</small></td><td class="n">${bytes(d.bytes)}</td><td class="n">${bytes(d.copy)}</td><td class="n">${bytes(d.already)}</td>
        <td>${d.covered ? '<span class="pill ok">every file will have a copy</span>' : `<span class="pill warn">${bytes(d.not_copied)} stays only here</span>`}</td></tr>`).join('')}</tbody></table>
      ${groups.map(([a, label]) => { const items = p.items.filter(i => i.action === a); return items.length ? `<h3>${label} <span class="muted">(${items.length})</span></h3><div class="pl-items">${items.map(i => `
        <div class="pl-item ${a}"><div class="pl-what"><b>${esc(i.what)}</b><span class="pl-size">${bytes(i.bytes)}</span></div>
          <button class="rowlink pl-src-path" type="button" data-ent-open="${i.source_key}" data-ent-file="${i.is_file ? 1 : ''}">${esc(i.source)}</button>
          ${mergeSources(i)}
          ${a !== 'skip' ? `<div class="pl-dest">→ <code>${esc(i.dest)}</code></div>` : ''}
          <div class="pl-why">${esc(i.why)}</div>${mergeButton(PL.data, p.items.indexOf(i))}</div>`).join('')}</div>` : ''; }).join('')}
      ${tidyCard(p.target)}
    </div>`;
  }
  wireTidy({ planId: PL.id, reload: () => showPlan(PL.id), error: toastError, poll: () => { if (!polling) pollDaemon(true); } });
  $('#pl-make')?.addEventListener('click', makePlan);
  $('#pl-junk')?.addEventListener('change', e => { PL.junk = e.target.checked; });
  document.querySelectorAll('.pl-src').forEach(c => c.addEventListener('change', junkSize));
  junkSize();
  $('#pl-newbtn')?.addEventListener('click', () => { const f = $('#pl-form'); f.hidden = !f.hidden; if (!f.hidden) $('#pl-to').focus(); });
  $('#pl-pick')?.addEventListener('change', e => showPlan(+e.target.value));
  $('#pl-dry')?.addEventListener('click', async () => {
    $('#pl-dry').disabled = true;
    try { PL.dry = await api('plan-dry', { id: PL.id }); } catch (e) { PL.dry = { error: e.message }; }
    renderPlan();
  });
  $('#pl-go')?.addEventListener('click', async () => {
    const p = PL.data.plan;
    if (!confirm(`Copy ${bytes(p.copy_bytes)} (${count(p.copy_files)} files) into ${p.target}?\n\nEvery copy is read back and verified. Nothing is overwritten or deleted.`)) return;
    const r = await fetch(`/api/apply?id=${PL.id}`, { method: 'POST', headers: { 'X-BackDown': '1' }, credentials: 'same-origin' });
    const j = await r.json().catch(() => ({}));
    if (!r.ok || j.error) return toastError(new Error(j.error || r.statusText));
    toast('Copying: follow it in Activity');
    if (!polling) pollDaemon(true);
  });
}

// how much the ticked drives hold in regenerable things (as recognised)
function junkSize() {
  const el = $('#pl-junk-size');
  if (!el || !PL.ents) return;
  const ids = new Set([...document.querySelectorAll('.pl-src:checked')].map(c => +c.value));
  let n = 0;
  for (const r of PL.ents.roots || []) {
    const root = S.roots.find(x => x.path === r.root);
    if (!root || !ids.has(root.id)) continue;
    n += r.junk.reduce((a, j) => a + j.bytes, 0) + r.entities.reduce((a, e) => a + e.junk.reduce((b, j) => b + j.bytes, 0), 0);
  }
  el.textContent = n ? `(${bytes(n)} on these drives)` : '';
}

async function makePlan() {
  const to = $('#pl-to').value.trim() || $('#pl-to').placeholder;
  const from = [...document.querySelectorAll('.pl-src:checked')].map(c => c.value).join(',');
  PL.to = to;
  $('#pl-make').disabled = true;
  const r = await fetch(`/api/plan?${new URLSearchParams({ to, from, skip_junk: PL.junk ? 1 : '' })}`, { method: 'POST', headers: { 'X-BackDown': '1' }, credentials: 'same-origin' });
  const j = await r.json().catch(() => ({}));
  if (!r.ok || j.error) { $('#pl-make').disabled = false; return toastError(new Error(j.error || r.statusText)); }
  PL.fresh = false;
  showPlan(j.id);
}

// ---------- Free up: wipe what has a verified copy elsewhere, move or keep what exists only here
//
// The tree picks what to wipe (drives, folders, recognised things); the server says, from the index,
// how much of it has an identical copy outside the selection and where. The daemon does the wiping:
// it re-reads each file and its copy before deleting, and journals every deletion.

const WP = { sel: new Map(), open: new Set(), lists: new Map(), ents: null, cov: null, a: null, err: null, seq: 0,
  checking: false, busy: false, only: 'move', to: '', room: null, trust: false, typed: '', history: [], targets: [] };
const WIPE_MAX = 200; // items in one selection: more is a folder's worth, so tick the folder
const isFull = p => /^([\\/]|[A-Za-z]:[\\/])/.test(p);
// `p` is inside the folder `d` (not `d` itself)
const under = (p, d) => { const b = d.replace(/[\\/]+$/, ''); return p.length > b.length && p.startsWith(b) && /[\\/]/.test(p[b.length]); };

function showWipe(pre = null) {
  if (pre?.length > WIPE_MAX) return toastError(new Error(`Pick at most ${WIPE_MAX} items at a time, or the folder that holds them`));
  S.mode = 'wipe';
  closeSearch(false);
  renderTabs(); renderPlaces(); renderPath();
  $('#banner').hidden = true; $('#info').hidden = true; $('#timebar').hidden = true;
  if (proGate('wipe')) return;
  if (pre) {
    WP.sel = new Map();
    for (const p of pre) if (!pre.some(o => under(p.path, o.path))) WP.sel.set(p.key, p);
    WP.typed = '';
    // show where the picked things are
    for (const p of WP.sel.values()) for (const r of S.roots) if (under(p.path, r.path)) WP.open.add(r.key);
  }
  WP.a = null; WP.err = null;
  layoutWipe();
  // what's recognised on each drive, and how much of each drive exists elsewhere, fill in as they come
  api('entities').then(d => { WP.ents = d; renderWipePick(); }).catch(() => {});
  api('overlaps').then(o => { WP.cov = new Map(o.map(x => [x.unit.key + ':' + x.unit.level, x])); renderWipePick(); }).catch(() => {});
  api('plans').then(p => { WP.targets = [...new Set(p.map(x => x.target))]; renderWipeTargets(); }).catch(() => {});
  loadWipeHistory();
  assessWipe();
  if (WP.to) checkRoom();
}

function layoutWipe() {
  $('#view').innerHTML = `<div class="plan wipe">
    <div class="pl-head"><div><h2>Free up drives</h2>
      <p class="muted">Tick what to wipe. A file is deleted only when an identical copy exists outside what you ticked, and both still read the same. What exists only here is moved to a folder you choose first, or left where it is.</p></div></div>
    <div class="wp-cols">
      <section class="card-ish wp-pick"><h3>What to wipe</h3><div id="wp-tree" role="tree" aria-label="Drives, folders and recognised things"></div></section>
      <div class="wp-side">
        <section class="card-ish" id="wp-assess" aria-live="polite"></section>
        <section class="card-ish wp-opts">
          <h3>What exists only here</h3>
          <label class="wp-chk"><input type="radio" name="wp-only" value="move"${WP.only === 'move' ? ' checked' : ''}> Move it first, to</label>
          <input id="wp-to" class="wp-input" list="wp-targets" spellcheck="false" autocomplete="off" placeholder="a folder on another drive" value="${esc(WP.to)}" aria-label="Folder to move what exists only here to">
          <datalist id="wp-targets"></datalist>
          <small id="wp-room" class="muted"></small>
          <label class="wp-chk"><input type="radio" name="wp-only" value="leave"${WP.only === 'leave' ? ' checked' : ''}> Leave it where it is</label>
          <h3>Before deleting</h3>
          <label class="wp-chk"><input type="checkbox" id="wp-trust"${WP.trust ? ' checked' : ''}> Trust the index: don't re-read each file and its copy</label>
          <small class="muted">Faster. Copies must still be there as last scanned, but a file that quietly went bad since its last hash wouldn’t be noticed.</small>
          <label class="wp-confirm"><span>Type <b id="wp-names">…</b> to confirm</span>
            <input id="wp-typed" class="wp-input" spellcheck="false" autocomplete="off" value="${esc(WP.typed)}"></label>
          <button class="btn primary" type="button" id="wp-go" disabled>Wipe</button>
          <p class="note" id="wp-why"></p>
        </section>
      </div>
    </div>
    <section id="wp-hist"></section>
  </div>`;
  const root = $('#view .wipe');
  root.addEventListener('change', onWipeChange);
  root.addEventListener('input', onWipeInput);
  root.addEventListener('click', onWipeClick);
  renderWipePick(); renderWipeTargets(); renderWipeAssess(); renderWipeHist();
}

function renderWipePick(focus = null) {
  const el = $('#wp-tree');
  if (S.mode !== 'wipe' || !el) return;
  el.innerHTML = S.roots.length ? S.roots.map(r => {
    const cov = WP.cov?.get(`${r.key}:drive`);
    const share = cov && cov.unit.bytes ? cov.elsewhere / cov.unit.bytes : null;
    const ents = WP.ents?.roots?.find(x => x.root === r.path)?.entities || [];
    const open = WP.open.has(r.key);
    return `<div class="wp-drive">${pickRow({ key: r.key, path: r.path, label: r.name, bytes: r.bytes, dir: true, drive: true,
      extra: share != null ? `<span class="pill ${share >= 0.999 ? 'ok' : ''}" title="Of this drive’s bytes, how much has an identical copy on another drive">${Math.floor(share * 100)}% exists elsewhere</span>` : '' }, 0)}
      ${open ? `<div role="group">${ents.length ? `<div class="wp-group">Recognised</div>${ents.map(e => pickRow({ key: e.key, path: e.path, label: e.title, kind: e.label, bytes: e.bytes }, 1)).join('')}<div class="wp-group">Folders and files</div>` : ''}${kids(r.key, 1)}</div>` : ''}</div>`;
  }).join('') : '<p class="muted pad">Nothing indexed yet.</p>';
  if (focus) el.querySelector(focus)?.focus();
}

function pickRow(n, depth) {
  const covered = [...WP.sel.values()].find(s => under(n.path, s.path));
  const on = WP.sel.has(n.key) || !!covered;
  const open = WP.open.has(n.key);
  return `<div class="wp-row${n.drive ? ' drive' : ''}${on ? ' on' : ''}" role="treeitem"${n.dir ? ` aria-expanded="${open}"` : ''} style="--d:${depth}">
    ${n.dir ? `<button class="wp-tw" type="button" data-wp-tw="${n.key}" aria-expanded="${open}" aria-label="${open ? 'Collapse' : 'Expand'} ${esc(n.label)}"><svg viewBox="0 0 24 24"><path d="m9 6 6 6-6 6"/></svg></button>` : '<span class="wp-tw"></span>'}
    <label class="wp-lab" title="${esc(covered ? `Included in ${covered.label}` : n.path)}"><input type="checkbox" data-wp-key="${n.key}" data-wp-path="${esc(n.path)}" data-wp-label="${esc(n.label)}"${on ? ' checked' : ''}${covered ? ' disabled' : ''}>
      ${n.drive ? driveIcon() : n.kind ? '' : n.dir ? folderIcon(18) : fileIcon(n.label, 18)}${n.kind ? `<span class="wp-kind">${esc(n.kind)}</span>` : ''}<span class="wp-name">${esc(n.label)}</span></label>
    <span class="wp-meta">${n.extra || ''}${n.copies ? `<span class="pill ok" title="Identical copies elsewhere">${n.copies} cop${n.copies === 1 ? 'y' : 'ies'}</span>` : ''}<span class="wp-size">${bytes(n.bytes || 0)}</span></span>
  </div>`;
}

// a folder's contents in the tree, biggest first (loaded when it's first opened)
function kids(key, depth) {
  let l = WP.lists.get(key);
  if (!l) {
    WP.lists.set(key, l = { loading: true });
    api('ls', { dir: key }).then(x => WP.lists.set(key, x)).catch(e => WP.lists.set(key, { error: e.message })).finally(() => renderWipePick());
  }
  if (l.loading) return `<div class="wp-row muted" style="--d:${depth}"><span class="wp-tw"></span>Loading…</div>`;
  if (l.error) return `<div class="wp-row muted" style="--d:${depth}"><span class="wp-tw"></span>${esc(l.error)}</div>`;
  if (!l.entries.length) return `<div class="wp-row muted" style="--d:${depth}"><span class="wp-tw"></span>Empty</div>`;
  const list = [...l.entries].sort((a, b) => b.size - a.size), shown = list.slice(0, 300);
  const base = l.path.replace(/[\\/]+$/, '') + sepOf(l.path);
  return shown.map(e => pickRow({ key: e.key, path: base + e.name, label: e.name, bytes: e.size, dir: e.kind === 'dir', copies: e.kind === 'file' ? e.copies : 0 }, depth)
      + (e.kind === 'dir' && WP.open.has(e.key) ? kids(e.key, depth + 1) : '')).join('')
    + (list.length > shown.length ? `<div class="wp-row muted" style="--d:${depth}"><span class="wp-tw"></span>and ${count(list.length - shown.length)} smaller items</div>` : '');
}

function renderWipeTargets() {
  const dl = $('#wp-targets');
  if (!dl) return;
  const sugg = [...WP.targets, ...S.roots.map(r => r.path)].filter((p, i, a) => a.indexOf(p) === i);
  dl.innerHTML = sugg.map(p => `<option value="${esc(p)}">`).join('');
  const to = $('#wp-to');
  if (!to.value && sugg[0]) to.placeholder = `e.g. ${sugg[0]}`;
}

async function assessWipe() {
  const seq = ++WP.seq;
  if (!WP.sel.size) { WP.a = null; WP.err = null; WP.checking = false; return renderWipeAssess(); }
  WP.checking = true; renderWipeAssess();
  try {
    const a = await api('wipe-assess', { keys: [...WP.sel.keys()].join(',') });
    if (seq !== WP.seq) return;
    WP.a = a; WP.err = null;
  } catch (e) {
    if (seq !== WP.seq) return;
    WP.a = null; WP.err = e.message;
  }
  WP.checking = false;
  renderWipeAssess();
}

function renderWipeAssess() {
  const el = $('#wp-assess');
  if (S.mode !== 'wipe' || !el) return;
  const a = WP.a;
  el.classList.toggle('stale', WP.checking && !!a);
  const head = `<h3>What would happen${WP.checking && a ? ' <span class="muted">· checking…</span>' : ''}</h3>`;
  if (!WP.sel.size) el.innerHTML = head + '<p class="muted">Tick a drive, a folder or something recognised on it. Nothing is deleted until you confirm.</p>';
  else if (WP.err) el.innerHTML = head + `<p class="warn-t">${esc(WP.err)}</p>`;
  else if (!a) el.innerHTML = head + '<p class="muted">Checking what has a copy elsewhere…</p>';
  else el.innerHTML = head + a.items.map(it => {
    const label = WP.sel.get(it.key)?.label || it.path;
    const w = n => it.bytes ? (100 * n / it.bytes).toFixed(2) : 0;
    return `<div class="wp-item${it.refused ? ' refused' : ''}">
      <div class="pl-what"><b>${esc(label)}</b><span class="pl-size">${bytes(it.bytes)} · ${count(it.files)} files</span>
        <button class="wp-x" type="button" data-wp-rm="${it.key}" aria-label="Remove ${esc(label)}" title="Remove">×</button></div>
      <code class="wp-path">${esc(it.path)}</code>
      ${it.refused ? `<p class="warn-t">Can’t be wiped: ${esc(it.refused)}</p>` : `
      <div class="wp-bar" aria-hidden="true"><i class="k" style="width:${w(it.kept_bytes)}%"></i><i class="o" style="width:${w(it.only_bytes)}%"></i></div>
      <ul class="wp-facts">
        <li><span class="dot k"></span>${it.kept_files ? `${bytes(it.kept_bytes)} has` : 'None of it has'} an identical copy elsewhere${it.copies.length ? `: ${it.copies.map(c => `${bytes(c.bytes)} in <code>${esc(c.path)}</code>`).join(', ')}` : ''}</li>
        ${it.only_files ? `<li><span class="dot o"></span>${bytes(it.only_bytes)} (${count(it.only_files)} files) exists only here</li>` : ''}
        ${it.unreadable_files ? `<li><span class="dot u"></span>${count(it.unreadable_files)} files couldn’t be read at the last scan: left in place</li>` : ''}
        ${wipeRemoteLine(it, a)}
        ${it.connected ? '' : '<li class="warn-t">Its drive isn’t connected: connect it to wipe</li>'}
      </ul>`}</div>`;
  }).join('') + '<p class="wp-total" id="wp-total"></p>';
  updateWipeGo();
}

// the parts of the form that depend on the assessment and the choices, updated in place so typing isn't interrupted
function updateWipeGo() {
  const go = $('#wp-go');
  if (!go || S.mode !== 'wipe') return;
  const a = WP.a, to = WP.to.trim(), move = WP.only === 'move' && !!to;
  // moving to the drive being freed keeps those bytes on it
  const toDrive = move ? S.roots.find(r => to === r.path || under(to, r.path)) : null;
  const same = it => !!toDrive && it.drive === toDrive.name;
  const freed = a ? a.kept_bytes + (move ? a.items.filter(it => !it.refused && !same(it)).reduce((n, it) => n + it.only_bytes, 0) : 0) : 0;
  let why = '';
  if (!WP.sel.size) why = 'Tick what to wipe first.';
  else if (!a) why = WP.err ? '' : 'Checking…';
  else if (a.items.some(i => i.refused)) why = 'Some of the selection can’t be wiped: untick it.';
  else if (a.items.some(i => !i.connected)) why = 'Connect the drive first: every file is read again before it’s deleted.';
  else if (WP.only === 'move' && a.only_bytes && !to) why = 'Choose a folder to move what exists only here to, or leave it where it is.';
  else if (move && !isFull(to)) why = 'The folder to move things to must be a full path.';
  else if (move && [...WP.sel.values()].some(s => to.replace(/[\\/]+$/, '') === s.path.replace(/[\\/]+$/, '') || under(to, s.path))) why = 'The folder to move things to is inside what’s being wiped.';
  else if (move && a.only_bytes && WP.room != null && WP.room < a.only_bytes) why = `There’s only ${bytes(WP.room)} free there, and ${bytes(a.only_bytes)} to move.`;
  else if (!freed) why = move && a.only_bytes ? `${toDrive.name} is the drive being freed: moving there frees nothing. Pick a folder on another drive.` : 'Nothing here has a copy elsewhere, so nothing would be deleted.';
  else if (a.why_not) why = `${a.why_not[0].toUpperCase()}${a.why_not.slice(1)}.`;
  else if (WP.typed.trim() !== a.confirm) why = `Type ${a.confirm} to confirm.`;
  go.disabled = !!why || WP.busy;
  go.textContent = a && freed ? `Wipe ${bytes(freed)}` : 'Wipe';
  $('#wp-why').textContent = why;
  $('#wp-names').textContent = a?.confirm || '…';
  const room = $('#wp-room');
  room.textContent = [WP.only === 'move' && to && WP.room != null ? `${bytes(WP.room)} free there` : '',
    a?.items.some(it => it.only_bytes && same(it)) ? `on ${toDrive.name}, which is being freed: what’s moved there stays on it` : ''].filter(Boolean).join(' · ');
  const total = $('#wp-total');
  if (total && a) total.innerHTML = !a.only_bytes ? `Deletes <b>${bytes(a.kept_bytes)}</b>: all of it has a copy elsewhere.`
    : `${a.kept_bytes ? `Deletes <b>${bytes(a.kept_bytes)}</b> that has a copy elsewhere.` : 'Nothing here has a copy elsewhere.'} <b>${bytes(a.only_bytes)}</b> exists only here: ${move ? `moved to <code>${esc(to)}</code> and checked, then deleted here` : 'left where it is'}.`;
}

async function checkRoom() {
  const to = WP.to.trim();
  WP.room = null;
  if (isFull(to)) { try { WP.room = (await api('room', { path: to })).free; } catch {} }
  if (WP.to.trim() === to) updateWipeGo();
}

function onWipeChange(e) {
  const t = e.target;
  if (t.matches('[data-wp-key]')) {
    const { wpKey: key, wpPath: path, wpLabel: label } = t.dataset;
    if (t.checked) {
      if (WP.sel.size >= WIPE_MAX) { t.checked = false; return toastError(new Error(`Pick at most ${WIPE_MAX} items at a time, or the folder that holds them`)); }
      for (const [k, s] of WP.sel) if (under(s.path, path)) WP.sel.delete(k); // the new tick covers them
      WP.sel.set(key, { key, path, label });
    } else WP.sel.delete(key);
    WP.typed = ''; $('#wp-typed').value = '';
    renderWipePick(`[data-wp-key="${CSS.escape(key)}"]`);
    return assessWipe();
  }
  if (t.name === 'wp-only') { WP.only = t.value; return updateWipeGo(); }
  if (t.id === 'wp-trust') WP.trust = t.checked;
}
function onWipeInput(e) {
  const t = e.target;
  if (t.id === 'wp-typed') { WP.typed = t.value; updateWipeGo(); }
  if (t.id === 'wp-to') {
    WP.to = t.value; WP.only = 'move';
    document.querySelector('input[name="wp-only"][value="move"]').checked = true;
    clearTimeout(WP.roomTimer); WP.roomTimer = setTimeout(checkRoom, 300);
    updateWipeGo();
  }
}
function onWipeClick(e) {
  const tw = e.target.closest('[data-wp-tw]');
  if (tw) {
    const k = tw.dataset.wpTw;
    if (!WP.open.delete(k)) WP.open.add(k);
    return renderWipePick(`[data-wp-tw="${CSS.escape(k)}"]`);
  }
  const rm = e.target.closest('[data-wp-rm]');
  if (rm) {
    WP.sel.delete(rm.dataset.wpRm);
    WP.typed = ''; $('#wp-typed').value = '';
    renderWipePick();
    return assessWipe();
  }
  if (e.target.closest('#wp-go')) return startWipe();
}

async function startWipe() {
  const a = WP.a, to = WP.to.trim();
  if (!a) return;
  WP.busy = true; updateWipeGo();
  const q = new URLSearchParams({ keys: [...WP.sel.keys()].join(','), confirm: WP.typed.trim() });
  if (WP.only === 'move' && to) q.set('move_to', to);
  if (WP.trust) q.set('trust', '1');
  const r = await fetch(`/api/wipe?${q}`, { method: 'POST', headers: { 'X-BackDown': '1' }, credentials: 'same-origin' }).catch(e => ({ ok: false, json: async () => ({ error: e.message }) }));
  const j = await r.json().catch(() => ({}));
  WP.busy = false;
  if (!r.ok || j.error) { updateWipeGo(); return toastError(new Error(j.error || r.statusText)); }
  toast('Wiping: follow it below or in Activity');
  WP.sel.clear(); WP.a = null; WP.typed = ''; $('#wp-typed').value = '';
  renderWipePick(); renderWipeAssess(); loadWipeHistory();
  if (!polling) pollDaemon(true);
}

function loadWipeHistory() { api('wipes').then(h => { WP.history = h; renderWipeHist(); }).catch(() => {}); }

function renderWipeHist() {
  const el = $('#wp-hist');
  if (S.mode !== 'wipe' || !el) return;
  const live = new Map((S.daemon ? [S.daemon.current, ...S.daemon.queued] : []).filter(j => j?.kind === 'wipe').map(j => [j.path, j]));
  if (!WP.history.length) { el.innerHTML = ''; return; }
  const STATE = { done: 'ok', partial: 'warn', failed: 'warn' };
  el.innerHTML = `<h3>Wipes <a class="btn sm" href="/api/custody" download title="Every deletion and move (and every copy, link and merge), as a signed, hash-chained export anyone can verify">Chain of custody</a></h3><table class="list wp-hist"><thead><tr><th>When</th><th>What</th><th>What existed only there</th><th>Result</th></tr></thead><tbody>
    ${WP.history.map(w => {
      const j = live.get(`wipe ${w.id}`);
      const state = j ? `<span class="pill busy">${j === S.daemon.current ? `wiping${j.progress ? ` · ${esc(j.progress)}` : ''}` : 'queued'}</span>`
        : `<span class="pill ${STATE[w.state] || ''}">${esc(w.state === 'new' ? 'waiting' : w.state === 'running' ? 'stopped' : w.state)}</span>`;
      return `<tr><td>${esc(when(w.created_at))}</td><td>${w.paths.map(p => `<code>${esc(p)}</code>`).join('<br>')}</td>
        <td>${w.move_to ? `moved to <code>${esc(w.move_to)}</code>` : 'left in place'}</td><td>${state}${w.summary ? `<small class="muted">${esc(w.summary)}</small>` : ''}</td></tr>`;
    }).join('')}</tbody></table>`;
}

// ---------- Rescue: failing drives, deleted files, lost partitions
//
// Three steps: which drive (or image file), what happened, then where to put what's rescued. The
// daemon does the work in its side lane (scans carry on), and each rescue stays on record below,
// with what it found and what to do next.

const RS = { vols: null, src: null, access: null, what: null, dest: '', kinds: new Set(['photos', 'videos', 'documents', 'audio', 'archives']),
  deep: false, retries: 1, list: [], busy: false, open: new Set(), range: null };
const RS_GROUPS = [['photos', 'Photos'], ['videos', 'Videos'], ['documents', 'Documents'], ['audio', 'Music and audio'], ['archives', 'Archives'], ['other', 'Other']];
const RS_WHAT = [
  ['image', 'It’s failing', 'Clicks, freezes, read errors, or it’s very slow. Copy it to an image first: every extra read can make it worse. Then look for files in the copy.'],
  ['carve', 'Files were deleted, or it was formatted', 'Find photos, videos and documents by what’s inside them, whatever the drive’s file system says.'],
  ['parts', 'It looks empty, or a partition vanished', 'Look for file systems the partition table no longer lists. Nothing on the drive is changed.'],
];

// "Copy it now" on a failing drive (health.js): Rescue, set up to copy that drive
function rescueDrive(device, title) {
  Object.assign(RS, { src: { path: device, title, kind: 'disk' }, what: 'image', dest: '', range: null, access: null });
  showRescue();
  checkAccess();
  // once the list of drives is in, show the chosen one
  const t0 = Date.now();
  const reveal = () => {
    const on = document.querySelector('.rs-opt.on');
    if (on) on.scrollIntoView({ block: 'center' });
    else if (S.mode === 'rescue' && Date.now() - t0 < 5000) setTimeout(reveal, 150);
  };
  reveal();
}

function showRescue() {
  S.mode = 'rescue';
  closeSearch(false);
  renderTabs(); renderPlaces(); renderPath();
  $('#banner').hidden = true; $('#info').hidden = true; $('#timebar').hidden = true;
  layoutRescue();
  if (!RS.vols) api('volumes').then(v => { RS.vols = v; renderRescueSources(); }).catch(e => { RS.vols = { error: e.message }; renderRescueSources(); });
  loadRescues();
  pollDaemon(true);
}

function layoutRescue() {
  $('#view').innerHTML = `<div class="plan rescue">
    <div class="pl-head"><div><h2>Rescue</h2>
      <p class="muted">Get files back from a drive or memory card that’s failing, was formatted, or lost its files. BackDown only reads it: whatever it rescues goes to another drive.</p></div></div>
    <section class="card-ish"><h3><span class="rs-n">1</span> Which drive or card?</h3><div id="rs-src"></div><div id="rs-access"></div></section>
    <section class="card-ish"><h3><span class="rs-n">2</span> What happened?</h3><div class="rs-what" id="rs-what" role="radiogroup" aria-label="What happened"></div></section>
    <section class="card-ish" id="rs-go-card"><h3><span class="rs-n">3</span> <span id="rs-go-title">Where should what’s rescued go?</span></h3><div id="rs-go"></div></section>
    <section id="rs-list"></section>
  </div>`;
  const root = $('#view .rescue');
  root.addEventListener('change', onRescueChange);
  root.addEventListener('input', onRescueInput);
  root.addEventListener('click', onRescueClick);
  renderRescueSources(); renderRescueAccess(); renderRescueWhat(); renderRescueGo(); renderRescueList();
}

// every disk and what's on it, from the same probe as Activity's "Connected drives"
function renderRescueSources() {
  const el = $('#rs-src');
  if (S.mode !== 'rescue' || !el) return;
  const v = RS.vols;
  const opt = (path, title, sub, kind, extra = '') => `<label class="rs-opt${RS.src?.path === path ? ' on' : ''}">
      <input type="radio" name="rs-src" value="${esc(path)}" data-rs-title="${esc(title)}" data-rs-kind="${kind}"${RS.src?.path === path ? ' checked' : ''}>
      <span class="rs-opt-t"><b>${esc(title)}</b><small class="muted">${esc(sub)}</small></span>${extra}</label>`;
  const flat = vols => vols.flatMap(x => [x, ...flat(x.holders || [])]);
  const file = RS.src?.kind === 'file';
  el.innerHTML = (!v ? '<p class="muted">Looking for drives…</p>' : v.error ? `<p class="muted">${esc(v.error)}</p>` : v.disks.map(dk => `<div class="rs-disk">
      ${opt(dk.device, dk.model || dk.name, [bytes(dk.size || 0), dk.transport, dk.device].filter(Boolean).join(' · '), 'disk', dk.system ? '<span class="pill off">the computer runs from it</span>' : '')}
      <div class="rs-parts">${flat(dk.volumes).filter(x => x.device && x.device !== dk.device).map(x => opt(x.device, x.label || x.name, [x.fs_name || x.fs || 'unknown contents', bytes(x.size || 0), x.device].join(' · '), 'part')).join('')}</div></div>`).join(''))
    + `<div class="rs-disk">${opt('file', 'An image file', 'a copy made earlier, by BackDown or another tool', 'file')}
      <input id="rs-file" class="wp-input rs-file" spellcheck="false" autocomplete="off" placeholder="/full/path/to/card.img" value="${esc(file ? RS.src.file || '' : '')}" aria-label="Image file"${file ? '' : ' hidden'}></div>`;
}

async function checkAccess() {
  const path = RS.src?.kind === 'file' ? (RS.src.file || '').trim() : RS.src?.path;
  RS.access = null; renderRescueAccess();
  if (!path) return;
  try { RS.access = await api('rescue-access', { path }); } catch (e) { RS.access = { readable: false, why: e.message }; }
  if ((RS.src?.kind === 'file' ? (RS.src.file || '').trim() : RS.src?.path) !== path) return;
  renderRescueAccess(); renderRescueGo();
}

function renderRescueAccess() {
  const el = $('#rs-access');
  if (!el) return;
  const a = RS.access;
  if (!RS.src || (RS.src.kind === 'file' && !RS.src.file)) { el.innerHTML = ''; return; }
  if (!a) { el.innerHTML = '<p class="muted rs-acc">Checking…</p>'; return; }
  el.innerHTML = a.readable
    ? `<p class="rs-acc ok"><span class="dot k"></span>BackDown can read it: ${bytes(a.size || 0)}${a.granted ? ' (opened for it by the system, read-only, until BackDown stops)' : ''}.</p>`
    : `<div class="rs-acc no"><p><span class="dot o"></span>${esc(a.why || 'It can’t be read.')}</p>
      ${a.fix ? `<p class="muted">Run this once in a terminal, then check again. It lets your account read this drive only (never change it), until it’s unplugged or the computer restarts:</p>
      <div class="rs-cmd"><code>${esc(a.fix)}</code><button class="btn sm" type="button" data-rs-copy="${esc(a.fix)}">Copy</button></div>` : ''}
      ${allowButton(a)}
      <button class="btn sm" type="button" data-rs-recheck>Check again</button></div>`;
}

// the system opened the drive for BackDown (drives.js); a copy-out was started from a browser
document.addEventListener('bd:access', e => {
  if (e.detail?.path === (RS.src?.kind === 'file' ? (RS.src.file || '').trim() : RS.src?.path)) { RS.access = e.detail; renderRescueAccess(); renderRescueGo(); }
});
document.addEventListener('bd:rescue-started', () => { loadRescues(); if (!polling) pollDaemon(true); });

function renderRescueWhat() {
  const el = $('#rs-what');
  if (!el) return;
  el.innerHTML = RS_WHAT.map(([k, t, d]) => `<button class="rs-card-btn${RS.what === k ? ' on' : ''}" type="button" role="radio" aria-checked="${RS.what === k}" data-rs-what="${k}"><b>${t}</b><span>${d}</span></button>`).join('');
}

// where rescued things go: a folder on an indexed drive that isn't the source
function rescueDestGuess() {
  const name = (RS.src?.title || 'drive').replace(/[\\/:*?"<>|]+/g, '-').trim();
  const day = new Date().toISOString().slice(0, 10);
  const base = S.roots.map(r => r.path).find(p => !RS.src?.path || !p.startsWith('/dev/')) || '';
  if (!base) return '';
  const dir = `${base.replace(/[\\/]+$/, '')}${sepOf(base)}BackDown Rescue`;
  return RS.what === 'image' ? `${dir}${sepOf(base)}${name} ${day}.img` : `${dir}${sepOf(base)}${name} ${day}`;
}

function renderRescueGo() {
  const el = $('#rs-go');
  if (!el) return;
  const ready = RS.src && RS.access?.readable && RS.what;
  $('#rs-go-title').textContent = RS.what === 'parts' ? 'How thoroughly?' : RS.what === 'image' ? 'Where should the copy go?' : 'What to look for, and where to put it';
  if (!RS.src || !RS.what) { el.innerHTML = '<p class="muted">Choose the drive and what happened first.</p>'; return; }
  const guess = rescueDestGuess();
  const dest = `<label class="rs-dest">${RS.what === 'image' ? 'Save the copy as' : 'Save what’s found in'}
      <input id="rs-dest" class="wp-input" spellcheck="false" autocomplete="off" value="${esc(RS.dest)}" placeholder="${esc(guess || '/full/path/on/another/drive')}"></label>
    <small class="muted" id="rs-room"></small>`;
  const body = RS.what === 'image' ? `${dest}
      <label class="wp-chk"><input type="checkbox" id="rs-retries"${RS.retries ? ' checked' : ''}> Try unreadable areas once more at the end</label>
      <p class="note">The good areas are copied first, the bad ones skipped and come back to later. It can be stopped and resumed. A map beside the copy (<code>.map</code>, as GNU ddrescue writes it) records what’s been read.</p>`
    : RS.what === 'carve' ? `<div class="rs-kinds">${RS_GROUPS.map(([k, l]) => `<label class="wp-chk"><input type="checkbox" data-rs-group="${k}"${RS.kinds.has(k) ? ' checked' : ''}> ${l}</label>`).join('')}</div>${dest}
      <p class="note">Found files get names from where they were found, sorted into a folder per kind. Pieces of a file that was scattered around the drive can’t be put back together: those come back damaged, or not at all.</p>`
    : `<label class="wp-chk"><input type="radio" name="rs-deep" value="0"${RS.deep ? '' : ' checked'}> Quick: the usual places partitions start (seconds)</label>
      <label class="wp-chk"><input type="radio" name="rs-deep" value="1"${RS.deep ? ' checked' : ''}> Thorough: read the whole drive (as long as copying it)</label>`;
  const range = RS.range && RS.what !== 'image' ? `<p class="rs-range">Only the ${esc(RS.range.label)} <button class="wp-x" type="button" data-rs-range-clear aria-label="Search the whole drive instead" title="Search the whole drive instead">×</button></p>` : '';
  el.innerHTML = range + body + `<button class="btn primary" type="button" id="rs-start"${ready ? '' : ' disabled'}>${RS.what === 'image' ? 'Start copying' : RS.what === 'carve' ? 'Start looking' : 'Look for partitions'}</button>
    <p class="note" id="rs-why">${!RS.access ? 'Checking the drive…' : !RS.access.readable ? 'BackDown needs to be able to read the drive first (above).' : ''}</p>`;
  if (RS.what !== 'parts') checkRescueRoom();
}

async function checkRescueRoom() {
  const el = $('#rs-room');
  const to = ($('#rs-dest')?.value || '').trim() || $('#rs-dest')?.placeholder || '';
  if (!el || !isFull(to)) return;
  try {
    const { free } = await api('room', { path: to });
    const need = RS.access?.size || 0;
    el.textContent = free == null ? '' : `${bytes(free)} free there${RS.what === 'image' && need > free ? ` — the copy needs up to ${bytes(need)}` : ''}`;
    el.classList.toggle('warn-t', RS.what === 'image' && need > (free ?? Infinity));
  } catch { el.textContent = ''; }
}

function onRescueChange(e) {
  const t = e.target;
  if (t.name === 'rs-src') {
    RS.src = { path: t.value, title: t.dataset.rsTitle, kind: t.dataset.rsKind, file: RS.src?.file || '' };
    RS.dest = ''; RS.range = null; renderRescueSources(); renderRescueGo();
    if (RS.src.kind === 'file') { $('#rs-file').focus(); if (!RS.src.file) { RS.access = null; return renderRescueAccess(); } }
    return checkAccess();
  }
  if (t.dataset.rsGroup) { t.checked ? RS.kinds.add(t.dataset.rsGroup) : RS.kinds.delete(t.dataset.rsGroup); return; }
  if (t.name === 'rs-deep') RS.deep = t.value === '1';
  if (t.id === 'rs-retries') RS.retries = t.checked ? 1 : 0;
}
function onRescueInput(e) {
  const t = e.target;
  if (t.id === 'rs-file') {
    RS.src.file = t.value; RS.src.title = t.value.split(/[\\/]/).pop() || 'image';
    clearTimeout(RS.fileTimer); RS.fileTimer = setTimeout(checkAccess, 400);
  }
  if (t.id === 'rs-dest') { RS.dest = t.value; clearTimeout(RS.roomTimer); RS.roomTimer = setTimeout(checkRescueRoom, 300); }
}
function onRescueClick(e) {
  const t = e.target;
  const w = t.closest('[data-rs-what]');
  if (w) { RS.what = w.dataset.rsWhat; RS.dest = ''; renderRescueWhat(); return renderRescueGo(); }
  const cp = t.closest('[data-rs-copy]');
  if (cp) return copyText(cp.dataset.rsCopy);
  if (t.closest('[data-rs-recheck]')) return checkAccess();
  if (t.closest('#rs-start')) return startRescue();
  const tg = t.closest('[data-rs-toggle]');
  if (tg) { const id = +tg.dataset.rsToggle; if (!RS.open.delete(id)) RS.open.add(id); return renderRescueList(); }
  const nx = t.closest('[data-rs-next]');
  if (nx) return startRescue(JSON.parse(nx.dataset.rsNext));
  const pk = t.closest('[data-rs-pick]');
  if (pk) return pickRange(JSON.parse(pk.dataset.rsPick));
  if (t.closest('[data-rs-range-clear]')) { RS.range = null; return renderRescueGo(); }
  const st = t.closest('[data-job-stop]');
  if (st) return stopJob(+st.dataset.jobStop);
  const op = t.closest('[data-rs-open]');
  if (op) return openRescued(op.dataset.rsOpen);
}

// "Find files in it" on a found partition: the form, set up for that part of the drive
function pickRange(p) {
  const dev = /^\/dev\//.test(p.source) || p.source.startsWith('\\\\.\\');
  RS.src = dev ? { path: p.source, title: baseName(p.source), kind: 'disk' } : { path: 'file', title: baseName(p.source), kind: 'file', file: p.source };
  RS.what = 'carve'; RS.dest = ''; RS.range = { start: p.start, end: p.end, label: p.label };
  renderRescueSources(); renderRescueWhat(); renderRescueGo(); checkAccess();
  $('#rs-go-card').scrollIntoView({ behavior: 'smooth', block: 'center' });
}

// start one from the form, or a follow-up from a finished rescue ("find files in the copy")
async function startRescue(next = null) {
  const src = next?.source || (RS.src.kind === 'file' ? RS.src.file.trim() : RS.src.path);
  const mode = next?.mode || RS.what;
  const q = next?.resume ? { resume: next.resume } : { mode, source: src };
  if (next && !next.resume) Object.assign(q, next);
  else if (!next) {
    if (RS.range) { q.start = RS.range.start; if (RS.range.end) q.end = RS.range.end; }
    if (mode !== 'parts') q.dest = ($('#rs-dest').value || '').trim() || $('#rs-dest').placeholder;
    if (mode === 'carve') q.kinds = [...RS.kinds].join(',');
    if (mode === 'image') q.retries = RS.retries;
    if (mode === 'parts') q.deep = RS.deep ? 1 : 0;
  }
  if (!q.resume && mode === 'carve' && !q.kinds) return toastError(new Error('Choose at least one kind of file to look for'));
  if (q.dest !== undefined && !isFull(q.dest)) return toastError(new Error('Choose where to put it: a full path on another drive'));
  const r = await fetch(`/api/rescue?${new URLSearchParams(q)}`, { method: 'POST', headers: { 'X-BackDown': '1' }, credentials: 'same-origin' }).catch(e => ({ ok: false, json: async () => ({ error: e.message }) }));
  const j = await r.json().catch(() => ({}));
  if (!r.ok || j.error) return toastError(new Error(j.error || r.statusText));
  RS.open.add(j.id);
  toast(mode === 'parts' ? 'Looking for partitions: it shows below' : 'Started: it shows below, and in Activity');
  loadRescues();
  if (!polling) pollDaemon(true);
  $('#rs-list')?.scrollIntoView({ behavior: 'smooth', block: 'start' });
}

async function stopJob(id) {
  const r = await fetch(`/api/job-stop?id=${id}`, { method: 'POST', headers: { 'X-BackDown': '1' }, credentials: 'same-origin' });
  const j = await r.json().catch(() => ({}));
  if (!r.ok || j.error) return toastError(new Error(j.error || r.statusText));
  toast(j.state === 'stopping' ? 'Stopping: what’s done so far is kept' : 'Taken off the queue');
  if (!polling) pollDaemon(true);
}

// recovered files open in the browser once their folder is indexed (a job the rescue queued)
async function openRescued(path) {
  try { const res = await api('resolve', { path }); await go(res.dir, { scan: null }); }
  catch { toast('The recovered files are being indexed: they’ll open once that’s done'); }
}

function loadRescues() { return api('rescues').then(l => { RS.list = l; renderRescueList(); }).catch(() => {}); }

function renderRescueList() {
  const el = $('#rs-list');
  if (S.mode !== 'rescue' || !el || held(renderRescueList)) return;
  if (!RS.list.length) { el.innerHTML = ''; return; }
  const jobs = S.daemon ? [S.daemon.side, ...S.daemon.queued].filter(j => j?.kind === 'rescue') : [];
  const live = new Map(jobs.map(j => [j.path, j]));
  el.innerHTML = `<h3>Rescues</h3>` + RS.list.map(r => rescueCard(r, live.get(`rescue ${r.id}`))).join('');
}

const RS_TITLE = { image: 'Copy', carve: 'Find files', parts: 'Look for partitions', files: 'Copy files out' };
const RS_STATE = { done: 'ok', stopped: 'warn', failed: 'warn', running: 'busy', new: 'busy' };
const baseName = p => (p || '').split(/[\\/]/).filter(Boolean).pop() || p;

function rescueCard(r, job) {
  const res = r.result || {}, o = r.options || {};
  const running = job && job === S.daemon?.side;
  const c = job?.counts || {};
  const p = running && c.total_bytes ? c.bytes / c.total_bytes : null;
  const state = job ? (running ? 'running' : 'waiting') : r.state === 'running' || r.state === 'new' ? 'interrupted' : r.state;
  const where = r.mode === 'parts' ? esc(r.source) : `${esc(r.source)} → <code>${esc(r.dest || '')}</code>`;
  let body = '';
  if (running) body = `<div class="meter${p == null ? ' unknown' : ''}"><i style="width:${p != null ? (p * 100).toFixed(1) : 100}%"></i></div>
      <p>${esc(job.progress || 'starting…')}</p>`;
  if (r.mode === 'image' && res.size) {
    body += mapBar(res) + `<p>${bytes(res.rescued)} of ${bytes(res.size)} copied${res.bad ? ` · <span class="warn-t">${bytes(res.bad)} unreadable in ${count(res.bad_areas)} place${res.bad_areas === 1 ? '' : 's'}</span>` : ''}${res.todo ? ` · ${bytes(res.todo)} still to try` : ''}</p>`;
    if (!job && r.dest) body += `<div class="rs-next">
        ${res.todo ? `<button class="btn sm" type="button" data-rs-next='${esc(JSON.stringify({ resume: r.id }))}'>Resume copying</button>` : ''}
        <button class="btn sm" type="button" data-rs-next='${esc(JSON.stringify({ mode: 'parts', source: r.dest, deep: 0 }))}'>Look for partitions in the copy</button>
        <button class="btn sm${res.todo ? '' : ' primary'}" type="button" data-rs-next='${esc(JSON.stringify({ mode: 'carve', source: r.dest, dest: r.dest.replace(/\.img$/i, '') + ' files', kinds: 'photos,videos,documents,audio,archives' }))}'>Find files in the copy</button></div>`;
  }
  if (r.mode === 'carve' && res.by_group) {
    const g = RS_GROUPS.filter(([k]) => res.by_group[k]);
    body += `<p>${g.length ? `${g.map(([k, l]) => `<b>${count(res.by_group[k])}</b> ${esc(l.toLowerCase())}`).join(' · ')} · ${bytes(res.found_bytes || 0)}` : 'Nothing found yet.'}${res.incomplete ? ` <span class="muted">(${count(res.incomplete)} cut off or damaged: those come back partly)</span>` : ''}</p>`;
    if (res.unreadable) body += `<p class="warn-t">${bytes(res.unreadable)} of the drive couldn’t be read: copying it first (“It’s failing”) gives those parts more tries.</p>`;
    const halted = state === 'stopped' || state === 'interrupted';
    if (!job && r.dest && (res.found || halted)) body += `<div class="rs-next">${res.found ? `<button class="btn sm primary" type="button" data-rs-open="${esc(r.dest)}">Open the recovered files</button>` : ''}
      ${halted ? `<button class="btn sm" type="button" data-rs-next='${esc(JSON.stringify({ resume: r.id }))}'>Carry on from where it stopped</button>` : ''}</div>`;
  }
  if (r.mode === 'parts' && res.found) {
    const fs = res.found, lost = fs.filter(f => !f.in_table);
    body += `<p>Partition table: <b>${esc(res.table?.scheme === 'none' ? 'none' : (res.table?.scheme || '?').toUpperCase())}</b>${res.table?.entries?.length ? `, listing ${res.table.entries.length}` : ''}.
        ${fs.length ? `Found <b>${fs.length}</b> file system${fs.length === 1 ? '' : 's'}${lost.length ? `, <b>${lost.length}</b> of them not in the table` : ''}.` : 'No file systems found.'}</p>
      ${(res.table?.problems || []).map(x => `<p class="warn-t">${esc(x)}</p>`).join('')}
      ${fs.length ? `<table class="list rs-parts-t"><thead><tr><th>Starts at</th><th class="n">Size</th><th>File system</th><th>Name</th><th>In the table</th><th></th></tr></thead><tbody>
        ${fs.map(f => `<tr><td>${bytes(f.start)}<small class="muted">sector ${(f.start / (res.sector || 512)).toLocaleString()}</small></td><td class="n">${f.size ? bytes(f.size) : '?'}</td>
          <td>${esc(f.fs)}${f.via !== 'boot sector' && f.via !== 'superblock' ? `<small class="muted">found by its ${esc(f.via)}</small>` : ''}</td><td>${esc(f.label || '')}${f.nested ? '<small class="muted">inside another file system</small>' : ''}</td>
          <td>${f.in_table ? '<span class="pill ok">yes</span>' : '<span class="pill warn">no: lost</span>'}${f.truncated ? '<span class="pill warn">runs past the end</span>' : ''}</td>
          <td class="rs-acts">${browseFoundBtn(r, f)}<button class="btn sm" type="button" data-rs-pick='${esc(JSON.stringify({ source: r.source, start: f.start, end: f.size ? f.start + f.size : null, label: `${f.fs}${f.label ? ` “${f.label}”` : ''} at ${bytes(f.start)}` }))}'>Find files in it</button></td></tr>`).join('')}</tbody></table>` : ''}`;
  }
  if (r.mode === 'files') body += filesBody(r, job);
  // stopped or interrupted before it got anywhere: start it again as it was
  if (!job && ['stopped', 'interrupted', 'failed'].includes(state) && !(r.mode === 'image' ? res.size : r.mode === 'carve' ? res.by_group : r.mode === 'files' ? res.files != null : res.found))
    body += `<div class="rs-next"><button class="btn sm" type="button" data-rs-next='${esc(JSON.stringify({ resume: r.id }))}'>Start again</button></div>`;
  if (r.state === 'failed' && !job) body += `<p class="warn-t">${esc(r.summary || 'It stopped with an error.')}</p>`;
  else if (r.summary && !job && r.mode !== 'parts' && r.mode !== 'files') body += `<p class="muted">${esc(r.summary)}</p>`;
  return `<article class="rs-item">
    <header><b>${RS_TITLE[r.mode] || r.mode}</b><span class="rs-where">${where}</span>
      <span class="pill ${RS_STATE[state] || (state === 'interrupted' ? 'warn' : '')}">${state}</span>
      <span class="muted" title="${esc(when(r.created_at))}">${rel(r.created_at)}</span>
      ${job ? `<button class="btn sm" type="button" data-job-stop="${job.id}">${running ? 'Stop' : 'Cancel'}</button>` : ''}</header>
    ${body}</article>`;
}

// the image as ddrescue's map sees it: a strip of bins, green copied, red unreadable, grey still to try
function mapBar(res) {
  if (!res.bins?.length) return '';
  return `<div class="rs-map" role="img" aria-label="${bytes(res.rescued)} copied, ${bytes(res.bad)} unreadable, ${bytes(res.todo)} still to try">${res.bins.map(([g, b, t]) => {
    const n = g + b + t || 1;
    return `<i style="--g:${(g / n * 100).toFixed(0)}%;--b:${(b / n * 100).toFixed(0)}%"${b ? ' class="bad"' : ''}></i>`;
  }).join('')}</div>`;
}

// ---------- the daemon: activity, rescans, live refresh
//
// Long polls /api/daemon: the daemon answers as soon as something changes (at most once a second while
// a job reports progress) or after 50 s, so an idle browser costs about one small request a minute.
// Nothing is asked while the tab is hidden; coming back asks at once.

let finishedSeen = null, lastDone, shownFail = 0, polling = false, pollTimer, busyKey = '', clockSkew = 0;
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

const jobName = j => j.kind === 'index' ? `Scan ${rootName(j.path)}` : j.kind === 'entities' ? 'Recognise what’s on the drives'
  : j.kind === 'apply' ? `Apply ${j.path || 'a plan'}` : j.kind === 'mirror-sync' ? `Sync ${j.path || 'a mirror'}`
  : j.kind === 'mirror-verify' ? `Verify ${j.path || 'a mirror'}` : j.kind === 'wipe' ? `Free up (${j.path || 'a wipe'})` : j.kind === 'rescue' ? rescueJobName(j)
  : j.kind === 'remote-sync' ? `Sync the machine ${j.path || ''}`
  : j.kind === 'dedup' ? `Tidy the archive (${j.path || 'dedup'})` : j.kind === 'mail-merge' ? `Merge mailboxes (${j.path || 'merge'})`
  : j.kind === 'indexes' || j.kind === 'dupes-index' ? 'Prepare the index for speed (once)' : 'Hash new and changed files';
function rescueJobName(j) {
  const r = RS.list.find(x => `rescue ${x.id}` === j.path);
  return r ? `${RS_TITLE[r.mode] || 'Rescue'}: ${baseName(r.source)}` : 'Rescue';
}
const rootName = p => S.roots.find(r => r.path === p)?.name || (p || '').split(/[\\/]/).filter(Boolean).pop() || p;

async function applyDaemon(d) {
  const had = !!S.daemon;
  let fin = [], prevSeen = null;
  S.daemon = d.running ? d : null;
  if (d.running) clockSkew = d.now - Date.now() / 1000;
  daemonChanged(d); // drive health or notifications changed: health.js fetches them again
  machinesDaemon(d); // a machine's sync started or ended: machines.js fetches them again
  const st = $('#status-act');
  if (S.daemon) {
    const cur = d.current, q = d.queued.length, c = cur?.counts;
    st.innerHTML = cur
      ? `<span class="act"><span class="pulse"></span>${cur.kind === 'index' ? `Scanning ${esc(rootName(cur.path))}` : cur.kind === 'hash' ? 'Hashing' : esc(jobName(cur))}${c ? ` · ${pct(cur) != null ? Math.round(pct(cur) * 100) + '%' : count(c.files) + ' files'}` : ''}</span>${q ? `<span>${q} more queued</span>` : ''}`
      : `<span class="act idle">Up to date</span><span>${d.rescan_every_secs ? `rescans every ${dur(d.rescan_every_secs)}` : 'rescans on request'}</span>`;
    // a job finished: fresh sizes, timeline and (unless looking at the past) the open folder
    const done = d.recent.find(j => j.state === 'done');
    if (lastDone !== undefined && done && done.id !== lastDone) {
      const data = await api('roots').catch(() => null);
      if (data) { S.roots = data.roots; showSummary(data.status); renderPlaces(); }
      if (S.mode === 'browse' && done.kind === 'index' && S.root && done.path === S.root.path) {
        await loadTimeline();
        if (S.scan == null) await go(S.dir, { push: false, scan: null, select: S.anchor });
      }
    }
    lastDone = done ? done.id : 0; // 0: nothing had finished yet, so the first job that does counts
    // a plan was applied: show its journal
    if (done && done.kind === 'apply' && done.id === lastDone && S.mode === 'plan' && done.finished_at > d.now - 5) showPlan(PL.id);
    // recognition finished: refresh What's here if it's on screen
    if (done && done.kind === 'entities' && done.id === lastDone && S.mode === 'entities' && done.finished_at > d.now - 5) showEntities();
    // a wipe finished, or a drive was scanned again (any since the last answer, not only the latest):
    // fresh history and folder sizes in Free up
    fin = d.recent.filter(j => j.finished_at);
    prevSeen = finishedSeen;
    if (finishedSeen && fin.some(j => !finishedSeen.has(j.id) && (j.kind === 'wipe' || j.kind === 'index'))) {
      WP.lists.clear();
      if (S.mode === 'wipe') { loadWipeHistory(); renderWipePick(); }
    }
    finishedSeen = new Set(fin.map(j => j.id));
    const failed = d.recent.find(j => j.state === 'failed');
    if (failed && failed.id > shownFail && failed.finished_at > d.now - 60 && S.mode !== 'activity') toastError(new Error(`${jobName(failed)} failed: ${failed.error}`));
    if (failed) shownFail = Math.max(shownFail, failed.id);
  }
  // the sidebar only changes when the set of busy drives does
  const key = JSON.stringify([had, !!S.daemon, S.daemon?.current?.path, S.daemon?.queued.map(j => j.path)]);
  if (key !== busyKey) { busyKey = key; renderPlaces(); }
  if (S.mode === 'activity') renderActivity();
  if (S.mode === 'wipe') renderWipeHist();
  // a rescue started elsewhere (the CLI, another tab): learn what it is, to name it
  if (S.daemon && [S.daemon.side, ...S.daemon.queued].some(j => j?.kind === 'rescue' && !RS.list.some(x => `rescue ${x.id}` === j.path)) && !RS.loading) {
    RS.loading = true;
    loadRescues().finally(() => { RS.loading = false; if (S.mode === 'activity') renderActivity(); });
  }
  if (S.mode === 'rescue') {
    if (S.daemon && fin.some(j => j.kind === 'rescue' && !prevSeen?.has(j.id))) loadRescues();
    else renderRescueList();
  }
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
  if (j.kind === 'rescue') return c.total_bytes ? c.bytes / c.total_bytes : null;
  if (j.kind === 'hash') return c.stage !== 'sample' && c.total_bytes ? c.bytes / c.total_bytes : c.total_files ? c.files / c.total_files : null;
  // a scan is measured against the drive's previous one, so it can run past 100%: cap it
  return c.total_files ? Math.min(0.99, c.files / c.total_files) : null;
}
function eta(j, now) {
  const p = pct(j), took = now - (j.started_at || now);
  if (p == null || p < 0.02 || took < 15) return '';
  return `about ${dur(took * (1 - p) / p)} left`;
}
const REASON = { requested: 'you asked', scheduled: 'scheduled', returned: 'drive came back', 'after-scan': 'after a scan', 'after-apply': 'after copying', 'after-wipe': 'after wiping', 'after-rescue': 'recovered files', 'after-dedup': 'after tidying', 'after-merge': 'after merging mail', 'after-sync': 'after a machine’s sync', 'after-hash': 'after hashing' };
const JOB_ICON = {
  'remote-sync': '<svg viewBox="0 0 24 24"><rect x="3" y="4" width="18" height="7" rx="1.5"/><rect x="3" y="13" width="18" height="7" rx="1.5"/><path d="M7 7.5h.01M7 16.5h.01"/></svg>',
  dedup: '<svg viewBox="0 0 24 24"><path d="M10 14a3.5 3.5 0 0 0 5 0l3-3a3.5 3.5 0 0 0-5-5l-.5.5"/><path d="M14 10a3.5 3.5 0 0 0-5 0l-3 3a3.5 3.5 0 0 0 5 5l.5-.5"/></svg>',
  'mail-merge': '<svg viewBox="0 0 24 24"><rect x="3" y="5" width="18" height="14" rx="2"/><path d="m3 7 9 6 9-6"/></svg>',
  index: '<svg viewBox="0 0 24 24"><path d="M20 11a8 8 0 1 0-2.3 5.7"/><path d="M20 5v6h-6"/></svg>',
  hash: '<svg viewBox="0 0 24 24"><path d="M4 9h16M4 15h16M10 3 8 21M16 3l-2 18"/></svg>',
  entities: '<svg viewBox="0 0 24 24"><path d="M20.6 13.4 13.4 20.6a2 2 0 0 1-2.8 0L3 13V3h10l7.6 7.6a2 2 0 0 1 0 2.8z"/><circle cx="7.5" cy="7.5" r="1.5"/></svg>',
  apply: '<svg viewBox="0 0 24 24"><rect x="8" y="8" width="12" height="12" rx="2"/><path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2"/></svg>',
  'mirror-sync': '<svg viewBox="0 0 24 24"><path d="M4 7h13l-3-3M20 17H7l3 3"/></svg>',
  'mirror-verify': '<svg viewBox="0 0 24 24"><path d="M9 12l2 2 4-4"/><path d="M12 3l8 3v6c0 4.5-3.4 8.3-8 9-4.6-.7-8-4.5-8-9V6z"/></svg>',
  wipe: '<svg viewBox="0 0 24 24"><path d="M4 7h16M10 11v6M14 11v6M6 7l1 13h10l1-13M9 7V4h6v3"/></svg>',
  rescue: '<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="9"/><circle cx="12" cy="12" r="4"/><path d="m5.6 5.6 3.6 3.6M14.8 14.8l3.6 3.6M14.8 9.2l3.6-3.6M5.6 18.4l3.6-3.6"/></svg>',
};

let activityClock;
const ACT = { mirrors: null, volumes: null, probing: false, scanning: null };
function probeVolumes() {
  ACT.probing = true;
  api('volumes').then(v => { ACT.volumes = v; }).catch(e => { ACT.volumes = { error: e.message }; })
    .finally(() => { ACT.probing = false; if (S.mode === 'activity') renderActivity(); });
}
// every disk and partition here (probed when Activity opens, or on "Look again"; never on a timer)
function connectedDrives() {
  const v = ACT.volumes;
  const head = `<h3>Connected drives <button class="btn sm" type="button" id="vol-again"${ACT.probing ? ' disabled' : ''}>${ACT.probing ? 'Looking…' : 'Look again'}</button>${healthHead()}</h3>`;
  if (!v) return `<section>${head}<p class="muted pad">Looking…</p></section>`;
  if (v.error) return `<section>${head}<p class="muted pad">${esc(v.error)}</p></section>`;
  const STATE = { system: ['off', 'system'], mounted: ['ok', 'mounted'], mountable: ['warn', 'not mounted'], encrypted: ['warn', 'encrypted'],
    member: ['off', 'part of a volume group'], swap: ['off', 'swap'], empty: ['off', 'empty'], unknown: ['off', 'unknown'] };
  const rows = vol => {
    const [cls, label] = STATE[vol.state] || ['off', vol.state];
    const mount = vol.mounts?.[0];
    const idx = vol.indexed?.[0];
    const act = idx ? `<span class="muted">indexed${idx.scanned_at ? ` · scanned ${rel(idx.scanned_at)}` : ''}</span>`
      : vol.state === 'mounted' && mount && S.daemon ? `<button class="btn sm" type="button" data-index-path="${esc(mount.path)}">Index this</button>` : '';
    return `<tr><td><b>${esc(vol.label || vol.name)}</b><small class="muted">${esc(vol.device)}${mount ? ` · ${esc(mount.path)}${mount.read_only ? ' (read-only)' : ''}` : ''}</small></td>
      <td>${esc(vol.fs_name || vol.fs || '—')}</td><td class="n">${bytes(vol.size || 0)}</td><td><span class="pill ${cls}">${label}</span>${vol.note ? `<small class="muted">${esc(vol.note)}</small>` : ''}</td><td class="act-do">${act}${mountCell(vol)}</td></tr>`
      + (vol.holders || []).map(rows).join('');
  };
  return `<section>${head}${mountNotice()}${v.disks.map(dk => `<div class="vol-disk"><div class="vol-head"><b>${esc(dk.model || dk.name)}</b>${pill(healthOfDisk(dk))}
      <span class="muted">${esc(dk.device)} · ${bytes(dk.size || 0)} · ${esc(dk.transport || '')}${dk.rotational ? ' · spinning disk' : dk.rotational === false ? ' · solid state' : ''}${dk.system ? ' · system disk' : ''}</span></div>
      <table class="list act-drives"><tbody>${dk.volumes.map(rows).join('')}</tbody></table></div>`).join('')}
    ${(v.notes || []).map(n => `<p class="muted pad">${esc(n)}</p>`).join('')}</section>`;
}
// a partition mounted or unmounted, an image attached or detached (drives.js): look again
document.addEventListener('bd:drives', e => {
  if (e.detail?.reprobe) probeVolumes();
  else if (S.mode === 'activity') renderActivity();
});
function showActivity() {
  loadMounts().then(() => { if (S.mode === 'activity') renderActivity(); });
  if (!ACT.volumes && !ACT.probing) probeVolumes();
  if (!RS.list.length) loadRescues().then(() => { if (S.mode === 'activity') renderActivity(); });
  api('mirrors').then(m => { ACT.mirrors = m; if (S.mode === 'activity') renderActivity(); }).catch(() => {});
  // how scans may be made (the change log, the file table, a walk now and then): GET/POST /api/scan-settings
  api('scan-settings').then(s => { ACT.scanning = s; if (S.mode === 'activity') renderActivity(); }).catch(() => {});
  S.mode = 'activity';
  closeSearch(false);
  renderTabs(); renderPlaces(); renderPath();
  $('#banner').hidden = true; $('#info').hidden = true; $('#timebar').hidden = true;
  renderActivity();
  pollDaemon(true);
}

// live views redraw every second or so while a job runs: not while a button is being pressed, or
// the press and the release land on different elements and the click is lost
let pressed = false, heldRedraw = null;
document.addEventListener('pointerdown', () => { pressed = true; }, true);
document.addEventListener('pointerup', () => { pressed = false; setTimeout(() => { const f = heldRedraw; heldRedraw = null; f?.(); }, 0); }, true);
document.addEventListener('pointercancel', () => { pressed = false; }, true);
const held = f => { if (!pressed) return false; heldRedraw = f; return true; };

function renderActivity() {
  if (held(renderActivity)) return;
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
      <p class="muted${cur.stoppable ? ' act-side-foot' : ''}"><span>Started ${rel(cur.started_at)} · ${REASON[cur.reason] || ''}${eta(cur, now) ? ` · ${eta(cur, now)}` : ''}</span>${cur.stoppable ? `
        <button class="btn sm" type="button" data-job-stop="${cur.id}" title="Stops at the next file; running it again carries on">Stop</button>` : ''}</p>
    </section>`;
  } else {
    const next = d.drives.filter(x => x.present && x.next_at).sort((a, b) => a.next_at - b.next_at)[0];
    now_ = `<section class="act-card idle">
      <div class="act-head"><span class="act-dot"></span><h2>Up to date</h2></div>
      <p>${next ? `Next: a scan of <b>${esc(rootName(next.path))}</b> ${rel(next.next_at)}.` : d.rescan_every_secs ? 'Nothing is due: drives that are away are scanned when they come back.' : 'Drives are scanned when you ask (rescan button, or <code>backdown index</code>).'}</p>
    </section>`;
  }
  // the side lane (a rescue) runs beside whatever the worker is doing
  if (d.side) {
    const j = d.side, p = pct(j);
    now_ += `<section class="act-card now side">
      <div class="act-head"><span class="pulse"></span><h2>${esc(jobName(j))}</h2><span class="act-pct">${p != null ? Math.round(p * 100) + '%' : ''}</span></div>
      <div class="meter${p == null ? ' unknown' : ''}"><i style="width:${p != null ? (p * 100).toFixed(1) : 100}%"></i></div>
      <p>${esc(j.progress || 'starting…')}</p>
      <p class="muted act-side-foot"><span>Started ${rel(j.started_at)} · runs beside scans and hashing${eta(j, now) ? ` · ${eta(j, now)}` : ''}</span>
        <button class="btn sm" type="button" data-job-stop="${j.id}">Stop</button></p>
    </section>`;
  }
  const queue = d.queued.length ? `<ol class="act-list">${d.queued.map((j, i) => `
      <li><span class="ji">${JOB_ICON[j.kind] || ''}</span><span class="jn">${i + 1}. ${esc(jobName(j))}</span><span class="pill">${REASON[j.reason] || ''}</span><span class="muted">queued ${rel(j.queued_at)}
        <button class="act-x" type="button" data-job-stop="${j.id}" title="Take it off the queue" aria-label="Take ${esc(jobName(j))} off the queue">×</button></span></li>`).join('')}</ol>`
    : '<p class="muted pad">Nothing waiting.</p>';
  const recent = d.recent.length ? `<ol class="act-list">${d.recent.map(j => `
      <li class="${j.state}"><span class="ji ${j.state}">${j.state === 'failed' ? '<svg viewBox="0 0 24 24"><path d="M12 8v5M12 16h.01"/><circle cx="12" cy="12" r="9"/></svg>' : j.state === 'stopped' ? '<svg viewBox="0 0 24 24"><rect x="7" y="7" width="10" height="10" rx="1.5"/></svg>' : '<svg viewBox="0 0 24 24"><path d="m5 12.5 4.5 4.5L19 7.5"/></svg>'}</span>
        <span class="jn">${esc(jobName(j))}<small>${esc(j.state === 'failed' ? j.error || 'failed' : j.summary)}</small></span>
        <span class="pill">${REASON[j.reason] || ''}</span>
        <span class="muted" title="${esc(when(j.finished_at))}">${rel(j.finished_at)}${j.started_at && j.finished_at ? ` · took ${dur(j.finished_at - j.started_at)}` : ''}</span></li>`).join('')}</ol>`
    : '<p class="muted pad">Nothing has run since the daemon started.</p>';
  const welcome = S.roots.length ? '' : `<section class="act-card welcome"><div class="act-head"><h2>Welcome to BackDown</h2></div>
      <p>Nothing is indexed yet. Below are the drives connected to this computer: choose <b>Index this</b> on each one you want to know about. BackDown only reads them.</p>
      <p class="muted">Each drive appears on the left when its scan is done. Then <b>Duplicates</b> shows what's stored twice, <b>What's here</b> what things are, and its <b>Report</b> button the whole picture.</p>
      <form class="welcome-folder" id="welcome-folder"><label>Or a folder: <input id="welcome-path" spellcheck="false" placeholder="${esc(d.db.includes('\\') ? 'D:\\Photos' : '/media/me/Photos')}"></label><button class="btn sm" type="submit">Index it</button></form></section>`;
  view.innerHTML = `<div class="activity">
    ${welcome}
    ${now_}
    ${notesSection()}
    <section><h3>Up next</h3>${queue}</section>
    <section><h3>Drives and schedule</h3>${drivesTable(d.drives, true)}</section>
    ${connectedDrives()}
    ${ACT.mirrors?.length ? `<section><h3>Mirrors</h3><ol class="act-list">${ACT.mirrors.map(m => `
      <li class="${m.last_verify_ok === false ? 'failed' : 'done'}"><span class="ji ${m.last_verify_ok === false ? 'failed' : 'done'}">${JOB_ICON['mirror-sync']}</span>
        <span class="jn">${esc(rootName(m.source))} → ${esc(m.target)}<small>${esc(m.last_verify_summary || 'not verified yet')}</small>${mirrorNote(m)}</span>
        <span class="pill ${m.connected ? 'ok' : 'off'}">${m.connected ? 'both drives here' : 'a drive isn’t connected'}</span>
        <span class="muted">synced ${rel(m.last_sync)} · verified ${rel(m.last_verify)}</span></li>`).join('')}</ol></section>` : ''}
    <section><h3>Recent <a class="btn sm" href="/api/custody" download title="Every copy, move, deletion, link and merge BackDown made, as a signed, hash-chained export anyone can verify">Chain of custody</a></h3>${recent}</section>
    <p class="act-foot muted">BackDown ${esc(d.version)} · running since ${rel(d.started_at)} · ${d.rescan_every_secs ? `rescans each drive every ${dur(d.rescan_every_secs)}` : 'rescans on request'} ·
      drives checked ${rel(d.checked_at)} · index <code>${esc(d.db)}</code>${scanWays()}</p>
  </div>`;
  view.scrollTop = keepScroll;
}

// how rescans are made (the Settings view changes it through POST /api/scan-settings)
function scanWays() {
  const a = ACT.scanning, s = a?.settings;
  if (!s) return '';
  const safety = [s.walk_every_scans ? `every ${s.walk_every_scans} scans` : '', s.walk_every_days ? `every ${s.walk_every_days} days` : ''].filter(Boolean).join(' or ');
  return s.fast
    ? `<br>Rescans use the drive’s change log or file table when they can (<span title="${esc(`change log: ${a.journal}; file table: ${a.table}`)}">where</span>)${safety ? `, and walk every folder ${safety} to be sure` : ''}.`
    : '<br>Every scan walks every folder (the change log and file table are turned off in settings).';
}

function drivesTable(drives, live) {
  if (!drives.length) return live ? '<p class="muted pad">No drives indexed yet: choose <b>Index this</b> on one below.</p>'
    : '<p class="muted pad">No drives indexed yet: <code>backdown index /media/my-drive</code></p>';
  const label = x => ({
    scanning: '<span class="pill busy">Scanning now</span>', queued: '<span class="pill busy">Waiting its turn</span>',
    away: '<span class="pill off">Not connected</span>', due: '<span class="pill warn">Due now</span>',
    'on-request': '<span class="pill">On request</span>', scheduled: '<span class="pill ok">Connected</span>',
  }[x.state] || '');
  const next = x => x.state === 'away' ? 'when it’s plugged back in' : x.state === 'on-request' ? '—' : x.state === 'scanning' ? 'now' : x.state === 'queued' ? 'next in line' : x.next_at ? rel(x.next_at) : '—';
  return `<table class="list act-drives"><thead><tr><th>Drive</th><th>Status</th><th>Last scan</th>${live ? '<th>Next scan</th>' : ''}<th class="n">Files</th><th class="n">Size</th>${live ? '<th></th>' : ''}</tr></thead><tbody>
    ${drives.map(x => { const r = S.roots.find(r => r.path === x.path); return `<tr>
      <td><b>${esc(rootName(x.path))}</b>${pill(healthOfPath(x.path), { small: true })}<small class="muted">${esc(x.path)}</small></td>
      <td>${live ? label(x) : ''}</td>
      <td title="${esc(when(x.last_scan_at))}">${x.last_scan_at ? rel(x.last_scan_at) : 'never finished'}${x.last_how ? `<small class="muted act-how ${esc(x.last_method || '')}" title="${esc(x.last_how)}">${esc(x.last_how)}</small>` : ''}</td>
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
  // a Pro feature: say how to get it
  if (e.refusal) b.insertAdjacentHTML('beforeend', ` <button class="btn sm" type="button" data-set-go="licence">Add a licence key</button>`);
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
  if (t.closest('#vol-again')) return probeVolumes();
  const js = t.closest('[data-job-stop]');
  if (js && S.mode === 'activity') { js.disabled = true; return stopJob(+js.dataset.jobStop); }
  const ix = t.closest('[data-index-path]');
  if (ix) {
    ix.disabled = true;
    return fetch(`/api/index?${new URLSearchParams({ path: ix.dataset.indexPath })}`, { method: 'POST', headers: { 'X-BackDown': '1' }, credentials: 'same-origin' })
      .then(r => r.json().then(j => { if (!r.ok || j.error) throw new Error(j.error || r.statusText); toast('Indexing: it shows up in the sidebar when it’s done'); if (!polling) pollDaemon(true); }))
      .catch(toastError);
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
  if (t.closest('#banner-mail')) { const m = maildirHere(); return m && openMaildir(m.key, m.name); }
  const th = t.closest('#view th[data-sort]');
  if (th) { const k = th.dataset.sort; S.sort = { key: k, asc: S.sort.key === k ? !S.sort.asc : k === 'name' }; return renderView(); }
  if (t.closest('[data-find-more]')) { findMore(); return rerunSearch(); }
  if (t.closest('[data-find-scope]')) { setScope('all'); return rerunSearch(); }
  if (t.closest('[data-find-earlier]')) { setEarlier(true); return rerunSearch(); }
  const hit = t.closest('[data-hit-dir]');
  if (hit) return go(hit.dataset.hitDir, { select: hit.dataset.hitSel || null, scan: hit.dataset.hitScan ? +hit.dataset.hitScan : null });
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
mountFind(rerunSearch);
$('#dupes-btn').onclick = showDupes;
$('#activity-btn').onclick = showActivity;
$('#entities-btn').onclick = showEntities;
$('#plan-btn').onclick = () => showPlan();
$('#wipe-btn').onclick = () => showWipe();
$('#rescue-btn').onclick = showRescue;
$('#settings-btn').onclick = () => showSettings();
// first run: index a folder by its path
document.addEventListener('submit', e => {
  if (e.target.id !== 'welcome-folder') return;
  e.preventDefault();
  const path = $('#welcome-path').value.trim();
  if (!path) return $('#welcome-path').focus();
  fetch(`/api/index?${new URLSearchParams({ path })}`, { method: 'POST', headers: { 'X-BackDown': '1' }, credentials: 'same-origin' })
    .then(r => r.json().then(j => { if (!r.ok || j.error) throw new Error(j.error || r.statusText); toast('Indexing: it shows up on the left when it’s done'); if (!polling) pollDaemon(true); }))
    .catch(toastError);
});
$('#time').addEventListener('input', e => setScan(+e.target.value));
$('#time-now').onclick = () => setScan(S.timeline.length - 1);

document.addEventListener('keydown', e => {
  if (!$('#viewer').hidden || menuOpen() || document.querySelector('.help-wrap')) return; // they have their own keys
  const typing = e.target.closest('input, textarea, select');
  const ctrl = e.ctrlKey || e.metaKey, k = e.key;
  if (ctrl && e.shiftKey && (k === 'F' || k === 'f')) { e.preventDefault(); setScope('all'); openSearch(); return rerunSearch(); }
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
loadFeatures();
mountHealth({ onRescue: rescueDrive, onChange: () => { renderPlaces(); if (S.mode === 'activity') renderActivity(); } });
mountMachines({
  // a machine's page or folders take over the view, like Duplicates does
  enter: enterView,
  leave: () => { S.mode = 'browse'; showCurrent(); },
  refreshPath: () => renderPath(),
  isMode: m => S.mode === m,
  openLocal: (dir, sel) => { $('#timebar').hidden = S.timeline.length < 2; go(dir, { select: sel, scan: null }); },
});
boot().then(restoreTabs).then(() => pollDaemon(true));
checkUpdates();

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
