// Tidying the archive (dedup.rs) and merging overlapping mailboxes (mailmerge.rs), in the Plan view.
//
// "Tidy the archive" asks the server what linking identical files inside one folder would save
// (/api/dedup-assess reads only the index and the files' metadata), then queues the daemon's job
// (POST /api/dedup), which re-reads both files of every pair before linking and journals each link.
// A plan's mailboxes that are copies of each other get a "Merge these mailboxes" button: POST
// /api/plan-merge turns the pair into one merge item, carried out when the plan is applied.
import { api, bytes, when } from './app.js';
import { toast } from './extras.js';

const esc = s => String(s ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const num = n => Number(n || 0).toLocaleString();
const $ = s => document.querySelector(s);
const post = async (path, params) => {
  const r = await fetch(`/demo/api/${path}?${new URLSearchParams(params)}`, { method: 'POST', headers: { 'X-BackDown': '1' }, credentials: 'same-origin' });
  const j = await r.json().catch(() => ({}));
  if (!r.ok || j.error) throw new Error(j.error || r.statusText);
  return j;
};

// what's typed and chosen survives redraws of the Plan view
export const TD = { folder: '', link: 'hard', min: '4K', a: null, err: null, busy: false, history: null, seq: 0 };

// ---------- merging a plan's mailboxes

/** The button under a plan item that can be merged with its twin (only before the plan runs). */
export function mergeButton(data, idx) {
  if (data.state !== 'new' || !(data.mergeable || []).includes(idx)) return '';
  return `<div class="pl-merge"><button class="btn sm" type="button" data-merge-item="${idx}">Merge these mailboxes</button>
    <span class="muted">one mailbox with every message once, by Message-ID, instead of this one's extra files beside the other</span></div>`;
}

/** A merge item's other snapshots. */
export function mergeSources(item) {
  if (item.action !== 'merge' || !item.merge?.length) return '';
  return `<ul class="pl-merged">${item.merge.map(m => `<li>+ <code>${esc(m)}</code></li>`).join('')}</ul>`;
}

// ---------- tidy the archive

function result(a) {
  if (TD.err) return `<p class="td-err warn-t">${esc(TD.err)}</p>`;
  if (!a) return '';
  const clone = a.link === 'clone';
  const facts = [
    a.already && `${num(a.already)} files already share storage`,
    a.kept_apart && `${num(a.kept_apart)} identical files kept apart: their permissions, owner or extended attributes${clone ? '' : ', or dates,'} differ from every other copy's`,
    a.elsewhere && `${num(a.elsewhere)} have other names outside this folder: left alone`,
    a.changed && `${num(a.changed)} changed since the last scan: left alone`,
    a.unhashed && `${num(a.unhashed)} aren't hashed yet, so their duplicates aren't known`,
  ].filter(Boolean);
  const blocked = a.why_not || (clone && !a.clones_possible
    ? `${a.filesystem || 'This filesystem'} can't share blocks between files: clones need btrfs, XFS or APFS (hardlinks work anywhere)` : '');
  const rel = p => p.startsWith(a.folder) ? p.slice(a.folder.length).replace(/^[\\/]/, '') : p;
  return `<div class="td-result">
    <p>${a.linked ? `<b>${num(a.linked)} files</b> in ${num(a.groups)} set${a.groups === 1 ? '' : 's'} of identical files could be ${clone ? 'cloned' : 'hardlinked'}, saving <b>${bytes(a.saved_bytes)}</b>`
      : 'Nothing to link: no two files here are identical and alike'}${a.filesystem ? ` <span class="muted">(${esc(a.filesystem)})</span>` : ''}.</p>
    ${facts.length ? `<ul class="wp-facts">${facts.map(f => `<li>${esc(f)}</li>`).join('')}</ul>` : ''}
    ${a.examples?.length ? `<details class="td-ex"><summary>Biggest savings</summary><ul>${a.examples.slice(0, 12).map(g => `
      <li><span class="pl-size">${bytes(g.saves)}</span> <code>${esc(rel(g.keeper))}</code> ← ${g.others.map(o => `<code>${esc(rel(o))}</code>`).join(', ')}</li>`).join('')}</ul></details>` : ''}
    <p class="td-warn ${clone ? '' : 'warn'}">${esc(a.warning)}</p>
    ${a.linked ? `<button class="btn primary" type="button" id="td-go"${blocked || TD.busy ? ` disabled title="${esc(blocked || '')}"` : ''}>${clone ? 'Clone' : 'Link'} ${num(a.linked)} files, saving ${bytes(a.saved_bytes)}</button>
      ${blocked ? `<p class="muted">${esc(blocked)}</p>` : ''}` : ''}
  </div>`;
}

function history() {
  const h = TD.history || [];
  if (!h.length) return '';
  return `<details class="td-hist"><summary>Earlier runs (${h.length})</summary><ul>${h.map(r => `
    <li><span class="pill ${r.state === 'done' ? 'ok' : r.state === 'failed' || r.state === 'partial' ? 'warn' : ''}">${esc(r.state)}</span>
      ${esc(when(r.created_at))} · ${esc(r.link === 'clone' ? 'clones' : 'hardlinks')} in <code>${esc(r.folder)}</code>${r.summary ? `<br><span class="muted">${esc(r.summary)}</span>` : ''}</li>`).join('')}</ul></details>`;
}

/** The "Tidy the archive" card; `folder` is where it starts (the plan's archive). */
export function tidyCard(folder) {
  if (!TD.folder && folder) TD.folder = folder;
  if (TD.history == null) {
    TD.history = [];
    api('dedups').then(h => { TD.history = h; const el = $('#td-hist'); if (el) el.innerHTML = history(); }).catch(() => {});
  }
  const radio = (v, label, note) => `<label class="chk"><input type="radio" name="td-link" value="${v}"${TD.link === v ? ' checked' : ''}> ${label} <span class="muted">${note}</span></label>`;
  return `<section class="card-ish td-card" id="td-card">
    <h3>Tidy the archive</h3>
    <p class="note">Files with the same content inside one folder can share their storage. Nothing outside the folder is touched, nothing is linked across drives, both files are read again before they're linked, and every link is journaled.</p>
    <label class="pl-to">Folder <input id="td-folder" spellcheck="false" value="${esc(TD.folder)}" placeholder="${esc(folder || '')}"></label>
    <div class="td-opts">
      ${radio('hard', 'Hardlinks', 'any drive; one file with several names, so editing one edits them all')}
      ${radio('clone', 'Clones', 'btrfs, XFS or APFS; separate files that share blocks until one changes')}
      <label class="td-min">Leave out files under <select id="td-min">${['4K', '64K', '1M', '100M'].map(m => `<option${TD.min === m ? ' selected' : ''}>${m}</option>`).join('')}</select></label>
    </div>
    <div><button class="btn" type="button" id="td-check"${TD.busy ? ' disabled' : ''}>${TD.busy ? 'Checking…' : 'See what it would save'}</button></div>
    <div id="td-out">${result(TD.a)}</div>
    <div id="td-hist">${history()}</div>
  </section>`;
}

async function check() {
  TD.folder = $('#td-folder').value.trim() || $('#td-folder').placeholder;
  TD.link = document.querySelector('input[name="td-link"]:checked')?.value || 'hard';
  TD.min = $('#td-min').value;
  const seq = ++TD.seq;
  TD.busy = true; TD.err = null;
  $('#td-check').disabled = true; $('#td-check').textContent = 'Checking…';
  try {
    const a = await api('dedup-assess', { folder: TD.folder, link: TD.link, min: TD.min });
    if (seq !== TD.seq) return;
    TD.a = { ...a, link: TD.link };
  } catch (e) {
    if (seq !== TD.seq) return;
    TD.a = null; TD.err = e.message;
  }
  TD.busy = false;
  const out = $('#td-out');
  if (out) out.innerHTML = result(TD.a);
  const b = $('#td-check');
  if (b) { b.disabled = false; b.textContent = 'See what it would save'; }
}

async function go(poll) {
  const a = TD.a;
  if (!a) return;
  const clone = a.link === 'clone';
  const msg = clone
    ? `Clone ${num(a.linked)} identical files in ${a.folder}, saving ${bytes(a.saved_bytes)}?\n\nEach stays a separate file; each pair is read again first.`
    : `Hardlink ${num(a.linked)} identical files in ${a.folder}, saving ${bytes(a.saved_bytes)}?\n\nLinked files become one file with several names: editing any of them changes them all. To make one separate again later, copy it and replace it with the copy.\n\nEach pair is read again first, and every link is journaled.`;
  if (!confirm(msg)) return;
  try {
    await post('dedup', { folder: a.folder, link: a.link, min: TD.min });
  } catch (e) { TD.err = e.message; $('#td-out').innerHTML = result(null); return; }
  TD.a = null; TD.history = null;
  $('#td-out').innerHTML = '<p class="muted">Queued: the BackDown service re-reads and links them, then rescans the folder\'s drive. Follow it in Activity.</p>';
  toast('Tidying the archive: follow it in Activity');
  poll?.();
}

/** Listeners for the card and the merge buttons, after the Plan view is drawn. */
export function wireTidy({ planId, reload, error, poll }) {
  $('#td-check')?.addEventListener('click', check);
  $('#td-folder')?.addEventListener('keydown', e => { if (e.key === 'Enter') check(); });
  document.querySelectorAll('input[name="td-link"]').forEach(r => r.addEventListener('change', () => { TD.link = r.value; if (TD.a) check(); }));
  $('#td-min')?.addEventListener('change', e => { TD.min = e.target.value; if (TD.a) check(); });
  $('#td-out')?.addEventListener('click', e => { if (e.target.closest('#td-go')) go(poll); });
  document.querySelectorAll('[data-merge-item]').forEach(b => b.addEventListener('click', async () => {
    b.disabled = true;
    try {
      await post('plan-merge', { id: planId, item: b.dataset.mergeItem });
      toast('Merged in the plan: applying it writes one mailbox');
      reload();
    } catch (e) { b.disabled = false; error(e); }
  }));
}
