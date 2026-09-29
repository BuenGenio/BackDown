// The files inside a filesystem that BackDown reads itself (FAT, exFAT, ext2/3/4, NTFS), in a disk
// image in the index (`source` = { id, part }) or where Rescue found one on a drive or image
// (`source` = { rescue, at }): folders listed by the server one at a time, a file asked for by its
// path through those listings. "Show deleted files" adds each folder's deleted files and folders,
// greyed, with how likely each comes back; a deleted one is asked for by the path segment its
// listing gave it. Chosen files and folders (deleted ones too) are copied out to another drive by
// the daemon, each copy checked (a "files" rescue). Nothing here writes to the source.
import { api, bytes, when } from './app.js';
import { fileIcon, folderIcon } from './icons.js';
import { entryPreview } from './viewers.js';
import { toast } from './extras.js';

const esc = s => String(s ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' });
const SHOW = new Set(['compressed', 'sparse', 'encrypted']);
const STATE = { recoverable: ['ok', 'deleted, recoverable'], 'partly overwritten': ['warn', 'partly overwritten'], overwritten: ['bad', 'overwritten'], "can't be read": ['bad', 'can’t be read'] };
const seg = x => x.deleted?.seg ?? x.name;
const shown = s => s.split('\0')[0];
let showDeleted = (() => { try { return localStorage.getItem('bd.fsdel') === '1'; } catch { return false; } })();

// the /api/entry link for a file at `segs` (its folder's segments, then its own)
export function fsEntryUrl(source, segs) {
  return `/api/entry?${new URLSearchParams({ ...source, name: segs.join('/') })}`;
}

// A two-pane browser: the folder (sortable, with checkboxes to choose what to copy out) and a
// preview of the file clicked. `rootName` names the root in the crumbs.
export function fsBrowser(host, { source, rootName }) {
  const st = { path: [], sort: { key: 'name', asc: true }, rows: [], chosen: new Map(), listing: null };
  host.innerHTML = `<div class="fsb">
    <div class="fsb-bar">
      <label class="wp-chk"><input type="checkbox" class="fsb-del"${showDeleted ? ' checked' : ''}> Show deleted files</label>
      <span class="fsb-count muted"></span>
      <button class="btn sm fsb-copy" type="button">Copy this folder out…</button>
    </div>
    <div class="fsb-go" hidden></div>
    <div class="arc"><div class="arc-list"><nav class="acrumbs" aria-label="Folder"></nav><div class="arc-note"></div><table class="list"><thead></thead><tbody></tbody></table></div>
      <div class="arc-pv"><div class="v-center"><p class="note">Select a file to preview it.</p></div></div></div></div>`;
  const q = s => host.querySelector(s);
  const params = () => ({ ...source, path: st.path.join('/'), deleted: showDeleted ? 1 : '' });

  const draw = async () => {
    const r = await api('fs', params());
    if (!host.isConnected) return;
    st.listing = r;
    const inDel = r.in_deleted;
    const rows = [...r.entries, ...(r.deleted || [])];
    const { key, asc } = st.sort, dir = asc ? 1 : -1;
    const val = x => key === 'size' ? (x.size ?? -1) : (x.mtime ?? 0);
    const by = (a, b) => (key === 'name' ? 0 : dir * (val(a) - val(b))) || (key === 'name' ? dir : 1) * collator.compare(a.name, b.name);
    st.rows = [...rows.filter(x => x.dir).sort(by), ...rows.filter(x => !x.dir).sort(by)];
    q('.acrumbs').innerHTML = [rootName, ...st.path.map(shown)].map((p, i) => `${i ? '<span class="crumb-sep">›</span>' : ''}<button class="crumb" type="button" data-depth="${i}">${esc(p)}</button>`).join('')
      + (inDel ? '<span class="bdg deleted">deleted folder</span>' : '');
    const notes = [];
    if (r.truncated) notes.push(`Showing the first ${r.entries.length.toLocaleString()} entries of this folder.`);
    for (const n of r.fs?.notes || []) notes.push(`${n[0].toUpperCase()}${n.slice(1)}.`);
    if (showDeleted && r.undelete) notes.push(`${r.undelete}.`);
    if (showDeleted && r.deleted_note) notes.push(`${r.deleted_note[0].toUpperCase()}${r.deleted_note.slice(1)}.`);
    if (showDeleted && !inDel && r.deleted && !r.deleted.length) notes.push('No deleted files in this folder.');
    q('.arc-note').innerHTML = notes.length ? `<div class="trunc">${esc(notes.join(' '))}</div>` : '';
    const col = (k, label, n) => `<th data-k="${k}" class="${n ? 'n' : ''}" ${key === k ? `aria-sort="${asc ? 'ascending' : 'descending'}"` : ''}>${label}</th>`;
    q('thead').innerHTML = `<tr><th class="fsb-ck"><input type="checkbox" class="fsb-all" aria-label="Choose everything here"></th>${col('name', 'Name')}${col('size', 'Size', 1)}${col('mtime', 'Modified')}</tr>`;
    q('tbody').innerHTML = st.rows.map((x, i) => {
      const d = x.deleted, [cls, label] = d ? STATE[d.state] || ['warn', d.state] : [];
      const badges = (x.flags || []).filter(f => SHOW.has(f)).map(f => `<span class="bdg">${esc(f)}</span>`).join('');
      const tip = d ? `${d.why}${d.state === 'overwritten' ? '' : ` (${d.confidence}% sure)`}` : '';
      return `<tr data-i="${i}"${d ? ` class="fsb-gone" title="${esc(tip)}"` : ''}>
        <td class="fsb-ck"><input type="checkbox" data-ck="${i}"${st.chosen.has(seg(x)) ? ' checked' : ''} aria-label="Choose ${esc(x.name)}"${x.link != null ? ' disabled' : ''}></td>
        <td><div class="nmc">${x.dir ? folderIcon(20) : fileIcon(x.name, 20)}<span class="nm">${esc(x.name)}</span>${x.link != null ? `<span class="dim">→ ${esc(x.link)}</span>` : ''}${badges}${d ? `<span class="bdg fsb-st ${cls}">${esc(label)}</span>` : ''}</div></td>
        <td class="n">${x.dir || x.link != null ? '' : bytes(x.size)}</td><td>${x.mtime ? when(x.mtime) : ''}</td></tr>`;
    }).join('') || '<tr><td colspan="4" class="dim">Empty folder</td></tr>';
    q('.arc-list').scrollTop = 0;
    count();
  };
  const fail = err => { if (host.isConnected) q('.arc-note').innerHTML = `<div class="trunc">${esc(err.message || err)}</div>`; };
  // what's chosen, and what the copy button does
  const count = () => {
    const n = st.chosen.size;
    q('.fsb-count').textContent = n ? `${n} chosen` : '';
    q('.fsb-copy').textContent = n ? `Copy ${n === 1 ? 'it' : 'these'} out…` : st.path.length ? 'Copy this folder out…' : 'Copy everything out…';
  };

  host.addEventListener('change', ev => {
    const t = ev.target;
    if (t.classList.contains('fsb-del')) {
      showDeleted = t.checked;
      try { localStorage.setItem('bd.fsdel', showDeleted ? '1' : '0'); } catch { /* only a preference */ }
      return void draw().catch(fail);
    }
    if (t.classList.contains('fsb-all')) {
      for (const x of st.rows) if (x.link == null) t.checked ? st.chosen.set(seg(x), [...st.path, seg(x)].join('/')) : st.chosen.delete(seg(x));
      host.querySelectorAll('[data-ck]').forEach(c => { if (!c.disabled) c.checked = t.checked; });
      return count();
    }
    if (t.dataset.ck != null) {
      const x = st.rows[+t.dataset.ck];
      t.checked ? st.chosen.set(seg(x), [...st.path, seg(x)].join('/')) : st.chosen.delete(seg(x));
      return count();
    }
  });
  host.addEventListener('click', ev => {
    const t = ev.target;
    if (t.closest('.fsb-copy')) return copyForm();
    if (t.closest('[data-ck], .fsb-all, .fsb-ck')) return;
    const c = t.closest('.acrumbs [data-depth]');
    if (c) { st.path = st.path.slice(0, +c.dataset.depth); st.chosen.clear(); return void draw().catch(fail); }
    const th = t.closest('th[data-k]');
    if (th) {
      const k = th.dataset.k;
      st.sort = { key: k, asc: st.sort.key === k ? !st.sort.asc : k === 'name' };
      return void draw().catch(fail);
    }
    const tr = t.closest('tr[data-i]');
    if (!tr) return;
    const x = st.rows[+tr.dataset.i];
    if (x.dir) { st.path.push(seg(x)); st.chosen.clear(); return void draw().catch(fail); }
    host.querySelectorAll('tbody tr').forEach(y => y.classList.toggle('sel', y === tr));
    const pv = q('.arc-pv');
    const why = x.unreadable || (x.link != null ? `A link to ${x.link}. Links are shown, not followed.` : null);
    if (why) { pv.innerHTML = `<div class="v-center"><div class="v-msg"><h3>${esc(x.name)}</h3><p>${esc(why)}</p></div></div>`; return; }
    entryPreview(pv, fsEntryUrl(source, [...st.path, seg(x)]), x.name, x.size, true).then(() => {
      if (x.deleted && pv.isConnected) pv.querySelector('.pv-head')?.insertAdjacentHTML('afterend', `<div class="trunc">Deleted: ${esc(x.deleted.why)}${x.deleted.state === 'recoverable' ? '' : ' Parts other files have taken since show as empty.'}</div>`);
    });
  });

  // copy what's chosen (or this folder) to a folder on another drive, as a rescue the daemon runs
  const copyForm = async () => {
    const box = q('.fsb-go');
    const items = st.chosen.size ? [...st.chosen.values()] : [st.path.join('/')];
    const roots = (await api('roots').catch(() => ({ roots: [] }))).roots || [];
    const base = roots.map(r => r.path)[0] || '';
    const sep = base.includes('\\') && !base.includes('/') ? '\\' : '/';
    const day = new Date().toISOString().slice(0, 10);
    const guess = base ? `${base.replace(/[\\/]+$/, '')}${sep}BackDown Rescue${sep}${rootName.replace(/[\\/:*?"<>|]+/g, '-')} ${day}` : '';
    box.hidden = false;
    box.innerHTML = `<p>Copy ${items.length === 1 && !st.chosen.size ? (st.path.length ? 'this folder' : 'everything') : `${items.length} chosen item${items.length === 1 ? '' : 's'}`} to a folder on another drive. Each copy is read back and checked; nothing there is overwritten.</p>
      <label class="rs-dest">Save in <input class="wp-input fsb-dest" spellcheck="false" autocomplete="off" placeholder="${esc(guess || '/full/path/on/another/drive')}"></label>
      <label class="wp-chk"><input type="checkbox" class="fsb-withdel"${showDeleted ? ' checked' : ''}> With the deleted files in chosen folders</label>
      <div class="rs-next"><button class="btn sm primary fsb-start" type="button">Start copying</button><button class="btn sm fsb-cancel" type="button">Cancel</button></div>`;
    box.querySelector('.fsb-dest').focus();
    box.querySelector('.fsb-cancel').onclick = () => { box.hidden = true; box.innerHTML = ''; };
    box.querySelector('.fsb-start').onclick = async () => {
      const dest = (box.querySelector('.fsb-dest').value || '').trim() || guess;
      if (!/^([A-Za-z]:[\\/]|\/|\\\\)/.test(dest)) return toast('Choose where to put them: a full path on another drive');
      const q2 = { mode: 'files', dest, items: JSON.stringify(items), deleted: box.querySelector('.fsb-withdel').checked ? 1 : 0 };
      if (source.rescue != null) Object.assign(q2, { from: source.rescue, at: source.at }); else Object.assign(q2, { id: source.id, part: source.part });
      const r = await fetch(`/api/rescue?${new URLSearchParams(q2)}`, { method: 'POST', headers: { 'X-BackDown': '1' }, credentials: 'same-origin' }).catch(e => ({ ok: false, json: async () => ({ error: e.message }) }));
      const j = await r.json().catch(() => ({}));
      if (!r.ok || j.error) { box.querySelector('.rs-next').insertAdjacentHTML('beforebegin', `<p class="warn-t">${esc(j.error || r.statusText)}</p>`); return; }
      box.innerHTML = `<p>Copying to <code>${esc(dest)}</code>: it shows in Rescue and in Activity, and the copies open in the browser once they’re indexed.</p>`;
      toast('Copying: it shows in Rescue and in Activity');
      document.dispatchEvent(new CustomEvent('bd:rescue-started', { detail: j }));
    };
  };

  return draw();
}
