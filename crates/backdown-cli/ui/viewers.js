// Read-only viewers. Everything is fetched through /api/content (by index id) or the read-only
// endpoints; nothing here can write to a drive.
import { api, bytes, when } from './app.js';
import { kindOf, fileIcon, folderIcon } from './icons.js';
import { mailboxInto, mailExt } from './mailbox.js';

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
// containers the server parses (peek.rs); their entries and attachments come back by name
const SHEETS = new Set('xlsx xlsm xlsb xls xltx xltm ods'.split(' '));
const DECKS = new Set('pptx pptm ppsx odp'.split(' '));
const ARCHIVES = new Set('zip jar apk epub war cbz whl tar gz tgz bz2 tbz tbz2 xz txz zst tzst'.split(' '));
const DISKS = new Set('iso img dd raw bin dmg vmdk vdi qcow qcow2 vhd vhdx'.split(' '));
const WEB_IMG = new Set('png jpg jpeg gif webp avif bmp ico svg'.split(' '));
const ENTRY_MAX = 64 << 20; // the server's cap on one entry
const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' });

let current = null; // { list, index }
const live = e => current?.list[current.index] === e; // is `e` still the file on screen?

function ext(name) { const l = name.toLowerCase(); return l.includes('.') ? l.slice(l.lastIndexOf('.') + 1) : l; }
function contentUrl(id) { return `/api/content?id=${id}`; }
const leaf = name => name.replace(/[\\/]+$/, '').split(/[\\/]/).pop();
const entryUrl = (id, name) => `/api/entry?id=${id}&name=${encodeURIComponent(name)}`;
const attUrl = (id, at, part) => `/api/attachment?id=${id}${at != null ? `&at=${at}` : ''}&part=${part}`;

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
  try {
    const x = await mailExt(e, ext(e.name)), k = kindOf(e.name); // no extension: maybe mail
    if (SHEETS.has(x)) return await sheetView(e);
    if (DECKS.has(x)) return await slidesView(e);
    if (x === 'eml') return await mailView(e);
    if (x === 'mbox' || x === 'mbx') return live(e) && await mailboxInto($('#vbody'), { id: e.id }, { render: mailHtml, tools: $('#v-tools') });
    if (ARCHIVES.has(x)) return await archiveView(e);
    if (DISKS.has(x) && await diskView(e)) return; // not a disk image after all: carry on
    if (!live(e)) return;
    if (k === 'image') return image(e);
    if (x === 'pdf') return await pdf(e);
    if (x === 'docx') return await docx(e);
    if (k === 'audio' || k === 'video') return media(e, k);
    const head = await fetchBytes(e.id, 65536);
    if (isSqlite(head)) return await sqliteView(e);
    if (TEXTY.has(x) || (e.size < TEXT_MAX && looksText(head))) return await text(e, x);
    return hex(e, head);
  } catch (err) {
    if (live(e)) message("Can't show this file", [esc(err.message || err)]);
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
  const { html, lang } = await highlight(src, e.name);
  const lines = src.split('\n').length;
  const outline = x === 'sql' ? sqlOutline(src) : '';
  $('#vbody').innerHTML = `<div class="v-text">${truncated ? `<div class="trunc">Showing the first ${bytes(buf.length)} of ${bytes(e.size)}.</div>` : ''}${outline}
    <div class="code"><div class="ln">${Array.from({ length: lines }, (_, i) => i + 1).join('\n')}</div><pre><code class="hljs">${html}</code></pre></div></div>`;
  $('#v-tools').innerHTML = `<span class="v-meta">${lines.toLocaleString()} lines${lang ? ' · ' + lang : ''}</span>`;
}

// highlight.js markup for a text (escaped by hljs; plain escaped text when it's too big to bother)
async function highlight(src, name) {
  const lang = HLJS_LANG[ext(name)] || (leaf(name).toLowerCase() === 'makefile' ? 'makefile' : null);
  let html = esc(src);
  if (src.length < 2_000_000) {
    const hljs = await loadHljs();
    try {
      html = lang && hljs.getLanguage(lang) ? hljs.highlight(src, { language: lang, ignoreIllegals: true }).value
        : src.length < 200_000 ? hljs.highlightAuto(src).value : esc(src);
    } catch { html = esc(src); }
  }
  return { html, lang };
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

function hexDump(buf, n) {
  let out = '';
  for (let o = 0; o < n; o += 16) {
    const chunk = buf.subarray(o, Math.min(o + 16, n));
    out += o.toString(16).padStart(8, '0') + '  ' + [...chunk].map(b => b.toString(16).padStart(2, '0')).join(' ').padEnd(48) + '  ' +
      [...chunk].map(b => b >= 32 && b < 127 ? String.fromCharCode(b) : '.').join('') + '\n';
  }
  return out;
}

function hex(e, buf) {
  const n = Math.min(buf.length, 4096);
  $('#vbody').innerHTML = `<div class="v-text"><div class="trunc">No viewer for this type. The first ${bytes(n)} as hex:</div><div class="hex">${esc(hexDump(buf, n))}</div></div>`;
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

// ---- containers: spreadsheets, slide decks, mail, archives and disk images, parsed server-side
// (peek.rs). Files inside them come back through /api/entry and /api/attachment, chosen by the
// name the listing gave, with the same sandboxed headers as any file; mail HTML arrives as text.

async function fetchUrl(url, max) {
  const r = await fetch(`${url}&max=${max}`, { credentials: 'same-origin' });
  if (!r.ok) throw new Error(await r.text());
  return new Uint8Array(await r.arrayBuffer());
}

// spreadsheet column letters: 0 -> A, 25 -> Z, 26 -> AA
function colName(i) {
  let s = '';
  for (i++; i > 0; i = Math.floor((i - 1) / 26)) s = String.fromCharCode(65 + (i - 1) % 26) + s;
  return s;
}

function cellHtml(v) {
  if (v === null || v === undefined) return '<td></td>';
  if (typeof v === 'number') return `<td class="num">${esc(Number.isInteger(v) ? v : +v.toPrecision(12))}</td>`;
  if (typeof v === 'boolean') return `<td class="bool">${v ? 'TRUE' : 'FALSE'}</td>`;
  return `<td${v.length > 40 ? ` title="${esc(v)}"` : ''}>${esc(v)}</td>`;
}

async function sheetView(e) {
  const PAGE = 500;
  let sheet = null, offset = 0;
  const fail = err => live(e) && message("Can't show this sheet", [esc(err.message || err)]);
  const load = async () => {
    const d = await api('sheet', { id: e.id, sheet, offset, limit: PAGE });
    if (!live(e)) return;
    sheet = d.sheet;
    const cols = [...Array(Math.max(1, d.width)).keys()], total = d.total_rows, n = d.rows.length;
    $('#v-tools').innerHTML = `<span class="v-meta">${d.sheets.length} sheet${d.sheets.length === 1 ? '' : 's'}${total != null ? ` · ${total.toLocaleString()} rows × ${d.total_cols.toLocaleString()} columns` : ''}</span>`;
    $('#vbody').innerHTML = `<div class="v-text sheet">
      ${d.sheets.length > 1 ? `<div class="sheet-tabs" role="tablist">${d.sheets.map(s => `<button type="button" role="tab" data-s="${esc(s.name)}" aria-selected="${s.name === d.sheet}"${s.hidden ? ' class="hid" title="Hidden sheet"' : ''}>${esc(s.name)}</button>`).join('')}</div>` : ''}
      <div class="pager"><b>${esc(d.sheet)}</b><span class="v-meta">${n ? `rows ${(offset + 1).toLocaleString()}–${(offset + n).toLocaleString()}` : 'no rows here'}${total != null ? ` of ${total.toLocaleString()}` : ''}</span>
        <button class="btn sm" type="button" id="sh-prev" ${offset ? '' : 'disabled'}>Previous</button><button class="btn sm" type="button" id="sh-next" ${d.more ? '' : 'disabled'}>Next</button></div>
      ${d.cut ? `<div class="trunc">Showing the first ${cols.length} columns.</div>` : ''}${d.early ? '<div class="trunc">A very large sheet: stopped reading early.</div>' : ''}
      <table class="grid-table sheet-table"><thead><tr><th class="rn"></th>${cols.map(c => `<th>${colName(c)}</th>`).join('')}</tr></thead>
      <tbody>${d.rows.map((r, i) => `<tr><th class="rn">${offset + i + 1}</th>${cols.map(c => cellHtml(r[c])).join('')}</tr>`).join('')}</tbody></table></div>`;
    $('#vbody').scrollTop = 0;
    $('#sh-prev').onclick = () => { offset = Math.max(0, offset - PAGE); load().catch(fail); };
    $('#sh-next').onclick = () => { offset += PAGE; load().catch(fail); };
    $('#vbody .sheet-tabs')?.addEventListener('click', ev => {
      const b = ev.target.closest('button[data-s]');
      if (b) { sheet = b.dataset.s; offset = 0; load().catch(fail); }
    });
  };
  await load();
}

// an image embedded in a container: shown small (click for full size) if the browser can
function entryImg(id, name) {
  return WEB_IMG.has(ext(name))
    ? `<img class="pk-img" src="${entryUrl(id, name)}" alt="${esc(leaf(name))}" title="${esc(leaf(name))}" loading="lazy">`
    : `<span class="chip" title="${esc(name)}">${esc(leaf(name))}</span>`;
}

async function slidesView(e) {
  const d = await api('slides', { id: e.id });
  if (!live(e)) return;
  if (!d.slides.length) return message('No slides', ['BackDown found no slides in this presentation.']);
  $('#v-tools').innerHTML = `<span class="v-meta">${d.count} slide${d.count === 1 ? '' : 's'} · ${esc(d.format.toUpperCase())}</span>`;
  $('#vbody').innerHTML = `<div class="slides">${d.slides.map(s => `
    <article class="slide-card"><span class="slide-n">${s.n}</span>
      <h3>${s.title ? esc(s.title) : `<span class="dim">Slide ${s.n}</span>`}</h3>
      ${s.paras.length ? `<ul class="slide-text">${s.paras.map(p => `<li style="--lvl:${Math.min(p.lvl, 8)}">${esc(p.t)}</li>`).join('')}</ul>` : ''}
      ${s.images.length ? `<div class="pk-imgs">${s.images.map(i => entryImg(e.id, i)).join('')}</div>` : ''}
      ${s.notes ? `<div class="slide-notes"><b>Notes</b>${esc(s.notes)}</div>` : ''}
      ${s.cut ? '<p class="note">This deck has more text than the viewer shows at once; this slide’s is left out.</p>' : ''}
    </article>`).join('')}
    ${d.count > d.slides.length ? `<p class="note">Showing the first ${d.slides.length} of ${d.count} slides.</p>` : ''}</div>`;
}

// one parsed message; `at` is its offset in an mbox (null for an .eml)
function mailHtml(id, m, at) {
  const rows = [['From', m.from], ['To', m.to], ['Cc', m.cc], ['Reply-To', m.reply_to], ['Date', m.date ? when(m.date) : m.date_text]].filter(([, v]) => v);
  const pics = m.attachments.filter(a => WEB_IMG.has(ext(a.name)));
  return `<div class="mail">
    <div class="mail-head"><h3>${esc(m.subject || '(no subject)')}</h3><dl class="kv">${rows.map(([k, v]) => `<dt>${k}</dt><dd>${esc(v)}</dd>`).join('')}</dl></div>
    ${m.attachments.length ? `<div class="mail-atts">${m.attachments.map(a => `<a class="att" href="${attUrl(id, at, a.part)}&dl=1" download="${esc(a.name)}" title="Download ${esc(a.name)}">${fileIcon(a.name, 20)}<span>${esc(a.name)}</span><span class="dim">${a.type ? `${esc(a.type)} · ` : ''}${bytes(a.size)}</span></a>`).join('')}</div>` : ''}
    ${pics.length ? `<div class="pk-imgs mail-pics">${pics.map(a => `<img class="pk-img" src="${attUrl(id, at, a.part)}" alt="${esc(a.name)}" title="${esc(a.name)}" loading="lazy">`).join('')}</div>` : ''}
    ${m.from_html ? '<div class="trunc">This message only has an HTML version, shown here as plain text: nothing in it is loaded or run.</div>' : ''}
    ${m.truncated ? '<div class="trunc">A very large message: only its beginning is shown.</div>' : ''}
    <pre class="mail-body">${m.text ? esc(m.text) : '<span class="dim">(no text)</span>'}</pre></div>`;
}

async function mailView(e) {
  const m = await api('mail', { id: e.id });
  if (!live(e)) return;
  $('#v-tools').innerHTML = `<span class="v-meta">${m.attachments.length} attachment${m.attachments.length === 1 ? '' : 's'}</span>`;
  $('#vbody').innerHTML = mailHtml(e.id, m, null);
}

// One file from inside a container, previewed in `host`: images, audio/video, PDFs, text and code;
// anything else as hex. Everything is fetched from `url` (an /api/entry link). `streams`: the
// server streams this entry at any size (a file in a disk image's filesystem), so only previews
// that would load it whole are held back when it's big.
async function entryPreview(host, url, name, size, streams = false) {
  const x = ext(name), k = kindOf(name), base = leaf(name);
  const big = size != null && size > ENTRY_MAX;
  host.innerHTML = `<div class="pv-head"><span class="pv-name" title="${esc(name)}">${esc(base)}</span><span class="v-meta">${size != null ? bytes(size) : ''}</span>
    ${big && !streams ? '' : `<a class="btn sm" href="${url}&dl=1" download="${esc(base)}">Download</a>`}</div><div class="pv-body"></div>`;
  const body = host.querySelector('.pv-body');
  const alive = () => body.isConnected; // a newer preview replaced this one
  if (big && streams && x === 'pdf') { body.innerHTML = '<p class="note pad">Too large to preview here: download it to open it.</p>'; return; }
  if (big && !streams) { body.innerHTML = `<p class="note pad">Too large to open from inside the container (the limit is ${bytes(ENTRY_MAX)}).</p>`; return; }
  try {
    if (k === 'image') { body.innerHTML = `<div class="pv-center"><img class="pk-img big" src="${url}" alt="${esc(base)}"></div>`; return; }
    if (k === 'audio' || k === 'video') { body.innerHTML = `<div class="pv-center"><${k} controls preload="metadata" src="${url}"></${k}></div>`; return; }
    if (x === 'pdf') return await pdfInto(body, url, alive);
    const head = await fetchUrl(url, 65536);
    if (!alive()) return;
    if (TEXTY.has(x) || looksText(head)) {
      const buf = head.length < 65536 ? head : await fetchUrl(url, 2 << 20);
      if (!alive()) return;
      const src = new TextDecoder('utf-8').decode(buf);
      const { html } = await highlight(src, name);
      if (!alive()) return;
      const lines = src.split('\n').length;
      body.innerHTML = `${size > buf.length ? `<div class="trunc">Showing the first ${bytes(buf.length)}.</div>` : ''}
        <div class="code"><div class="ln">${Array.from({ length: lines }, (_, i) => i + 1).join('\n')}</div><pre><code class="hljs">${html}</code></pre></div>`;
      return;
    }
    const n = Math.min(head.length, 4096);
    body.innerHTML = `<div class="trunc">No preview for this type. The first ${bytes(n)} as hex:</div><div class="hex">${esc(hexDump(head, n))}</div>`;
  } catch (err) {
    if (alive()) body.innerHTML = `<p class="note pad">${esc(err.message || err)}</p>`;
  }
}

async function pdfInto(host, url, alive, maxPages = 30) {
  pdfjs ||= await import('/vendor/pdf.min.mjs');
  pdfjs.GlobalWorkerOptions.workerSrc = '/vendor/pdf.worker.min.mjs';
  const doc = await pdfjs.getDocument({ url, isEvalSupported: false, withCredentials: true, disableRange: true, disableStream: true, disableAutoFetch: true }).promise;
  if (!alive()) return;
  host.innerHTML = '<div class="pdf-pages"></div>';
  const pages = host.firstChild, width = Math.max(200, host.clientWidth - 40);
  for (let i = 1; i <= Math.min(doc.numPages, maxPages) && alive(); i++) {
    const page = await doc.getPage(i);
    const vp = page.getViewport({ scale: (width / page.getViewport({ scale: 1 }).width) * devicePixelRatio });
    const c = document.createElement('canvas');
    c.width = vp.width; c.height = vp.height; c.style.width = `${width}px`;
    pages.append(c);
    await page.render({ canvasContext: c.getContext('2d'), viewport: vp, canvas: c }).promise;
  }
  if (doc.numPages > maxPages) pages.insertAdjacentHTML('beforeend', `<p class="note">The first ${maxPages} of ${doc.numPages} pages.</p>`);
}

// A two-pane browser over a container's folders: a sortable list, and a preview of the chosen
// file. `open(path)` lists a folder as { dirs: [{name, size?, count?, mtime?}], files: [{name,
// base, size, mtime, ...}], note? }; `url(path, file)` is where that file's bytes are. A row's
// `badges` are shown by its name, and a file with `unreadable` says why instead of a preview.
function browseInto(host, { rootName, open, url, streams = false }) {
  const st = { path: [], sort: { key: 'name', asc: true }, rows: [] };
  host.innerHTML = `<div class="arc"><div class="arc-list"><nav class="acrumbs" aria-label="Folder"></nav><div class="arc-note"></div><table class="list"><thead></thead><tbody></tbody></table></div>
    <div class="arc-pv"><div class="v-center"><p class="note">Select a file to preview it.</p></div></div></div>`;
  const q = s => host.querySelector(s);
  const draw = async () => {
    const d = await open(st.path);
    if (!host.isConnected) return;
    const { key, asc } = st.sort, dir = asc ? 1 : -1;
    const name = r => r.base ?? r.name;
    const val = r => key === 'size' ? (r.size ?? -1) : (r.mtime ?? 0);
    const by = (a, b) => (key === 'name' ? 0 : dir * (val(a) - val(b))) || (key === 'name' ? dir : 1) * collator.compare(name(a), name(b));
    st.rows = [...d.dirs.map(r => ({ ...r, dir: true })).sort(by), ...[...d.files].sort(by)];
    q('.acrumbs').innerHTML = [rootName, ...st.path].map((p, i) => `${i ? '<span class="crumb-sep">›</span>' : ''}<button class="crumb" type="button" data-depth="${i}">${esc(p)}</button>`).join('');
    q('.arc-note').innerHTML = d.note ? `<div class="trunc">${esc(d.note)}</div>` : '';
    const col = (k, label, n) => `<th data-k="${k}" class="${n ? 'n' : ''}" ${key === k ? `aria-sort="${asc ? 'ascending' : 'descending'}"` : ''}>${label}</th>`;
    q('thead').innerHTML = `<tr>${col('name', 'Name')}${col('size', 'Size', 1)}${col('mtime', 'Modified')}</tr>`;
    q('tbody').innerHTML = st.rows.map((r, i) => `<tr data-i="${i}">
      <td><div class="nmc">${r.dir ? folderIcon(20) : fileIcon(r.base, 20)}<span class="nm">${esc(name(r))}</span>${r.link ? `<span class="dim">→ ${esc(r.link)}</span>` : ''}${r.encrypted ? '<span class="bdg">encrypted</span>' : ''}${(r.badges || []).map(b => `<span class="bdg">${esc(b)}</span>`).join('')}</div></td>
      <td class="n">${r.dir ? (r.count != null ? `<span class="dim">${r.count.toLocaleString()} item${r.count === 1 ? '' : 's'} · </span>${bytes(r.size)}` : '') : r.size != null ? bytes(r.size) : '—'}</td>
      <td>${r.mtime ? when(r.mtime) : ''}</td></tr>`).join('') || '<tr><td colspan="3" class="dim">Empty folder</td></tr>';
    q('.arc-list').scrollTop = 0;
  };
  const fail = err => { if (host.isConnected) q('.arc-note').innerHTML = `<div class="trunc">${esc(err.message || err)}</div>`; };
  host.addEventListener('click', ev => {
    const c = ev.target.closest('.acrumbs [data-depth]');
    if (c) { st.path = st.path.slice(0, +c.dataset.depth); return void draw().catch(fail); }
    const th = ev.target.closest('th[data-k]');
    if (th) {
      const k = th.dataset.k;
      st.sort = { key: k, asc: st.sort.key === k ? !st.sort.asc : k === 'name' };
      return void draw().catch(fail);
    }
    const tr = ev.target.closest('tr[data-i]');
    if (!tr) return;
    const r = st.rows[+tr.dataset.i];
    if (r.dir) { st.path.push(r.name); return void draw().catch(fail); }
    host.querySelectorAll('tbody tr').forEach(x => x.classList.toggle('sel', x === tr));
    if (r.unreadable) { q('.arc-pv').innerHTML = `<div class="v-center"><div class="v-msg"><h3>${esc(r.name)}</h3><p>${esc(r.unreadable)}</p></div></div>`; return; }
    entryPreview(q('.arc-pv'), url(st.path, r), r.name, r.size, streams);
  });
  return draw();
}

// an archive listing as folders: implied parents made up, sizes and counts summed
function archiveTree(entries) {
  const node = name => ({ name, dirs: new Map(), files: [], size: 0, count: 0, mtime: null });
  const root = node('');
  for (const en of entries) {
    const parts = en.name.split(/[\\/]/).filter(p => p && p !== '.');
    if (!parts.length) continue;
    let n = root;
    for (const p of en.dir ? parts : parts.slice(0, -1)) {
      if (!n.dirs.has(p)) n.dirs.set(p, node(p));
      n = n.dirs.get(p);
    }
    if (en.dir) n.mtime = en.mtime;
    else n.files.push({ ...en, base: parts.at(-1) });
  }
  const sum = n => {
    for (const c of n.dirs.values()) { sum(c); n.size += c.size; n.count += c.count; }
    for (const f of n.files) { n.size += f.size || 0; n.count++; }
  };
  sum(root);
  return root;
}

async function archiveView(e) {
  const d = await api('archive', { id: e.id });
  if (!live(e)) return;
  const tree = archiveTree(d.entries);
  const count = d.total ?? d.counted, more = d.complete ? '' : '+';
  $('#v-tools').innerHTML = `<span class="v-meta">${esc(d.format)} · ${count.toLocaleString()}${more} entr${count === 1 ? 'y' : 'ies'} · ${bytes(d.total_size)}${more} unpacked</span>`;
  const notes = [];
  if (d.entries.length < count) notes.push(`Listing the first ${d.entries.length.toLocaleString()} of ${count.toLocaleString()}${more} entries.`);
  if (d.note) notes.push(d.note);
  $('#vbody').innerHTML = `<div class="arc-wrap">${notes.map(n => `<div class="trunc">${esc(n)}</div>`).join('')}<div class="arc-host"></div></div>`;
  await browseInto($('#vbody .arc-host'), {
    rootName: e.name,
    open: async path => { let n = tree; for (const p of path) n = n.dirs.get(p); return { dirs: [...n.dirs.values()], files: n.files }; },
    url: (path, f) => entryUrl(e.id, f.name),
  });
}

const DISK_NAMES = {
  iso9660: 'ISO 9660 disc image', raw: 'Disk image', dmg: 'Apple disk image (UDIF)', vmdk: 'VMware virtual disk (VMDK)',
  qcow2: 'QEMU virtual disk (qcow2)', qcow: 'QEMU virtual disk (qcow)', vdi: 'VirtualBox virtual disk (VDI)',
  vhd: 'Virtual PC / Hyper-V disk (VHD)', vhdx: 'Hyper-V virtual disk (VHDX)',
};
const fsText = fs => fs ? `${esc(fs.kind)}${fs.label ? ` “${esc(fs.label)}”` : ''}` : '<span class="dim">not recognised</span>';
// the filesystems whose files can be listed (peek.rs fsview, over backdown_core::fsread)
const browsable = fs => !!fs && /^(FAT(12|16|32)?|exFAT|ext[234]|NTFS)$/.test(fs.kind);
const browseBtn = (part, fs) => `<button class="btn sm" type="button" data-part="${part}" data-root="${esc(fs.label || fs.kind)}">Browse files</button>`;

// Disk images: what the headers and tables say (partitions and their filesystems, a virtual disk's
// size), an ISO's files, and the files in each partition whose filesystem can be read (the whole
// image's, or the largest partition's, open at once). False if the server doesn't recognise it.
async function diskView(e) {
  const d = await api('disk', { id: e.id });
  if (!live(e)) return true;
  if (d.format === 'unknown') return false;
  const title = DISK_NAMES[d.format] || d.format, parts = d.partitions || [];
  const whole = browsable(d.fs), iso = d.format === 'iso9660';
  const layoutless = !d.table && !d.fs && !['raw', 'iso9660', 'dmg'].includes(d.format);
  $('#v-tools').innerHTML = `<span class="v-meta">${esc(title)}${d.virtual_size ? ` · ${bytes(d.virtual_size)}` : ''}</span>`;
  const summary = `<div class="disk">
    <h3>${esc(title)}${d.label ? ` · ${esc(d.label)}` : ''}</h3>
    <dl class="kv">${d.virtual_size ? `<dt>${d.format === 'raw' ? 'Size' : 'Virtual size'}</dt><dd>${bytes(d.virtual_size)} <span class="note">(${d.virtual_size.toLocaleString()} bytes)</span></dd>` : ''}
      ${d.format !== 'raw' && d.format !== 'iso9660' ? `<dt>File size</dt><dd>${bytes(d.file_size)}</dd>` : ''}
      ${d.table ? `<dt>Partition table</dt><dd>${esc(d.table)}</dd>` : ''}
      ${d.fs ? `<dt>Filesystem</dt><dd>${fsText(d.fs)}${d.fs.size ? ` · ${bytes(d.fs.size)}` : ''}</dd>` : ''}
      ${(d.details || []).map(([k, v]) => `<dt>${esc(k)}</dt><dd>${esc(v)}</dd>`).join('')}</dl>
    ${iso && parts.some(p => browsable(p.fs)) ? '<p class="fs-pick"><button class="btn sm" type="button" data-part="iso">Browse the disc</button> or a partition below.</p>' : ''}
    ${parts.length ? `<table class="list parts"><thead><tr><th class="n">#</th><th>Type</th><th>Name</th><th>Filesystem</th><th class="n">Start</th><th class="n">Size</th><th></th></tr></thead>
      <tbody>${parts.map(p => `<tr data-row="${p.index}"><td class="n">${p.index}</td><td title="${esc(p.code)}">${esc(p.type)}${p.bootable ? ' <span class="bdg">boot</span>' : ''}</td><td>${esc(p.name || '')}</td>
        <td>${fsText(p.fs)}</td><td class="n">${bytes(p.start)}</td><td class="n">${bytes(p.size)}</td><td class="n">${browsable(p.fs) ? browseBtn(p.index, p.fs) : ''}</td></tr>`).join('')}</tbody></table>` : ''}
    ${d.dmg_parts ? `<h4>Partitions</h4><ul class="pv-list">${d.dmg_parts.map(p => `<li>${esc(p.name)} <span>${bytes(p.size)}</span></li>`).join('')}</ul>` : ''}
    ${layoutless ? '<p class="note">To browse the files inside, convert it to a raw image first (for example with <code>qemu-img convert -O raw</code>).</p>' : ''}
  </div>`;
  const first = whole ? { part: '0', fs: d.fs } : parts.filter(p => browsable(p.fs)).sort((a, b) => b.size - a.size).map(p => ({ part: String(p.index), fs: p.fs }))[0];
  if (!iso && !first) { $('#vbody').innerHTML = `<div class="v-text">${summary}</div>`; return true; }
  $('#vbody').innerHTML = `<div class="iso-view">${summary}<div class="iso-files"></div></div>`;
  let host = $('#vbody .iso-files');
  const list = r => ({
    dirs: r.entries.filter(x => x.dir).map(x => ({ name: x.name, mtime: x.mtime })),
    files: r.entries.filter(x => !x.dir).map(x => ({ ...x, base: x.name })),
    note: r.truncated ? `Showing the first ${r.entries.length.toLocaleString()} entries.` : '',
  });
  const show = (part, root) => {
    $('#vbody').querySelectorAll('.parts tr[data-row]').forEach(tr => tr.classList.toggle('cur', tr.dataset.row === part));
    const fresh = host.cloneNode(false); // without the last browser's listeners
    host.replaceWith(fresh);
    host = fresh;
    if (part !== 'iso') return fsBrowse(host, e, part, root);
    return browseInto(host, {
      rootName: d.label || 'Disc',
      open: async path => list(path.length ? await api('disk', { id: e.id, path: path.join('/') }) : d),
      url: (path, f) => entryUrl(e.id, [...path, f.name].join('/')),
    });
  };
  $('#vbody .disk').addEventListener('click', ev => {
    const b = ev.target.closest('button[data-part]');
    if (b) show(b.dataset.part, b.dataset.root).catch(err => { if (live(e)) host.innerHTML = `<p class="note pad">${esc(err.message || err)}</p>`; });
  });
  if (iso) await show('iso');
  else await show(first.part, first.fs.label || first.fs.kind).catch(err => { if (live(e)) host.innerHTML = `<p class="note pad">${esc(err.message || err)}</p>`; });
  return true;
}

// The files of the filesystem in partition `part` of a disk image ("0": the whole image), listed
// folder by folder by the server; a file is asked for by its path in those listings.
function fsBrowse(host, e, part, rootName) {
  const SHOW = new Set(['compressed', 'sparse', 'encrypted']);
  const badges = x => (x.flags || []).filter(f => SHOW.has(f));
  const list = r => ({
    dirs: r.entries.filter(x => x.dir).map(x => ({ name: x.name, mtime: x.mtime, badges: badges(x) })),
    files: r.entries.filter(x => !x.dir).map(x => ({
      ...x, base: x.name, badges: badges(x), size: x.link != null ? null : x.size,
      unreadable: x.unreadable || (x.link != null ? `A link to ${x.link}. Links are shown, not followed.` : null),
    })),
    note: [r.truncated ? `Showing the first ${r.entries.length.toLocaleString()} entries of this folder.` : '', ...(r.fs.notes || []).map(n => `${n[0].toUpperCase()}${n.slice(1)}.`)].filter(Boolean).join(' '),
  });
  return browseInto(host, {
    rootName, streams: true,
    open: async path => list(await api('fs', { id: e.id, part, path: path.join('/') })),
    url: (path, f) => `${entryUrl(e.id, [...path, f.name].join('/'))}&part=${encodeURIComponent(part)}`,
  });
}

// a disk image in a few lines, for the details pane
function diskSummary(d) {
  const parts = d.partitions || [];
  return `<div class="pv-meta">${esc(DISK_NAMES[d.format] || d.format)}${d.virtual_size ? ` · ${bytes(d.virtual_size)}` : ''}${d.table ? ` · ${esc(d.table)}` : ''}</div>
    ${parts.length ? `<ul class="pv-list">${parts.map(p => `<li>${p.index}. ${p.fs ? fsText(p.fs) : esc(p.type)} <span>${bytes(p.size)}</span></li>`).join('')}</ul>`
    : d.fs ? `<ul class="pv-list"><li>${fsText(d.fs)} <span>${d.fs.size ? bytes(d.fs.size) : ''}</span></li></ul>`
    : d.entries ? `<ul class="pv-list">${d.entries.slice(0, 8).map(en => `<li>${esc(en.name)}${en.dir ? '/' : ` <span>${bytes(en.size)}</span>`}</li>`).join('')}</ul>`
    : ''}`;
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

// a Maildir folder (cur/ and new/) as one mailbox: threads across both, messages by index id
export async function openMaildir(dirKey, name) {
  current = null;
  show(`${name} · mailbox`, '');
  $('#v-prev').disabled = $('#v-next').disabled = true;
  try {
    await mailboxInto($('#vbody'), { dir: dirKey }, { render: mailHtml, tools: $('#v-tools'), title: $('#v-name') });
  } catch (err) {
    message("Can't open this mailbox", [esc(err.message || err)]);
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
  const x = await mailExt(e, ext(e.name)), k = kindOf(e.name);
  const put = html => { if (alive()) el.innerHTML = html; };
  try {
    if (SHEETS.has(x)) {
      const d = await api('sheet', { id: e.id, limit: 10 });
      const w = Math.min(d.width, 6);
      return put(`<div class="pv-meta">${d.sheets.length} sheet${d.sheets.length === 1 ? '' : 's'} · ${esc(d.sheet)}${d.total_rows != null ? ` · ${d.total_rows.toLocaleString()} rows` : ''}</div>
        <div class="pv-table"><table class="grid-table">${d.rows.map(r => `<tr>${Array.from({ length: w }, (_, c) => cellHtml(r[c])).join('')}</tr>`).join('')}</table></div>`);
    }
    if (DECKS.has(x)) {
      const d = await api('slides', { id: e.id });
      return put(`${d.thumbnail ? `<img class="pk-hide" src="${entryUrl(e.id, d.thumbnail)}" alt="">` : ''}<div class="pv-meta">${d.count} slide${d.count === 1 ? '' : 's'}</div>
        <ul class="pv-list">${d.slides.slice(0, 8).map(s => `<li>${s.n}. ${s.title ? esc(s.title) : '<span>untitled</span>'}</li>`).join('')}</ul>`);
    }
    if (x === 'eml') {
      const m = await api('mail', { id: e.id });
      const rows = [['From', m.from], ['To', m.to], ['Date', m.date ? when(m.date) : m.date_text], ['Subject', m.subject || '(no subject)'],
        ['Attached', m.attachments.map(a => a.name).join(', ')]].filter(([, v]) => v);
      return put(`<dl class="kv pv-kv">${rows.map(([k2, v]) => `<dt>${k2}</dt><dd>${esc(v)}</dd>`).join('')}</dl><pre>${esc(m.text.split('\n').slice(0, 12).join('\n'))}</pre>`);
    }
    if (x === 'mbox' || x === 'mbx') {
      const d = await api('mbox', { id: e.id, limit: 8 });
      return put(`<div class="pv-meta">mbox · ${d.messages.length}${d.next != null ? '+' : ''} message${d.messages.length === 1 ? '' : 's'}</div>
        <ul class="pv-list">${d.messages.map(m => `<li>${esc(m.subject || '(no subject)')} <span>${esc(m.from)}</span></li>`).join('')}</ul>`);
    }
    if (ARCHIVES.has(x)) {
      const d = await api('archive', { id: e.id, limit: 8, quick: 1 });
      const n = d.total ?? d.counted, more = d.complete ? '' : '+';
      return put(`<div class="pv-meta">${esc(d.format)} · ${n.toLocaleString()}${more} entr${n === 1 ? 'y' : 'ies'} · ${bytes(d.total_size)}${more}</div>
        <ul class="pv-list">${d.entries.map(en => `<li>${esc(en.name)}${en.dir ? '' : ` <span>${en.size != null ? bytes(en.size) : ''}</span>`}</li>`).join('')}</ul>`);
    }
    if (DISKS.has(x)) {
      const d = await api('disk', { id: e.id });
      if (d.format !== 'unknown') return put(diskSummary(d));
    }
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

// images from inside containers: click for full size; one the browser can't decode (EMF, TIFF)
// becomes its name, and a preview's optional thumbnail just goes away (no inline handlers: CSP)
$('#vbody').addEventListener('click', ev => {
  const img = ev.target.closest('img.pk-img');
  if (img && !img.closest('.pv-body')) img.classList.toggle('big');
});
document.addEventListener('error', ev => {
  const img = ev.target;
  if (!(img instanceof HTMLImageElement)) return;
  if (img.classList.contains('pk-hide')) img.remove();
  else if (img.classList.contains('pk-img')) img.replaceWith(Object.assign(document.createElement('span'), { className: 'chip', textContent: img.alt || 'image' }));
}, true);

$('#v-close').onclick = closeViewer;
$('#v-prev').onclick = () => { if (current && current.index > 0) { current.index--; render(); } };
$('#v-next').onclick = () => { if (current && current.index < current.list.length - 1) { current.index++; render(); } };
document.addEventListener('keydown', e => {
  if ($('#viewer').hidden) return;
  if (e.key === 'Escape') { e.preventDefault(); closeViewer(); }
  else if (e.key === 'ArrowLeft' && !e.target.closest('input')) $('#v-prev').click();
  else if (e.key === 'ArrowRight' && !e.target.closest('input')) $('#v-next').click();
});
