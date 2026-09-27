// Read-only viewers. Everything is fetched through /api/content (by index id) or the read-only
// endpoints; nothing here can write to a drive.
import { api, bytes, when } from './app.js';
import { kindOf } from './icons.js';

const $ = s => document.querySelector(s);
const esc = s => String(s ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const TEXT_MAX = 8 << 20;
const HLJS_LANG = {
  js: 'javascript', mjs: 'javascript', cjs: 'javascript', jsx: 'javascript', ts: 'typescript', tsx: 'typescript', py: 'python', rs: 'rust',
  go: 'go', c: 'c', h: 'c', cc: 'cpp', cpp: 'cpp', hpp: 'cpp', java: 'java', kt: 'kotlin', swift: 'swift', rb: 'ruby', php: 'php',
  pl: 'perl', lua: 'lua', r: 'r', sh: 'bash', bash: 'bash', zsh: 'bash', ps1: 'powershell', css: 'css', scss: 'scss', less: 'less',
  html: 'xml', htm: 'xml', xml: 'xml', svg: 'xml', json: 'json', yaml: 'yaml', yml: 'yaml', toml: 'ini', ini: 'ini', cfg: 'ini', conf: 'ini',
  sql: 'sql', md: 'markdown', markdown: 'markdown', diff: 'diff', patch: 'diff', makefile: 'makefile', dockerfile: 'dockerfile', cs: 'csharp',
};
const TEXTY = new Set('txt md markdown rst log nfo csv tsv eml mbox env gitignore gitattributes editorconfig lock'.split(' ').concat(Object.keys(HLJS_LANG)));

let current = null; // { list, index }

function ext(name) { const l = name.toLowerCase(); return l.includes('.') ? l.slice(l.lastIndexOf('.') + 1) : l; }
function contentUrl(id) { return `/api/content?id=${id}`; }

function show(title, meta) {
  $('#viewer').hidden = false;
  $('#v-name').textContent = title;
  $('#v-meta').textContent = meta || '';
  $('#v-tools').innerHTML = '';
  $('#vbody').innerHTML = '<div class="v-center"><div class="v-msg"><p>Loading…</p></div></div>';
  $('#vbody').scrollTop = 0;
}

function message(h, p) {
  $('#vbody').innerHTML = `<div class="v-center"><div class="v-msg"><h3>${esc(h)}</h3>${p.map(x => `<p>${x}</p>`).join('')}</div></div>`;
}

export function closeViewer() {
  $('#viewer').hidden = true;
  $('#vbody').innerHTML = '';
  current = null;
  document.querySelector('#view')?.focus();
}

async function fetchBytes(id, max) {
  const r = await fetch(contentUrl(id), { headers: max ? { Range: `bytes=0-${max - 1}` } : {}, credentials: 'same-origin' });
  if (!r.ok) throw new Error(await r.text());
  return new Uint8Array(await r.arrayBuffer());
}

function looksText(buf) {
  const n = Math.min(buf.length, 8192);
  for (let i = 0; i < n; i++) if (buf[i] === 0) return false;
  try { new TextDecoder('utf-8', { fatal: true }).decode(buf.subarray(0, n > 4 ? n - 4 : n)); return true; } catch { return false; }
}

function isSqlite(buf) {
  return buf.length >= 16 && new TextDecoder().decode(buf.subarray(0, 15)) === 'SQLite format 3';
}

export async function openViewer(entry, siblings = []) {
  const list = siblings.length ? siblings : [entry];
  current = { list, index: Math.max(0, list.findIndex(x => x.id === entry.id)) };
  render();
}

async function render() {
  const e = current.list[current.index];
  show(e.name, `${bytes(e.size)}${e.mtime ? ' · ' + when(e.mtime) : ''}${current.list.length > 1 ? ` · ${current.index + 1} of ${current.list.length}` : ''}`);
  $('#v-prev').disabled = current.index === 0;
  $('#v-next').disabled = current.index >= current.list.length - 1;
  const x = ext(e.name), k = kindOf(e.name);
  try {
    if (k === 'image') return image(e);
    if (x === 'pdf') return await pdf(e);
    if (x === 'docx') return await docx(e);
    if (k === 'audio' || k === 'video') return media(e, k);
    const head = await fetchBytes(e.id, 65536);
    if (isSqlite(head)) return await sqliteView(e);
    if (TEXTY.has(x) || (e.size < TEXT_MAX && looksText(head))) return await text(e, x);
    return hex(e, head);
  } catch (err) {
    message("Can't show this file", [esc(err.message || err)]);
  }
}

function image(e) {
  $('#vbody').innerHTML = `<div class="v-center"><img src="${contentUrl(e.id)}" alt="${esc(e.name)}"></div>`;
  const img = $('#vbody img');
  img.onerror = () => message("Can't display this image", ['Your browser may not support this format, or its bytes are no longer on an indexed drive.']);
  $('#v-tools').innerHTML = '<button class="btn sm" type="button" id="img-zoom">Actual size</button>';
  $('#img-zoom').onclick = ev => { const on = img.classList.toggle('actual'); ev.target.textContent = on ? 'Fit' : 'Actual size'; };
}

function media(e, k) {
  $('#vbody').innerHTML = `<div class="media"><${k} controls preload="metadata" src="${contentUrl(e.id)}"></${k}></div>`;
}

async function text(e, x) {
  const buf = await fetchBytes(e.id, TEXT_MAX);
  const src = new TextDecoder('utf-8').decode(buf);
  const truncated = e.size > buf.length;
  if (x === 'csv' || x === 'tsv') return csv(src, x === 'tsv' ? '\t' : null, truncated, e);
  const lang = HLJS_LANG[x] || (e.name.toLowerCase() === 'makefile' ? 'makefile' : null);
  let html = esc(src);
  if (src.length < 2_000_000) {
    const hljs = await loadHljs();
    try {
      html = lang && hljs.getLanguage(lang) ? hljs.highlight(src, { language: lang, ignoreIllegals: true }).value
        : src.length < 200_000 ? hljs.highlightAuto(src).value : esc(src);
    } catch { html = esc(src); }
  }
  const lines = src.split('\n').length;
  const outline = x === 'sql' ? sqlOutline(src) : '';
  $('#vbody').innerHTML = `<div class="v-text">${truncated ? `<div class="trunc">Showing the first ${bytes(buf.length)} of ${bytes(e.size)}.</div>` : ''}${outline}
    <div class="code"><div class="ln">${Array.from({ length: lines }, (_, i) => i + 1).join('\n')}</div><pre><code class="hljs">${html}</code></pre></div></div>`;
  $('#v-tools').innerHTML = `<span class="v-meta">${lines.toLocaleString()} lines${lang ? ' · ' + lang : ''}</span>`;
}

// a SQL dump's shape at a glance: the tables it creates and how many rows it inserts into each
function sqlOutline(src) {
  const tables = [...src.matchAll(/create\s+table\s+(?:if\s+not\s+exists\s+)?[`"\[]?([\w.$]+)[`"\]]?/gi)].map(m => m[1]);
  if (!tables.length) return '';
  const inserts = {};
  for (const m of src.matchAll(/insert\s+into\s+[`"\[]?([\w.$]+)[`"\]]?/gi)) inserts[m[1]] = (inserts[m[1]] || 0) + 1;
  return `<div class="outline"><b>${tables.length} table${tables.length === 1 ? '' : 's'}:</b>${tables.map(t => `<span class="chip">${esc(t)}${inserts[t] ? ` · ${inserts[t].toLocaleString()} insert${inserts[t] === 1 ? '' : 's'}` : ''}</span>`).join('')}</div>`;
}

function csv(src, sep, truncated, e) {
  const first = src.slice(0, src.indexOf('\n') + 1 || undefined);
  sep = sep || ([',', ';', '\t'].map(s => [s, first.split(s).length]).sort((a, b) => b[1] - a[1])[0][0]);
  const rows = [];
  let row = [], cell = '', q = false;
  for (let i = 0; i < src.length && rows.length < 5000; i++) {
    const c = src[i];
    if (q) { if (c === '"' && src[i + 1] === '"') { cell += '"'; i++; } else if (c === '"') q = false; else cell += c; }
    else if (c === '"') q = true;
    else if (c === sep) { row.push(cell); cell = ''; }
    else if (c === '\n') { row.push(cell.replace(/\r$/, '')); rows.push(row); row = []; cell = ''; }
    else cell += c;
  }
  if (cell || row.length) { row.push(cell); rows.push(row); }
  const [head, ...body] = rows;
  $('#vbody').innerHTML = `<div class="v-text">${truncated || rows.length >= 5000 ? `<div class="trunc">Showing the first ${body.length.toLocaleString()} rows.</div>` : ''}
    <table class="grid-table"><thead><tr>${(head || []).map(h => `<th>${esc(h)}</th>`).join('')}</tr></thead>
    <tbody>${body.map(r => `<tr>${r.map(c => `<td>${esc(c)}</td>`).join('')}</tr>`).join('')}</tbody></table></div>`;
  $('#v-tools').innerHTML = `<span class="v-meta">${body.length.toLocaleString()} rows × ${(head || []).length} columns</span>`;
}

function hex(e, buf) {
  const n = Math.min(buf.length, 4096);
  let out = '';
  for (let o = 0; o < n; o += 16) {
    const chunk = buf.subarray(o, Math.min(o + 16, n));
    out += o.toString(16).padStart(8, '0') + '  ' + [...chunk].map(b => b.toString(16).padStart(2, '0')).join(' ').padEnd(48) + '  ' +
      [...chunk].map(b => b >= 32 && b < 127 ? String.fromCharCode(b) : '.').join('') + '\n';
  }
  $('#vbody').innerHTML = `<div class="v-text"><div class="trunc">No viewer for this type. The first ${bytes(n)} as hex:</div><div class="hex">${esc(out)}</div></div>`;
}

// ---- PDF (pdf.js, bundled; eval disabled)

let pdfjs;
async function pdf(e) {
  pdfjs ||= await import('/vendor/pdf.min.mjs');
  pdfjs.GlobalWorkerOptions.workerSrc = '/vendor/pdf.worker.min.mjs';
  const doc = await pdfjs.getDocument({ url: contentUrl(e.id), isEvalSupported: false, withCredentials: true, disableAutoFetch: true }).promise;
  let scale = 1.25;
  const host = document.createElement('div');
  host.className = 'pdf-pages';
  $('#vbody').innerHTML = ''; $('#vbody').append(host);
  $('#v-tools').innerHTML = `<span class="v-meta">${doc.numPages} page${doc.numPages === 1 ? '' : 's'}</span>
    <button class="btn sm" type="button" id="pdf-out">−</button><button class="btn sm" type="button" id="pdf-in">+</button>`;
  const draw = () => {
    host.innerHTML = '';
    const io = new IntersectionObserver(entries => entries.forEach(async en => {
      if (!en.isIntersecting || en.target.dataset.done) return;
      en.target.dataset.done = '1';
      const page = await doc.getPage(+en.target.dataset.page);
      const vp = page.getViewport({ scale: scale * devicePixelRatio });
      const c = en.target;
      c.width = vp.width; c.height = vp.height;
      await page.render({ canvasContext: c.getContext('2d'), viewport: vp, canvas: c }).promise;
    }), { root: $('#vbody'), rootMargin: '600px' });
    for (let i = 1; i <= doc.numPages; i++) {
      const c = document.createElement('canvas');
      c.dataset.page = i; c.style.width = `${Math.round(612 * scale)}px`; c.style.aspectRatio = '612 / 792';
      host.append(c); io.observe(c);
    }
    doc.getPage(1).then(p => { const v = p.getViewport({ scale }); host.querySelectorAll('canvas').forEach(c => { c.style.width = v.width + 'px'; c.style.aspectRatio = `${v.width} / ${v.height}`; }); });
  };
  $('#pdf-in').onclick = () => { scale = Math.min(4, scale * 1.2); draw(); };
  $('#pdf-out').onclick = () => { scale = Math.max(0.4, scale / 1.2); draw(); };
  draw();
}

// ---- DOCX (docx-preview + JSZip, bundled)

function script(src) {
  return new Promise((ok, fail) => {
    if (document.querySelector(`script[src="${src}"]`)) return ok();
    const s = document.createElement('script');
    s.src = src; s.onload = ok; s.onerror = () => fail(new Error(`couldn't load ${src}`));
    document.head.append(s);
  });
}

async function docx(e) {
  await script('/vendor/jszip.min.js');
  await script('/vendor/docx-preview.min.js');
  const blob = new Blob([await fetchBytes(e.id)]);
  const host = document.createElement('div');
  host.className = 'docx-host';
  $('#vbody').innerHTML = ''; $('#vbody').append(host);
  await window.docx.renderAsync(blob, host, null, { inWrapper: true, ignoreLastRenderedPageBreak: true, experimental: true, useBase64URL: true });
}

// ---- SQLite (server side, opened immutable)

async function sqliteView(e) {
  const info = await api('sqlite', { id: e.id });
  if (!info.tables.length) return message('Empty database', ['This SQLite file has no tables.']);
  $('#vbody').innerHTML = `<div class="split"><nav id="sq-tables">${info.tables.map(t => `
      <button type="button" data-t="${esc(t.name)}"><div class="t">${esc(t.name)}</div><div class="m">${t.kind}${t.rows != null ? ` · ${t.rows.toLocaleString()} rows` : ''}</div></button>`).join('')}</nav>
    <section id="sq-data"></section></div>`;
  $('#v-tools').innerHTML = `<span class="v-meta">SQLite · ${info.tables.length} tables · opened read-only</span>`;
  const load = async (t, offset = 0) => {
    document.querySelectorAll('#sq-tables button').forEach(b => b.setAttribute('aria-current', String(b.dataset.t === t)));
    const d = (await api('sqlite', { id: e.id, table: t, offset })).data;
    const meta = info.tables.find(x => x.name === t);
    $('#sq-data').innerHTML = `<div class="pager"><b>${esc(t)}</b><span class="v-meta">rows ${(offset + 1).toLocaleString()}–${(offset + d.rows.length).toLocaleString()}${meta.rows != null ? ` of ${meta.rows.toLocaleString()}` : ''}</span>
        <button class="btn sm" type="button" id="sq-prev" ${offset ? '' : 'disabled'}>Previous</button><button class="btn sm" type="button" id="sq-next" ${d.rows.length < 200 ? 'disabled' : ''}>Next</button></div>
      ${meta.sql ? `<pre class="sqlsrc">${esc(meta.sql)}</pre>` : ''}
      <table class="grid-table"><thead><tr>${d.columns.map(c => `<th>${esc(c)}</th>`).join('')}</tr></thead>
      <tbody>${d.rows.map(r => `<tr>${r.map(v => v === null ? '<td class="null">NULL</td>' : `<td title="${esc(v)}">${esc(v)}</td>`).join('')}</tr>`).join('')}</tbody></table>`;
    $('#sq-prev').onclick = () => load(t, Math.max(0, offset - 200));
    $('#sq-next').onclick = () => load(t, offset + 200);
  };
  $('#sq-tables').onclick = ev => { const b = ev.target.closest('button[data-t]'); if (b) load(b.dataset.t); };
  load(info.tables[0].name);
}

// ---- diffs and git

function renderPatch(patch) {
  return `<div class="diff">${patch.split('\n').map(l => {
    const cls = l.startsWith('+++') || l.startsWith('---') || l.startsWith('diff ') || l.startsWith('index ') ? 'file'
      : l.startsWith('@@') ? 'hunk' : l.startsWith('+') ? 'add' : l.startsWith('-') ? 'del' : '';
    return `<div class="${cls}">${esc(l) || ' '}</div>`;
  }).join('')}</div>`;
}

export async function openDiff(a, b) {
  current = null;
  show('Compare', '');
  $('#v-prev').disabled = $('#v-next').disabled = true;
  try {
    const d = await api('diff', { a, b });
    $('#v-name').textContent = 'Compare';
    $('#v-meta').textContent = `${Math.round(d.ratio * 100)}% similar`;
    $('#vbody').innerHTML = d.unified.trim() ? renderPatch(d.unified) : `<div class="v-center"><div class="v-msg"><h3>Identical text</h3><p>${esc(d.a)}<br>${esc(d.b)}</p></div></div>`;
  } catch (err) {
    message("Can't compare these", [esc(err.message)]);
  }
}

export async function openGit(dirKey, name) {
  current = null;
  show(`${name} · git history`, '');
  $('#v-prev').disabled = $('#v-next').disabled = true;
  let log;
  try { log = await api('git', { dir: dirKey }); } catch (err) { return message("Can't read this repository", [esc(err.message)]); }
  $('#v-meta').textContent = `${log.branch ? log.branch + ' · ' : ''}${log.commits.length} commits`;
  $('#vbody').innerHTML = `<div class="split"><nav id="git-log">${log.commits.map(c => `
    <button type="button" data-rev="${c.id}"><div class="t">${esc(c.subject)}</div><div class="m">${esc(c.author)} · ${when(c.time)} · ${c.id.slice(0, 8)}</div></button>`).join('')}</nav>
    <section id="git-show"></section></div>`;
  const load = async rev => {
    document.querySelectorAll('#git-log button').forEach(b => b.setAttribute('aria-current', String(b.dataset.rev === rev)));
    const s = await api('git', { dir: dirKey, rev });
    $('#git-show').innerHTML = `<div class="commit-head"><h3>${esc(s.commit.message)}</h3><div class="m">${esc(s.commit.author)} &lt;${esc(s.commit.email)}&gt; · ${when(s.commit.time)} · ${esc(s.commit.id)}</div></div>${renderPatch(s.patch)}`;
  };
  $('#git-log').onclick = ev => { const b = ev.target.closest('button[data-rev]'); if (b) load(b.dataset.rev); };
  if (log.commits.length) load(log.commits[0].id);
}

// ---- a small preview for the details pane: images, a PDF's first page, the start of a text file,
// a database's tables. `alive()` says whether the pane still shows this file.

export async function previewInto(el, e, alive = () => true) {
  if (!el) return;
  const x = ext(e.name), k = kindOf(e.name);
  const put = html => { if (alive()) el.innerHTML = html; };
  try {
    if (k === 'image') {
      put(`<img src="${contentUrl(e.id)}" alt="">`);
      el.querySelector('img').onerror = () => put('');
      return;
    }
    if (x === 'pdf') {
      pdfjs ||= await import('/vendor/pdf.min.mjs');
      pdfjs.GlobalWorkerOptions.workerSrc = '/vendor/pdf.worker.min.mjs';
      const doc = await pdfjs.getDocument({ url: contentUrl(e.id), isEvalSupported: false, withCredentials: true, disableAutoFetch: true }).promise;
      const page = await doc.getPage(1);
      const base = page.getViewport({ scale: 1 });
      const vp = page.getViewport({ scale: (280 / base.width) * devicePixelRatio });
      const c = document.createElement('canvas');
      c.width = vp.width; c.height = vp.height;
      await page.render({ canvasContext: c.getContext('2d'), viewport: vp, canvas: c }).promise;
      if (!alive()) return;
      el.innerHTML = '';
      el.append(c);
      el.insertAdjacentHTML('beforeend', `<div class="pv-meta">${doc.numPages} page${doc.numPages === 1 ? '' : 's'}</div>`);
      return;
    }
    if (k === 'audio' || k === 'video') return put(`<${k} controls preload="metadata" src="${contentUrl(e.id)}"></${k}>`);
    const head = await fetchBytes(e.id, 4096);
    if (isSqlite(head)) {
      const info = await api('sqlite', { id: e.id });
      return put(`<div class="pv-meta">SQLite · ${info.tables.length} table${info.tables.length === 1 ? '' : 's'}</div><ul class="pv-list">${info.tables.slice(0, 12).map(t => `<li>${esc(t.name)}${t.rows != null ? ` <span>${t.rows.toLocaleString()} rows</span>` : ''}</li>`).join('')}</ul>`);
    }
    if (TEXTY.has(x) || looksText(head)) {
      const lines = new TextDecoder('utf-8').decode(head).split('\n').slice(0, 24).join('\n');
      return put(`<pre>${esc(lines)}</pre>`);
    }
    put('');
  } catch {
    put('');
  }
}

// ---- viewer chrome

let hljsPromise;
function loadHljs() {
  hljsPromise ||= script('/vendor/highlight.min.js').then(() => window.hljs);
  return hljsPromise;
}

$('#v-close').onclick = closeViewer;
$('#v-prev').onclick = () => { if (current && current.index > 0) { current.index--; render(); } };
$('#v-next').onclick = () => { if (current && current.index < current.list.length - 1) { current.index++; render(); } };
document.addEventListener('keydown', e => {
  if ($('#viewer').hidden) return;
  if (e.key === 'Escape') { e.preventDefault(); closeViewer(); }
  else if (e.key === 'ArrowLeft' && !e.target.closest('input')) $('#v-prev').click();
  else if (e.key === 'ArrowRight' && !e.target.closest('input')) $('#v-next').click();
});
