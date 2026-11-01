// The pool in the browser (docs/SWARM.md §3–4): every machine's pool folders as one archive, adding
// and changing this machine's own, and the wizard that pours a drive into the pool: what of it is
// in the pool already (by content, on any machine), what's new, and which pool folder each thing
// goes into, with the person's choices. Everything goes through /api/pool-* (pool.rs in the Rust).
import { bytes } from './app.js';
import { toast } from './extras.js';

const $ = s => document.querySelector(s);
const esc = s => String(s ?? '').normalize('NFC').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const count = n => n >= 1e6 ? (n / 1e6).toFixed(2) + ' M' : n >= 1e3 ? (n / 1e3).toFixed(1) + ' K' : String(n ?? 0);
const rtf = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' });
function ago(t) {
  if (!t) return 'never';
  const s = t - Date.now() / 1000;
  for (const [unit, n] of [['day', 86400], ['hour', 3600], ['minute', 60]]) if (Math.abs(s) >= n) return rtf.format(Math.round(s / n), unit);
  return 'just now';
}

async function post(path, params = {}) {
  const r = await fetch(`/api/${path}?${new URLSearchParams(params)}`, { method: 'POST', headers: { 'X-BackDown': '1' }, credentials: 'same-origin' });
  const j = await r.json().catch(() => ({ error: r.statusText }));
  if (!r.ok || j.error) throw new Error(j.error || r.statusText);
  return j;
}

// the wizard: which drive, its plan, and what's happening (`job`: the copy, as the service runs it)
export const P = { root: null, plan: null, busy: '', error: null, adding: false, all: false, job: null, jobId: null, timer: 0 };
let hooks = { reload: () => {}, back: () => {}, isOpen: () => false, freeUp: () => {} };

export function mountPool(h) {
  hooks = { ...hooks, ...h };
  document.addEventListener('click', onClick);
  document.addEventListener('submit', onSubmit);
  document.addEventListener('change', onChange);
}

const areaKey = at => `${at.machine_id ?? 'here'}:${at.area}`;
const areaLabel = at => `${at.machine}: ${at.path}`;

// ---------- the pool, in the Swarm view

/// The pool card: every machine's pool folders (from the overview), with this machine's to change.
export function poolHtml(data) {
  const folders = [];
  for (const m of data?.machines || []) for (const a of m.pool || []) folders.push({ ...a, machine: m.name, here: m.here, machine_id: m.id });
  const t = data?.totals?.pool || {};
  const add = P.adding ? addForm() : '<button class="btn sm" type="button" data-pool="adding">Add a pool folder here</button>';
  if (!folders.length) {
    return `<section class="sw-pool act-card"><div class="sw-mhead"><h3>The pool</h3></div>
      <p>The pool is where your drives end up: folders on any of your machines' drives that together make one filed archive (Mail, Media/Photos, Machines, …). Each file goes in once, wherever it came from; then the old drive can be freed.</p>
      <p class="muted">Add a folder on a big drive of this machine, or run <code>backdown pool add FOLDER</code> on another machine to add one there.</p>
      ${add}</section>`;
  }
  const rows = folders.map(a => {
    const takes = a.takes?.length ? `takes ${a.takes.map(esc).join(', ')}` : 'takes anything';
    let room;
    if (a.space?.total) {
      const used = a.space.total - a.space.free, p = used / a.space.total, r = a.reserve / a.space.total;
      room = `<span class="sw-room"><span class="meter sw-meter${p > 0.95 ? ' full' : p > 0.85 ? ' high' : ''}" title="${Math.round(p * 100)}% used; ${bytes(a.reserve)} kept free">
          <i style="width:${(p * 100).toFixed(1)}%"></i><b class="sw-reserve" style="width:${Math.min(100, r * 100).toFixed(1)}%"></b></span>
        <span class="muted">${bytes(a.room || 0)} to fill</span></span>`;
    } else room = `<span class="muted">${a.answering === false ? 'its drive isn’t answering' : 'its drive isn’t there'}</span>`;
    const state = a.indexed ? '' : ' <span class="pill warn">not indexed yet</span>';
    const actions = a.here ? `<span class="sw-pa"><button class="btn sm" type="button" data-pool="remove" data-id="${a.id}" title="Stop treating it as part of the pool (nothing in it is touched)">Remove</button></span>` : '';
    return `<li class="sw-vol"><span class="sw-p"><b>${esc(a.machine)}</b> <span class="muted">${esc(a.path)}</span>${state}
        <small>${esc(a.drive?.volume?.label || a.drive?.disk?.model || '')}${a.drive?.disk?.serial ? ` s/n ${esc(a.drive.disk.serial)}` : ''} · ${takes} · holds ${count(a.files)} files, ${bytes(a.bytes || 0)}</small></span>
      ${room}${actions}</li>`;
  }).join('');
  return `<section class="sw-pool sw-machine"><div class="sw-mhead"><h3>The pool</h3>
      <span class="muted">${folders.length} folder${folders.length === 1 ? '' : 's'} · ${count(t.files || 0)} files, ${bytes(t.bytes || 0)} · ${bytes(t.room || 0)} to fill</span>
      <span class="sw-mbtns">${P.adding ? '' : add}</span></div>
    ${P.adding ? add : ''}
    <ul class="sw-list">${rows}</ul></section>`;
}

function addForm() {
  return `<form class="sw-addpool" id="pool-add">
    <label>Folder on this machine <input name="path" required placeholder="/media/me/BIG/Pool" autocomplete="off"></label>
    <label>Keep free <input name="reserve" placeholder="5% (at least 20 GB)" autocomplete="off"></label>
    <label>Takes <input name="takes" placeholder="anything (or Media, Mail)" autocomplete="off"></label>
    <span class="sw-mbtns"><button class="btn primary" type="submit">Add</button><button class="btn" type="button" data-pool="cancel">Cancel</button></span>
  </form>`;
}

/// "Into the pool" for an indexed drive of this machine.
export const intoButton = r => `<button class="btn sm sw-into" type="button" data-pool="into" data-root="${r.id}" data-name="${esc(r.name || '')}" data-path="${esc(r.path || '')}" data-key="${esc(r.key || '')}">Into the pool…</button>`;

// ---------- the wizard

export async function openWizard(root) {
  P.root = root; P.plan = null; P.error = null; P.all = false; P.job = null; P.jobId = null; clearTimeout(P.timer);
  P.busy = 'Comparing every file on it with the pool…';
  render();
  try { P.plan = await post('pool-plan-make', { root: root.id }); }
  catch (e) { P.error = e.message; }
  P.busy = '';
  render();
}

export function wizardOpen() { return !!P.root; }

function close() { P.root = null; P.plan = null; P.job = null; P.jobId = null; clearTimeout(P.timer); hooks.back(); }

const KIND = { 'mail-store': 'Mail', maildir: 'Mail', mdbox: 'Mail', thunderbird: 'Mail', photos: 'Photos', music: 'Music', videos: 'Videos', timemachine: 'Time Machine', system: 'System', git: 'Code', unsorted: 'Folder' };

export function render() {
  if (!P.root || !hooks.isOpen()) return;
  const r = P.root, pp = P.plan;
  const head = `<header class="sw-top"><div><h2>Into the pool: ${esc(r.name || r.path)}</h2>
      <p class="muted">${esc(r.path || '')}${pp ? ` · ${count(pp.files)} files, ${bytes(pp.bytes)} · plan ${pp.id}, made ${ago(pp.created_at)}` : ''}</p></div>
    <span class="sw-mbtns">${pp ? '<button class="btn sm" type="button" data-pool="again">Plan again</button>' : ''}<button class="btn sm" type="button" data-pool="close">Back to the swarm</button></span></header>`;
  if (P.busy) { $('#view').innerHTML = `<div class="swarm">${head}<section class="act-card"><p class="muted">${esc(P.busy)}</p><div class="meter unknown"><i style="width:100%"></i></div></section></div>`; return; }
  if (P.error) { $('#view').innerHTML = `<div class="swarm">${head}<section class="act-card"><p class="sw-fail">${esc(P.error)}</p></section></div>`; return; }
  if (!pp) return;
  const toCopy = pp.copy_bytes ?? pp.new_bytes + pp.unknown_bytes;
  const chip = (n, label, cls = '') => `<span class="sw-chip ${cls}"><b>${bytes(n)}</b>${label}</span>`;
  const chips = `<div class="sw-chips">
      ${chip(pp.in_pool_bytes, 'in the pool already', 'ok')}
      ${pp.twice_bytes ? chip(pp.twice_bytes, 'twice on this drive (goes once)') : ''}
      ${chip(toCopy, 'to copy', 'go')}
      ${pp.unknown_bytes ? chip(pp.unknown_bytes, 'not compared yet', 'warn') : ''}
      ${pp.left_bytes ? chip(pp.left_bytes, 'left out') : ''}
      ${pp.unplaced_bytes ? chip(pp.unplaced_bytes, 'fits nowhere', 'warn') : ''}</div>`;
  const notes = [...(pp.notes || [])];
  const unknown = pp.unknown_bytes ? `<p class="note">${bytes(pp.unknown_bytes)} on this drive has the size of something in the pool, but hasn’t been compared by content yet. It’s counted as needing room until it is. <button class="btn sm" type="button" data-pool="compare">Rescan and compare</button></p>` : '';
  const used = pp.areas.filter(u => u.assigned_bytes > 0 || u.indexed);
  const areas = used.map(u => {
    const after = Math.max(0, u.room - u.assigned_bytes), p = u.room ? Math.min(1, u.assigned_bytes / u.room) : 0;
    return `<li class="sw-vol"><span class="sw-p"><b>${esc(u.at.machine)}</b> <span class="muted">${esc(u.at.path)}</span>
        <small>${esc(u.drive || '')}${u.takes?.length ? ` · takes ${u.takes.map(esc).join(', ')}` : ''}${!u.indexed ? ' · not indexed yet' : !u.present ? ' · not there' : ''}</small></span>
      <span class="sw-room"><span class="meter sw-meter${p > 0.95 ? ' full' : p > 0.85 ? ' high' : ''}" title="${bytes(u.assigned_bytes)} of ${bytes(u.room)}"><i style="width:${(p * 100).toFixed(1)}%"></i></span>
        <span class="muted">${u.assigned_bytes ? `+${bytes(u.assigned_bytes)} · ` : ''}${bytes(after)} left to fill</span></span></li>`;
  }).join('');
  const usable = pp.areas.filter(u => u.indexed && u.present && u.at);
  const things = (P.all ? pp.things : pp.things.slice(0, 150)).map((t, i) => thingHtml(t, i, usable)).join('');
  const more = !P.all && pp.things.length > 150 ? `<button class="btn sm" type="button" data-pool="all">Show all ${pp.things.length}</button>` : '';
  $('#view').innerHTML = `<div class="swarm sw-wizard">${head}
    <section class="sw-machine">${chips}${unknown}${notes.length ? `<ul class="note sw-notes">${notes.map(n => `<li>${esc(n)}</li>`).join('')}</ul>` : ''}</section>
    <section class="sw-machine"><div class="sw-mhead"><h3>Where it goes</h3></div>${areas ? `<ul class="sw-list">${areas}</ul>` : '<p class="muted sw-none">There’s no pool folder yet: add one in the Swarm view.</p>'}</section>
    <section class="sw-machine"><div class="sw-mhead"><h3>What’s on it</h3><span class="muted">${pp.things.length} thing${pp.things.length === 1 ? '' : 's'}, each filed whole</span></div>
      <ul class="sw-list sw-things">${things}</ul>${more}</section>
    ${copyHtml(pp)}
  </div>`;
}

// ---------- copying it, then freeing the drive

const JOB_DONE = new Set(['done', 'failed', 'stopped']);

function copyHtml(pp) {
  const j = P.job, toCopy = pp.copy_bytes ?? 0;
  const machines = [...new Set(pp.areas.filter(u => u.assigned_bytes > 0 && u.at?.machine_id != null).map(u => u.at.machine))];
  let body;
  if (j && !JOB_DONE.has(j.state)) {
    const c = j.counts || {}, p = c.total_bytes ? Math.min(1, c.bytes / c.total_bytes) : null;
    body = `<div class="act-head"><span class="pulse"></span><h3>${j.state === 'queued' ? 'Waiting its turn…' : 'Copying into the pool…'}</h3>
        <span class="act-pct">${p != null ? Math.round(p * 100) + '%' : ''}</span>
        <button class="btn sm" type="button" data-pool="stop">Stop</button></div>
      <div class="meter${p == null ? ' unknown' : ''}"><i style="width:${p != null ? (p * 100).toFixed(1) : 100}%"></i></div>
      <p class="muted">${esc(j.progress || '')}</p>
      <p class="muted">Each file is read back where it lands and compared by SHA-256; stopped, it carries on where it was next time.</p>`;
  } else if (j) {
    const ok = j.state === 'done' && pp.state === 'done';
    body = `<h3>${j.state === 'failed' ? 'The copy stopped with a problem' : j.state === 'stopped' ? 'Stopped' : ok ? 'In the pool' : 'Copied, with things to look at'}</h3>
      <p>${esc(j.error || j.summary || '')}</p>
      ${j.state === 'done' ? freeUpHtml(pp, machines) : `<p><button class="btn primary" type="button" data-pool="send">Carry on copying</button></p>`}`;
  } else if (pp.state === 'done') {
    body = `<h3>In the pool</h3><p class="muted">This plan has been carried out.</p>${freeUpHtml(pp, machines)}`;
  } else if (toCopy > 0) {
    body = `<h3>Copy it into the pool</h3>
      <p>${bytes(toCopy)} goes into ${pp.areas.filter(u => u.assigned_bytes > 0).length} pool folder${pp.areas.filter(u => u.assigned_bytes > 0).length === 1 ? '' : 's'}${pp.unplaced_bytes ? `; ${bytes(pp.unplaced_bytes)} fits nowhere and stays on the drive` : ''}. Each file is written beside its place, read back there and compared by SHA-256; nothing is ever overwritten, and nothing on this drive is changed.</p>
      <p><button class="btn primary" type="button" data-pool="send">Copy ${bytes(toCopy)} into the pool</button>${pp.state === 'partial' ? ' <span class="muted">(it carries on where it stopped)</span>' : ''}</p>`;
  } else {
    body = `<h3>Nothing to copy</h3><p class="muted">Everything on it is in the pool already, or left out.</p>${freeUpHtml(pp, machines)}`;
  }
  return `<section class="act-card sw-copy">${body}</section>`;
}

function freeUpHtml(pp, machines) {
  const there = machines.length ? `Once ${machines.map(esc).join(' and ')} ${machines.length === 1 ? 'has' : 'have'} indexed what ${machines.length === 1 ? 'it' : 'they'} received (it started as soon as the copy ended), sync ${machines.length === 1 ? 'it' : 'them'} here so the copies count. ` : '';
  return `<div class="sw-free"><h3>Free up ${esc(P.root?.name || 'the drive')}</h3>
    <p>${there}Free up then checks every file on the drive has a copy that counts (copies on other machines are read again there just before anything is deleted), shows what's left, and asks you to type the drive's name before deleting anything.</p>
    <p class="sw-mbtns">${machines.length ? '<button class="btn" type="button" data-pool="sync">Sync the pool’s machines</button>' : ''}<button class="btn primary" type="button" data-pool="free">Free up ${esc(P.root?.name || 'the drive')}…</button></p></div>`;
}

async function send() {
  try {
    const r = await post('pool-send', { id: P.plan.id });
    P.jobId = r.job; P.job = { id: r.job, state: 'queued' };
    render();
    watch();
  } catch (e) { toast(e.message); }
}

// the copy's progress, from the service, until it ends
async function watch() {
  clearTimeout(P.timer);
  if (!P.jobId || !P.root) return;
  try {
    const d = await fetch('/api/daemon', { credentials: 'same-origin' }).then(r => r.json());
    const all = [d.side, d.current, ...(d.queued || []), ...(d.recent || [])].filter(Boolean);
    const j = all.find(x => x.id === P.jobId);
    if (j) P.job = j;
    if (j && JOB_DONE.has(j.state)) {
      try { P.plan = await fetch(`/api/pool-plan?id=${P.plan.id}`, { credentials: 'same-origin' }).then(r => r.json()); } catch {}
      if (hooks.isOpen()) render();
      return;
    }
  } catch {}
  if (hooks.isOpen()) render();
  P.timer = setTimeout(watch, 1500);
}

async function stopJob() {
  if (!P.jobId) return;
  await fetch(`/api/job-stop?id=${P.jobId}`, { method: 'POST', headers: { 'X-BackDown': '1' }, credentials: 'same-origin' }).catch(() => {});
  watch();
}

async function syncMachines() {
  const ids = new Set(P.plan.areas.filter(u => u.assigned_bytes > 0).map(u => u.at?.machine_id).filter(x => x != null));
  for (const m of ids) await post('machine-sync', { m }).catch(e => toast(e.message));
  toast('Syncing: the copies count once it’s done (Activity shows it)');
}

function thingHtml(t, i, usable) {
  const need = t.skip ? 0 : t.new_bytes + t.unknown_bytes;
  const parts = [];
  if (need) parts.push(`${bytes(need)} to copy`);
  if (t.in_pool_bytes) parts.push(`${bytes(t.in_pool_bytes)} in the pool already`);
  if (t.twice_bytes) parts.push(`${bytes(t.twice_bytes)} twice`);
  if (t.left_bytes) parts.push(`${bytes(t.left_bytes)} left out`);
  const sel = t.to ? areaKey(t.to) : t.skip ? 'skip' : '';
  const canCopy = t.new_bytes + t.unknown_bytes > 0;
  const choice = canCopy ? `<select class="sw-to" data-i="${i}" aria-label="Where ${esc(t.item.what)} goes">
      ${t.to || t.skip ? '' : '<option value="" selected>Nowhere has room</option>'}
      ${usable.map(u => `<option value="${areaKey(u.at)}"${sel === areaKey(u.at) ? ' selected' : ''}>${esc(areaLabel(u.at))}</option>`).join('')}
      <option value="skip"${t.skip ? ' selected' : ''}>Leave it out</option></select>` : '<span class="muted">nothing to copy</span>';
  const kind = KIND[t.item.kind] || t.item.kind;
  return `<li class="sw-thing${t.skip ? ' off' : ''}"><span class="sw-p"><b>${esc(t.item.what)}</b>${kind && !t.item.what.startsWith(kind) ? ` <span class="pill off">${esc(kind)}</span>` : ''}
      <small>→ ${esc(t.item.dest)}</small><small class="muted">${esc(parts.join(' · '))}${t.in_pool_at ? ` · e.g. ${esc(t.in_pool_at)}` : ''}</small></span>
    <span class="sw-tchoice">${choice}<small class="muted">${esc(t.why_to || '')}</small></span></li>`;
}

// ---------- what people do

async function choose(i, value) {
  try {
    const params = { id: P.plan.id, i };
    if (value === 'skip') params.skip = 1; else params.to = value;
    P.plan = await post('pool-plan-choose', params);
  } catch (e) { toast(e.message); }
  render();
}

async function compare() {
  try {
    await post('rescan', { root: P.root.key });
    const ids = new Set(P.plan.areas.map(u => u.at?.machine_id).filter(x => x != null));
    for (const m of ids) await post('machine-sync', { m }).catch(() => {});
    toast('Rescanning and comparing: plan again when Activity shows it’s done');
  } catch (e) { toast(e.message); }
}

async function onSubmit(e) {
  if (e.target.id !== 'pool-add') return;
  e.preventDefault();
  const f = new FormData(e.target);
  try {
    const r = await post('pool-area-add', { path: f.get('path').trim(), reserve: f.get('reserve').trim(), takes: f.get('takes').trim() });
    P.adding = false;
    toast(r.indexing ? 'Added: it’s being indexed now' : 'Added to the pool');
    hooks.reload();
  } catch (err) { toast(err.message); }
}

function onChange(e) {
  const s = e.target.closest?.('.sw-to');
  if (!s || !P.plan) return;
  choose(Number(s.dataset.i), s.value);
}

async function onClick(e) {
  const b = e.target.closest('[data-pool]');
  if (!b) return;
  switch (b.dataset.pool) {
    case 'adding': P.adding = true; hooks.reload(true); break;
    case 'cancel': P.adding = false; hooks.reload(true); break;
    case 'remove':
      if (!confirm('Stop treating this folder as part of the pool? Nothing in it is touched.')) return;
      try { await post('pool-area-remove', { id: b.dataset.id }); hooks.reload(); } catch (err) { toast(err.message); }
      break;
    case 'into': openWizard({ id: Number(b.dataset.root), name: b.dataset.name, path: b.dataset.path, key: b.dataset.key }); break;
    case 'close': close(); break;
    case 'again': openWizard(P.root); break;
    case 'all': P.all = true; render(); break;
    case 'compare': compare(); break;
    case 'send': send(); break;
    case 'stop': stopJob(); break;
    case 'sync': syncMachines(); break;
    case 'free': hooks.freeUp(P.root); break;
  }
}
