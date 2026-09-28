// SQL dumps as schemas. /api/sqldump (sqldump.rs) reads the whole dump on the server, a few
// seconds at a time, and this shows what it found as it goes: the header's facts, each table's
// columns, keys, indexes and foreign keys, its rows and data size, and the views, triggers and
// routines. The dump's own text stays one tab away.
import { api, bytes } from './app.js';

const esc = s => String(s ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const num = n => Number(n || 0).toLocaleString();
const plural = (n, one, many = one + 's') => `${num(n)} ${n === 1 ? one : many}`;
const TITLES = { mysql: 'MySQL dump', postgres: 'PostgreSQL dump', sqlite: 'SQLite dump', sql: 'SQL script' };
const KIND = { unique: 'unique', index: 'index', fulltext: 'full-text', spatial: 'spatial' };
const OTHER = ''; // the nav entry for views, triggers and routines (no table's key is empty)

export const isSqlDump = name => /\.(sql|mysql|pgsql|psql|dump)(\.(gz|bz2|xz|zst))?$/i.test(name);

function loadSort() {
  try { const s = JSON.parse(sessionStorage.getItem('bd.sqlsort')); if (s?.key) return s; } catch {}
  return { key: 'bytes', asc: false };
}

// Show the dump `e` as a schema in the viewer; false if the server says it isn't one after all
// (a pg_dump archive in custom format, binary data), so the usual viewers can take it.
// `text()` draws the plain-text view; `preview(host, url, name)` shows a file inside a container.
let shown = 0; // which call of sqlDumpView owns the viewer
export async function sqlDumpView(e, { live: onScreen, text, preview, entryUrl }) {
  const $ = s => document.querySelector(s);
  const me = ++shown, live = x => onScreen(x) && shown === me;
  const first = await api('sqldump', { id: e.id });
  if (!live(e)) return true;
  if (first.format !== 'sql') return false;
  const st = { d: first, tab: 'schema', sel: null, q: '', sort: loadSort(), reading: false, stopped: false, error: '' };
  const byKey = () => new Map(st.d.tables.map(t => [t.key, t]));

  const tabs = () => `<div class="sqd-tabs" role="tablist" aria-label="Show">
    <button type="button" role="tab" data-sqd-tab="schema" aria-selected="${st.tab === 'schema'}">Schema</button>
    <button type="button" role="tab" data-sqd-tab="sql" aria-selected="${st.tab === 'sql'}">SQL</button></div>`;

  const totals = () => {
    const d = st.d, t = d.tables;
    const rows = t.reduce((a, x) => a + x.rows, 0), size = t.reduce((a, x) => a + x.bytes, 0);
    const parts = [plural(t.length, 'table'), plural(rows, 'row'), `${bytes(size)} of data`];
    const objs = [[d.views, 'view'], [d.triggers, 'trigger'], [d.routines, 'routine'], [d.events, 'event'], [d.sequences, 'sequence']];
    for (const [list, what] of objs) if (list.length) parts.push(plural(list.length, what));
    return parts.join(' · ');
  };

  const progress = () => {
    const d = st.d;
    if (d.complete) return `<div class="sqd-prog"><span>Read to the end${d.elapsed_ms > 1500 ? ` in ${(d.elapsed_ms / 1000).toFixed(1)} s` : ''}.</span></div>`;
    const pct = d.size ? Math.min(100, Math.floor(100 * d.read / d.size)) : 0;
    const state = !st.reading ? 'Paused at' : st.stopped ? 'Stopping at' : 'Reading';
    return `<div class="sqd-prog" role="status"><div class="meter${st.reading ? '' : ' unknown'}" aria-hidden="true"><i style="width:${pct}%"></i></div>
      <span>${state} ${pct}% of ${bytes(d.size)}${d.compression ? ` (${esc(d.compression)})` : ''}</span>
      ${!st.reading ? '<button class="btn sm" type="button" data-sqd-go>Keep reading</button>' : st.stopped ? '' : '<button class="btn sm" type="button" data-sqd-stop>Stop</button>'}
      ${st.error ? `<span class="sqd-err">${esc(st.error)}</span>` : ''}</div>`;
  };

  const head = () => {
    const d = st.d;
    const notes = [];
    if (d.cut) notes.push(`The dump looks cut off: ${d.cut}. What comes before is shown.`);
    notes.push(...d.notes);
    return `<div class="sqd-title"><h3>${esc(TITLES[d.dialect] || 'SQL dump')}</h3><span class="v-meta">${totals()}</span></div>
      ${d.facts.length ? `<dl class="kv">${d.facts.map(([k, v]) => `<dt>${esc(k)}</dt><dd>${esc(v)}</dd>`).join('')}</dl>` : ''}
      ${progress()}${notes.map(n => `<div class="trunc">${esc(n)}</div>`).join('')}`;
  };

  const sorted = () => {
    const { key, asc } = st.sort, dir = asc ? 1 : -1, q = st.q.toLowerCase();
    const list = st.d.tables.filter(t => !q || t.key.toLowerCase().includes(q));
    const cmp = (a, b) => key === 'name' ? dir * a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: 'base' })
      : dir * (a[key] - b[key]) || a.name.localeCompare(b.name);
    return list.sort((a, b) => a.schema.localeCompare(b.schema) || cmp(a, b));
  };

  const nav = () => {
    const list = sorted(), max = Math.max(1, ...st.d.tables.map(t => t.bytes));
    const grouped = st.d.schemas.length > 1;
    let out = '', group = null;
    for (const t of list) {
      if (grouped && t.schema !== group) { group = t.schema; out += `<div class="sqd-group" role="presentation">${esc(group || '(no schema)')}</div>`; }
      const meta = t.rows || t.bytes ? `${plural(t.rows, 'row')}${t.uncounted ? '+' : ''} · ${bytes(t.bytes)}` : t.defined ? 'no rows' : '';
      out += `<button type="button" role="option" data-sqd-key="${esc(t.key)}" aria-selected="${st.sel === t.key}" tabindex="${st.sel === t.key ? 0 : -1}">
        <div class="t">${esc(t.name)}</div><div class="m">${meta}${t.defined ? '' : ' <i>(data only)</i>'}</div>
        <i class="sqd-bar" style="--w:${(100 * t.bytes / max).toFixed(1)}%"></i></button>`;
    }
    const d = st.d, n = d.views.length + d.triggers.length + d.routines.length + d.events.length + d.sequences.length + d.other.length;
    if (n && !st.q) out += `<button type="button" role="option" data-sqd-key="${OTHER}" aria-selected="${st.sel === OTHER}" tabindex="${st.sel === OTHER ? 0 : -1}">
      <div class="t">Views, triggers and routines</div><div class="m">${plural(n, 'object')}</div></button>`;
    return out || `<p class="note pad">${st.q ? 'No table matches.' : 'No tables found yet.'}</p>`;
  };

  const tools = () => `<div class="sqd-tools"><input type="search" id="sqd-q" placeholder="Filter tables" aria-label="Filter tables" autocomplete="off" value="${esc(st.q)}">
    <div class="sqd-sort" role="group" aria-label="Sort tables">${[['name', 'Name'], ['rows', 'Rows'], ['bytes', 'Size']].map(([k, l]) =>
      `<button type="button" data-sqd-sort="${k}" aria-pressed="${st.sort.key === k}" title="Sort by ${l.toLowerCase()}">${l}${st.sort.key === k ? (st.sort.asc ? ' ↑' : ' ↓') : ''}</button>`).join('')}</div></div>`;

  const link = key => {
    const t = byKey().get(key);
    return t ? `<button class="sqd-link" type="button" data-sqd-key="${esc(key)}">${esc(key)}</button>` : `<span class="mono">${esc(key)}</span>`;
  };

  const tableDetail = t => {
    const fkCols = new Map();
    for (const k of t.foreign_keys) for (const c of k.columns) fkCols.set(c, k.ref);
    const idxCols = new Set(t.indexes.flatMap(i => i.columns));
    const facts = [t.rows || t.bytes ? `${plural(t.rows, 'row')}${t.uncounted ? ' (and some from INSERT … SELECT, not counted)' : ''}` : 'no rows in the dump',
      t.bytes ? `${bytes(t.bytes)} of data in ${plural(t.statements, t.copy ? 'statement' : 'INSERT statement')}` : '',
      t.copy ? 'COPY data' : '', t.engine, t.charset].filter(Boolean);
    const refs = st.d.tables.flatMap(o => o.foreign_keys.filter(k => k.ref === t.key).map(k => ({ o, k })));
    const cols = t.columns.map(c => `<tr><td><b>${esc(c.name)}</b>${c.comment ? `<div class="dim">${esc(c.comment)}</div>` : ''}</td>
      <td class="mono">${esc(c.type)}</td><td>${c.null ? 'yes' : '<span class="dim">no</span>'}</td>
      <td class="mono">${c.default != null ? esc(c.default) : '<span class="dim">—</span>'}</td>
      <td>${c.pk ? '<span class="bdg pk" title="Primary key">PK</span> ' : ''}${fkCols.has(c.name) ? `<span class="bdg fk" title="References ${esc(fkCols.get(c.name))}">FK</span> ` : ''}${!c.pk && idxCols.has(c.name) ? '<span class="bdg" title="In an index">IDX</span> ' : ''}${c.auto ? '<span class="bdg" title="Filled in automatically">auto</span> ' : ''}${c.extra.map(x => `<span class="dim">${esc(x)}</span>`).join(' ')}</td></tr>`).join('');
    return `<div class="sqd-table">
      <h3>${esc(t.name)}${t.schema ? ` <span class="dim">${esc(t.schema)}</span>` : ''}</h3>
      <div class="v-meta">${facts.map(esc).join(' · ')}</div>
      ${t.comment ? `<p class="sqd-comment">${esc(t.comment)}</p>` : ''}${t.note ? `<p class="note">${esc(t.note)}</p>` : ''}
      ${t.columns.length ? `<table class="list sqd-cols"><thead><tr><th>Column</th><th>Type</th><th>Null</th><th>Default</th><th>Keys</th></tr></thead><tbody>${cols}</tbody></table>`
        : `<p class="note">${t.defined ? 'No columns listed.' : 'The dump holds data for this table but no CREATE TABLE for it.'}</p>`}
      ${t.primary_key.length ? `<h4>Primary key</h4><p class="mono">${t.primary_key.map(esc).join(', ')}</p>` : ''}
      ${t.indexes.length ? `<h4>Indexes</h4><ul class="sqd-keys">${t.indexes.map(i => `<li><span class="bdg">${KIND[i.kind] || esc(i.kind)}</span> ${i.name ? `${esc(i.name)} ` : ''}<span class="mono">(${i.columns.map(esc).join(', ')})</span></li>`).join('')}</ul>` : ''}
      ${t.foreign_keys.length ? `<h4>Foreign keys</h4><ul class="sqd-keys">${t.foreign_keys.map(k => `<li><span class="mono">${k.columns.map(esc).join(', ')}</span> → ${link(k.ref)}${k.ref_columns.length ? ` <span class="mono">(${k.ref_columns.map(esc).join(', ')})</span>` : ''}${k.on_delete ? ` <span class="dim">on delete ${esc(k.on_delete)}</span>` : ''}${k.on_update ? ` <span class="dim">on update ${esc(k.on_update)}</span>` : ''}${k.name ? ` <span class="dim">· ${esc(k.name)}</span>` : ''}</li>`).join('')}</ul>` : ''}
      ${refs.length ? `<h4>Referenced by</h4><ul class="sqd-keys">${refs.map(({ o, k }) => `<li>${link(o.key)} <span class="mono">(${k.columns.map(esc).join(', ')})</span></li>`).join('')}</ul>` : ''}
      ${t.checks ? `<p class="note">${plural(t.checks, 'CHECK constraint')}.</p>` : ''}
      ${t.sql ? `<details class="sqd-sql"><summary>CREATE statement</summary><pre class="sqlsrc">${esc(t.sql)}</pre></details>` : ''}
    </div>`;
  };

  const otherDetail = () => {
    const d = st.d;
    const block = (title, list, fmt) => list.length ? `<h4>${title}</h4><ul class="sqd-keys">${list.map(fmt).join('')}</ul>` : '';
    const name = o => `${esc(o.name)}${o.schema ? ` <span class="dim">${esc(o.schema)}</span>` : ''}`;
    return `<div class="sqd-table"><h3>Views, triggers and routines</h3>
      ${block('Views', d.views, o => `<li>${name(o)}${o.kind !== 'view' ? ` <span class="bdg">${esc(o.kind)}</span>` : ''}</li>`)}
      ${block('Triggers', d.triggers, o => `<li>${name(o)}${o.on ? ` <span class="dim">on</span> ${link(o.on)}` : ''}</li>`)}
      ${block('Routines', d.routines, o => `<li>${name(o)} <span class="bdg">${esc(o.kind)}</span></li>`)}
      ${block('Events', d.events, o => `<li>${name(o)}</li>`)}
      ${block('Sequences', d.sequences, o => `<li>${name(o)}</li>`)}
      ${block('Other objects', d.other, o => `<li>${name(o)} <span class="bdg">${esc(o.kind)}</span></li>`)}</div>`;
  };

  const detail = () => {
    const host = $('#sqd-detail');
    if (!host) return;
    const t = st.sel === OTHER ? null : byKey().get(st.sel);
    const keep = host.scrollTop, open = host.querySelector('details')?.open;
    host.innerHTML = st.sel === OTHER ? otherDetail() : t ? tableDetail(t) : `<div class="v-center"><p class="note">${st.d.tables.length ? 'Choose a table.' : st.d.complete ? 'This dump creates no tables.' : 'Reading…'}</p></div>`;
    host.scrollTop = keep;
    if (open) host.querySelector('details')?.setAttribute('open', '');
  };

  const refreshNav = () => {
    const list = $('#sqd-list');
    if (!list) return;
    const hadFocus = list.contains(document.activeElement), top = list.parentElement.scrollTop;
    list.innerHTML = nav();
    list.parentElement.scrollTop = top;
    if (hadFocus) list.querySelector('[aria-selected=true]')?.focus();
  };

  const select = (key, focus = false) => {
    st.sel = key;
    refreshNav();
    detail();
    const b = [...document.querySelectorAll('#sqd-list [data-sqd-key]')].find(x => x.dataset.sqdKey === key);
    b?.scrollIntoView({ block: 'nearest' });
    if (focus) b?.focus();
  };

  // the whole schema view (on opening, and coming back from the SQL tab)
  const drawSchema = () => {
    if (st.sel === null || (st.sel !== OTHER && !byKey().has(st.sel))) st.sel = sorted()[0]?.key ?? null;
    $('#v-tools').innerHTML = tabs();
    $('#vbody').innerHTML = `<div class="sqd"><header class="sqd-head" id="sqd-head">${head()}</header>
      <div class="split fixed"><nav aria-label="Tables">${tools()}<div id="sqd-list" role="listbox" aria-label="Tables">${nav()}</div></nav>
      <section id="sqd-detail" tabindex="-1"></section></div></div>`;
    detail();
  };

  // new data from the server: redraw in place, keeping the choice, the scroll and the focus
  const refresh = () => {
    if (st.tab !== 'schema' || !$('#sqd-head')) return;
    $('#sqd-head').innerHTML = head();
    if (st.sel === null) st.sel = sorted()[0]?.key ?? null;
    refreshNav();
    detail();
  };

  const drawSql = async () => {
    try {
      if (!st.d.compression) {
        await text();
      } else {
        // compressed: the decompressed text, from the archive viewer's single-file entry
        const a = await api('archive', { id: e.id, limit: 1, quick: 1 });
        const name = a.entries?.[0]?.name;
        if (!name) throw new Error('can’t decompress this file');
        $('#vbody').innerHTML = '<div class="sqd-raw"></div>';
        $('#v-tools').innerHTML = '';
        await preview($('#vbody .sqd-raw'), entryUrl(e.id, name), name, null);
      }
    } catch (err) {
      if (live(e)) $('#vbody').innerHTML = `<div class="v-center"><div class="v-msg"><h3>Can’t show the text</h3><p>${esc(err.message || err)}</p></div></div>`;
    }
    if (!live(e)) return;
    if (st.tab === 'sql') $('#v-tools').insertAdjacentHTML('afterbegin', tabs());
    else drawSchema(); // back on the schema before the text arrived
  };

  const syncSort = () => document.querySelectorAll('#vbody [data-sqd-sort]').forEach(b => {
    const on = st.sort.key === b.dataset.sqdSort;
    b.setAttribute('aria-pressed', String(on));
    b.textContent = { name: 'Name', rows: 'Rows', bytes: 'Size' }[b.dataset.sqdSort] + (on ? (st.sort.asc ? ' ↑' : ' ↓') : '');
  });

  const pump = async () => {
    if (st.reading || !st.d.more) return;
    Object.assign(st, { reading: true, stopped: false, error: '' });
    refresh();
    try {
      while (st.d.more && !st.stopped) {
        const next = await api('sqldump', { id: e.id, more: st.d.more });
        if (!live(e)) return;
        st.d = next;
        refresh();
      }
    } catch (err) {
      st.error = err.message || String(err);
    } finally {
      st.reading = false;
      if (live(e)) refresh();
    }
  };

  // one set of listeners on the viewer body and tools, dropped when another file is shown
  const body = $('#vbody'), bar = $('#v-tools');
  const onClick = ev => {
    if (!live(e)) return;
    const tab = ev.target.closest('[data-sqd-tab]');
    if (tab && tab.dataset.sqdTab !== st.tab) {
      st.tab = tab.dataset.sqdTab;
      return void (st.tab === 'schema' ? drawSchema() : drawSql());
    }
    const k = ev.target.closest('[data-sqd-key]');
    if (k) return select(k.dataset.sqdKey, k.classList.contains('sqd-link'));
    const s = ev.target.closest('[data-sqd-sort]');
    if (s) {
      const key = s.dataset.sqdSort;
      st.sort = { key, asc: st.sort.key === key ? !st.sort.asc : key === 'name' };
      try { sessionStorage.setItem('bd.sqlsort', JSON.stringify(st.sort)); } catch {}
      syncSort();
      return refreshNav();
    }
    if (ev.target.closest('[data-sqd-stop]')) { st.stopped = true; return refresh(); } // after the part being read
    if (ev.target.closest('[data-sqd-go]')) pump();
  };
  const onInput = ev => {
    if (ev.target.id !== 'sqd-q' || !live(e)) return;
    st.q = ev.target.value.trim();
    refreshNav();
  };
  const onKey = ev => {
    const b = ev.target.closest?.('#sqd-list [data-sqd-key]');
    if (!b || !live(e) || !['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(ev.key)) return;
    ev.preventDefault();
    const all = [...document.querySelectorAll('#sqd-list [data-sqd-key]')], i = all.indexOf(b);
    const to = ev.key === 'Home' ? 0 : ev.key === 'End' ? all.length - 1 : Math.max(0, Math.min(all.length - 1, i + (ev.key === 'ArrowDown' ? 1 : -1)));
    select(all[to].dataset.sqdKey, true);
  };
  const off = () => {
    body.removeEventListener('click', onClick); bar.removeEventListener('click', onClick);
    body.removeEventListener('input', onInput); body.removeEventListener('keydown', onKey);
  };
  body.addEventListener('click', onClick); bar.addEventListener('click', onClick);
  body.addEventListener('input', onInput); body.addEventListener('keydown', onKey);
  const watch = setInterval(() => { if (!live(e)) { clearInterval(watch); off(); st.stopped = true; } }, 500);

  drawSchema();
  pump();
  return true;
}
