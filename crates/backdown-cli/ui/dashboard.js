// The landing dashboard uses the same live snapshot as Activity. Job history is scoped to
// this service session; notifications and the index keep their own, persistent records.
const esc = s => String(s ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const number = n => Number(n || 0).toLocaleString();
const stamp = j => j.finished_at || j.started_at || j.queued_at || 0;

export const SERVICES = [
  { id: 'service', name: 'Background service', icon: 'activity', kinds: null },
  { id: 'scans', name: 'Drive scans', icon: 'scan', kinds: ['index', 'forget'] },
  { id: 'hashing', name: 'File hashing', icon: 'hash', kinds: ['hash'] },
  { id: 'library', name: 'Library & search', icon: 'library', kinds: ['entities', 'content', 'places', 'indexes', 'dupes-index'] },
  { id: 'protection', name: 'Protection & machines', icon: 'shield', kinds: ['attest', 'mirror-sync', 'mirror-verify', 'rescue', 'remote-sync'] },
  { id: 'cleanup', name: 'Plans & cleanup', icon: 'copies', kinds: ['apply', 'wipe', 'dedup', 'mail-merge'] },
];

export function jobMessage(j) {
  if (j.state === 'failed') return j.error || 'The job failed. Open Activity for details.';
  if (j.state === 'stopped') return j.summary || 'Stopped before completion.';
  if (j.state === 'done') return j.summary || 'Completed.';
  if (j.state === 'queued') return 'Waiting in the queue.';
  return j.progress || 'Working…';
}

export function serviceState({ daemon, checked = false, error = null }, kinds = null) {
  if (error) return { label: 'Unavailable', tone: 'warn', message: error, jobs: [] };
  if (!checked) return { label: 'Connecting', tone: 'off', message: 'Checking the background service…', jobs: [] };
  if (!daemon?.running) return { label: 'View only', tone: 'off', message: 'The background service is not running. Open Activity for setup details.', jobs: [] };
  const matches = j => j && (!kinds || kinds.includes(j.kind));
  const active = [daemon.current, daemon.side].filter(matches);
  const queued = (daemon.queued || []).filter(matches);
  const recent = (daemon.recent || []).filter(matches).sort((a, b) => stamp(b) - stamp(a));
  const jobs = [...active, ...queued, ...recent].filter((j, i, all) => all.findIndex(x => x.id === j.id) === i);
  const latest = active[0] || queued[0] || recent[0];
  const [label, tone] = active.length ? ['Running', 'busy'] : queued.length ? ['Queued', 'busy']
    : latest?.state === 'failed' ? ['Failed', 'warn'] : latest?.state === 'stopped' ? ['Stopped', 'off']
    : !kinds ? ['Online', 'ok'] : latest ? ['Completed', 'ok'] : ['Idle', 'off'];
  return { label, tone, latest, jobs, message: latest ? jobMessage(latest)
    : kinds ? 'No recent activity in this service session.' : 'Ready. Nothing is waiting in the queue.' };
}

const SHAPES = {
  dashboard: '<rect x="3" y="3" width="7" height="7" rx="2"/><rect x="14" y="3" width="7" height="7" rx="2"/><rect x="3" y="14" width="7" height="7" rx="2"/><rect x="14" y="14" width="7" height="7" rx="2"/>',
  drives: '<rect x="3" y="12" width="18" height="8" rx="2"/><path d="m3 12 4-8h10l4 8M7 16h.01M11 16h.01"/>',
  library: '<path d="M4 4h6v16H4zM14 4h6v16h-6zM4 8h6M14 8h6"/>',
  copies: '<rect x="8" y="8" width="12" height="12" rx="2"/><path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2"/>',
  places: '<path d="M19 10c0 5-7 11-7 11S5 15 5 10a7 7 0 0 1 14 0Z"/><circle cx="12" cy="10" r="2"/>',
  plan: '<rect x="5" y="4" width="14" height="17" rx="2"/><path d="M9 4V3h6v1M9 10h6M9 14h6M9 18h4"/>',
  wipe: '<path d="M4 7h16M10 11v6M14 11v6M6 7l1 13h10l1-13M9 7V4h6v3"/>',
  rescue: '<circle cx="12" cy="12" r="9"/><circle cx="12" cy="12" r="4"/><path d="m6 6 3 3m6 6 3 3m-3-9 3-3M6 18l3-3"/>',
  swarm: '<circle cx="12" cy="5" r="2.5"/><circle cx="5" cy="18" r="2.5"/><circle cx="19" cy="18" r="2.5"/><path d="m11 7-5 9m7-9 5 9M8 18h8"/>',
  activity: '<path d="M3 12h4l3-8 4 16 3-8h4"/>',
  settings: '<path d="M4 6h16M4 12h16M4 18h16"/><circle cx="8" cy="6" r="2"/><circle cx="16" cy="12" r="2"/><circle cx="10" cy="18" r="2"/>',
  scan: '<path d="M20 11a8 8 0 1 0-2 6M20 5v6h-6"/>',
  hash: '<path d="M4 9h16M4 15h16M10 3 8 21M16 3l-2 18"/>',
  shield: '<path d="m12 3 8 3v6c0 4-3 7-8 9-5-2-8-5-8-9V6zM8 12l3 3 5-6"/>',
};
const icon = name => `<svg viewBox="0 0 24 24" aria-hidden="true">${SHAPES[name] || SHAPES.dashboard}</svg>`;
const FUNCTIONS = [
  ['drives', 'Browse drives', 'Files, folders & scan history', 'drives'],
  ['entities', 'What’s here', 'Explore your library', 'library'],
  ['dupes', 'Duplicates', 'Find identical files', 'copies'],
  ['places', 'Places', 'Photos by date & location', 'places'],
  ['plan', 'Plan', 'Organise & copy files', 'plan'],
  ['wipe', 'Free up', 'Review space you can reclaim', 'wipe'],
  ['rescue', 'Rescue', 'Recover files & drives', 'rescue'],
  ['swarm', 'Swarm', 'Storage across machines', 'swarm'],
  ['activity', 'Activity', 'Jobs, drives & notifications', 'activity'],
  ['settings', 'Settings', 'Preferences & configuration', 'settings'],
];

export function dashboardHtml(state, { bytes, when, jobName, pct }) {
  const { roots, index, daemon, health, notes } = state;
  const healthDrives = health?.drives || [];
  const attention = healthDrives.filter(d => d.present && ['warning', 'failing'].includes(d.verdict));
  const services = SERVICES.map(service => {
    const s = serviceState(state, service.kinds), j = s.latest, progress = j?.state === 'running' ? pct(j) : null;
    return `<details class="dash-service" data-service="${service.id}">
      <summary><span class="dash-service-top"><span class="dash-icon">${icon(service.icon)}</span><span class="pill ${s.tone}">${esc(s.label)}</span></span>
        <span class="dash-service-name">${service.name}</span>
        <span class="dash-message">${j ? `<b>${esc(jobName(j))}</b> · ` : ''}${esc(s.message)}</span>
        ${progress != null ? `<progress max="1" value="${Math.min(1, Math.max(0, progress))}" aria-label="${esc(service.name)} progress"></progress>` : ''}
        <span class="dash-disclosure">Details & history <span aria-hidden="true">⌄</span></span></summary>
      <div class="dash-service-detail">
        ${!service.kinds && daemon ? `<p>BackDown ${esc(daemon.version)} · started ${esc(when(daemon.started_at))}</p>` : ''}
        ${s.jobs.length ? `<ol class="dash-history">${s.jobs.slice(0, 8).map(job => `<li><div><b>${esc(jobName(job))}</b><span class="pill ${job.state === 'failed' ? 'warn' : job.state === 'done' ? 'ok' : 'off'}">${esc(job.state)}</span></div>
          <p>${esc(jobMessage(job))}</p><time>${esc(when(stamp(job)))}</time></li>`).join('')}</ol>` : `<p>${esc(s.message)}</p>`}
        <p class="muted">Recent jobs from this service session.</p><button class="btn sm" type="button" data-dashboard-view="activity">Open Activity</button>
      </div></details>`;
  }).join('');
  return `<div class="dashboard">
    <header class="dash-heading"><div><p class="dash-eyebrow">BackDown overview</p><h1>Dashboard</h1><p>Your files, tools and services in one place.</p></div>
      <div class="dash-totals"><span><b>${number(roots.length)}</b> drives</span>${index ? `<span><b>${number(index.files)}</b> files</span><span><b>${esc(bytes(index.bytes))}</b> indexed</span>` : ''}</div></header>
    <section aria-labelledby="dash-functions"><div class="dash-section-head"><h2 id="dash-functions">What would you like to do?</h2></div>
      <div class="dash-functions">${FUNCTIONS.map(([id, title, text, glyph]) => `<button class="dash-function" type="button" data-dashboard-view="${id}"><span class="dash-icon">${icon(glyph)}</span><span><b>${title}</b><small>${text}</small></span></button>`).join('')}</div></section>
    <section aria-labelledby="dash-services"><div class="dash-section-head"><h2 id="dash-services">Service status</h2><span>Expand a tile for details & history</span></div><div class="dash-services">${services}</div></section>
    ${attention.length || health?.error || notes?.unread ? `<section class="dash-attention" aria-labelledby="dash-attention"><div><h2 id="dash-attention">Needs attention</h2><p>${health?.error ? `Drive health is unavailable: ${esc(health.error)}. ` : ''}${attention.length ? `${number(attention.length)} connected drive${attention.length === 1 ? '' : 's'} with health warnings. ` : ''}${notes?.unread ? `${number(notes.unread)} unread notification${notes.unread === 1 ? '' : 's'}.` : ''}</p></div><button class="btn" type="button" data-dashboard-view="activity">Review in Activity</button></section>` : ''}
    <section id="dashboard-drives" aria-labelledby="dash-drives"><div class="dash-section-head"><h2 id="dash-drives">Your drives</h2><button class="btn sm" type="button" data-dashboard-view="activity">Manage drives</button></div>
      ${roots.length ? `<div class="dash-drives">${roots.map(r => {
        const live = daemon?.drives?.find(d => d.path === r.path);
        const h = healthDrives.find(d => d.present && d.health?.mounts?.includes(r.path));
        const status = live?.state === 'away' ? 'Not connected' : live?.state === 'scanning' ? 'Scanning' : live?.state === 'queued' ? 'Queued' : live?.present ? 'Connected' : 'Indexed';
        return `<button class="dash-drive" type="button" data-dashboard-root="${esc(r.key)}"><span class="dash-icon">${icon('drives')}</span><span><b>${esc(r.name)}</b><small>${esc(bytes(r.bytes))} · ${number(r.files)} files</small><small>${status}${h && ['warning', 'failing'].includes(h.verdict) ? ` · ${esc(h.verdict)}` : ''}</small></span></button>`;
      }).join('')}</div>` : `<div class="dash-welcome"><h3>Start with a drive or folder</h3><p>Index your files to browse them, find duplicates and plan your backups.</p><button class="btn primary" type="button" data-dashboard-view="activity">Add your first drive</button></div>`}
    </section>
  </div>`;
}
