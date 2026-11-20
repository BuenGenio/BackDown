// Settings, the licence and Pro features, and updates.
//
// Settings live in the daemon's data folder (settings.rs) and apply without a restart. Two
// sections belong to other parts of BackDown and appear when this daemon has them: notifications
// (/api/notify-settings) and fast rescans (/api/scan-settings); their fields are drawn from what
// the endpoint returns. Pro badges and explanations come from /api/features, whose table
// (tiers.rs) is the one place that says what's paid; while BackDown is a preview everything is
// unlocked, and this says so.
import { api, bytes, when, enterView, daemonState, isView } from './app.js';
import { toast } from './extras.js';

const $ = s => document.querySelector(s);
const esc = s => String(s ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const load = k => { try { return localStorage.getItem(k); } catch { return null; } };
const store = (k, v) => { try { localStorage.setItem(k, v); } catch {} };

async function post(path, params = {}, body) {
  const q = new URLSearchParams(Object.entries(params).filter(([, v]) => v !== undefined && v !== null));
  const headers = { 'X-BackDown': '1' };
  if (body !== undefined) headers['Content-Type'] = typeof body === 'string' ? 'text/plain; charset=utf-8' : 'application/json';
  const r = await fetch(`/api/${path}?${q}`, { method: 'POST', headers, credentials: 'same-origin', body: body === undefined || typeof body === 'string' ? body : JSON.stringify(body) });
  const j = await r.json().catch(() => ({ error: r.statusText }));
  if (!r.ok || j.error) { const e = new Error(j.error || r.statusText); e.refusal = j.needs ? j : null; throw e; }
  return j;
}

// an endpoint another part of BackDown adds: null when this daemon doesn't have it
async function optional(path) {
  try {
    const r = await fetch(`/api/${path}`, { credentials: 'same-origin' });
    const j = await r.json().catch(() => null);
    if (r.status === 404 || /no such endpoint/i.test(j?.error || '')) return null;
    if (!r.ok || !j || j.error) return { error: j?.error || r.statusText };
    return j;
  } catch { return null; }
}

// ---------- Pro features: badges, and what a locked view shows instead

export const FEAT = { data: null };

export async function loadFeatures() {
  try { FEAT.data = await api('features'); } catch { FEAT.data = null; }
  paintBadges();
  return FEAT.data;
}

const viewFeature = view => FEAT.data?.features.find(f => f.tier !== 'community' && f.views.includes(view));

export function paintBadges() {
  for (const f of FEAT.data?.features || []) {
    if (f.tier === 'community') continue;
    for (const v of f.views) {
      const btn = document.getElementById(`${v}-btn`);
      if (!btn) continue;
      let b = btn.querySelector('.pro-badge');
      if (!b) { b = document.createElement('span'); b.className = 'pro-badge'; btn.append(b); }
      b.textContent = f.tier_name;
      b.classList.toggle('locked', !f.allowed);
      b.title = !f.allowed ? `${f.name} needs BackDown ${f.tier_name}`
        : FEAT.data.preview ? `${f.name} is a ${f.tier_name} feature: free during the preview` : `${f.name}: in your licence`;
    }
  }
}

function lockedCard(f) {
  return `<div class="gate"><div class="gate-card">
    <span class="pro-badge big">${esc(f.tier_name)}</span>
    <h2>${esc(f.name)}</h2>
    <p>${esc(f.unlocks)}</p>
    <p class="muted">${esc(f.name)} is part of BackDown ${esc(f.tier_name)}. Everything that tells you what you have and what's duplicated stays free.</p>
    <div class="gate-acts"><button class="btn primary" type="button" data-set-go="licence">Add a licence key</button>
      <a class="btn" href="${esc(FEAT.data?.pricing || 'https://backdown.io/pricing')}" target="_blank" rel="noopener noreferrer">See pricing</a></div>
  </div></div>`;
}

// a view whose feature is locked shows what it would do and how to unlock it; true if it did
export function proGate(view) {
  const f = viewFeature(view);
  if (!f || f.allowed) return false;
  $('#view').innerHTML = lockedCard(f);
  return true;
}

// ---------- the update banner: quiet, dismissible per version

const UPD = { data: null, timer: null, installing: false };

export async function checkUpdates() {
  try { UPD.data = await api('update'); } catch { UPD.data = null; }
  paintUpdateBanner();
  clearTimeout(UPD.timer);
  UPD.timer = setTimeout(checkUpdates, 6 * 3600 * 1000);
}

function paintUpdateBanner() {
  const el = $('#update-banner');
  if (!el) return;
  const u = UPD.data, s = u?.status, i = u?.install;
  if (i?.state === 'running' || i?.state === 'done' || (i?.state === 'failed' && UPD.installing)) {
    el.hidden = false;
    el.className = `update-banner${i.state === 'failed' ? ' warn' : ''}`;
    el.innerHTML = i.state === 'failed'
      ? `<span>The update didn't install: ${esc(i.message)}</span><button class="update-x" type="button" data-upd-dismiss aria-label="Dismiss">×</button>`
      : `<span class="pulse"></span><span>Updating to BackDown ${esc(i.version)}: ${esc(i.message)}…</span>`;
    return;
  }
  if (!s?.newer || !s.latest || load('bd.updateDismissed') === s.latest.version) { el.hidden = true; return; }
  el.hidden = false;
  el.className = 'update-banner';
  el.innerHTML = `<span>BackDown ${esc(s.latest.version)} is available</span>
    <a href="${esc(s.latest.notes_url)}" target="_blank" rel="noopener noreferrer">Release notes</a>
    ${u.can_install ? '<button class="btn sm" type="button" data-upd-install>Update now</button>' : '<button class="btn sm" type="button" data-set-go="updates">How to update</button>'}
    <button class="update-x" type="button" data-upd-dismiss title="Not now" aria-label="Dismiss">×</button>`;
}

async function installUpdate() {
  if (UPD.installing) return;
  const s = UPD.data?.status;
  if (!confirm(`Install BackDown ${s?.latest?.version}?\n\nIt's downloaded from GitHub, checked against the release's signature and SHA-256 sums, and BackDown's background service restarts with it. The version you have now is kept, so it can be put back.`)) return;
  UPD.installing = true;
  try { await post('update-install'); } catch (e) { UPD.installing = false; return toast(e.message); }
  const from = s?.current;
  // follow the install, then wait for the daemon to come back as the new version and reload
  const tick = async () => {
    let u = null;
    try { u = await api('update'); } catch {}
    if (u) {
      UPD.data = u;
      if (u.status?.current && u.status.current !== from) return location.reload();
      if (u.install?.state === 'failed') { UPD.installing = true; paintUpdateBanner(); UPD.installing = false; return; }
    }
    paintUpdateBanner();
    setTimeout(tick, 1000);
  };
  tick();
}

document.addEventListener('click', e => {
  const t = e.target;
  if (t.closest('[data-upd-dismiss]')) {
    const v = UPD.data?.status?.latest?.version;
    if (v) store('bd.updateDismissed', v);
    UPD.installing = false;
    $('#update-banner').hidden = true;
    return;
  }
  if (t.closest('[data-upd-install]')) return installUpdate();
  const go = t.closest('[data-set-go]');
  if (go) return showSettings(go.dataset.setGo);
});

// ---------- the Settings view

const SET = { st: null, feat: null, sch: null, upd: null, notify: null, scan: null, schemaTimer: null, schemaSeq: 0 };

const dur = s => s % 86400 === 0 ? `${s / 86400} day${s === 86400 ? '' : 's'}` : s % 3600 === 0 ? `${s / 3600} hours` : `${Math.round(s / 60)} minutes`;
const RESCAN = [['never', 'Only when I ask, or a drive comes back'], ['6h', 'Every 6 hours'], ['12h', 'Every 12 hours'], ['24h', 'Every day'], ['3d', 'Every 3 days'], ['7d', 'Every week']];

export async function showSettings(section) {
  enterView('settings');
  const v = $('#view');
  v.innerHTML = '<div class="empty"><p>Loading…</p></div>';
  const soft = p => p.catch(e => ({ error: e.message }));
  [SET.st, SET.feat, SET.sch, SET.upd, SET.notify, SET.scan, SET.machines] = await Promise.all([
    soft(api('settings')), soft(api('features')), soft(api('schema')), soft(api('update')), optional('notify-settings'), optional('scan-settings'), optional('machines')]);
  if (!isView('settings')) return;
  if (!SET.feat.error) { FEAT.data = SET.feat; paintBadges(); }
  if (!SET.upd.error) { UPD.data = SET.upd; paintUpdateBanner(); }
  if (section) SET.tab = section;
  render();
}

// Keep section links stable while presenting a small set of task-based tabs.
const SETTINGS_GROUPS = [
  { id: 'general', title: 'General', hint: 'Updates & licence', sections: ['updates', 'licence', 'about'], icon: '<circle cx="12" cy="12" r="8"/><path d="M12 8h.01M12 11v5"/>' },
  { id: 'scanning', title: 'Scanning', hint: 'Scans & hashing', sections: ['scanning', 'hashing'], icon: '<path d="M20 11a8 8 0 1 0-2.3 5.7M20 5v6h-6"/>' },
  { id: 'organisation', title: 'Organisation', hint: 'Filing & junk rules', sections: ['schema', 'junk'], icon: '<path d="M3 7h7l2 2h9v11H3zM3 7V4h7l2 3"/>' },
  { id: 'protection', title: 'Protection', hint: 'Proof & notifications', sections: ['attest', 'notify'], icon: '<path d="m12 3 8 3v6c0 5-8 9-8 9s-8-4-8-9V6zM8 12l3 3 5-6"/>' },
  { id: 'storage', title: 'Storage & machines', hint: 'Index & connections', sections: ['machines', 'data'], icon: '<rect x="3" y="4" width="18" height="12" rx="2"/><path d="M8 20h8M12 16v4"/>' },
];
const settingsGroup = section => SETTINGS_GROUPS.find(g => g.id === section || g.sections.includes(section));

function render() {
  if (!isView('settings')) return;
  const secs = new Map([
    ['licence', ['Licence', licenceSection()]],
    ['updates', ['Updates', updatesSection()]],
    ['scanning', ['Drive scanning', scanningSection()]],
    ['hashing', ['File hashing', hashingSection()]],
    ['attest', ['Attestations', attestSection()]],
    ['junk', ['Junk rules', junkSection()]],
    ['schema', ['Filing schema', schemaSection()]],
    ...(SET.notify ? [['notify', ['Notifications', notifySection()]]] : []),
    ...(SET.machines ? [['machines', ['Other machines', machinesSection()]]] : []),
    ['data', ['Index and data', dataSection()]],
    ['about', ['About BackDown', aboutSection()]],
  ]);
  const section = SET.tab;
  SET.tab = settingsGroup(section)?.id || 'general';
  const on = id => id === SET.tab;
  // All fields stay mounted for their existing event handlers; one group is visible.
  $('#view').innerHTML = `<div class="settings settings-dashboard">
    <header class="set-heading"><h1>Settings</h1><p>Manage BackDown, your drives and how your files are organised.</p></header>
    <div class="set-tabs" role="tablist" aria-label="Settings">${SETTINGS_GROUPS.map(g =>
      `<button type="button" class="set-tab" role="tab" id="set-tab-${g.id}" aria-controls="set-panel-${g.id}" aria-selected="${on(g.id)}" tabindex="${on(g.id) ? 0 : -1}" data-set-tab="${g.id}"><svg viewBox="0 0 24 24" aria-hidden="true">${g.icon}</svg><span><b>${esc(g.title)}</b><small>${esc(g.hint)}</small></span></button>`).join('')}</div>
    ${SETTINGS_GROUPS.map(g => `<div class="set-panel" id="set-panel-${g.id}" role="tabpanel" tabindex="0" aria-labelledby="set-tab-${g.id}"${on(g.id) ? '' : ' hidden'}>${g.sections.filter(id => secs.has(id)).map(id => {
      const [title, body] = secs.get(id);
      return `<section class="set-sec" id="set-${id}" aria-labelledby="set-heading-${id}"><h2 id="set-heading-${id}">${esc(title)}</h2>${body}</section>`;
    }).join('')}</div>`).join('')}
  </div>`;
  wire();
  if (section && section !== SET.tab && secs.has(section)) {
    const target = document.getElementById(`set-${section}`);
    target?.scrollIntoView({ block: 'start' });
  }
}

function selectTab(id, focus) {
  const v = $('#view');
  SET.tab = id;
  v.querySelectorAll('.set-tab').forEach(b => {
    const sel = b.dataset.setTab === id;
    b.setAttribute('aria-selected', String(sel));
    b.tabIndex = sel ? 0 : -1;
    if (sel && focus) b.focus({ preventScroll: true });
  });
  v.querySelectorAll('.set-panel').forEach(p => { p.hidden = p.id !== `set-panel-${id}`; });
  v.scrollTop = 0;
}

const failed = x => x?.error ? `<p class="set-err">${esc(x.error)}</p>` : '';

// other machines (machines.js has their pages, and how their copies count is set there)
function machinesSection() {
  const m = SET.machines;
  if (m.error) return failed(m);
  const list = m.machines || [], s = m.settings || {};
  return `
    <p>A NAS or another computer with BackDown on it, read over ssh with your keys. A copy there counts as proof when it’s hashed there and was
      confirmed there within <b>${esc(s.proof_days)} days</b>; each machine is synced every ${esc(s.sync_hours)} hours.</p>
    ${list.length ? `<ul class="set-machines">${list.map(x => `<li><button class="rowlink" type="button" data-mc="${x.id}"><b>${esc(x.name)}</b> <span class="muted">${esc(x.url)}</span></button></li>`).join('')}</ul>`
      : '<p class="muted">None yet.</p>'}
    ${m.why_not ? `<p class="set-err">${esc(m.why_not)}</p>` : ''}
    <div class="set-acts"><button class="btn" type="button" data-mc-add>Add a machine</button></div>`;
}

function licenceSection() {
  const f = SET.feat;
  if (f.error) return failed(f);
  const l = f.licence;
  const tiers = ['community', 'pro', 'team', 'forensic'];
  const state = x => !x.ready ? ['off', 'coming'] : x.tier === 'community' ? ['ok', 'free'] : !x.allowed ? ['warn', `needs ${x.tier_name}`]
    : f.preview && f.tier === 'community' ? ['busy', 'free in the preview'] : ['ok', 'unlocked'];
  return `
    ${f.preview ? `<div class="set-callout"><b>Pro features are free during the preview.</b> Nothing is locked while BackDown is a preview release; a licence key you add now carries over to the release.</div>` : ''}
    <div class="set-row"><div><b>BackDown ${esc(f.tier_name)}</b><p class="muted">${f.preview ? 'Everything is unlocked.'
      : f.tier === 'community' ? 'Pro features need a licence key.' : `Your licence unlocks ${esc(f.tier_name)} features.`}</p></div></div>
    ${f.licence_problem ? `<p class="set-err">${esc(f.licence_problem)}</p>` : ''}
    ${l ? `<dl class="set-dl">
      <dt>Licence</dt><dd><code>${esc(l.id)}</code> · ${esc(l.product[0].toUpperCase() + l.product.slice(1))}</dd>
      <dt>For</dt><dd>${esc(l.name ? `${l.name} <${l.email}>` : l.email)}</dd>
      <dt>${l.product === 'forensic' ? 'Sites' : 'Machines'}</dt><dd>${esc(l.seats)}</dd>
      <dt>Issued</dt><dd>${esc(l.issued)}</dd>
      <dt>Expires</dt><dd>${l.expires ? esc(l.expires) : l.kind === 'preorder' ? 'A pre-order: your first year starts with the release that ships Pro' : 'Doesn’t expire'}</dd>
    </dl>` : ''}
    <form class="set-key" id="set-key-form">
      <label for="set-key">${l ? 'Replace it with another key' : 'Add a licence key'}</label>
      <textarea id="set-key" rows="3" spellcheck="false" autocomplete="off" placeholder="BD1.…"></textarea>
      <div class="set-acts"><button class="btn primary" type="submit">Add key</button>
        <a class="btn" href="${esc(f.pricing)}" target="_blank" rel="noopener noreferrer">Pricing</a>
        <span class="muted">Checked on this computer; nothing is sent anywhere. Kept in <code>${esc(f.licence_file)}</code>.</span></div>
    </form>
    <table class="list set-feats"><thead><tr><th>Feature</th><th>Tier</th><th></th></tr></thead><tbody>
    ${tiers.flatMap(t => f.features.filter(x => x.tier === t)).map(x => { const [cls, label] = state(x); return `
      <tr><td><b>${esc(x.name)}</b><small class="muted">${esc(x.unlocks)}</small></td><td>${esc(x.tier_name)}</td><td><span class="pill ${cls}">${esc(label)}</span></td></tr>`; }).join('')}
    </tbody></table>`;
}

function updatesSection() {
  const u = SET.upd, st = SET.st;
  if (u.error) return failed(u);
  const s = u.status, cfg = st.settings || {};
  const latest = s.latest;
  const line = s.newer && latest
    ? `<b>BackDown ${esc(latest.version)} is available.</b> <a href="${esc(latest.notes_url)}" target="_blank" rel="noopener noreferrer">Release notes</a>`
    : s.checked_at ? `This is the newest ${s.channel === 'stable' ? 'release' : 'version, previews included'} (checked ${esc(when(s.checked_at))}).`
    : 'Not checked yet.';
  return `
    <div class="set-row"><div><b>This is BackDown ${esc(s.current)}</b><p class="muted">${line}</p>
      ${s.error ? `<p class="set-err">${esc(s.error)}</p>` : ''}
      ${u.install?.state === 'failed' ? `<p class="set-err">The last update didn't install: ${esc(u.install.message)}</p>` : ''}</div>
      <div class="set-acts">${s.newer && u.can_install ? '<button class="btn primary" type="button" data-upd-install>Update now</button>' : ''}
        <button class="btn" type="button" id="set-upd-check"${s.blocked_until ? ' disabled' : ''}>Check now</button></div></div>
    ${!u.can_install && u.why_not ? `<p class="muted">${esc(u.why_not)}</p>` : ''}
    <label class="set-field"><span>Updates to offer</span>
      <select id="set-channel"><option value="auto"${!cfg.update_channel ? ' selected' : ''}>Automatic (previews while this is a preview)</option>
        <option value="stable"${cfg.update_channel === 'stable' ? ' selected' : ''}>Releases only</option>
        <option value="preview"${cfg.update_channel === 'preview' ? ' selected' : ''}>Previews too</option></select></label>
    <label class="chk set-chk"><input type="checkbox" id="set-upd-auto"${cfg.update_check ? ' checked' : ''}> Look for updates once a day</label>
    <p class="note">Off unless you turn it on: BackDown makes no network requests by default. When it looks (or when you press Check now), it asks GitHub's public list of BackDown releases; nothing about you, your drives or your files is sent. Downloads are checked against the release's signature and SHA-256 sums before they're installed.</p>`;
}

function scanningSection() {
  const st = SET.st;
  if (st.error) return failed(st);
  const cur = st.settings.rescan_every;
  const known = RESCAN.some(([v]) => v === cur);
  const running = st.rescan_every_secs;
  return `
    <label class="set-field"><span>Rescan each drive</span>
      <select id="set-rescan">${RESCAN.map(([v, l]) => `<option value="${v}"${v === cur ? ' selected' : ''}>${esc(l)}</option>`).join('')}
        ${known ? '' : `<option value="${esc(cur)}" selected>Every ${esc(cur)}</option>`}</select></label>
    <p class="note">A drive that comes back after being away is scanned then, whatever this says. ${st.daemon ? 'This applies straight away.' : 'It applies to BackDown’s background service; this window was opened with <code>backdown serve</code>.'}
      ${st.rescan_flag ? ` The service was started with <code>--rescan-every</code>, which it goes back to when it starts again.` : ''}</p>
    ${st.daemon ? `<p class="muted">Now: ${running ? `each drive is rescanned every ${esc(dur(running))}` : 'drives are scanned when you ask, or when they come back'}.</p>` : ''}
    <label class="chk set-chk"><input type="checkbox" id="set-new-drives"${st.settings.index_new_drives ? ' checked' : ''}> Index a drive the first time it's plugged in</label>
    <p class="note">For external drives (USB, Thunderbolt, SD cards) once they're mounted; indexing only reads. Each drive is considered once: one you remove from the index isn't added again, and one plugged in while this is off is left alone. On Linux, a new drive that stays unmounted (a server nobody is logged in to) is listed in Activity with the command that mounts it read-only.</p>
    ${SET.scan ? `<h3>Fast rescans</h3>${extSection('scan-settings', SET.scan)}` : ''}`;
}

function hashingSection() {
  const st = SET.st;
  if (st.error) return failed(st);
  const h = st.hashing, cfg = st.settings;
  return `
    <p>BackDown reads only what could be a duplicate, in three steps:</p>
    <ol class="set-steps"><li><b>Size.</b> A file whose size no other file shares is unique, and is never read.</li>
      <li><b>Sample.</b> ${h.sample_kib} KiB from the start, middle and end (files up to ${h.small_kib} KiB whole).</li>
      <li><b>SHA-256</b> of the whole file, only where size and sample still match another file.</li></ol>
    <label class="set-field"><span>Readers per drive</span><input type="number" id="set-hash-jobs" min="1" max="16" value="${esc(cfg.hash_jobs)}"></label>
    <p class="note">1 suits spinning disks, which slow down when made to read two places at once. SSDs read faster with 2 to 4.</p>
    <label class="chk set-chk"><input type="checkbox" id="set-hash-retry"${cfg.hash_retry_errors ? ' checked' : ''}> After each scan, try again files that couldn't be read before</label>
    <p class="note">These apply to the hashing that follows each scan.</p>`;
}

// the service signs a manifest of each drive (manifesting.rs): Team, as tiers.rs says
const ATTEST = [['off', 'Off'], ['after-scan', 'After each scan'], ['daily', 'Once a day'], ['weekly', 'Once a week']];
function attestSection() {
  const st = SET.st;
  if (st.error) return failed(st);
  const cur = st.settings.attest || 'off';
  const f = SET.feat?.features?.find(x => x.key === 'attestations');
  const locked = f && !f.allowed && cur === 'off';
  const k = st.signing_key || {};
  return `
    <label class="set-field"><span>Sign a manifest of each drive</span>
      <select id="set-attest"${locked ? ' disabled' : ''}>${ATTEST.map(([v, l]) => `<option value="${v}"${v === cur ? ' selected' : ''}>${esc(l)}</option>`).join('')}</select></label>
    <p class="note">A manifest is a signed statement of exactly what a drive held at a scan, every path, size and hash, that anyone can check was not changed since; one is made only when the drive has a scan no manifest covers yet.</p>
    ${locked ? `<p class="muted">${esc(f.name)} are part of BackDown ${esc(f.tier_name)}. <button class="btn sm" type="button" data-set-go="licence">Add a licence key</button></p>` : ''}
    <p class="muted">${k.fingerprint ? `Signed with this BackDown’s own key, <code>${esc(k.fingerprint)}</code>, kept in <code>${esc(k.file)}</code>${st.shared ? ' (the machine-wide service’s, not yours)' : ''}.` : 'The key is made with the first manifest.'}
      <code>backdown manifest list</code> shows them, and a drive’s details offer each one to download.</p>`;
}

function junkSection() {
  const st = SET.st;
  if (st.error) return failed(st);
  const names = st.junk_rules.reduce((m, r) => { (m[r.why] ||= []).push(r.name); return m; }, {});
  return `
    <p>Caches, previews, build output and trash can be made again by the programs that made them. Recognition marks them;
      a plan can leave them out; Free up counts them.</p>
    <table class="list set-junk"><tbody>
      ${Object.entries(names).map(([why, ns]) => `<tr><td>${ns.map(n => `<code>${esc(n)}</code>`).join(' ')}</td><td class="muted">${esc(why)}</td></tr>`).join('')}
      ${st.junk_places.map(r => `<tr><td>${esc(r.name)}</td><td class="muted">${esc(r.why)}</td></tr>`).join('')}
    </tbody></table>
    <label class="set-field col"><span>Also count these folder names (one per line)</span>
      <textarea id="set-junk-names" rows="3" spellcheck="false" placeholder=".terraform&#10;DerivedData">${esc(st.settings.junk_names.join('\n'))}</textarea></label>
    <div class="set-acts"><button class="btn" type="button" id="set-junk-save">Save</button>
      <span class="muted">They count from the next time drives are recognised: after a scan, or What's here → Recognise again.</span></div>`;
}

function schemaSection() {
  const s = SET.sch;
  if (s.error) return failed(s);
  return `
    <p>Where a plan puts each kind of thing in the archive: <code>{origin}</code> is the host, domain, device or person it came from,
      <code>{name}</code> its folder or file name, <code>{drive}</code> the drive it's on, <code>{year}</code> its newest file, <code>{years}</code> oldest to newest.</p>
    <p class="muted">${s.custom ? `Yours, from <code>${esc(s.file)}</code>.` : 'The built-in schema. Saving an edited one keeps it for every plan made from here or with <code>backdown plan</code>.'}</p>
    <textarea id="set-schema-text" class="set-code" rows="22" spellcheck="false" aria-label="Filing schema">${esc(s.text)}</textarea>
    <div id="set-schema-state" class="set-state">${schemaState(s.problem, s.warnings)}</div>
    <div class="set-acts"><button class="btn primary" type="button" id="set-schema-save">Save</button>
      ${s.custom ? '<button class="btn" type="button" id="set-schema-reset">Go back to the built-in schema</button>' : ''}</div>
    <details class="set-kinds"><summary>Kinds BackDown recognises</summary>
      <dl class="set-dl">${s.kinds.map(k => `<dt><code>${esc(k.kind)}</code></dt><dd>${esc(k.label)}</dd>`).join('')}<dt><code>unsorted</code></dt><dd>Anything not recognised</dd></dl></details>`;
}

const schemaState = (problem, warnings = []) => problem
  ? `<p class="set-err">${esc(problem)}</p>`
  : `<p class="set-ok">The schema is valid.</p>${warnings.length ? `<ul class="set-warn">${warnings.map(w => `<li>${esc(w)}</li>`).join('')}</ul>` : ''}`;

// fields for a settings endpoint another part of BackDown provides, from what it returns
function extSection(path, data) {
  if (data.error) return failed(data);
  const obj = data.settings && typeof data.settings === 'object' ? data.settings : data;
  const label = k => data.labels?.[k] || k.replace(/_/g, ' ').replace(/^./, c => c.toUpperCase());
  const help = k => data.help?.[k] ? `<small class="muted">${esc(data.help[k])}</small>` : '';
  const info = ['why', 'note', 'error'].map(k => typeof obj[k] === 'string' ? `<p class="muted">${esc(obj[k])}</p>` : '').join('');
  const fields = Object.entries(obj).filter(([k, v]) => !['labels', 'help', 'ok', 'available', 'why', 'note', 'error'].includes(k) && ['boolean', 'number', 'string'].includes(typeof v));
  if (!fields.length) return info || '<p class="muted">Nothing to set here yet.</p>';
  return `${info}<div class="set-ext" data-ext="${esc(path)}">${fields.map(([k, v]) => typeof v === 'boolean'
    ? `<label class="chk set-chk"><input type="checkbox" data-ext-key="${esc(k)}"${v ? ' checked' : ''}> ${esc(label(k))}${help(k)}</label>`
    : `<label class="set-field"><span>${esc(label(k))}${help(k)}</span><input type="${typeof v === 'number' ? 'number' : 'text'}" data-ext-key="${esc(k)}" value="${esc(v)}"></label>`).join('')}</div>`;
}

// /api/notify-settings (health.rs): which news pops up on the desktop; Activity lists it all
function notifySection() {
  const n = SET.notify;
  if (n.error) return failed(n);
  const rows = [['desktop', 'Show notifications on the desktop'], ['health', 'When a drive’s health gets worse'],
    ['drift', 'When a mirror no longer matches what it copies'], ['jobs', 'When a scheduled job fails']];
  return `<p>Activity keeps a list of what happened; these choose what also pops up on your desktop.</p>
    ${n.why ? `<p class="muted">${esc(n.why)}</p>` : ''}
    <div class="set-ext" data-ext="notify-settings">${rows.filter(([k]) => typeof n[k] === 'boolean').map(([k, l]) =>
      `<label class="chk set-chk"><input type="checkbox" data-ext-key="${k}"${n[k] ? ' checked' : ''}> ${esc(l)}</label>`).join('')}</div>
    ${n.available ? '<div class="set-acts"><button class="btn" type="button" id="set-notify-test">Send a test notification</button></div>' : ''}`;
}

function dataSection() {
  const st = SET.st;
  if (st.error) return failed(st);
  const p = st.paths;
  const win = st.about.os === 'windows', mac = st.about.os === 'macos';
  const row = (t, v) => `<dt>${esc(t)}</dt><dd><code>${esc(v)}</code> <button class="btn sm" type="button" data-copy="${esc(v)}">Copy</button></dd>`;
  return `<dl class="set-dl">${row('Index', p.index)}${row('Data folder', p.data)}${row('Settings', p.settings)}${row('Licence', p.licence)}${st.daemon ? row('Service log', p.log) : ''}</dl>
    <details class="set-move"><summary>Moving the index to another drive</summary>
      <ol class="set-steps">
        <li>Stop BackDown's background service: ${win || mac ? 'quit it' : '<code>systemctl --user stop backdown</code> if it runs as a service'} (<code>backdown status</code> says whether one is running).</li>
        <li>Move <code>index.db</code>, with <code>index.db-wal</code> and <code>index.db-shm</code> if they're there, to the new folder.</li>
        <li>Tell BackDown where it is: set <code>BACKDOWN_DB</code> to the new path${win ? ' (System properties → Environment variables)' : ` (for the service: <code>systemctl --user edit backdown</code>, then <code>[Service]</code> and <code>Environment=BACKDOWN_DB=/new/place/index.db</code>)`}, or pass <code>--db /new/place/index.db</code>.</li>
        <li>Start BackDown again. Settings and your licence stay in the data folder.</li>
      </ol></details>`;
}

function aboutSection() {
  const st = SET.st, f = SET.feat, d = daemonState();
  if (st.error) return failed(st);
  const a = st.about;
  return `<dl class="set-dl">
      <dt>Version</dt><dd>BackDown ${esc(a.version)}${f && !f.error && f.preview ? ' (preview)' : ''}</dd>
      <dt>Edition</dt><dd>${f && !f.error ? esc(f.tier_name) + (f.preview ? ' · Pro features are free during the preview' : '') : '—'}</dd>
      <dt>System</dt><dd>${esc(a.os)} · ${esc(a.arch)}</dd>
      <dt>Program</dt><dd><code>${esc(a.exe || '')}</code></dd>
      <dt>Background service</dt><dd>${d ? `running since ${esc(when(d.started_at))} (process ${esc(d.pid)})` : st.daemon ? 'running' : 'not running: this window was opened with <code>backdown serve</code>'}</dd>
    </dl>
    <p class="set-links"><a href="https://backdown.io" target="_blank" rel="noopener noreferrer">backdown.io</a>
      <a href="https://github.com/BuenGenio/backdown-releases/releases" target="_blank" rel="noopener noreferrer">All releases</a>
      <a href="https://backdown.io/pricing" target="_blank" rel="noopener noreferrer">Pricing</a></p>`;
}

async function saveSetting(params, what) {
  try {
    SET.st = await post('settings', params);
    toast(`${what} saved`);
    return true;
  } catch (e) { toast(e.message); return false; }
}

function wire() {
  const v = $('#view');
  const tabs = v.querySelector('.set-tabs');
  tabs?.addEventListener('click', e => { const b = e.target.closest('[data-set-tab]'); if (b) selectTab(b.dataset.setTab); });
  tabs?.addEventListener('keydown', e => {
    const all = [...tabs.querySelectorAll('[data-set-tab]')], i = all.indexOf(document.activeElement);
    if (i < 0) return;
    const j = { ArrowRight: i + 1, ArrowLeft: i - 1, Home: 0, End: all.length - 1 }[e.key];
    if (j === undefined) return;
    e.preventDefault();
    selectTab(all[(j + all.length) % all.length].dataset.setTab, true);
  });
  v.querySelectorAll('[data-copy]').forEach(b => b.addEventListener('click', () => navigator.clipboard?.writeText(b.dataset.copy).then(() => toast('Copied'), () => {})));
  $('#set-key-form')?.addEventListener('submit', async e => {
    e.preventDefault();
    const key = $('#set-key').value.trim();
    if (!key) return $('#set-key').focus();
    try {
      SET.feat = await post('license', {}, key);
      FEAT.data = SET.feat; paintBadges();
      toast(`Licence added: BackDown ${SET.feat.tier_name}`);
      render();
    } catch (err) { toast(err.message); }
  });
  $('#set-upd-check')?.addEventListener('click', async e => {
    e.target.disabled = true; e.target.textContent = 'Checking…';
    try { SET.upd = await post('update-check'); UPD.data = SET.upd; paintUpdateBanner(); } catch (err) { toast(err.message); }
    render();
  });
  $('#set-channel')?.addEventListener('change', async e => {
    if (await saveSetting({ update_channel: e.target.value }, 'Update channel')) { SET.upd = await api('update').catch(() => SET.upd); UPD.data = SET.upd; paintUpdateBanner(); render(); }
  });
  $('#set-upd-auto')?.addEventListener('change', e => saveSetting({ update_check: e.target.checked ? 1 : 0 }, 'Update checks'));
  $('#set-rescan')?.addEventListener('change', e => saveSetting({ rescan_every: e.target.value }, 'Rescan interval'));
  $('#set-hash-jobs')?.addEventListener('change', async e => { if (!await saveSetting({ hash_jobs: e.target.value }, 'Readers per drive')) render(); });
  $('#set-hash-retry')?.addEventListener('change', e => saveSetting({ hash_retry_errors: e.target.checked ? 1 : 0 }, 'Retrying'));
  $('#set-new-drives')?.addEventListener('change', e => saveSetting({ index_new_drives: e.target.checked ? 1 : 0 }, 'New drives'));
  $('#set-attest')?.addEventListener('change', async e => { if (!await saveSetting({ attest: e.target.value }, 'Attestations')) render(); });
  $('#set-junk-save')?.addEventListener('click', async () => { if (await saveSetting({ junk_names: $("#set-junk-names").value }, 'Junk rules')) render(); });
  const sch = $("#set-schema-text");
  sch?.addEventListener('input', () => {
    clearTimeout(SET.schemaTimer);
    SET.schemaTimer = setTimeout(async () => {
      const seq = ++SET.schemaSeq;
      let html;
      try { const r = await post('schema', { check: 1 }, sch.value); html = schemaState(null, r.warnings); }
      catch (e) { html = schemaState(e.message); }
      const state = $('#set-schema-state');
      if (seq === SET.schemaSeq && sch.isConnected && state) state.innerHTML = html;
    }, 350);
  });
  $('#set-schema-save')?.addEventListener('click', async () => {
    try { SET.sch = await post('schema', {}, sch.value); toast('Filing schema saved: new plans use it'); render(); document.getElementById("set-schema-text")?.scrollIntoView({ block: "center" }); }
    catch (e) { const state = $('#set-schema-state'); if (sch.isConnected && state) state.innerHTML = schemaState(e.message); }
  });
  $('#set-schema-reset')?.addEventListener('click', async () => {
    if (!confirm('Go back to the built-in filing schema? Your edited one is deleted.')) return;
    try { SET.sch = await post('schema', { reset: 1 }); toast('Back to the built-in schema'); render(); } catch (e) { toast(e.message); }
  });
  $('#set-notify-test')?.addEventListener('click', async () => {
    try { await post('notify-test'); toast('Sent: it should be on your desktop now'); } catch (e) { toast(e.message); }
  });
  v.querySelectorAll('.set-ext').forEach(box => box.addEventListener('change', async e => {
    const inp = e.target.closest('[data-ext-key]');
    if (!inp) return;
    const k = inp.dataset.extKey, val = inp.type === 'checkbox' ? inp.checked : inp.type === 'number' ? Number(inp.value) : inp.value;
    try {
      await post(box.dataset.ext, { [k]: inp.type === 'checkbox' ? (val ? 1 : 0) : val }, { [k]: val });
      toast('Saved');
    } catch (err) { toast(err.message); }
  }));
}
