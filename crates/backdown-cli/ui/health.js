// Drive health in the browser: a pill per drive (Activity's connected drives, the drive schedule,
// the sidebar), a details panel with the readings and small trends, a banner when a connected
// drive is failing (Rescue one click away), mirrors on a drive that's dying, and the list of
// notifications. Everything comes from /api/health and /api/notifications (health.rs); nothing
// here touches a drive. The daemon's snapshot says when either changed, so they're fetched then.
// The other machines' drives (Team) come from /api/health-machines: as each machine's own BackDown
// last read them, fetched with its last sync; nothing here reaches a machine either.
import { api, when } from './app.js';
import { toast } from './extras.js';

const $ = s => document.querySelector(s);
const esc = s => String(s ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const num = n => Number(n || 0).toLocaleString();
const load = k => { try { return sessionStorage.getItem(k); } catch { return null; } };
const store = (k, v) => { try { sessionStorage.setItem(k, v); } catch {} };

export const H = { data: null, notes: null, settings: null, machines: null, change: undefined, onRescue: null, onChange: null };
const WORD = { ok: 'healthy', warning: 'warning', failing: 'failing', unknown: 'no health data' };
const RANK = { failing: 3, warning: 2, ok: 1, unknown: 0 };

const rtf = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' });
function ago(t) {
  if (!t) return '—';
  const s = t - Date.now() / 1000;
  for (const [unit, n] of [['day', 86400], ['hour', 3600], ['minute', 60]]) if (Math.abs(s) >= n) return rtf.format(Math.round(s / n), unit);
  return 'just now';
}
const norm = s => String(s || '').toUpperCase().replace(/[^0-9A-Z]/g, '');
const label = s => s.health.model || s.health.device || 'A drive';

// ---------- loading

async function post(path, params = {}) {
  const r = await fetch(`/api/${path}?${new URLSearchParams(params)}`, { method: 'POST', headers: { 'X-BackDown': '1' }, credentials: 'same-origin' });
  const j = await r.json().catch(() => ({ error: r.statusText }));
  if (!r.ok || j.error) throw new Error(j.error || r.statusText);
  return j;
}

export async function loadHealth() {
  try { H.data = await api('health'); } catch (e) { H.data = { drives: [], notes: [], error: e.message }; }
  renderBanner();
  H.onChange?.();
  const open = $('.hp-wrap');
  if (open) showDetails(open.dataset.id, false);
}

export async function loadNotes() {
  try { H.notes = await api('notifications', { limit: 30 }); } catch { H.notes = { notes: [], unread: 0 }; }
  paintBadge();
  H.onChange?.();
}

async function loadSettings() {
  try { H.settings = await api('notify-settings'); } catch { H.settings = null; }
  H.onChange?.();
}

// the other machines' drives; locked (Team), it's said only to someone who has added machines
async function loadMachines() {
  try { H.machines = await api('health-machines'); }
  catch (e) {
    let n = 0;
    if (e.refusal) { try { n = (await api('machines')).machines?.length || 0; } catch {} }
    H.machines = { machines: [], refusal: n ? e.refusal : null, error: e.refusal ? null : e.message };
  }
  H.onChange?.();
  const open = $('.hp-wrap');
  if (open && open.dataset.id?.startsWith('@')) showDetails(open.dataset.id, false);
}

// The daemon's snapshot changed: fetch again only if health or notifications did (a sync of
// another machine counts: what's known of its drives changed).
export function daemonChanged(d) {
  const c = d?.health?.change;
  if (c === H.change) return;
  const first = H.change === undefined;
  H.change = c;
  if (!first || !H.data) loadHealth();
  loadNotes();
  if (!first || !H.machines) loadMachines();
  if (!H.settings) loadSettings();
}

export function mountHealth({ onRescue, onChange }) {
  Object.assign(H, { onRescue, onChange });
  document.addEventListener('click', onClick);
  loadHealth();
  loadNotes();
  loadMachines();
}

// ---------- which drive is which

const drives = () => H.data?.drives || [];
// a drive on another machine goes by "@machine:its id" (a notification about it says the same)
const remoteDrives = () => (H.machines?.machines || []).flatMap(m => (m.drives || []).map(s => ({ ...s, key: `@${m.name}:${s.id}` })));
const findDrive = id => drives().find(x => x.id === id || x.health.device === id) || remoteDrives().find(x => x.key === id) || null;

// a disk from the volume probe: by serial (however each platform spells it), else by device
export function healthOfDisk(dk) {
  const sn = norm(dk?.serial);
  return drives().find(s => s.present && ((sn && norm(s.health.serial) === sn) || s.health.device === dk?.device)) || null;
}

// the connected drive a folder is on: the deepest mount point holding it
export function healthOfPath(path) {
  if (!path) return null;
  let best = null, len = -1;
  for (const s of drives()) {
    if (!s.present) continue;
    for (const m of s.health.mounts || []) {
      const inside = path === m || path.startsWith(m.endsWith('/') || m.endsWith('\\') ? m : m + (m.includes('\\') ? '\\' : '/'));
      if (inside && m.length > len) { best = s; len = m.length; }
    }
  }
  return best;
}

// ---------- pills, dots and notes for other views

function title(s) {
  const f = (s.findings || []).filter(x => x.level === s.verdict).map(x => x.text);
  const bits = [`${label(s)}: ${WORD[s.verdict] || s.verdict}`];
  if (f.length) bits.push(f.join('; '));
  else if (s.verdict === 'unknown' && s.health.note) bits.push(s.health.note);
  bits.push('Click for details');
  return bits.join('\n');
}

export function pill(s, { small = false } = {}) {
  if (!s) return '';
  return `<button class="hpill v-${esc(s.verdict)}${small ? ' sm' : ''}" type="button" data-health="${esc(s.key || s.id || s.health.device)}" title="${esc(title(s))}"><i></i>${esc(WORD[s.verdict] || s.verdict)}</button>`;
}

// the sidebar: a dot, and a word only when something's wrong
export function dot(s) {
  if (!s || s.verdict === 'unknown') return '';
  return `<span class="hdot v-${esc(s.verdict)}" title="${esc(title(s))}"></span>${RANK[s.verdict] > 1 ? `<b class="hword v-${esc(s.verdict)}">${esc(WORD[s.verdict])}</b>` : ''}`;
}

// a mirror whose copy sits on a drive that's going: say so, and what to do
export function mirrorNote(m) {
  const s = healthOfPath(m.target);
  if (!s || RANK[s.verdict] < 2) return '';
  return `<span class="hmirror v-${esc(s.verdict)}">This mirror drive is dying (${esc(label(s))}: ${esc(WORD[s.verdict])}): add another mirror on a healthy drive, <code>backdown mirror add ${esc(m.source)} /another/drive/folder</code>.
    <button class="btn sm" type="button" data-health="${esc(s.id)}">Details</button></span>`;
}

// a line for the top of Activity's connected drives: when health was last read, and read now
export function healthHead() {
  const d = H.data;
  if (!d) return '<span class="muted hhead">Reading drive health…</span>';
  const bad = drives().filter(s => s.present && RANK[s.verdict] > 1).length;
  return `<span class="muted hhead">${d.running ? 'Reading drive health…' : `Health read ${ago(d.checked_at)}`}${bad ? ` · <b class="v-warning">${bad} drive${bad === 1 ? '' : 's'} need${bad === 1 ? 's' : ''} attention</b>` : ''}
    ${d.daemon ? `<button class="btn sm" type="button" data-health-check${d.running ? ' disabled' : ''}>Check now</button>` : ''}</span>`;
}

function paintBadge() {
  const b = $('#activity-btn');
  if (!b) return;
  let el = b.querySelector('.nbadge');
  const n = H.notes?.unread || 0;
  if (!n) { el?.remove(); return; }
  if (!el) { el = document.createElement('span'); el.className = 'nbadge'; b.append(el); }
  el.textContent = n > 99 ? '99+' : String(n);
  el.title = `${n} unread notification${n === 1 ? '' : 's'}`;
}

// ---------- the failing-drive banner

function renderBanner() {
  const el = $('#health-banner');
  if (!el) return;
  const failing = drives().filter(s => s.present && s.verdict === 'failing' && load(`bd.hb.${s.id}`) !== '1');
  if (!failing.length) { el.hidden = true; el.innerHTML = ''; return; }
  const s = failing[0];
  const why = (s.findings || []).find(f => f.level === 'failing')?.text || 'its own check says it is failing';
  el.hidden = false;
  el.innerHTML = `<span class="hb-icon" aria-hidden="true"><svg viewBox="0 0 24 24"><path d="M12 3 2 21h20z"/><path d="M12 10v5M12 18h.01"/></svg></span>
    <span class="hb-text"><b>${esc(label(s))} is failing</b> (${esc(s.health.device)}): ${esc(why)}.
      What's on it may soon be unreadable: copy it now, while it still reads, to another drive.${failing.length > 1 ? ` <span class="muted">${failing.length - 1} more failing.</span>` : ''}</span>
    <button class="btn sm primary" type="button" data-health-rescue="${esc(s.id)}">Copy it now</button>
    <button class="btn sm" type="button" data-health="${esc(s.id)}">Details</button>
    <button class="wp-x" type="button" data-health-dismiss="${esc(s.id)}" aria-label="Hide this until the browser is closed" title="Hide this until the browser is closed">×</button>`;
}

function rescue(id) {
  const s = drives().find(x => x.id === id || x.health.device === id);
  if (!s) return;
  $('.hp-wrap')?.remove();
  H.onRescue?.(s.health.device, label(s));
}

// ---------- the details panel

// a small line chart of one series; gaps (unknown values) are skipped
function spark(points, key, { unit = '', w = 240, h = 44 } = {}) {
  const pts = points.filter(p => p[key] != null);
  if (pts.length < 2) return '';
  const xs = pts.map(p => p.at), ys = pts.map(p => p[key]);
  const [x0, x1] = [Math.min(...xs), Math.max(...xs)], [lo, hi] = [Math.min(...ys), Math.max(...ys)];
  const X = x => 2 + (x1 > x0 ? (x - x0) / (x1 - x0) : 0.5) * (w - 4);
  const Y = y => h - 4 - (hi > lo ? (y - lo) / (hi - lo) : 0.5) * (h - 8);
  const line = pts.map(p => `${X(p.at).toFixed(1)},${Y(p[key]).toFixed(1)}`).join(' ');
  const last = pts.at(-1);
  const worse = key !== 'temp' && ys.at(-1) > ys[0];
  return `<svg class="spark${worse ? ' up' : ''}" viewBox="0 0 ${w} ${h}" width="${w}" height="${h}" role="img" aria-label="${esc(key)} from ${esc(ys[0])}${unit} to ${esc(ys.at(-1))}${unit}">
    <polyline points="${line}"/><circle cx="${X(last.at).toFixed(1)}" cy="${Y(last[key]).toFixed(1)}" r="2.5"/></svg>
    <span class="spark-range">${esc(lo)}${unit} – ${esc(hi)}${unit}</span>`;
}

const COUNTERS = [
  ['temperature', 'Temperature', v => `${v} °C`],
  ['power_on_hours', 'Powered on', v => `${num(v)} h (${(v / 8766).toFixed(1)} years)`],
  ['reallocated', 'Reallocated sectors', num],
  ['pending', 'Pending sectors', num],
  ['uncorrectable', 'Uncorrectable sectors', num],
  ['bad_sectors', 'Bad sectors (reallocated + pending)', num],
  ['crc_errors', 'Cable (CRC) errors', num],
  ['percentage_used', 'Write endurance used', v => `${v}%`],
  ['available_spare', 'Spare blocks left', (v, h) => `${v}%${h.spare_threshold ? ` (its limit: ${h.spare_threshold}%)` : ''}`],
  ['media_errors', 'Media errors', num],
  ['critical_warning', 'Critical warnings', v => v ? `0x${v.toString(16)}` : 'none'],
  ['unsafe_shutdowns', 'Unsafe shutdowns', num],
  ['power_cycles', 'Power cycles', num],
];

export function showDetails(id, focus = true) {
  const s = findDrive(id);
  if (!s) return;
  const h = s.health;
  let d = $('.hp-wrap');
  if (!d) {
    d = document.createElement('div');
    d.className = 'help-wrap hp-wrap';
    document.body.append(d);
    const prev = document.activeElement;
    const close = () => { d.remove(); document.removeEventListener('keydown', onKey, true); prev?.focus?.(); };
    const onKey = e => { if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); close(); } };
    document.addEventListener('keydown', onKey, true);
    d.addEventListener('click', e => { if (e.target === d || e.target.closest('[data-hp-close]')) close(); });
  }
  d.dataset.id = s.key || s.id || h.device;
  const kv = COUNTERS.filter(([k]) => h[k] != null).map(([k, name, f]) => `<dt>${esc(name)}</dt><dd>${esc(f(h[k], h))}</dd>`).join('');
  const pts = s.series || [];
  const trends = [
    ['temp', 'Temperature', ' °C'], ['bad', 'Bad sectors', ''], ['wear', 'Wear', '%'], ['crc', 'Cable errors', ''], ['media', 'Media errors', ''],
  ].map(([k, name, unit]) => { const g = spark(pts, k, { unit }); return g ? `<div class="hp-trend"><span>${name}</span>${g}</div>` : ''; }).join('');
  const failing = s.verdict === 'failing';
  d.innerHTML = `<div class="help hpanel v-${esc(s.verdict)}" role="dialog" aria-modal="true" aria-labelledby="hp-t">
    <header><h2 id="hp-t">${esc(label(s))}</h2><button class="hb-btn" type="button" data-hp-close aria-label="Close"><svg viewBox="0 0 24 24"><path d="M6 6l12 12M18 6 6 18"/></svg></button></header>
    <p class="hp-sub">${pill(s)} <span class="muted">${[s.machine ? `on ${s.machine}${h.device ? ` (${h.device})` : ''}` : s.present ? h.device : 'not connected', h.transport, h.serial ? `S/N ${h.serial}` : '', h.protocol ? h.protocol.toUpperCase() : ''].filter(Boolean).map(esc).join(' · ')}</span></p>
    ${failing ? `<div class="hp-callout"><p><b>Copy what matters off this drive now.</b> A failing drive can stop reading at any moment, and every read wears it further: BackDown's Rescue copies it once, good areas first, and never writes to it.</p>
      ${s.machine ? `<p class="muted">It’s on ${esc(s.machine)}: copy it there, with BackDown’s Rescue on that machine.</p>`
        : s.present ? `<button class="btn primary" type="button" data-health-rescue="${esc(s.id)}">Copy it now</button>` : '<p class="muted">Connect it, then copy it from Rescue.</p>'}</div>` : ''}
    ${(s.findings || []).length ? `<ul class="hp-find">${s.findings.map(f => `<li class="v-${esc(f.level)}"><i></i>${esc(f.text)}</li>`).join('')}</ul>`
      : s.verdict === 'ok' ? '<p class="hp-ok">Nothing in its health data points to trouble.</p>' : ''}
    ${h.note ? `<p class="hp-note">${esc(h.note)}</p>` : ''}
    ${kv ? `<dl class="hp-kv">${kv}</dl>` : ''}
    ${trends ? `<h3>Since ${esc(when(s.first_at))}</h3><div class="hp-trends">${trends}</div>` : ''}
    ${(s.trend || []).length ? `<p class="muted">Since the first reading: ${esc(s.trend.join(', '))}.</p>` : s.readings > 1 ? '<p class="muted">No change since the first reading.</p>' : ''}
    ${(s.changes || []).length ? `<h3>What changed</h3><ol class="hp-changes">${[...s.changes].reverse().map(c => `<li class="v-${esc(c.level)}"><span>${esc(c.text)}</span><small class="muted" title="${esc(when(c.at))}">${ago(c.at)}</small></li>`).join('')}</ol>` : ''}
    <p class="hp-foot muted">Read ${s.machine ? `on ${esc(s.machine)} ` : ''}${ago(h.read_at)} from ${esc(h.source || 'nothing')}${s.readings ? ` · ${num(s.readings)} reading${s.readings === 1 ? '' : 's'} kept${s.machine ? ' there' : ''}` : ''}${s.machine ? ` · fetched ${ago(s.fetched_at)}, with its last sync` : ''}
      ${H.data?.daemon && s.present ? `<button class="btn sm" type="button" data-health-check${H.data.running ? ' disabled' : ''}>Check now</button>` : ''}</p>
  </div>`;
  if (focus) d.querySelector('[data-hp-close]').focus();
}

// ---------- the other machines' drives, in Activity (Team)

const cap = s => s ? s[0].toUpperCase() + s.slice(1) : '';

function machineDrives(m) {
  const rows = (m.drives || []).map(s => {
    const k = { ...s, key: `@${m.name}:${s.id}` };
    const h = s.health || {};
    const where = [h.serial ? `S/N ${h.serial}` : '', (h.mounts || []).slice(0, 2).join(', ')].filter(Boolean).join(' · ');
    const why = RANK[s.verdict] > 1 ? (s.findings || []).find(f => f.level === s.verdict)?.text : '';
    return `<tr${s.current ? '' : ' class="hm-stale"'}><td><b>${esc(label(k))}</b><small class="muted">${esc(where)}</small></td>
      <td>${pill(k)}${why ? `<small class="muted">${esc(why)}</small>` : ''}</td>
      <td class="muted" title="${esc(when(h.read_at))}">read there ${ago(h.read_at)}${s.current ? '' : '<small>not read there lately: taken out, or its BackDown isn’t running</small>'}</td></tr>`;
  }).join('');
  const told = (m.notes || []).slice(0, 3).map(n => `<li class="v-${esc(n.level)}"><i></i>${esc(n.title)} <span class="muted" title="${esc(when(n.at))}">${ago(n.at)}</span></li>`).join('');
  return `<div class="vol-disk"><div class="vol-head"><b>${esc(m.name)}</b>${RANK[m.worst] > 1 ? `<b class="hword v-${esc(m.worst)}">${esc(WORD[m.worst])}</b>` : ''}
      <span class="muted">${esc(m.url)} · ${m.last_sync ? `synced ${ago(m.last_sync)}` : 'not synced yet'}</span></div>
    ${rows ? `<table class="list act-drives hm-drives"><tbody>${rows}</tbody></table>` : ''}
    ${m.health?.note ? `<p class="muted pad">${esc(cap(m.health.note))}.</p>` : ''}
    ${told ? `<ul class="hm-told"><li class="muted">Its own latest notifications:</li>${told}</ul>` : ''}</div>`;
}

// each machine's drives with their verdict and how old the reading is, a word for a machine that
// can't say (an older BackDown there), and when it was synced
export function machinesSection() {
  const o = H.machines;
  if (!o) return '';
  const head = '<h3>Other machines’ drives <span class="muted hhead">as each machine’s own BackDown read them, fetched when it’s synced</span></h3>';
  if (o.refusal) {
    return `<section class="hmach">${head}<p class="muted pad">${esc(o.refusal.error)} ${esc(o.refusal.unlocks)}
      <button class="btn sm" type="button" data-set-go="licence">Add a licence key</button></p></section>`;
  }
  const ms = o.machines || [];
  if (!ms.length) return '';
  return `<section class="hmach">${head}${o.why_not ? `<p class="muted pad">${esc(cap(o.why_not))}.</p>` : ''}${ms.map(machineDrives).join('')}</section>`;
}

// ---------- notifications, in Activity

export function notesSection() {
  const n = H.notes;
  if (!n) return '';
  const set = H.settings;
  const toggle = set && set.available !== undefined ? `<label class="wp-chk hn-desk" title="${esc(set.why || 'Pop-ups on this computer\'s desktop; everything is listed here either way')}">
      <input type="checkbox" data-notify-desktop${set.desktop ? ' checked' : ''}${set.available ? '' : ' disabled'}> Show on the desktop</label>` : '';
  const head = `<h3>Notifications${n.unread ? ` <span class="nbadge in">${n.unread}</span>` : ''}
    ${n.unread ? '<button class="btn sm" type="button" data-notes-all>Mark all read</button>' : ''}${toggle}</h3>`;
  if (!n.notes.length) return `<section class="hnotes">${head}<p class="muted pad">Nothing to tell: BackDown notifies when a drive gets worse, a mirror stops matching its source, or a scheduled job fails.</p></section>`;
  // the unread ones and a few more; the rest on request
  const few = Math.max(5, n.unread);
  const list = H.allNotes ? n.notes : n.notes.slice(0, few);
  const more = n.notes.length > list.length ? `<button class="btn sm hn-more" type="button" data-notes-more>Show ${n.notes.length - list.length} older</button>` : '';
  return `<section class="hnotes">${head}<ol class="act-list">${list.map(x => `
    <li class="hn v-${esc(x.level)}${x.read ? '' : ' unread'}" data-note="${x.id}">
      <span class="ji"><i class="hn-dot"></i></span>
      <span class="jn">${esc(x.title)}<small>${esc(x.body)}</small></span>
      <span class="hn-act">${x.drive && findDrive(x.drive) ? `<button class="btn sm" type="button" data-health="${esc(x.drive)}">Details</button>` : ''}
        ${x.read ? '' : `<button class="act-x" type="button" data-note-read="${x.id}" title="Mark as read" aria-label="Mark as read">✓</button>`}</span>
      <span class="muted" title="${esc(when(x.at))}">${ago(x.at)}${x.shown ? '' : ' · <span title="Not shown on the desktop (turned off, or too many at once)">listed only</span>'}</span></li>`).join('')}</ol>${more}</section>`;
}

// ---------- clicks

async function onClick(e) {
  const t = e.target;
  const hd = t.closest('[data-health]');
  if (hd) { e.preventDefault(); e.stopPropagation(); return showDetails(hd.dataset.health); }
  const rs = t.closest('[data-health-rescue]');
  if (rs) return rescue(rs.dataset.healthRescue);
  const dm = t.closest('[data-health-dismiss]');
  if (dm) { store(`bd.hb.${dm.dataset.healthDismiss}`, '1'); return renderBanner(); }
  const ck = t.closest('[data-health-check]');
  if (ck) {
    ck.disabled = true;
    try { await post('health-check'); toast('Reading every drive’s health now'); } catch (err) { toast(err.message); }
    return;
  }
  const one = t.closest('[data-note-read]');
  if (one) { await post('notifications', { read: one.dataset.noteRead }).catch(() => {}); return loadNotes(); }
  if (t.closest('[data-notes-all]')) { await post('notifications', { read: 'all' }).catch(() => {}); return loadNotes(); }
  if (t.closest('[data-notes-more]')) { H.allNotes = true; return H.onChange?.(); }
  const desk = t.closest('[data-notify-desktop]');
  if (desk) {
    // what the box says now, even if the view is redrawn before the answer comes
    const want = desk.checked;
    if (H.settings) H.settings.desktop = want;
    try { H.settings = await post('notify-settings', { desktop: want ? 1 : 0 }); toast(want ? 'Notifications will show on the desktop' : 'Notifications are listed here only'); }
    catch (err) { if (H.settings) H.settings.desktop = !want; toast(err.message); }
    H.onChange?.();
  }
}
