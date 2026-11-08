// The Places view: photos and videos found by where they were taken and what they're tagged with
// (docs/TAGGING.md). A search is a place to search around (an airport, a town, a spot on the map)
// and how far, tags that must all be there, and dates. The map shows where the matches are, the
// panel beside it what they're tagged with (click to narrow), and the grid the photos, nearest or
// newest first. Choosing photos shows their tags, decisions and copies, to tag them by hand (an
// event, an album, whose they are). Reading the dates and places of a drive's photos runs in the
// background service. Everything goes through /api/places* (places.rs in the Rust).

import { fileIcon, kindOf } from './icons.js';
import { openViewer } from './viewers.js';
import { menu, toast } from './extras.js';
import { GeoMap, circleBounds } from './geomap.js';

const $ = s => document.querySelector(s);
const esc = s => String(s ?? '').normalize('NFC').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const count = n => (n ?? 0).toLocaleString();
const plural = (n, one, many = `${one}s`) => `${count(n)} ${n === 1 ? one : many}`;
const DEFAULT_KM = 120;

// what each kind of tag is called, as a heading and on its own
const KINDS = {
  country: ['Countries', 'Country'], region: ['Regions', 'Region'], city: ['Towns and cities', 'Town'],
  airport: ['Nearest airports', 'Airport'], year: ['Years', 'Year'], event: ['Events', 'Event'], trip: ['Trips', 'Trip'],
  place: ['Places', 'Place'], album: ['Albums', 'Album'], memory: ['Memories', 'Memory'], owner: ['Whose', 'Owner'], kind: ['Kinds', 'Kind'],
};
const kindName = (k, many = false) => KINDS[k]?.[many ? 0 : 1] || (k.charAt(0).toUpperCase() + k.slice(1));
// the kinds a person adds by hand
const OWN_KINDS = ['event', 'trip', 'album', 'place', 'memory', 'owner'];
const SOURCE = { exif: 'from the photo', geo: 'from place data', user: 'by you' };
const FLAGS = [['keep', 'Keep'], ['drop', 'Drop'], ['reviewed', 'Reviewed']];

const P = {
  centre: null, km: DEFAULT_KM, tags: [], from: '', to: '', sort: 'auto',
  data: null, hits: [], sel: new Set(), anchor: null, item: null, itemSeq: 0,
  side: 'refine', open: new Set(), allTags: null, tagFilter: '', values: new Map(),
  seq: 0, sugSeq: 0, sug: [], sugAt: -1, map: null, job: null, lastDone: null, refreshed: 0, renaming: null,
};
let hooks = { enter: () => {}, isMode: () => false, openLocal: () => {}, thumb: () => '', roots: () => [], copyText: () => {}, toastError: () => {} };

try {
  const s = JSON.parse(sessionStorage.getItem('bd.places') || 'null');
  if (s) Object.assign(P, { centre: s.centre || null, km: +s.km || DEFAULT_KM, tags: s.tags || [], from: s.from || '', to: s.to || '', sort: s.sort || 'auto' });
} catch { /* a fresh search */ }
const remember = () => { try { sessionStorage.setItem('bd.places', JSON.stringify({ centre: P.centre, km: P.km, tags: P.tags, from: P.from, to: P.to, sort: P.sort })); } catch { /* private window */ } };

export const PLACES_ICON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 21s-7-6.2-7-11.5a7 7 0 0 1 14 0C19 14.8 12 21 12 21z"/><circle cx="12" cy="9.5" r="2.5"/></svg>';

export function mountPlaces(h) {
  hooks = { ...hooks, ...h };
  document.addEventListener('click', onClick);
  document.addEventListener('input', onInput);
  document.addEventListener('change', onChange);
  document.addEventListener('submit', onSubmit);
  document.addEventListener('keydown', onKey, true);
  document.addEventListener('dblclick', onDblClick);
  document.addEventListener('pointerover', onHover);
}

async function post(path, body = null, params = {}) {
  const r = await fetch(`/api/${path}?${new URLSearchParams(params)}`, {
    method: 'POST', credentials: 'same-origin',
    headers: { 'X-BackDown': '1', 'Content-Type': 'application/json' },
    body: body ? JSON.stringify(body) : undefined,
  });
  const j = await r.json().catch(() => ({ error: r.statusText }));
  if (!r.ok || j.error) throw new Error(j.error || r.statusText);
  return j;
}

// ---------- the view

export function showPlaces() {
  hooks.enter('places');
  $('#view').innerHTML = `<div class="pl">
    <div class="pl-main">
      <header class="pl-head">
        <div><h2>Places</h2><p class="muted" id="pl-sum">Photos and videos by where and when they were taken</p></div>
        <button class="btn" type="button" id="pl-read" aria-haspopup="menu">${PLACES_ICON}Read dates and places…</button>
      </header>
      <div class="pl-job" id="pl-job" hidden></div>
      <div class="pl-q">
        <div class="pl-box" id="pl-box">
          <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="11" cy="11" r="7"/><path d="m20 20-3.5-3.5"/></svg>
          <span class="pl-chips" id="pl-chips"></span>
          <input id="pl-input" type="search" autocomplete="off" spellcheck="false" placeholder="An airport, town, tag or year: HKG, Paris, Boat show, 2019"
            role="combobox" aria-autocomplete="list" aria-expanded="false" aria-controls="pl-sug" aria-label="Search by place, tag or year">
          <div class="pl-sug" id="pl-sug" role="listbox" hidden></div>
        </div>
        <div class="pl-opts">
          <label class="pl-km" title="How far from the place searched around">within <input id="pl-km" type="number" min="0.1" max="20000" step="any" inputmode="decimal"> km</label>
          <label>from <input id="pl-from" type="date"></label>
          <label>to <input id="pl-to" type="date"></label>
          <button class="btn sm" type="button" id="pl-clear">Clear</button>
        </div>
      </div>
      <div class="pl-map" id="pl-map"></div>
      <div class="pl-bar">
        <span class="pl-count" id="pl-count" role="status"></span>
        <div class="seg txt" role="group" aria-label="Order">
          <button type="button" data-pl-sort="nearest" title="Nearest the place searched around first">Nearest</button>
          <button type="button" data-pl-sort="newest">Newest</button>
          <button type="button" data-pl-sort="oldest">Oldest</button>
        </div>
        <button class="btn sm" type="button" id="pl-selall" title="Choose every photo shown (Ctrl+A)">Select all</button>
        <button class="btn sm" type="button" id="pl-copy" title="Copy the path of every copy of every match">Copy paths</button>
      </div>
      <div class="pl-grid" id="pl-grid" aria-label="Photos found"></div>
      <div class="pl-more" id="pl-more"></div>
    </div>
    <aside class="pl-side" id="pl-side" aria-label="Refine and tag"></aside>
  </div>`;
  P.map = new GeoMap($('#pl-map'), {
    onPick: (lat, lon, km) => aroundPoint(lat, lon, km),
    onArea: (lat, lon, km) => aroundPoint(lat, lon, km),
  });
  syncInputs();
  renderChips();
  renderJob();
  search({ fit: true });
}

function syncInputs() {
  const km = $('#pl-km');
  if (!km) return;
  km.value = P.km; km.disabled = !P.centre;
  $('#pl-from').value = P.from; $('#pl-to').value = P.to;
  for (const b of document.querySelectorAll('[data-pl-sort]')) {
    const s = b.dataset.plSort;
    b.setAttribute('aria-pressed', String(sortNow() === s));
    b.disabled = s === 'nearest' && !P.centre;
  }
}
const sortNow = () => P.sort === 'auto' ? (P.centre ? 'nearest' : 'newest') : P.sort;

function query(extra = {}) {
  const q = new URLSearchParams();
  if (P.centre) { q.set('lat', P.centre.lat); q.set('lon', P.centre.lon); q.set('km', P.km); q.set('label', P.centre.label); }
  for (const t of P.tags) q.append('tag', t.id);
  if (P.from) q.set('from', P.from);
  if (P.to) q.set('to', P.to);
  if (P.sort !== 'auto') q.set('sort', P.sort);
  for (const [k, v] of Object.entries(extra)) q.set(k, v);
  return q;
}

async function get(path, q) {
  const r = await fetch(`/api/${path}?${q}`, { credentials: 'same-origin' });
  const j = await r.json().catch(() => ({ error: r.statusText }));
  if (!r.ok || j.error) throw new Error(j.error || r.statusText);
  return j;
}

// run the search again: from the top (fit: frame the map on the answer), or the next page (more)
async function search({ fit = false, more = false, keep = false } = {}) {
  remember();
  const seq = ++P.seq;
  const offset = more ? P.hits.length : 0;
  // a refresh keeps as many photos as were shown
  const limit = keep ? Math.max(120, Math.min(1000, P.hits.length)) : 120;
  let j;
  try { j = await get('places', query({ offset, limit })); } catch (e) {
    if (seq === P.seq) { hooks.toastError(e); if (!more) { P.data = null; P.hits = []; renderAll(); } }
    return;
  }
  if (seq !== P.seq || !hooks.isMode('places')) return;
  P.data = j;
  P.hits = more ? [...P.hits, ...j.found.hits] : j.found.hits;
  // what's chosen stays chosen while it's still found
  const here = new Set(P.hits.map(h => h.key));
  for (const k of [...P.sel]) if (!here.has(k)) P.sel.delete(k);
  if (P.anchor && !here.has(P.anchor)) P.anchor = null;
  P.map.setPoints(j.found.points);
  P.map.setCircle(P.centre ? { lat: P.centre.lat, lon: P.centre.lon, km: P.km } : null);
  const box = P.centre ? circleBounds(P.centre.lat, P.centre.lon, P.km) : j.found.bounds;
  P.map.setBounds(j.found.bounds || box);
  if (fit) P.map.fit(box || null);
  renderAll();
}

function renderAll() {
  if (!hooks.isMode('places')) return;
  syncInputs();
  renderSummary();
  renderGrid();
  renderSide();
  markMap();
}

function renderSummary() {
  const d = P.data, sum = $('#pl-sum'), c = $('#pl-count');
  if (!d) { c.textContent = ''; return; }
  sum.textContent = d.tagged
    ? `${plural(d.located_all, 'photo or video', 'photos and videos')} with a place, ${count(d.tagged)} tagged in all`
    : 'Photos and videos by where and when they were taken';
  const f = d.found;
  c.innerHTML = f.total
    ? `<b>${count(f.total)}</b> found${f.located < f.total ? ` · ${count(f.located)} on the map` : ''}${P.sel.size ? ` · <b>${count(P.sel.size)}</b> chosen` : ''}`
    : '';
  $('#pl-selall').disabled = !P.hits.length;
  $('#pl-copy').disabled = !f.total;
}

// ---------- the search box: chips for what's asked, suggestions as you type

function renderChips() {
  const chips = [];
  if (P.centre) {
    chips.push(`<span class="pl-chip near" title="${esc(P.centre.detail || P.centre.label)}">◎ within ${fmtKm(P.km)} of <b>${esc(P.centre.label)}</b><button type="button" data-pl="un-near" aria-label="Stop searching around ${esc(P.centre.label)}">×</button></span>`);
  }
  for (const t of P.tags) {
    chips.push(`<span class="pl-chip"><span class="muted">${esc(kindName(t.kind))}</span> ${esc(t.value)}<button type="button" data-pl-untag-q="${t.id}" aria-label="Stop looking for ${esc(t.value)}">×</button></span>`);
  }
  if (P.from || P.to) {
    chips.push(`<span class="pl-chip"><span class="muted">Taken</span> ${esc(dateRange(P.from, P.to))}<button type="button" data-pl="un-dates" aria-label="Any date">×</button></span>`);
  }
  $('#pl-chips').innerHTML = chips.join('');
  $('#pl-input').placeholder = chips.length ? 'Add a place, tag or year…' : 'An airport, town, tag or year: HKG, Paris, Boat show, 2019';
}

function dateRange(from, to) {
  const y = /^(\d{4})-01-01$/.exec(from), z = /^(\d{4})-12-31$/.exec(to);
  if (y && z && y[1] === z[1]) return y[1];
  return from && to ? `${from} to ${to}` : from ? `from ${from}` : `up to ${to}`;
}

async function suggest(q) {
  const seq = ++P.sugSeq;
  q = q.trim();
  if (!q) return closeSug();
  const extra = [];
  // a year, a month or a day: that span of time
  const ym = /^(\d{4})(?:-(\d{2}))?(?:-(\d{2}))?$/.exec(q);
  if (ym) {
    const [, y, m, d] = ym;
    const last = m ? String(new Date(+y, +m, 0).getDate()).padStart(2, '0') : '31';
    extra.push({ type: 'dates', from: `${y}-${m || '01'}-${d || '01'}`, to: `${y}-${m || '12'}-${d || last}`, label: `Taken in ${q}` });
  }
  let j = { centres: [], tags: [] };
  try { j = await get('places-suggest', new URLSearchParams({ q })); } catch { /* what's typed still stands */ }
  if (seq !== P.sugSeq) return;
  // around a place first (HKG means "near the airport"), then tags of that name, then dates
  P.sug = [
    ...j.centres.map(c => ({ type: 'centre', centre: c })),
    ...j.tags.filter(t => !P.tags.some(x => x.id === t.id)).map(t => ({ type: 'tag', ...t })),
    ...extra,
  ];
  P.sugAt = P.sug.length ? 0 : -1;
  renderSug();
}

function renderSug() {
  const el = $('#pl-sug');
  if (!el) return;
  if (!P.sug.length) { el.innerHTML = '<div class="pl-sug-none muted">Nothing by that name: try an airport’s code (HKG), a town, or a year</div>'; el.hidden = !$('#pl-input').value.trim(); return; }
  const row = (s, i) => {
    const on = i === P.sugAt ? ' on' : '';
    if (s.type === 'tag') return `<button type="button" class="pl-s${on}" role="option" aria-selected="${!!on}" data-sug="${i}"><span class="pl-si">#</span><span><b>${esc(s.value)}</b><small>${esc(kindName(s.kind))} · ${plural(s.count, 'photo')}</small></span></button>`;
    if (s.type === 'centre') {
      const c = s.centre, icon = c.kind === 'airport' ? '✈' : c.kind === 'point' ? '⌖' : '◎';
      return `<button type="button" class="pl-s${on}" role="option" aria-selected="${!!on}" data-sug="${i}"><span class="pl-si">${icon}</span><span><b>Around ${esc(c.label)}</b><small>${esc(c.detail || 'a point on the map')}</small></span></button>`;
    }
    return `<button type="button" class="pl-s${on}" role="option" aria-selected="${!!on}" data-sug="${i}"><span class="pl-si">▦</span><span><b>${esc(s.label)}</b><small>${esc(dateRange(s.from, s.to))}</small></span></button>`;
  };
  const head = (t, i) => (i === 0 || P.sug[i - 1].type !== t) ? `<div class="pl-sh">${{ tag: 'Tagged', centre: 'Search around', dates: 'When' }[t]}</div>` : '';
  el.innerHTML = P.sug.map((s, i) => head(s.type, i) + row(s, i)).join('');
  el.hidden = false;
  $('#pl-input').setAttribute('aria-expanded', 'true');
}

function closeSug() {
  P.sug = []; P.sugAt = -1;
  const el = $('#pl-sug');
  if (el) { el.hidden = true; el.innerHTML = ''; }
  $('#pl-input')?.setAttribute('aria-expanded', 'false');
}

function pickSug(i) {
  const s = P.sug[i];
  if (!s) return;
  if (s.type === 'tag') addTagQuery({ id: s.id, kind: s.kind, value: s.value });
  else if (s.type === 'centre') { P.centre = s.centre; if (P.sort === 'nearest') P.sort = 'auto'; }
  else { P.from = s.from; P.to = s.to; }
  $('#pl-input').value = '';
  closeSug(); renderChips();
  search({ fit: true });
}

function addTagQuery(t) {
  if (!P.tags.some(x => x.id === t.id)) P.tags.push({ id: t.id, kind: t.kind, value: t.value });
}

function aroundPoint(lat, lon, km) {
  P.centre = { kind: 'point', label: `${lat.toFixed(4)}, ${lon.toFixed(4)}`, detail: 'a point on the map', lat, lon };
  if (km) P.km = km;
  renderChips();
  search({ fit: !km });
  // name the point after the town nearest it
  get('places-suggest', new URLSearchParams({ q: `${lat},${lon}` })).then(j => {
    const c = j.centres?.[0];
    if (c && P.centre?.lat === lat && P.centre?.lon === lon) { P.centre = { ...P.centre, detail: c.detail || P.centre.detail }; renderChips(); }
  }).catch(() => {});
}

const fmtKm = km => km == null ? '' : km < 1 ? `${Math.round(km * 1000)} m` : km < 10 ? `${(Math.round(km * 10) / 10).toLocaleString()} km` : `${Math.round(km).toLocaleString()} km`;

// ---------- the grid

const MONTHS = new Intl.DateTimeFormat(undefined, { month: 'long', year: 'numeric' });
const DAY = new Intl.DateTimeFormat(undefined, { day: 'numeric', month: 'short', year: 'numeric' });
const dayOf = d => d ? DAY.format(new Date(`${d}T12:00:00`)) : '';
const BANDS = [1, 5, 10, 25, 50, 120, 250, 500, 1000, 2500, 5000, 20040];
function groupOf(h) {
  if (sortNow() === 'nearest') {
    if (h.km == null) return 'Without a place';
    const i = BANDS.findIndex(b => h.km <= b), hi = BANDS[i], lo = BANDS[i - 1];
    return lo ? `${fmtKm(lo)} to ${fmtKm(hi)}` : `Within ${fmtKm(hi)}`;
  }
  return h.date ? MONTHS.format(new Date(`${h.date.slice(0, 7)}-15T12:00:00`)) : 'Without a date';
}
const tagValue = (h, kind) => h.tags.find(t => t.kind === kind)?.value;
const where = h => [tagValue(h, 'city'), tagValue(h, 'country')].filter((v, i, a) => v && a.indexOf(v) === i).join(', ');

function pic(h, cls = 'pt-img') {
  const c = h.copies[0];
  if (!c) return `<div class="pt-gone">${fileIcon('photo.jpg', 40)}<small>No copy on an indexed drive now</small></div>`;
  if (kindOf(c.name) === 'video') return `<video class="${cls}" src="/api/content?id=${c.id}#t=0.5" preload="metadata" muted playsinline></video><span class="g-play" aria-hidden="true">▶</span>`;
  return hooks.thumb({ id: c.id, name: c.name, size: c.size }, cls) || `<div class="pt-icon">${fileIcon(c.name, 48)}</div>`;
}

function tile(h) {
  const flag = h.flags.includes('drop') ? ' dropped' : '';
  const badges = [h.flags.includes('keep') ? '<span class="pt-flag keep" title="Keep">★</span>' : '', h.flags.includes('drop') ? '<span class="pt-flag drop" title="Drop">✕</span>' : '', h.flags.includes('reviewed') ? '<span class="pt-flag" title="Reviewed">✓</span>' : ''].join('');
  const own = h.tags.filter(t => OWN_KINDS.includes(t.kind)).map(t => t.value);
  const title = [h.copies[0]?.name, dayOf(h.date), where(h), ...own].filter(Boolean).join(' · ');
  return `<figure class="pt${P.sel.has(h.key) ? ' sel' : ''}${flag}" data-k="${h.key}" title="${esc(title)}">
    <div class="pt-pic">${pic(h)}</div>
    ${h.km != null ? `<span class="pt-km">${fmtKm(h.km)}</span>` : ''}${badges ? `<span class="pt-flags">${badges}</span>` : ''}
    <figcaption><span>${esc(where(h) || tagValue(h, 'airport') || '—')}</span><small>${esc(dayOf(h.date))}</small></figcaption>
  </figure>`;
}

function renderGrid() {
  const g = $('#pl-grid'), more = $('#pl-more'), d = P.data;
  if (!d) { g.innerHTML = '<p class="muted">Looking…</p>'; more.innerHTML = ''; return; }
  if (!d.tagged) { g.innerHTML = welcome(); more.innerHTML = ''; return; }
  if (!P.hits.length) {
    const wider = P.centre ? [300, 1000, 5000].filter(k => k > P.km).slice(0, 2) : [];
    g.innerHTML = `<div class="pl-none"><h3>Nothing matches</h3><p class="muted">${P.centre ? 'Widen the circle, or take' : 'Take'} a filter off${P.centre && !d.located_all ? ': no photo has a position yet' : ''}.</p>
      <div class="pl-none-acts">${wider.map(k => `<button class="btn sm" type="button" data-pl-km="${k}">Within ${fmtKm(k)}</button>`).join('')}<button class="btn sm" type="button" data-pl="clear">Clear the search</button></div></div>`;
    more.innerHTML = '';
    return;
  }
  let out = '', last = null;
  const counts = new Map();
  for (const h of P.hits) { const k = groupOf(h); counts.set(k, (counts.get(k) || 0) + 1); }
  for (const h of P.hits) {
    const k = groupOf(h);
    if (k !== last) { out += `<h4 class="pl-gh">${esc(k)} <small>${count(counts.get(k))}${P.hits.length < d.found.total ? '+' : ''}</small></h4>`; last = k; }
    out += tile(h);
  }
  g.innerHTML = out;
  const left = d.found.total - P.hits.length;
  more.innerHTML = left > 0 ? `<button class="btn" type="button" data-pl="more">Show ${count(Math.min(120, left))} more</button><span class="muted">of ${count(left)} not shown</span>` : '';
}

function welcome() {
  return `<div class="pl-welcome">
    <div class="pl-w-icon" aria-hidden="true">${PLACES_ICON}</div>
    <h3>Find your photos by where and when they were taken</h3>
    <p>Most photos and videos record when and where they were taken. BackDown reads that from the files and names the country, region, town and nearest airport from place data built into it, without going online. Then search: <i>within 120 km of HKG</i>, <i>Paris in 2019</i>, an event you name yourself.</p>
    <div class="pl-w-acts"><button class="btn primary" type="button" data-pl="read-all">Read every drive’s photos</button><button class="btn" type="button" data-pl="read-menu">Choose a drive…</button></div>
    <p class="muted small">It runs in the background (see Activity), reads each photo once, and touches nothing on the drives. In a terminal: <code>backdown tag exif FOLDER --geo</code>.</p>
  </div>`;
}

function markMap() {
  if (!P.map) return;
  const marks = P.hits.filter(h => P.sel.has(h.key) || h.key === P.hover).map(h => [h.lat, h.lon]);
  P.map.setMarks(marks);
}

// ---------- the side panel: refine (facets), every tag, or what's chosen

function renderSide() {
  const el = $('#pl-side');
  if (!el) return;
  if (P.sel.size) { el.innerHTML = selectionHtml(); return; }
  const tabs = `<div class="pl-tabs" role="tablist">
    <button type="button" role="tab" data-pl-side="refine" aria-selected="${P.side === 'refine'}">Refine</button>
    <button type="button" role="tab" data-pl-side="tags" aria-selected="${P.side === 'tags'}">All tags</button></div>`;
  el.innerHTML = tabs + (P.side === 'tags' ? allTagsHtml() : facetsHtml());
  if (P.side === 'tags' && !P.allTags) loadAllTags();
}

function facetsHtml() {
  const f = P.data?.found;
  if (!f || !f.total) return '<p class="muted pl-pad">What the matches are tagged with shows here, to narrow them down.</p>';
  const by = new Map();
  for (const x of f.facets) { if (!by.has(x.kind)) by.set(x.kind, []); by.get(x.kind).push(x); }
  const active = new Set(P.tags.map(t => t.id));
  let out = '';
  for (const [kind, list] of by) {
    const rows = kind === 'year' ? [...list].sort((a, b) => a.value.localeCompare(b.value)) : list;
    const max = Math.max(...rows.map(r => r.count));
    const open = P.open.has(kind) || kind === 'year';
    const shown = open ? rows : rows.slice(0, 6);
    out += `<section class="pl-fg"><h4>${esc(kindName(kind, true))}</h4>
      ${shown.map(r => `<button type="button" class="pl-f" data-pl-facet="${r.id}" aria-pressed="${active.has(r.id)}" title="${esc(r.value)}: ${plural(r.count, 'photo')}"><i style="width:${Math.max(4, Math.round(r.count / max * 100))}%"></i><span class="pl-fv">${esc(r.value)}</span><span class="pl-fn">${count(r.count)}</span></button>`).join('')}
      ${rows.length > 6 && kind !== 'year' ? `<button type="button" class="pl-fmore" data-pl-expand="${esc(kind)}">${open ? 'Fewer' : `All ${count(rows.length)}`}</button>` : ''}
    </section>`;
  }
  return `<div class="pl-facets">${out}</div>`;
}

async function loadAllTags() {
  try { P.allTags = (await get('places-tags', new URLSearchParams())).tags; } catch (e) { hooks.toastError(e); P.allTags = []; }
  if (hooks.isMode('places') && !P.sel.size && P.side === 'tags') renderSide();
}

function allTagsHtml() {
  if (!P.allTags) return '<p class="muted pl-pad">Loading the tags…</p>';
  const q = P.tagFilter.trim().toLowerCase();
  const list = P.allTags.filter(t => !q || t.value.toLowerCase().includes(q) || t.kind.includes(q));
  const by = new Map();
  for (const t of list) { if (!by.has(t.kind)) by.set(t.kind, []); by.get(t.kind).push(t); }
  const order = [...by.keys()].sort((a, b) => (OWN_KINDS.includes(b) - OWN_KINDS.includes(a)) || a.localeCompare(b));
  const row = t => P.renaming === t.id
    ? `<form class="pl-tr editing" data-pl-rename-form="${t.id}"><input name="value" value="${esc(t.value)}" aria-label="New name" required maxlength="200"><button class="btn sm primary" type="submit">Save</button><button class="btn sm" type="button" data-pl="rename-cancel">Cancel</button></form>`
    : `<div class="pl-tr"><button type="button" class="pl-tv" data-pl-find="${t.id}" title="Find what carries it">${esc(t.value)}</button><span class="pl-fn">${count(t.count)}</span>
        ${t.own ? `<button type="button" class="pl-ta" data-pl-rename="${t.id}" title="Rename (a name another tag has merges the two)">Rename</button><button type="button" class="pl-ta" data-pl-delete="${t.id}" title="Delete it: it comes off every photo">Delete</button>` : ''}</div>`;
  return `<div class="pl-alltags">
    <input class="pl-tf" id="pl-tf" type="search" placeholder="Filter the tags" value="${esc(P.tagFilter)}" aria-label="Filter the tags">
    ${order.length ? order.map(k => `<section class="pl-fg"><h4>${esc(kindName(k, true))} <small class="muted">${count(by.get(k).length)}</small></h4>${by.get(k).map(row).join('')}</section>`).join('')
      : `<p class="muted pl-pad">${P.allTags.length ? 'No tag by that name.' : 'No tags yet.'}</p>`}
    <p class="muted small pl-pad">Your own tags can be renamed or deleted here; tags BackDown read from the photos can be taken off the photos they’re wrong for.</p>
  </div>`;
}

const chosen = () => P.hits.filter(h => P.sel.has(h.key));

function selectionHtml() {
  const hs = chosen(), one = hs.length === 1 ? hs[0] : null;
  const head = `<div class="pl-sel-top"><b>${plural(P.sel.size, 'photo')} chosen</b><button class="btn sm" type="button" data-pl="unselect" title="Esc">Done</button></div>`;
  return `<div class="pl-sel">${head}${one ? oneHtml(one) : manyHtml(hs)}</div>`;
}

function addForm() {
  const kind = P.addKind || 'event';
  const vals = P.values.get(kind) || [];
  return `<form class="pl-add" id="pl-add">
    <select name="kind" id="pl-add-kind" aria-label="Kind of tag">${OWN_KINDS.map(k => `<option value="${k}"${k === kind ? ' selected' : ''}>${esc(kindName(k))}</option>`).join('')}</select>
    <input name="value" list="pl-values" required maxlength="200" placeholder="${kind === 'event' ? 'Boat show 2019' : kind === 'owner' ? 'Whose they are' : 'A name'}" aria-label="Tag">
    <datalist id="pl-values">${vals.map(v => `<option value="${esc(v)}">`).join('')}</datalist>
    <button class="btn sm primary" type="submit">Add</button>
  </form>`;
}

function flagButtons(hs) {
  return `<div class="pl-flags">${FLAGS.map(([f, label]) => {
    const all = hs.length > 0 && hs.every(h => h.flags.includes(f));
    return `<button class="btn sm" type="button" data-pl-flag="${f}" aria-pressed="${all}">${label}</button>`;
  }).join('')}</div>`;
}

function oneHtml(h) {
  const it = P.item?.key === h.key ? P.item : null;
  const tags = it ? it.tags.filter(t => !['location', 'date'].includes(t.tag.kind)).map(t => ({ ...t.tag, source: t.source, confidence: t.confidence })) : h.tags;
  const name = h.copies[0]?.name || 'Not on an indexed drive now';
  const facts = [dayOf(h.date), h.km != null && P.centre ? `${fmtKm(h.km)} from ${P.centre.label}` : ''].filter(Boolean).join(' · ');
  const copies = it ? it.copies : h.copies;
  return `<div class="pl-prev">${pic(h, 'pl-prev-img')}</div>
    <h3 class="pl-name" title="${esc(name)}">${esc(name)}</h3>
    ${facts ? `<p class="muted">${esc(facts)}</p>` : ''}
    ${h.lat != null ? `<p class="pl-pos"><span class="chip">${h.lat.toFixed(5)}, ${h.lon.toFixed(5)}</span><button class="btn sm" type="button" data-pl="around">Search around it</button></p>` : '<p class="muted">Its metadata has no position.</p>'}
    <h4>Tags</h4>
    <div class="pl-tags">${tags.length ? tags.map(t => `<span class="pl-chip${t.source === 'user' || (!t.source && OWN_KINDS.includes(t.kind)) ? ' own' : ''}" title="${esc(kindName(t.kind))}${t.source ? `, ${esc(SOURCE[t.source] || t.source)}` : ''}${t.confidence != null && t.confidence < 1 ? `, ${Math.round(t.confidence * 100)}% sure` : ''}"><span class="muted">${esc(kindName(t.kind))}</span> ${esc(t.value)}<button type="button" data-pl-detach="${t.id}" aria-label="Take ${esc(t.value)} off">×</button></span>`).join('') : '<span class="muted">None yet</span>'}</div>
    ${addForm()}
    <h4>Decisions</h4>${flagButtons([h])}
    <h4>${copies.length === 1 ? 'Where it is' : `Copies <small class="muted">${count(copies.length)}</small>`}</h4>
    <ul class="pl-copies">${copies.map((c, i) => `<li><span class="pl-cp" title="${esc(c.path)}">${esc(c.path)}</span><span class="pl-cb"><button class="btn sm" type="button" data-pl-open="${i}">Open</button><button class="btn sm" type="button" data-pl-reveal="${i}">Show in folder</button></span></li>`).join('') || '<li class="muted">No indexed drive has it now.</li>'}</ul>`;
}

function manyHtml(hs) {
  const by = new Map();
  for (const h of hs) for (const t of h.tags) {
    const e = by.get(t.id) || { ...t, n: 0 };
    e.n++; by.set(t.id, e);
  }
  const tags = [...by.values()].sort((a, b) => b.n - a.n || a.value.localeCompare(b.value));
  const copies = hs.reduce((n, h) => n + h.copies.length, 0);
  const strip = hs.slice(0, 12).map(h => `<span class="pl-mini">${pic(h, 'pl-mini-img')}</span>`).join('');
  return `<div class="pl-strip">${strip}${hs.length > 12 ? `<span class="pl-mini more">+${count(hs.length - 12)}</span>` : ''}</div>
    <h4>Tags on them</h4>
    <div class="pl-tags">${tags.length ? tags.map(t => `<span class="pl-chip${OWN_KINDS.includes(t.kind) ? ' own' : ''}" title="On ${t.n} of ${hs.length}"><span class="muted">${esc(kindName(t.kind))}</span> ${esc(t.value)} <small class="muted">${t.n === hs.length ? 'all' : `${t.n} of ${hs.length}`}</small><button type="button" data-pl-detach="${t.id}" aria-label="Take ${esc(t.value)} off them all">×</button></span>`).join('') : '<span class="muted">None yet</span>'}</div>
    ${addForm()}
    <h4>Decisions</h4>${flagButtons(hs)}
    <h4>Copies</h4>
    <p class="muted">${plural(copies, 'copy', 'copies')} on the indexed drives <button class="btn sm" type="button" data-pl="copy-sel">Copy their paths</button></p>`;
}

async function loadItem(key) {
  const seq = ++P.itemSeq;
  try {
    const it = await get('places-item', new URLSearchParams({ key }));
    if (seq === P.itemSeq) { P.item = it; if (P.sel.size === 1 && P.sel.has(key)) renderSide(); }
  } catch { /* the hit's own data stands */ }
}

async function loadValues(kind) {
  if (P.values.has(kind)) return;
  P.values.set(kind, []);
  try {
    const j = await get('places-tags', new URLSearchParams({ kind }));
    P.values.set(kind, j.tags.map(t => t.value));
    const dl = $('#pl-values');
    if (dl && (P.addKind || 'event') === kind) dl.innerHTML = P.values.get(kind).map(v => `<option value="${esc(v)}">`).join('');
  } catch { /* typing still works */ }
}

// ---------- choosing photos

function choose(key, ev) {
  if (ev?.shiftKey && P.anchor) {
    const keys = P.hits.map(h => h.key), a = keys.indexOf(P.anchor), b = keys.indexOf(key);
    if (!(ev.ctrlKey || ev.metaKey)) P.sel.clear();
    for (let i = Math.min(a, b); i <= Math.max(a, b); i++) P.sel.add(keys[i]);
  } else if (ev?.ctrlKey || ev?.metaKey) {
    P.sel.has(key) ? P.sel.delete(key) : P.sel.add(key);
    P.anchor = key;
  } else {
    P.sel = new Set([key]); P.anchor = key;
  }
  chosenChanged();
}

function chosenChanged() {
  for (const el of document.querySelectorAll('#pl-grid .pt')) el.classList.toggle('sel', P.sel.has(el.dataset.k));
  if (P.sel.size === 1) { const [k] = P.sel; if (P.item?.key !== k) { P.item = null; loadItem(k); } }
  loadValues(P.addKind || 'event');
  renderSummary(); renderSide(); markMap();
}

function openHit(key, copy = 0) {
  const list = P.hits.filter(h => h.copies.length);
  const entry = h => { const c = h.copies[h.key === key ? copy : 0] || h.copies[0]; return { id: c.id, name: c.name, size: c.size, mtime: c.mtime, key: c.key, kind: 'file' }; };
  const h = list.find(x => x.key === key);
  if (!h) return toast('No indexed drive has this photo now');
  openViewer(entry(h), list.map(entry));
}

// ---------- changes: tags, flags, the tags themselves

async function change(path, body, done) {
  try {
    await post(path, body);
    if (done) toast(done);
    P.allTags = null; P.values.clear();
    if (P.item) loadItem(P.item.key);
    await search({ keep: true });
  } catch (e) { hooks.toastError(e); }
}

async function readDrive(path) {
  try {
    await post('places-read', null, path ? { path } : {});
    toast('Reading dates and places: it shows here and in Activity as it goes');
    P.job = { kind: 'places', state: 'queued', path: path || null };
    renderJob();
  } catch (e) { hooks.toastError(e); }
}

function readMenu(anchor) {
  const b = anchor.getBoundingClientRect();
  const roots = hooks.roots();
  menu(b.left, b.bottom + 4, [
    { label: 'Every drive', hint: roots.length ? plural(roots.length, 'drive') : '', act: () => readDrive(null) },
    ...(roots.length ? [{ sep: true }] : []),
    ...roots.map(r => ({ label: esc(r.name), hint: esc(r.path), act: () => readDrive(r.path) })),
    { sep: true },
    { label: 'Another folder…', act: () => { const p = prompt('Read the photos under which folder? (its full path; it has to be indexed)'); if (p?.trim()) readDrive(p.trim()); } },
  ]);
}

function renderJob() {
  const el = $('#pl-job');
  if (!el) return;
  const j = P.job;
  if (!j) { el.hidden = true; return; }
  const c = j.counts, pct = c?.total_files ? Math.min(100, Math.round(c.files / c.total_files * 100)) : null;
  const where = j.path ? ` in ${esc(j.path.split(/[\\/]/).filter(Boolean).pop() || j.path)}` : '';
  el.hidden = false;
  el.innerHTML = `<span class="pulse" aria-hidden="true"></span><span><b>${j.state === 'queued' ? 'Waiting to read' : 'Reading'} dates and places${where}</b>${j.progress ? ` · ${esc(j.progress)}` : ''}</span>
    ${pct != null ? `<span class="pl-meter" role="progressbar" aria-valuenow="${pct}" aria-valuemin="0" aria-valuemax="100"><i style="width:${pct}%"></i></span>` : ''}
    ${j.id && j.stoppable ? `<button class="btn sm" type="button" data-pl-stop="${j.id}">Stop</button>` : ''}`;
}

// the background service's state, each time app.js hears it: a read in progress shows, and its
// photos show up as it goes (every few seconds) and when it's done
export function placesDaemon(d) {
  const jobs = d?.running ? [d.current, ...(d.queued || [])].filter(Boolean) : [];
  const job = jobs.find(j => j.kind === 'places') || null;
  const done = (d?.recent || []).filter(j => j.kind === 'places' && j.finished_at);
  const newest = done.reduce((m, j) => Math.max(m, j.id), 0);
  const finished = P.lastDone != null && newest > P.lastDone ? done.find(j => j.id === newest) : null;
  P.lastDone = newest;
  P.job = job;
  if (finished) {
    if (finished.state === 'done') toast(`Read ${finished.summary}`);
    if (hooks.isMode('places')) search({ keep: true });
  } else if (job?.state === 'running' && hooks.isMode('places') && Date.now() - P.refreshed > 8000) {
    P.refreshed = Date.now();
    search({ keep: true });
  }
  if (hooks.isMode('places')) renderJob();
}

// ---------- events (document-wide, acting only while this view is up)

function onClick(e) {
  if (!hooks.isMode('places')) return;
  const t = e.target;
  if (!t.closest('#pl-box')) closeSug();
  const sug = t.closest('[data-sug]');
  if (sug) return pickSug(+sug.dataset.sug);
  if (t.closest('#pl-box') && !t.closest('button')) return $('#pl-input').focus();
  if (t.closest('#pl-read') || t.closest('[data-pl=read-menu]')) return readMenu(t.closest('button'));
  const sortB = t.closest('[data-pl-sort]');
  if (sortB) { P.sort = sortB.dataset.plSort; return search(); }
  const fb = t.closest('[data-pl-facet]');
  if (fb) {
    const id = +fb.dataset.plFacet, f = P.data.found.facets.find(x => x.id === id);
    P.tags = P.tags.some(x => x.id === id) ? P.tags.filter(x => x.id !== id) : [...P.tags, { id, kind: f.kind, value: f.value }];
    renderChips(); return search({ fit: true });
  }
  const uq = t.closest('[data-pl-untag-q]');
  if (uq) { P.tags = P.tags.filter(x => x.id !== +uq.dataset.plUntagQ); renderChips(); return search({ fit: true }); }
  const ex = t.closest('[data-pl-expand]');
  if (ex) { const k = ex.dataset.plExpand; P.open.has(k) ? P.open.delete(k) : P.open.add(k); return renderSide(); }
  const ob = t.closest('[data-pl-open]');
  if (ob) { const [k] = P.sel; return openHit(k, +ob.dataset.plOpen); }
  const rv = t.closest('[data-pl-reveal]');
  if (rv) { const c = (P.item?.copies || chosen()[0]?.copies || [])[+rv.dataset.plReveal]; return c && hooks.openLocal(c.dir, c.key); }
  const sd = t.closest('[data-pl-side]');
  if (sd) { P.side = sd.dataset.plSide; return renderSide(); }
  const kmB = t.closest('[data-pl-km]');
  if (kmB) { P.km = +kmB.dataset.plKm; renderChips(); return search({ fit: true }); }
  const fl = t.closest('[data-pl-flag]');
  if (fl) { const on = fl.getAttribute('aria-pressed') !== 'true'; return change('places-flag', { keys: [...P.sel], state: fl.dataset.plFlag, on }); }
  const dt = t.closest('[data-pl-detach]');
  if (dt) return change('places-untag', { keys: [...P.sel], tag: +dt.dataset.plDetach });
  const fd = t.closest('[data-pl-find]');
  if (fd) { const tg = P.allTags.find(x => x.id === +fd.dataset.plFind); addTagQuery({ id: tg.id, kind: tg.kind, value: tg.value }); P.side = 'refine'; renderChips(); return search({ fit: true }); }
  const rn = t.closest('[data-pl-rename]');
  if (rn) { P.renaming = +rn.dataset.plRename; renderSide(); return $('#pl-side form input')?.select(); }
  const del = t.closest('[data-pl-delete]');
  if (del) {
    const tg = P.allTags.find(x => x.id === +del.dataset.plDelete);
    if (tg && confirm(`Delete the tag “${tg.value}”? It comes off ${plural(tg.count, 'photo')}.`)) {
      P.tags = P.tags.filter(x => x.id !== tg.id); renderChips();
      return change('places-tag-delete', { tag: tg.id }, `Deleted “${tg.value}”`);
    }
    return;
  }
  const st = t.closest('[data-pl-stop]');
  if (st) {
    st.disabled = true;
    return fetch(`/api/job-stop?id=${st.dataset.plStop}`, { method: 'POST', headers: { 'X-BackDown': '1' }, credentials: 'same-origin' })
      .then(r => r.json()).then(j => { if (j.error) throw new Error(j.error); toast('Stopping at the next photo'); }).catch(hooks.toastError);
  }
  const tl = t.closest('#pl-grid .pt');
  if (tl) return choose(tl.dataset.k, e);
  switch (t.closest('[data-pl]')?.dataset.pl) {
    case 'un-near': P.centre = null; if (P.sort === 'nearest') P.sort = 'auto'; renderChips(); return search({ fit: true });
    case 'un-dates': P.from = P.to = ''; renderChips(); return search({ fit: true });
    case 'clear': return clearSearch();
    case 'more': return search({ more: true });
    case 'unselect': P.sel.clear(); return chosenChanged();
    case 'around': { const h = chosen()[0]; if (h?.lat != null) { P.sel.clear(); P.centre = { kind: 'point', label: `${h.lat.toFixed(4)}, ${h.lon.toFixed(4)}`, detail: where(h), lat: h.lat, lon: h.lon }; P.km = Math.min(P.km, 25); renderChips(); search({ fit: true }); } return; }
    case 'copy-sel': return hooks.copyText(chosen().flatMap(h => h.copies.map(c => c.path)).join('\n'));
    case 'read-all': return readDrive(null);
    case 'rename-cancel': P.renaming = null; return renderSide();
  }
  if (t.closest('#pl-clear')) return clearSearch();
  if (t.closest('#pl-selall')) return selectAll();
  if (t.closest('#pl-copy')) return copyAll();
  // a click on the grid's background lets go of what's chosen
  if (t.closest('#pl-grid') && !t.closest('.pt') && P.sel.size && !e.ctrlKey && !e.metaKey && !e.shiftKey) { P.sel.clear(); chosenChanged(); }
}

function onDblClick(e) {
  if (!hooks.isMode('places')) return;
  const tl = e.target.closest('#pl-grid .pt');
  if (tl) openHit(tl.dataset.k);
}

function onHover(e) {
  if (!hooks.isMode('places')) return;
  const tl = e.target.closest?.('#pl-grid .pt');
  const k = tl?.dataset.k || null;
  if (k === P.hover) return;
  P.hover = k; markMap();
}

let sugTimer, kmTimer;
function onInput(e) {
  if (!hooks.isMode('places')) return;
  const t = e.target;
  if (t.id === 'pl-input') { clearTimeout(sugTimer); sugTimer = setTimeout(() => suggest(t.value), 140); return; }
  if (t.id === 'pl-km') {
    clearTimeout(kmTimer);
    kmTimer = setTimeout(() => { const v = +t.value; if (v > 0 && v <= 20040) { P.km = v; renderChips(); search({ fit: true }); } }, 350);
    return;
  }
  if (t.id === 'pl-tf') {
    P.tagFilter = t.value;
    const at = t.selectionStart;
    renderSide();
    const f = $('#pl-tf'); f.focus(); f.setSelectionRange(at, at);
  }
}

function onChange(e) {
  if (!hooks.isMode('places')) return;
  const t = e.target;
  if (t.id === 'pl-from' || t.id === 'pl-to') {
    P[t.id === 'pl-from' ? 'from' : 'to'] = t.value;
    renderChips(); return search({ fit: true });
  }
  if (t.id === 'pl-add-kind') {
    P.addKind = t.value;
    const v = $('#pl-add input[name=value]').value;
    renderSide();
    const inp = $('#pl-add input[name=value]'); inp.value = v; inp.focus();
    loadValues(t.value);
  }
}

function onSubmit(e) {
  if (!hooks.isMode('places')) return;
  const f = e.target;
  if (f.id === 'pl-add') {
    e.preventDefault();
    const kind = f.kind.value, value = f.value.value.trim();
    if (!value) return;
    P.addKind = kind;
    return change('places-tag', { keys: [...P.sel], kind, value }, `Tagged ${plural(P.sel.size, 'photo')} “${value}”`);
  }
  if (f.dataset.plRenameForm) {
    e.preventDefault();
    const id = +f.dataset.plRenameForm, value = f.value.value.trim();
    P.renaming = null;
    // a tag being searched for keeps being searched for under its new name (or the one it joined)
    post('places-tag-edit', { tag: id, value }).then(j => {
      P.tags = P.tags.map(t => t.id === id ? { ...t, id: j.tag, value } : t);
      P.allTags = null; P.values.clear(); renderChips(); search({ keep: true });
    }).catch(hooks.toastError);
  }
}

function onKey(e) {
  if (!hooks.isMode('places') || !$('#viewer').hidden || document.querySelector('.menu') || document.querySelector('.help-wrap')) return;
  const t = e.target, k = e.key, ctrl = e.ctrlKey || e.metaKey;
  const stop = () => { e.preventDefault(); e.stopPropagation(); };
  if (t.id === 'pl-input') {
    if (k === 'ArrowDown' || k === 'ArrowUp') {
      if (!P.sug.length) return;
      stop(); P.sugAt = (P.sugAt + (k === 'ArrowDown' ? 1 : -1) + P.sug.length) % P.sug.length; return renderSug();
    }
    if (k === 'Enter') { stop(); if (P.sugAt >= 0) return pickSug(P.sugAt); return t.value.trim() && suggest(t.value); }
    if (k === 'Escape') { stop(); if (!$('#pl-sug').hidden) return closeSug(); t.value = ''; return t.blur(); }
    if (k === 'Backspace' && !t.value) {
      // the last condition goes
      if (P.from || P.to) { P.from = P.to = ''; } else if (P.tags.length) { P.tags.pop(); } else if (P.centre) { P.centre = null; } else return;
      stop(); renderChips(); return search({ fit: true });
    }
    return;
  }
  if (ctrl && (k === 'f' || k === 'F') && !e.shiftKey) { stop(); return $('#pl-input').focus(); }
  if (t.closest('input, textarea, select, canvas, [contenteditable]')) return;
  if (k === 'Escape' && P.sel.size) { stop(); P.sel.clear(); return chosenChanged(); }
  if (ctrl && (k === 'a' || k === 'A')) { stop(); return selectAll(); }
  // Enter on a button presses it; elsewhere it opens the photo chosen last
  if (k === 'Enter' && P.anchor && !t.closest('button, a')) { stop(); return openHit(P.anchor); }
}

function selectAll() {
  P.sel = new Set(P.hits.map(h => h.key));
  P.anchor = P.hits[0]?.key || null;
  chosenChanged();
  const left = (P.data?.found.total || 0) - P.hits.length;
  if (left > 0) toast(`Chose the ${count(P.hits.length)} shown: “Show more” brings in the other ${count(left)}`);
}

async function copyAll() {
  try {
    const j = await get('places', query({ paths: 1 }));
    hooks.copyText(j.paths.join('\n'));
  } catch (e) { hooks.toastError(e); }
}

function clearSearch() {
  Object.assign(P, { centre: null, km: DEFAULT_KM, tags: [], from: '', to: '', sort: 'auto' });
  P.sel.clear();
  renderChips(); search({ fit: true });
}
