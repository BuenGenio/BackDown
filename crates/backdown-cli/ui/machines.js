// Other machines in the browser: the Machines section of the sidebar (each machine with its
// drives, when it was last synced, whether it can be reached), a machine's page (drives there,
// sync, test, remove, how its copies count), browsing its folders read-only (live over ssh, or as
// last synced when it can't be reached), adding one, and where other machines' copies show up
// elsewhere: a file's details, Duplicates, Free up. Everything goes through /api/machine* (remote/
// in the Rust); ssh runs as the person using this browser, never from here.
import { api, bytes, when } from './app.js';
import { fileIcon, folderIcon, driveIcon } from './icons.js';
import { toast } from './extras.js';

const $ = s => document.querySelector(s);
const esc = s => String(s ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const count = n => n >= 1e6 ? (n / 1e6).toFixed(2) + ' M' : n >= 1e3 ? (n / 1e3).toFixed(1) + ' K' : String(n ?? 0);
const rtf = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' });
export function ago(t) {
  if (!t) return 'never';
  const s = t - Date.now() / 1000;
  for (const [unit, n] of [['day', 86400], ['hour', 3600], ['minute', 60]]) if (Math.abs(s) >= n) return rtf.format(Math.round(s / n), unit);
  return 'just now';
}

const ICON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="4" width="18" height="7" rx="1.5"/><rect x="3" y="13" width="18" height="7" rx="1.5"/><path d="M7 7.5h.01M7 16.5h.01M11 7.5h6M11 16.5h6"/></svg>';
const PLUS = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M12 5v14M5 12h14"/></svg>';

// what's shown: the machines, which one, which folder there (a path key there), the file picked
const M = { data: null, cur: null, dir: null, listing: null, sel: null, file: null, adding: false, add: null, seq: 0, fseq: 0, jobs: '' };
let hooks = { enter: () => {}, leave: () => {}, openLocal: () => {}, isMode: () => false };

async function post(path, params = {}) {
  const r = await fetch(`/api/${path}?${new URLSearchParams(params)}`, { method: 'POST', headers: { 'X-BackDown': '1' }, credentials: 'same-origin' });
  const j = await r.json().catch(() => ({ error: r.statusText }));
  if (!r.ok || j.error) throw new Error(j.error || r.statusText);
  return j;
}

export function mountMachines(h) {
  hooks = { ...hooks, ...h };
  document.addEventListener('click', onClick);
  document.addEventListener('submit', onSubmit);
  loadMachines();
}

export async function loadMachines() {
  try { M.data = await api('machines'); } catch (e) { M.data = { machines: [], error: e.message, settings: { proof_days: 7 } }; }
  renderMachinesNav();
  if (hooks.isMode('machine') && !M.dir && !M.adding) renderMachine();
}

const machine = id => M.data?.machines?.find(m => m.id === id);
const busy = m => m?.job && ['queued', 'running'].includes(m.job.state);

// ---------- the sidebar

function status(m) {
  if (busy(m)) return `<span class="mc-dot busy"></span>${m.job.state === 'running' ? 'syncing…' : 'sync waiting'}`;
  if (m.last_error && (!m.last_sync || m.last_try > m.last_sync)) return `<span class="mc-dot bad"></span>can’t be reached`;
  if (!m.last_sync) return `<span class="mc-dot"></span>not synced yet`;
  return `<span class="mc-dot ok"></span>synced ${ago(m.last_sync)}`;
}

export function renderMachinesNav() {
  const nav = $('#machines');
  if (!nav) return;
  const d = M.data;
  const on = hooks.isMode('machine');
  const list = d?.machines || [];
  nav.innerHTML = list.map(m => `
    <button class="place" type="button" data-mc="${m.id}" aria-current="${on && M.cur === m.id && !M.dir && !M.adding}" title="${esc(m.url)}">
      <span class="pi">${ICON}</span><span class="pn">${esc(m.name)}</span><span class="ps">${status(m)}</span>
    </button>
    ${(m.roots || []).map(r => `<button class="place sub" type="button" data-mc="${m.id}" data-mc-dir="${r.key}" aria-current="${on && M.cur === m.id && M.dir === r.key}" title="${esc(r.path)} on ${esc(m.name)}">
      <span class="pi">${driveIcon()}</span><span class="pn">${esc(r.name)}</span><span class="ps">${bytes(r.bytes)} · ${count(r.files)} files</span></button>`).join('')}`).join('')
    + (d?.allowed === false ? `<p class="mc-pro">Several machines are a <b>Pro</b> feature.</p>`
      : `<button class="place add" type="button" data-mc-add aria-current="${on && M.adding}"><span class="pi">${PLUS}</span><span class="pn">${list.length ? 'Add a machine' : 'Add a NAS or computer'}</span></button>`);
}

// the daemon's snapshot changed: a sync started or ended, so fetch the machines again
export function machinesDaemon(d) {
  const key = JSON.stringify([d?.side?.kind === 'remote-sync' ? [d.side.path, d.side.progress] : null,
    (d?.queued || []).filter(j => j.kind === 'remote-sync').map(j => j.path),
    (d?.recent || []).filter(j => j.kind === 'remote-sync').map(j => j.id)]);
  if (key === M.jobs) return;
  const first = M.jobs === '';
  M.jobs = key;
  if (!first) loadMachines();
}

// ---------- the pathbar in a machine's views

export function machinePath() {
  const m = machine(M.cur);
  if (M.adding || !m) return `<span class="crumb" style="cursor:default">${M.adding ? 'Add a machine' : 'Machines'}</span>`;
  const head = `<button class="crumb" type="button" data-mc="${m.id}" title="${esc(m.url)}">${esc(m.name)}</button>`;
  const crumbs = M.listing?.crumbs || [];
  return head + crumbs.map(c => `<span class="crumb-sep">›</span><button class="crumb" type="button" data-mc="${m.id}" data-mc-dir="${c.key}">${esc(c.name)}</button>`).join('');
}

// ---------- a machine's page

export function showMachine(id = M.cur, dir = null, sel = null) {
  if (id != null) M.cur = id;
  M.adding = false;
  M.dir = dir;
  M.sel = sel;
  M.file = null;
  hooks.enter('machine');
  if (dir) return browse(dir, sel);
  M.listing = null;
  renderMachine();
  hooks.refreshPath?.();
  loadMachines();
}

function renderMachine() {
  const m = machine(M.cur);
  const v = $('#view');
  if (!m) {
    v.innerHTML = `<div class="empty"><div>${ICON}<h2>No such machine</h2><p>It may have been removed.</p></div></div>`;
    return;
  }
  const s = M.data.settings || { proof_days: 7 };
  const info = m.info || {};
  const why = M.data.why_not;
  const unreachable = m.last_error && (!m.last_sync || m.last_try > m.last_sync);
  const roots = m.roots || [];
  v.innerHTML = `<div class="plan machines">
    <div class="pl-head"><div><h2>${esc(m.name)}</h2>
      <p class="muted"><code>${esc(m.url)}</code>${m.db ? ` · index <code>${esc(m.db)}</code>` : ''} · read-only, reached with your ssh keys</p></div>
      <div class="pl-acts">
        <button class="btn primary" type="button" data-mc-sync="${m.id}" ${why || busy(m) || !M.data.daemon ? 'disabled' : ''} title="${M.data.daemon ? 'Fetch what changed there, and hash there what could be a copy of a file here' : 'Syncing runs in the BackDown service: backdown remote sync in a terminal'}">${busy(m) ? 'Syncing…' : 'Sync now'}</button>
        <button class="btn" type="button" data-mc-test="${m.id}" ${why ? 'disabled' : ''}>Test connection</button>
        <button class="btn" type="button" data-mc-remove="${m.id}">Remove…</button>
      </div></div>
    ${why ? `<p class="warn-t">${esc(why)}</p>` : ''}
    <section class="card-ish mc-status">
      <div class="mc-state ${unreachable ? 'bad' : m.last_sync ? 'ok' : ''}">${unreachable ? `<b>Can’t be reached</b> (tried ${ago(m.last_try)})<p class="mc-err">${esc(m.last_error)}</p>`
        : m.last_sync ? `<b>Synced ${ago(m.last_sync)}</b>` : '<b>Not synced yet</b>'}
        ${busy(m) && m.job.progress ? `<p class="muted">${esc(m.job.progress)}</p>` : ''}</div>
      <dl class="kv">
        <dt>Host</dt><dd>${esc(info.host || '—')}</dd>
        <dt>BackDown there</dt><dd>${esc(info.version || '—')}</dd>
        <dt>Its index</dt><dd>${esc(info.db || m.db || '—')}</dd>
        <dt>Last answered</dt><dd>${info.seen ? when(info.seen) : '—'}${info.skew && Math.abs(info.skew) > 120 ? ` <span class="warn-t">(its clock is ${Math.round(Math.abs(info.skew) / 60)} min ${info.skew > 0 ? 'ahead' : 'behind'}; allowed for)</span>` : ''}</dd>
      </dl>
    </section>
    <section class="card-ish"><h3>Drives there</h3>
      ${roots.length ? `<table class="list mc-drives"><thead><tr><th>Drive</th><th class="n">Files</th><th class="n">Size</th><th>Scanned there</th><th>Its copies</th></tr></thead><tbody>
        ${roots.map(r => `<tr><td><button class="rowlink" type="button" data-mc="${m.id}" data-mc-dir="${r.key}"><b>${esc(r.name)}</b> <span class="muted">${esc(r.path)}</span></button></td>
          <td class="n">${count(r.files)}</td><td class="n">${bytes(r.bytes)}</td>
          <td title="${esc(when(r.seen_at))}">${ago(r.seen_at)}</td>
          <td>${r.counts ? `<span class="pill ok">count as proof</span>` : `<span class="pill warn" title="Confirmed more than ${s.proof_days} days ago">too long ago to count</span>`}
            <small class="muted">${count(r.hashed)} of ${count(r.cached)} hashed</small></td></tr>`).join('')}</tbody></table>`
        : `<p class="muted">${m.last_sync ? 'Nothing is indexed there yet: run <code>backdown index</code> on that machine.' : 'Sync it to see its drives.'}</p>`}
    </section>
    <section class="card-ish"><h3>How copies there count</h3>
      <p>A file here counts as kept when an identical copy is on ${esc(m.name)} — by SHA-256, never by name or size — and ${esc(m.name)} confirmed it
        within the last <b>${s.proof_days} days</b>: its own scan saw the file, or it re-read it for BackDown. Before Free up deletes anything
        whose only copy is there, ${esc(m.name)} reads that copy again and it must still match; if ${esc(m.name)} can’t be reached then, the file stays.</p>
      <form class="mc-settings" data-mc-settings><label>Count copies confirmed within <input type="number" name="proof_days" min="1" max="90" value="${s.proof_days}"> days</label>
        <label>Sync every <input type="number" name="sync_hours" min="1" max="168" value="${s.sync_hours}"> hours</label>
        <button class="btn sm" type="submit">Save</button></form>
      <p class="muted">Nothing on ${esc(m.name)} is ever changed: <code>backdown serve-stdio</code> there only reads its index, and hashes a file only when asked.</p>
    </section>
  </div>`;
}

// ---------- browsing a machine's folders (read-only)

async function browse(dir, sel = null) {
  const seq = ++M.seq;
  const v = $('#view');
  if (!M.listing || M.listing.key !== dir) v.innerHTML = '<div class="empty"><p>Asking…</p></div>';
  let l;
  try { l = await api('machine-ls', { m: M.cur, dir }); }
  catch (e) { if (seq === M.seq) v.innerHTML = `<div class="empty"><div><h2>Can’t show that folder</h2><p>${esc(e.message)}</p></div></div>`; return; }
  if (seq !== M.seq || !hooks.isMode('machine')) return;
  M.listing = l; M.dir = dir;
  hooks.refreshPath?.();
  renderMachinesNav();
  renderListing();
  if (sel) pick(sel);
}

function renderListing() {
  const m = machine(M.cur), l = M.listing, v = $('#view');
  if (!l || !m) return;
  const entries = [...(l.entries || [])].sort((a, b) => (a.kind === b.kind ? 0 : a.kind === 'dir' ? -1 : 1) || a.name.localeCompare(b.name, undefined, { numeric: true }));
  const banner = l.live
    ? `<span><b>Read-only</b> · live from ${esc(m.name)} over ssh, as its last scan there saw it${l.scanned_at ? ` (${esc(when(l.scanned_at))})` : ''}</span>`
    : `<span><b>Read-only</b> · as synced from ${esc(m.name)} ${ago(l.synced_at || m.last_sync)} (folders without sizes)${l.why_not_live ? ` — it can’t be asked now: ${esc(l.why_not_live)}` : ''}</span>`;
  v.innerHTML = `<div class="mc-browse">
    <div class="banner ${l.live ? '' : 'warn'} mc-banner">${banner}${l.truncated ? ' · the first 10,000 entries' : ''}</div>
    <div class="mc-split">
      <div class="mc-list">${entries.length ? `<table class="list"><thead><tr><th>Name</th><th class="n">Size</th><th>Modified</th><th class="n">Copies there</th></tr></thead><tbody>
        ${entries.map(e => `<tr data-mc-e="${e.kind === 'dir' ? 'd' : 'f'}" data-mk="${e.key}" ${e.id ? `data-rid="${e.id}"` : ''} class="${M.sel && e.key === M.sel ? 'sel' : ''}" tabindex="-1">
          <td><span class="nmc">${e.kind === 'dir' ? folderIcon(20) : fileIcon(e.name, 20)}<span>${esc(e.name)}</span></span></td>
          <td class="n">${e.kind === 'dir' && !e.size ? '' : bytes(e.size)}</td><td class="dim">${e.mtime ? esc(when(e.mtime)) : ''}</td>
          <td class="n dim">${e.copies ? e.copies : ''}</td></tr>`).join('')}</tbody></table>` : '<p class="muted pad">Empty.</p>'}</div>
      <aside class="mc-detail" id="mc-detail">${detailHtml()}</aside>
    </div></div>`;
}

function pick(key) {
  M.sel = key;
  document.querySelectorAll('.mc-list tr.sel').forEach(r => r.classList.remove('sel'));
  const row = document.querySelector(`.mc-list tr[data-mk="${CSS.escape(key)}"]`);
  row?.classList.add('sel');
  row?.scrollIntoView({ block: 'nearest' });
  const rid = row?.dataset.rid;
  if (!rid) { M.file = null; $('#mc-detail').innerHTML = detailHtml(); return; }
  loadFile(+rid);
}

async function loadFile(rid) {
  const seq = ++M.fseq;
  $('#mc-detail').innerHTML = '<p class="note">Asking…</p>';
  let f;
  try { f = await api('machine-file', { m: M.cur, id: rid }); } catch (e) { if (seq === M.fseq) $('#mc-detail').innerHTML = `<p class="note">${esc(e.message)}</p>`; return; }
  if (seq !== M.fseq) return;
  M.file = f;
  $('#mc-detail').innerHTML = detailHtml();
}

function detailHtml() {
  const m = machine(M.cur), f = M.file;
  if (!f) {
    const l = M.listing;
    return l ? `<div class="hero">${folderIcon(56)}<h3>${esc(l.name)}</h3></div>
      <dl class="kv">${l.files != null ? `<dt>Contains</dt><dd>${count(l.files)} files</dd><dt>Size</dt><dd>${bytes(l.bytes)}</dd>` : ''}<dt>On</dt><dd>${esc(m?.name)}</dd><dt>Path there</dt><dd>${esc(l.path)}</dd></dl>
      <p class="note">Pick a file to see its copies here and on other machines. Files on ${esc(m?.name)} are shown, never copied or changed.</p>` : '';
  }
  const local = f.local_copies || [], others = f.remote_copies || [], there = f.copies || [];
  return `<div class="hero">${fileIcon(f.name, 56)}<h3>${esc(f.name)}</h3><div class="note">${bytes(f.size)} · on ${esc(m?.name)}${f.live ? '' : ' (as last synced)'}</div></div>
    ${!f.live && f.why_not_live ? `<p class="note">${esc(m?.name)} can’t be asked now: ${esc(f.why_not_live)}</p>` : ''}
    <dl class="kv"><dt>Modified</dt><dd>${esc(when(f.mtime))}</dd>
      <dt>SHA-256</dt><dd class="hash">${f.sha256 ? esc(f.sha256) : '<span class="note">not hashed there (it is when a file here has its size)</span>'}</dd>
      ${f.hashed_at ? `<dt>Read there</dt><dd>${esc(when(f.hashed_at))} <span class="note">(hashed for BackDown here)</span></dd>` : ''}
      <dt>Path there</dt><dd>${esc(f.path)}</dd>
      ${f.error ? `<dt>Problem</dt><dd>${esc(f.error)}</dd>` : ''}</dl>
    <h4>On this computer (${local.length})</h4>
    ${local.length ? local.map(c => `<button class="rowlink" type="button" data-mc-local="${c.dir_key}" data-mc-local-sel="${c.key}">${esc(c.path)}</button>`).join('') : `<p class="note">${f.sha256 ? 'No identical file here.' : 'Unknown until it’s hashed there.'}</p>`}
    ${there.length ? `<h4>Other copies on ${esc(m?.name)} (${there.length})</h4>${there.map(c => `<p class="mc-path">${esc(c.path)}</p>`).join('')}` : ''}
    ${others.length ? `<h4>On other machines (${others.length})</h4>${others.map(copyRow).join('')}` : ''}
    ${(f.versions || []).length > 1 ? `<h4>Versions there (${f.versions.length})</h4>${[...f.versions].reverse().map(x => `<div class="ver"><span class="when">${esc(new Date(x.from * 1000).toLocaleDateString())}</span><span class="meta">${bytes(x.size)}${x.sha256 ? ` · ${x.sha256.slice(0, 10)}…` : ''}</span></div>`).join('')}` : ''}
    <p class="note">To open it, open it on ${esc(m?.name)}: BackDown doesn’t bring files over from other machines.</p>`;
}

function copyRow(c) {
  const dir = c.dir_key;
  return `<button class="rowlink mc-copy${c.counts ? '' : ' stale'}" type="button" data-mc="${c.machine_id}" data-mc-dir="${dir}" data-mc-sel="${c.key}">
    on <b>${esc(c.machine)}</b>: ${esc(c.path)}
    <span class="note">${c.counts ? `confirmed ${ago(c.confirmed_at)} · counts as a verified copy` : esc(c.why_not || 'doesn’t count')}</span></button>`;
}

// ---------- adding a machine

export function showAdd() {
  M.adding = true; M.dir = null; M.listing = null;
  hooks.enter('machine');
  hooks.refreshPath?.();
  renderMachinesNav();
  const a = M.add || {};
  $('#view').innerHTML = `<div class="plan machines">
    <div class="pl-head"><div><h2>Add a machine</h2>
      <p class="muted">A NAS or another computer with BackDown on it. Its index is read over ssh, with your keys — the same as <code>ssh nas.example.org</code> in a terminal.
        BackDown never asks for a password, never listens on the network, and never changes anything there.</p></div></div>
    ${M.data?.why_not ? `<p class="warn-t">${esc(M.data.why_not)}</p>` : ''}
    <form class="card-ish mc-add" data-mc-add-form>
      <label>Name <input name="name" required maxlength="32" pattern="[A-Za-z0-9][A-Za-z0-9._\\-]{0,31}" placeholder="nas" value="${esc(a.name || '')}" autocomplete="off"></label>
      <label>Address <input name="url" required placeholder="ssh://me@nas.example.org:22" value="${esc(a.url || '')}" autocomplete="off" spellcheck="false">
        <small class="muted">ssh://[user@]host[:port]; a host alias from your ~/.ssh/config works too</small></label>
      <details ${a.db || a.command ? 'open' : ''}><summary>If BackDown there isn’t set up the usual way</summary>
        <label>Its index there <input name="db" placeholder="its default, or the one its service uses" value="${esc(a.db || '')}" spellcheck="false"></label>
        <label>Where backdown is there <input name="command" placeholder="backdown (on the PATH of ssh sessions)" value="${esc(a.command || '')}" spellcheck="false"></label>
      </details>
      <div class="pl-acts"><button class="btn primary" type="submit" ${M.data?.why_not ? 'disabled' : ''}>Add and connect</button><span class="muted" id="mc-add-state"></span></div>
      <div id="mc-add-result"></div>
    </form>
    <section class="card-ish"><h3>Before it works</h3>
      <ol class="mc-steps"><li>Install BackDown on that machine and index its drives there (<code>backdown index /volume1</code>, or <code>backdown service install</code>).</li>
        <li>Make sure <code>ssh</code> from this computer logs in there without a password (<code>ssh-copy-id</code>, or your ssh agent), and that you’ve accepted its host key once in a terminal.</li>
        <li>Add it here. BackDown then syncs it every few hours: what’s indexed there, and the hashes of files there that could be copies of files here.</li></ol>
    </section>
  </div>`;
  $('#view [name=name]')?.focus();
}

async function onSubmit(e) {
  const f = e.target;
  if (f.matches('[data-mc-add-form]')) {
    e.preventDefault();
    const p = Object.fromEntries(new FormData(f));
    M.add = p;
    const btn = f.querySelector('[type=submit]');
    btn.disabled = true;
    $('#mc-add-state').textContent = 'Connecting over ssh…';
    let r;
    try { r = await post('machine-add', p); } catch (err) {
      btn.disabled = false; $('#mc-add-state').textContent = '';
      $('#mc-add-result').innerHTML = `<p class="warn-t">${esc(err.message)}</p>`;
      return;
    }
    M.add = null;
    await loadMachines();
    if (r.reached) toast(r.syncing ? `Added ${r.machine.name}: syncing it now` : `Added ${r.machine.name}: sync it to count its copies`);
    else toast(`Added ${r.machine.name}, but it can’t be reached yet`);
    showMachine(r.machine.id);
    return;
  }
  if (f.matches('[data-mc-settings]')) {
    e.preventDefault();
    try { await post('machine-settings', Object.fromEntries(new FormData(f))); toast('Saved'); } catch (err) { return toast(err.message); }
    loadMachines();
  }
}

// ---------- clicks

async function onClick(e) {
  const t = e.target;
  const add = t.closest('[data-mc-add]');
  if (add) return showAdd();
  const loc = t.closest('[data-mc-local]');
  if (loc) return hooks.openLocal(loc.dataset.mcLocal, loc.dataset.mcLocalSel);
  const sync = t.closest('[data-mc-sync]');
  if (sync) {
    sync.disabled = true;
    try { await post('machine-sync', { m: sync.dataset.mcSync }); toast('Syncing: see Activity for progress'); } catch (err) { toast(err.message); }
    return loadMachines();
  }
  const test = t.closest('[data-mc-test]');
  if (test) {
    test.disabled = true; test.textContent = 'Connecting…';
    let r;
    try { r = await post('machine-test', { m: test.dataset.mcTest }); } catch (err) { r = { reached: false, test_error: err.message }; }
    await loadMachines();
    renderMachine();
    toast(r.reached ? r.test : 'Can’t be reached: see why on the page');
    return;
  }
  const rm = t.closest('[data-mc-remove]');
  if (rm) {
    const m = machine(+rm.dataset.mcRemove);
    if (!m || !confirm(`Forget ${m.name}? Nothing on it is touched; its copies stop counting here, and what was cached about it is dropped.`)) return;
    try { await post('machine-remove', { m: m.id }); } catch (err) { return toast(err.message); }
    M.cur = null;
    await loadMachines();
    hooks.leave();
    return;
  }
  const row = t.closest('.mc-list tr[data-mk]');
  if (row) {
    if (row.dataset.mcE === 'd') return browse(row.dataset.mk);
    return pick(row.dataset.mk);
  }
  const go = t.closest('[data-mc]');
  if (go) {
    const id = +go.dataset.mc;
    return showMachine(id, go.dataset.mcDir || null, go.dataset.mcSel || null);
  }
}

// ---------- other machines' copies, where files here are shown

// a file's details: the same content on other machines, and whether each copy counts
export function remoteCopiesHtml(f) {
  const c = f?.remote_copies;
  if (!c?.length) return '';
  return `<h4>On other machines (${c.length})</h4>${c.map(copyRow).join('')}
    <p class="note">A copy there counts as proof when it’s hashed there and was confirmed there within ${f.proof_days || 7} days.</p>`;
}

// a duplicate group: where the same content is on other machines
export function dupesRemoteHtml(g) {
  if (!g?.remote?.length) return '';
  return g.remote.map(c => `<button class="rowlink mc-copy${c.counts ? '' : ' stale'}" type="button" data-mc="${c.machine_id}" data-mc-dir="${c.dir_key}" data-mc-sel="${c.key}">on <b>${esc(c.machine)}</b>: ${esc(c.path)}</button>`).join('');
}

// Duplicates: content that's here once and again on another machine
export function acrossHtml(d) {
  const a = d?.across;
  if (!a?.files) return '';
  return `<h3 class="mc-h">Here once, and again on another machine</h3>
    <p class="muted mc-sub">${bytes(a.bytes)} in ${count(a.files)} file${a.files === 1 ? '' : 's'} here ha${a.files === 1 ? 's' : 've'} an identical copy on another machine;
      for ${bytes(a.proven_bytes)} of it that copy counts as proof (hashed there, and confirmed there within ${M.data?.settings?.proof_days || 7} days), so Free up can free it here.</p>
    <table class="list mc-across"><thead><tr><th class="n">Each</th><th>Here</th><th>There</th></tr></thead><tbody>${a.largest.map(g => `
      <tr><td class="n"><b>${bytes(g.size)}</b></td><td><button class="rowlink" type="button" data-dupe="${esc(g.path)}">${esc(g.path)}</button></td>
      <td>${dupesRemoteHtml({ remote: g.remote })}</td></tr>`).join('')}</tbody></table>`;
}

// Free up: what of a selection is kept only on another machine
export function wipeRemoteLine(it, a) {
  if (!it?.remote_files) return '';
  return `<li><span class="dot k mc-k"></span>${bytes(it.remote_bytes)} (${count(it.remote_files)} files) of that is kept only on another machine, confirmed there
    ${it.remote_confirmed ? ago(it.remote_confirmed) : ''} (within ${a.proof_days} days). That machine reads each copy again just before anything is deleted here; if it can’t be reached then, those files stay.</li>`;
}

