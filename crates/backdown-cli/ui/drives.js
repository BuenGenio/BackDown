// Drives, beyond the index: mounting read-only (Activity's "Connected drives", and disk images
// whose filesystems BackDown doesn't read itself), undoing only what BackDown mounted, letting
// BackDown read a drive without a terminal (Rescue, Linux), browsing a filesystem Rescue found, and
// how a "copy files out" rescue went. The server picks every source itself (drives.rs): a device
// the probe lists, an image by its index id, a found filesystem by its Rescue record.
import { api, bytes } from './app.js';
import { toast } from './extras.js';
import { showPanel } from './viewers.js';
import { fsBrowser } from './fsbrowse.js';

const $ = s => document.querySelector(s);
const esc = s => String(s ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const count = n => n >= 1e6 ? (n / 1e6).toFixed(2) + ' M' : n >= 1e3 ? (n / 1e3).toFixed(1) + ' K' : String(n);

// what BackDown mounted (and so can undo), whether this BackDown may mount at all, and what's
// happening now
export const MNT = { list: [], can: false, busy: new Set(), msg: null };

export async function loadMounts() {
  try { const r = await api('mounts'); MNT.list = r.mounts || []; MNT.can = !!r.can; } catch { /* not this BackDown's to say */ }
}

async function post(path, params) {
  const r = await fetch(`/api/${path}?${new URLSearchParams(params)}`, { method: 'POST', headers: { 'X-BackDown': '1' }, credentials: 'same-origin' })
    .catch(e => ({ ok: false, statusText: e.message, json: async () => ({ error: e.message }) }));
  const j = await r.json().catch(() => ({ error: r.statusText }));
  if (!r.ok || j.error) throw Object.assign(new Error(j.error || r.statusText), { command: j.command });
  return j;
}

// Activity redraws (and, after a change, looks at the drives again)
const changed = reprobe => document.dispatchEvent(new CustomEvent('bd:drives', { detail: { reprobe } }));
const ours = dev => MNT.list.find(m => m.source === dev || m.mounts.some(([d]) => d === dev));

// a volume's mount action in Connected drives: mount a mountable one read-only, or undo what
// BackDown mounted
export function mountCell(vol) {
  if (MNT.busy.has(vol.device)) return `<button class="btn sm" type="button" disabled>${vol.state === 'mountable' ? 'Mounting…' : 'Unmounting…'}</button>`;
  const m = ours(vol.device);
  if (m) return `<button class="btn sm" type="button" data-unmount="${esc(m.source)}" data-busy="${esc(vol.device)}" title="Mounted read-only by BackDown">${m.attached ? 'Detach' : 'Unmount'}</button>`;
  if (vol.state === 'mountable' && MNT.can) return `<button class="btn sm" type="button" data-mount-dev="${esc(vol.device)}" title="Mount it read-only: nothing on it can be changed">Mount read-only</button>`;
  return '';
}

// what the last mount or unmount had to say (a password needed: the command to run instead)
export function mountNotice() {
  const m = MNT.msg;
  if (!m) return '';
  return `<div class="rs-acc no mnt-msg" role="status"><p><span class="dot o"></span>${esc(m.error)}</p>
    ${m.command ? `<div class="rs-cmd"><code>${esc(m.command)}</code><button class="btn sm" type="button" data-mnt-copy="${esc(m.command)}">Copy</button></div>` : ''}
    <button class="btn sm" type="button" data-mnt-dismiss>OK</button></div>`;
}

// ---- disk images: what the system can mount when BackDown can't read it itself

const NOT_MOUNTABLE = /LUKS|BitLocker|encrypt|swap|LVM|RAID|member|ZFS|Storage Spaces/i;

// In the disk-image viewer: when a filesystem in it is one BackDown doesn't read (XFS, btrfs,
// HFS+, APFS…), offer to attach the image read-only and mount it, then index it; and to detach it.
export async function mountOffer(host, e, d, browsable) {
  if (!host) return;
  const kinds = [d.fs, ...(d.partitions || []).map(p => p.fs)].filter(fs => fs && !browsable(fs) && !NOT_MOUNTABLE.test(fs.kind));
  if (!kinds.length) return;
  let st;
  try { st = await api('mounts', { id: e.id }); } catch { return; }
  if (!host.isConnected || !st.images) return;
  const names = [...new Set(kinds.map(k => k.kind))].join(', ');
  const draw = (m, msg = null) => {
    host.innerHTML = m
      ? `<h4>Mounted read-only</h4><ul class="pv-list">${m.mounts.map(([dev, at]) => `<li><code>${esc(at)}</code> <span class="muted">${esc(dev)}</span>
          <button class="btn sm" type="button" data-index-path="${esc(at)}">Index this</button></li>`).join('')}</ul>
        <p class="note">Nothing on it can be changed. Detach it when you’re done: that unmounts it and removes the attached copy.</p>
        <button class="btn sm" type="button" data-img-detach>Detach</button>`
      : `<h4>Open it with this computer’s own reader</h4>
        <p class="note">BackDown reads FAT, exFAT, ext and NTFS itself; this image has ${esc(names)}. This computer can mount it read-only instead (nothing on it can be changed), then it can be indexed and browsed.</p>
        <button class="btn sm" type="button" data-img-attach>Mount read-only</button>`;
    if (msg) host.insertAdjacentHTML('beforeend', `<div class="rs-acc no"><p>${esc(msg.error)}</p>${msg.command ? `<div class="rs-cmd"><code>${esc(msg.command)}</code><button class="btn sm" type="button" data-mnt-copy="${esc(msg.command)}">Copy</button></div>` : ''}</div>`);
  };
  draw(st.image);
  host.onclick = async ev => {
    const a = ev.target.closest('[data-img-attach], [data-img-detach]');
    if (!a) return;
    const attach = a.matches('[data-img-attach]');
    a.disabled = true; a.textContent = attach ? 'Mounting…' : 'Detaching…';
    try {
      const j = await post(attach ? 'attach' : 'detach', { id: e.id });
      toast(attach ? `Mounted read-only${j.mounts?.length ? ` at ${j.mounts.map(m => m[1]).join(', ')}` : ''}` : 'Detached');
      await loadMounts();
      const now = await api('mounts', { id: e.id }).catch(() => ({}));
      draw(now.image);
      changed(true);
    } catch (err) { draw(attach ? null : st.image, { error: err.message, command: err.command }); }
  };
}

// ---- Rescue: reading a drive without a terminal (udisks), and browsing a found filesystem

// in the access box, when BackDown can't read a drive: ask the system (a password dialog)
export function allowButton(a) {
  if (!a?.allow) return '';
  return `<p class="muted">Or let the system open it for BackDown, read-only, until BackDown stops (it asks for an administrator’s password):</p>
    <button class="btn sm primary" type="button" data-rs-allow="${esc(a.path)}">Allow BackDown to read this drive</button>`;
}

async function allow(btn) {
  btn.disabled = true;
  const was = btn.textContent;
  btn.textContent = 'Waiting for the password dialog…';
  btn.parentElement.querySelector('.allow-err')?.remove();
  try {
    const a = await post('rescue-allow', { path: btn.dataset.rsAllow });
    toast('BackDown can read it now, until it stops');
    document.dispatchEvent(new CustomEvent('bd:access', { detail: a }));
  } catch (err) {
    btn.disabled = false; btn.textContent = was;
    btn.insertAdjacentHTML('afterend', `<p class="warn-t allow-err">${esc(err.message)}. The command above still works.</p>`);
  }
}

const FSREAD = /^(FAT(12|16|32)?|exFAT|ext[234]|NTFS)$/;

// "Browse files" for a filesystem a partition search found (or the table lists), when BackDown
// reads that kind and found it by its own first structure
export function browseFoundBtn(r, f) {
  if (!FSREAD.test(f.fs) || /^(backup|alternate)/.test(f.via || '')) return '';
  const label = `${f.fs}${f.label ? ` “${f.label}”` : ''} at ${bytes(f.start)}`;
  return `<button class="btn sm" type="button" data-rs-browse='${esc(JSON.stringify({ rescue: r.id, at: f.start, label }))}'>Browse files</button>`;
}

export function browseFound({ rescue, at, label }) {
  showPanel(label, 'found by Rescue · read-only');
  $('#vbody').innerHTML = '<div class="arc-wrap"><div class="arc-host"></div></div>';
  fsBrowser($('#vbody .arc-host'), { source: { rescue, at }, rootName: label })
    .catch(err => { const h = $('#vbody .arc-host'); if (h) h.innerHTML = `<p class="note pad">${esc(err.message || err)}</p>`; });
}

// what a "copy files out" rescue did, for its card
export function filesBody(r, job) {
  const res = r.result || {}, o = r.options || {};
  const n = o.items?.length || 0;
  let b = `<p class="muted">From the ${esc(o.what || 'file system')} at ${bytes(o.start || 0)}: ${n === 1 && !o.items[0] ? 'everything' : `${count(n)} item${n === 1 ? '' : 's'} chosen`}${o.with_deleted ? ', deleted files included' : ''}.</p>`;
  if (res.files != null) {
    b += `<p><b>${count(res.files)}</b> file${res.files === 1 ? '' : 's'} copied and checked · ${bytes(res.bytes || 0)}${res.deleted ? ` · ${count(res.deleted)} of them deleted ones` : ''}${res.partly ? `, <span class="warn-t">${count(res.partly)} partly overwritten</span> (those parts come back empty)` : ''}${res.renamed ? ` · ${count(res.renamed)} saved under a new name, not over what was there` : ''}${res.failed ? ` · <span class="warn-t">${count(res.failed)} couldn’t be copied</span>` : ''}</p>`;
    if (res.problems?.length) b += `<details class="fsb-problems"><summary>What couldn’t be copied</summary><ul>${res.problems.slice(0, 50).map(p => `<li>${esc(p)}</li>`).join('')}</ul></details>`;
  }
  const halted = !job && (r.state === 'stopped' || r.state === 'running' || r.state === 'new');
  if (!job && r.dest && (res.files || halted)) b += `<div class="rs-next">${res.files ? `<button class="btn sm primary" type="button" data-rs-open="${esc(r.dest)}">Open the copies</button>` : ''}
    ${halted ? `<button class="btn sm" type="button" data-rs-next='${esc(JSON.stringify({ resume: r.id }))}'>Carry on from where it stopped</button>` : ''}</div>`;
  return b;
}

document.addEventListener('click', async e => {
  const t = e.target;
  const md = t.closest('[data-mount-dev]');
  if (md) {
    const dev = md.dataset.mountDev;
    MNT.busy.add(dev); MNT.msg = null; changed(false);
    try { const j = await post('mount', { device: dev }); toast(`Mounted read-only at ${j.mounted}`); }
    catch (err) { MNT.msg = { error: err.message, command: err.command }; }
    MNT.busy.delete(dev);
    await loadMounts();
    return changed(true);
  }
  const um = t.closest('[data-unmount]');
  if (um) {
    const dev = um.dataset.busy;
    MNT.busy.add(dev); MNT.msg = null; changed(false);
    try { await post('unmount', { target: um.dataset.unmount }); toast('Unmounted'); }
    catch (err) { MNT.msg = { error: err.message, command: err.command }; }
    MNT.busy.delete(dev);
    await loadMounts();
    return changed(true);
  }
  const cp = t.closest('[data-mnt-copy]');
  if (cp) {
    try { await navigator.clipboard.writeText(cp.dataset.mntCopy); toast('Copied'); } catch { toast('Select the command and copy it'); }
    return;
  }
  if (t.closest('[data-mnt-dismiss]')) { MNT.msg = null; return changed(false); }
  const al = t.closest('[data-rs-allow]');
  if (al) return allow(al);
  const br = t.closest('[data-rs-browse]');
  if (br) return browseFound(JSON.parse(br.dataset.rsBrowse));
});
