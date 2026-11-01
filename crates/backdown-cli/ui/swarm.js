// The Swarm view: every machine as one (docs/SWARM.md). Each machine's drives as bars of used and
// free space with what's indexed on them, the drives that aren't connected and the drive they were
// last scanned on, the totals, and "where is it": a name searched on every machine at once, each
// hit saying which machine and which drive. This machine is probed when the view opens; the
// others show as last kept, then each is asked over ssh (by this machine's BackDown, with your
// keys) and redrawn as it answers. Everything goes through /api/swarm* (swarm.rs in the Rust).
import { api, bytes } from './app.js';
import { fileIcon, folderIcon } from './icons.js';
import { mountPool, poolHtml, intoButton, render as renderWizard, wizardOpen } from './pool.js';

const $ = s => document.querySelector(s);
const esc = s => String(s ?? '').normalize('NFC').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const count = n => n >= 1e6 ? (n / 1e6).toFixed(2) + ' M' : n >= 1e3 ? (n / 1e3).toFixed(1) + ' K' : String(n ?? 0);
const day = t => t ? new Date(t * 1000).toLocaleDateString(undefined, { dateStyle: 'medium' }) : '—';
const rtf = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' });
function ago(t) {
  if (!t) return 'never';
  const s = t - Date.now() / 1000;
  for (const [unit, n] of [['day', 86400], ['hour', 3600], ['minute', 60]]) if (Math.abs(s) >= n) return rtf.format(Math.round(s / n), unit);
  return 'just now';
}

export const SWARM_ICON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="5" r="2.5"/><circle cx="5" cy="18" r="2.5"/><circle cx="19" cy="18" r="2.5"/><path d="M10.8 7.2 6.3 15.8M13.2 7.2l4.5 8.6M7.5 18h9"/></svg>';

// what's shown: the overview, which machines are being asked, the search and its answer
const W = { data: null, asking: new Set(), errors: new Map(), q: '', found: null, searching: false, seq: 0, qseq: 0 };
let hooks = { enter: () => {}, isMode: () => false, openLocal: () => {}, openMachine: () => {} };

export function mountSwarm(h) {
  hooks = { ...hooks, ...h };
  // the pool (pool.js): its card here, and the wizard that takes over this view for a drive
  mountPool({
    reload: local => local ? render() : showSwarm(),
    back: () => showSwarm(),
    isOpen: () => hooks.isMode('swarm') && wizardOpen(),
  });
  document.addEventListener('click', onClick);
  document.addEventListener('submit', onSubmit);
  document.addEventListener('keydown', onKey);
  document.addEventListener('input', e => { if (e.target.id === 'sw-q' && !e.target.value.trim()) { W.q = ''; W.found = null; renderFound(); } });
}

export async function showSwarm() {
  hooks.enter('swarm');
  if (wizardOpen()) return renderWizard();
  const seq = ++W.seq;
  if (!W.data) $('#view').innerHTML = '<div class="swarm"><p class="muted">Looking at the drives…</p></div>';
  else render();
  try { W.data = await api('swarm'); } catch (e) { if (seq === W.seq && hooks.isMode('swarm')) $('#view').innerHTML = `<div class="swarm"><p class="sw-fail">${esc(e.message)}</p></div>`; return; }
  if (seq !== W.seq || !hooks.isMode('swarm') || wizardOpen()) return;
  render();
  askAll();
}

// each other machine asked about its drives at once, each redrawn as it answers
function askAll() {
  if (!W.data?.can_ask) return;
  for (const m of W.data.machines.filter(m => !m.here)) ask(m.id);
}

async function ask(id) {
  if (W.asking.has(id)) return;
  W.asking.add(id); W.errors.delete(id);
  renderMachine(id);
  try {
    const v = await api('swarm-machine', { m: id });
    const i = W.data.machines.findIndex(m => m.id === id);
    if (i >= 0) W.data.machines[i] = v;
    if (!v.asked && v.ask_error) W.errors.set(id, v.ask_error);
    totals();
  } catch (e) { W.errors.set(id, e.message); }
  W.asking.delete(id);
  if (hooks.isMode('swarm') && !wizardOpen()) { renderMachine(id); renderTotals(); renderPool(); }
}

// the swarm's totals from the machines as they are now (some may have answered since)
function totals() {
  const t = { machines: W.data.machines.length, reporting: 0, disks: 0, capacity: 0, free: 0, files: 0, bytes: 0, pool: { areas: 0, files: 0, bytes: 0, room: 0 } };
  for (const m of W.data.machines) {
    if (m.disks?.length) t.reporting++;
    for (const k of ['disks', 'capacity', 'free', 'files', 'bytes']) t[k] += m.totals?.[k] || 0;
    for (const k of ['areas', 'files', 'bytes', 'room']) t.pool[k] += m.totals?.pool?.[k] || 0;
  }
  W.data.totals = t;
}

// ---------- drawing

// The view's frame (heading, search box, results) is drawn once and kept, so fresh data never
// clobbers what's being typed; the machines below it are redrawn.
function render() {
  if (!hooks.isMode('swarm') || !W.data || wizardOpen()) return;
  const d = W.data;
  if (!$('#view .swarm #sw-find')) {
    $('#view').innerHTML = `<div class="swarm">
      <header class="sw-top"><div><h2>Swarm</h2><p class="muted" id="sw-totals"></p></div><span id="sw-top-btn"></span></header>
      <form class="sw-find" id="sw-find" role="search"><input id="sw-q" type="search" placeholder="Where is it? A name, looked for on every machine" aria-label="Find on every machine" autocomplete="off" value="${esc(W.q)}"><button class="btn" type="submit">Find</button></form>
      <div id="sw-found"></div>
      <div id="sw-pool"></div>
      <div id="sw-body"></div>
    </div>`;
    renderFound();
  }
  $('#sw-top-btn').innerHTML = d.can_ask && d.machines.length > 1 ? '<button class="btn sm" type="button" data-sw="ask-all">Ask the machines again</button>' : '';
  $('#sw-body').innerHTML = `${d.why_not && d.machines.length > 1 ? `<p class="note">${esc(d.why_not)}: the other machines show as last synced.</p>` : ''}
    <div class="sw-machines">${d.machines.map(m => `<section class="sw-machine" id="sw-m-${m.here ? 'here' : m.id}">${machineHtml(m)}</section>`).join('')}</div>
    ${d.machines.length < 2 && !poolFolders() ? `<section class="act-card"><p>This is a swarm of one. Add the other machines you run BackDown on (a NAS, a server, another computer) under <b>Machines</b> in the sidebar, reached with your own ssh keys: their drives show up here too, and where every file is.</p></section>` : ''}`;
  renderTotals();
  renderPool();
}

const poolFolders = () => (W.data?.machines || []).reduce((n, m) => n + (m.pool?.length || 0), 0);
// an indexed drive of this machine that is (or is in) one of its pool folders isn't poured into the pool
const inPool = path => (W.data?.machines?.find(m => m.here)?.pool || [])
  .some(a => path === a.path || String(path).startsWith(String(a.path).replace(/\/+$/, '') + '/'));

function renderPool() {
  const el = $('#sw-pool');
  if (el) el.innerHTML = poolHtml(W.data);
}

function renderTotals() {
  const t = W.data?.totals, el = $('#sw-totals');
  if (!t || !el) return;
  el.textContent = `${t.machines} machine${t.machines === 1 ? '' : 's'} · ${t.disks} drive${t.disks === 1 ? '' : 's'} · ${bytes(t.free)} free of ${bytes(t.capacity)} · ${count(t.files)} files indexed (${bytes(t.bytes)})`;
}

function renderMachine(id) {
  const m = W.data?.machines.find(x => x.id === id);
  const el = $(`#sw-m-${id}`);
  if (m && el) el.innerHTML = machineHtml(m);
}

function stateLine(m) {
  if (m.here) return '<span class="pill ok">this machine</span>';
  if (W.asking.has(m.id)) return '<span class="pill busy">asking…</span>';
  const err = W.errors.get(m.id) || (m.state === 'error' ? m.error : null);
  const as = m.ok_at ? `drives as of ${ago(m.ok_at)}` : '';
  if (m.state === 'unsupported') return `<span class="pill warn">update BackDown there</span> <span class="muted">BackDown ${esc(m.version || '')} on ${esc(m.name)} doesn’t say which drives it has or how full they are</span>`;
  if (err) return `<span class="pill warn">couldn’t be asked</span> <span class="muted sw-err" title="${esc(err)}">${esc(String(err).split('\n')[0].slice(0, 160))}${as ? ' · ' + as : ''}</span>`;
  if (m.state === 'never') return '<span class="pill off">not asked yet</span>';
  return `<span class="pill ok">reached</span> <span class="muted">${as}</span>`;
}

function machineHtml(m) {
  const t = m.totals || {};
  const head = `<div class="sw-mhead"><h3>${esc(m.name)}</h3>${stateLine(m)}
    <span class="sw-mtot muted">${t.capacity ? `${bytes(t.free)} free of ${bytes(t.capacity)}` : ''}${t.files ? `${t.capacity ? ' · ' : ''}${count(t.files)} files indexed` : ''}</span>
    ${m.here ? '' : `<span class="sw-mbtns"><button class="btn sm" type="button" data-sw="browse" data-m="${m.id}">Browse</button>${W.data?.can_ask ? `<button class="btn sm" type="button" data-sw="ask" data-m="${m.id}"${W.asking.has(m.id) ? ' disabled' : ''}>Ask again</button>` : ''}</span>`}</div>`;
  const disks = (m.disks || []).map(d => diskHtml(d, m.here)).join('');
  const rows = (list, f) => list?.length ? list.map(f).join('') : '';
  const elsewhere = rows(m.elsewhere, r => `<li class="sw-extra">${folderIcon(18)}<span class="sw-p"><b>${esc(r.name || r.path)}</b>${m.here && poolFolders() && !inPool(r.path) ? ` ${intoButton(r)}` : ''}<small>${esc(r.path)}</small></span>
      <span class="muted">${count(r.files)} files · ${bytes(r.bytes)}${r.space ? ` · ${bytes(r.space.free)} free of ${bytes(r.space.total)}` : ''}</span></li>`);
  const away = rows(m.away, r => `<li class="sw-extra away">${folderIcon(18)}<span class="sw-p"><b>${esc(r.name || r.path)}</b> <span class="pill off">not connected</span><small>${esc(r.path)}${r.drive ? ` · last on ${esc(driveWords(r.drive))}` : ''}</small></span>
      <span class="muted">${count(r.files)} files · ${bytes(r.bytes)} · scanned ${day(r.scanned_at)}</span></li>`);
  const unanswered = rows(m.unanswered, r => `<li class="sw-extra">${folderIcon(18)}<span class="sw-p"><b>${esc(r.name || r.path)}</b> <span class="pill warn">not answering</span><small>${esc(r.path)}</small></span><span class="muted">${count(r.files)} files · ${bytes(r.bytes)}</span></li>`);
  const extra = elsewhere + away + unanswered;
  const label = !m.disks?.length ? 'Indexed drives' : 'Indexed, not on a drive above';
  const empty = !disks && !extra ? `<p class="muted sw-none">${m.here ? 'No drives found.' : m.state === 'never' ? 'Not asked yet.' : 'No drives listed.'}</p>` : '';
  return head + disks + (extra ? `<h4>${label}</h4><ul class="sw-list">${extra}</ul>` : '') + empty;
}

const HEALTH = { ok: ['ok', 'healthy'], warning: ['warn', 'warning'], failing: ['warn', 'failing'] };

function diskHtml(d, here) {
  const what = [d.model, d.serial && `s/n ${d.serial}`, bytes(d.size || 0), d.transport, d.rotational === true ? 'spinning' : d.rotational === false ? 'solid state' : null].filter(Boolean);
  const h = HEALTH[d.health];
  const vols = (d.volumes || []).map(v => volHtml(v, here)).join('');
  return `<div class="sw-disk"><div class="sw-dhead"><b>${esc(d.name)}</b> <span class="muted">${esc(what.join(' · '))}</span>${h ? ` <span class="pill ${h[0]}">${h[1]}</span>` : ''}${d.system ? ' <span class="pill off">system</span>' : ''}</div>
    ${vols ? `<ul class="sw-list">${vols}</ul>` : '<p class="muted sw-none">Nothing on it BackDown can read.</p>'}</div>`;
}

function volHtml(v, here) {
  const name = v.label || v.part_label || v.name;
  const where = (v.mounts || []).join(', ');
  const kind = [v.fs_name, v.via && `in ${v.via}`].filter(Boolean).join(' ');
  let room;
  if (v.space?.total) {
    const used = v.space.total - v.space.free, p = used / v.space.total;
    room = `<span class="sw-room"><span class="meter sw-meter${p > 0.95 ? ' full' : p > 0.85 ? ' high' : ''}" title="${Math.round(p * 100)}% used"><i style="width:${(p * 100).toFixed(1)}%"></i></span>
      <span class="muted">${bytes(v.space.free)} free of ${bytes(v.space.total)}</span></span>`;
  } else room = `<span class="muted">${bytes(v.size || 0)}${v.state === 'encrypted' ? ', locked' : ', not mounted'}</span>`;
  const into = i => here && poolFolders() && !inPool(i.path) ? ` ${intoButton({ id: i.id, name: i.name, path: i.path, key: i.key })}` : '';
  const indexed = (v.indexed || []).map(i => `<span class="sw-ix" title="${esc(i.path)}">${esc(i.name || i.path)} · ${count(i.files)} files · ${bytes(i.bytes)}${into(i)}</span>`).join('');
  const pool = (v.pool || []).length ? ' <span class="pill ok">pool</span>' : '';
  return `<li class="sw-vol${v.system ? ' sys' : ''}"><span class="sw-p"><b>${esc(name)}</b> <span class="muted">${esc(kind)}</span>${pool}${where ? `<small>${esc(where)}</small>` : ''}${indexed ? `<span class="sw-ixs">${indexed}</span>` : ''}</span>${room}</li>`;
}

// "Hercules (WD Elements 2620 s/n WX…)"
function driveWords(d) {
  const label = d?.volume?.label, model = d?.disk?.model, serial = d?.disk?.serial;
  const m = model ? `${model}${serial ? ` s/n ${serial}` : ''}` : '';
  return label && m ? `${label} (${m})` : label || m || d?.volume?.device || 'a drive';
}

// ---------- where is it

async function search(q) {
  W.q = q.trim(); W.found = null;
  if (!W.q) return renderFound();
  W.searching = true; renderFound();
  const seq = ++W.qseq;
  try {
    const r = await api('swarm-where', { q: W.q, limit: 200 });
    if (seq === W.qseq) W.found = r;
  } catch (e) { if (seq === W.qseq) W.found = { error: e.message }; }
  if (seq === W.qseq) { W.searching = false; renderFound(); }
}

function renderFound() {
  const el = $('#sw-found');
  if (!el) return;
  if (!W.q) { el.innerHTML = ''; return; }
  if (W.searching) { el.innerHTML = `<p class="muted">Looking on every machine for “${esc(W.q)}”…</p>`; return; }
  const f = W.found;
  if (!f) { el.innerHTML = ''; return; }
  if (f.error) { el.innerHTML = `<p class="sw-fail">${esc(f.error)}</p>`; return; }
  const notes = (f.machines || []).map(m => {
    if (m.how === 'cached') return `<li>${esc(m.name)} couldn’t be asked, so its files as synced ${ago(m.synced_at)} were searched${m.complete === false ? ' (only some of them, in the time there was)' : ''}.</li>`;
    if (m.how === 'none') return `<li>${esc(m.name)} wasn’t searched: ${esc(m.error || '')}</li>`;
    return '';
  }).join('');
  const hits = f.hits || [];
  el.innerHTML = `<section class="sw-hits">
    <h4>${hits.length ? `${hits.length >= 200 ? 'The first 200' : hits.length} found for “${esc(f.q)}”` : `Nothing named “${esc(f.q)}” on any machine`}</h4>
    ${hits.length ? `<ul class="sw-list">${hits.map((h, i) => hitHtml(h, i)).join('')}</ul>` : ''}
    ${notes ? `<ul class="note sw-notes">${notes}</ul>` : ''}
  </section>`;
}

function hitHtml(h, i) {
  const icon = h.kind === 'dir' ? folderIcon(20) : fileIcon(h.name || '', 20);
  const drive = h.place && typeof h.place === 'object' ? driveWords(h.place) + (h.place.present === false ? ' (not connected)' : '') : (h.drive || '');
  const size = h.kind === 'dir' ? 'folder' : bytes(h.size || 0);
  return `<li class="sw-hit" data-sw="hit" data-i="${i}" tabindex="0">${icon}<span class="sw-p"><b>${esc(h.name)}</b>
      <small><span class="sw-where">${esc(h.machine)}</span>${drive ? ` · ${esc(drive)}` : ''} · ${esc(h.path)}</small></span>
    <span class="muted">${size} · ${day(h.mtime)}${h.cached ? ' · as synced' : ''}</span></li>`;
}

function openHit(h) {
  if (!h) return;
  if (h.here) hooks.openLocal(h.dir_key, h.key);
  else hooks.openMachine(h.machine_id, h.dir_key, h.key);
}

function onSubmit(e) {
  if (e.target.id !== 'sw-find') return;
  e.preventDefault();
  search($('#sw-q').value);
}

function onClick(e) {
  const b = e.target.closest('[data-sw]');
  if (!b || !hooks.isMode('swarm')) return;
  const id = Number(b.dataset.m);
  switch (b.dataset.sw) {
    case 'ask-all': askAll(); break;
    case 'ask': ask(id); break;
    case 'browse': hooks.openMachine(id, null, null); break;
    case 'hit': openHit(W.found?.hits?.[Number(b.dataset.i)]); break;
  }
}

function onKey(e) {
  if (e.key !== 'Enter' || !hooks.isMode('swarm')) return;
  const li = e.target.closest?.('.sw-hit');
  if (li) openHit(W.found?.hits?.[Number(li.dataset.i)]);
}
