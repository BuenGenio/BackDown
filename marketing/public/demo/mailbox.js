// Mailboxes as conversations: an mbox file or a Maildir folder, threaded by the server
// (mailbox.rs) from headers alone, a time-boxed slice at a time. A message opens with the same
// renderer as an .eml (viewers.js hands it in), fetched by index id like every other file.
import { api, when } from './app.js';
import { kindOf } from './icons.js';

const esc = s => String(s ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const PAGE = 100;
const num = n => (n ?? 0).toLocaleString();
const plural = (n, one, many = `${one}s`) => `${num(n)} ${n === 1 ? one : many}`;
const DAY = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium' });
const MONTH_DAY = new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric' });
const TIME = new Intl.DateTimeFormat(undefined, { timeStyle: 'short' });

// a list's date: the time for today, the day for this year, else the full date
function short(t) {
  if (!t) return '';
  const d = new Date(t * 1000), now = new Date();
  if (d.toDateString() === now.toDateString()) return TIME.format(d);
  return d.getFullYear() === now.getFullYear() ? MONTH_DAY.format(d) : DAY.format(d);
}

function range(a, b) {
  if (!a && !b) return '';
  const x = DAY.format(new Date((a || b) * 1000)), y = DAY.format(new Date((b || a) * 1000));
  return x === y ? x : `${x} – ${y}`;
}

// "Ann Example <ann@example.org>" -> "Ann Example"
const nameOf = from => (/^\s*"?([^"<]*?)"?\s*<[^>]*>\s*$/.exec(from || '')?.[1] || from || '(unknown sender)');
// a subject without "Re:", "AW:", "Fwd:" …, to tell whether a message's differs from its thread's
const bare = s => String(s || '').replace(/^\s*((re|aw|sv|fwd?|wg|tr|rv|vs|antw|odp|rif|r|i)\s*(\[\d+\]|\(\d+\))?\s*[:：]\s*)+/i, '').trim();

// Maildir flags (or an mbox's status headers) as small markers, with words for screen readers
const MARKS = [['F', 'flag', '⚑', 'flagged'], ['R', 'rep', '↩', 'replied'], ['P', 'fwd', '↪', 'forwarded']];
function marks(flags = '', unread = false) {
  let out = unread ? '<span class="mbx-mk unread" title="Unread" aria-hidden="true"></span><span class="sr">unread,</span>' : '';
  for (const [c, cls, glyph, word] of MARKS) {
    if (flags.includes(c)) out += `<span class="mbx-mk ${cls}" title="${word[0].toUpperCase() + word.slice(1)}" aria-hidden="true">${glyph}</span><span class="sr">${word},</span>`;
  }
  if (flags.includes('D')) out += '<span class="bdg">draft</span>';
  if (flags.includes('T')) out += '<span class="bdg deleted">trashed</span>';
  return out;
}

// Files the UI has no viewer for by name (Thunderbird's "Inbox", a Maildir message's
// "1698765432.M5P6.host:2,S") may be mail: the server looks at their first few KB. Returns the
// extension to treat the file as.
export async function mailExt(e, x) {
  if (kindOf(e.name) !== 'file') return x;
  try {
    const { kind } = await api('mail-kind', { id: e.id });
    return kind === 'mbox' ? 'mbox' : kind === 'message' ? 'eml' : x;
  } catch {
    return x;
  }
}

// A mailbox in `host`: its threads (or its messages one by one), newest activity first, a search
// over senders and subjects, and the chosen thread as a conversation whose messages open in place.
// `src` is { id } (an mbox file) or { dir } (a Maildir folder's key); `render(id, message, at)`
// draws one message; `tools` is the viewer header's tool area, `title` its name.
export async function mailboxInto(host, src, { render, tools, title }) {
  const st = { src: { ...src }, q: '', flat: false, rows: [], total: 0, d: null, key: null, reading: false, stop: false, seq: 0, tseq: 0, msgs: [] };
  host.innerHTML = `<div class="mbx"><nav class="mbx-folders" aria-label="Mail folders" hidden></nav>
    <div class="mbx-side"><div class="mbox-search"><input type="search" placeholder="Search senders and subjects" aria-label="Search senders and subjects" autocomplete="off"></div>
      <div class="mbx-state" role="status"><div class="mbx-note"><span class="mbx-spin" aria-hidden="true"></span><span>Reading the mailbox…</span></div></div>
      <div class="mbx-rows"></div><div class="mbox-more"></div></div>
    <section class="mbx-thread" aria-label="Conversation"></section></div>`;
  const root = host.firstElementChild, $ = s => root.querySelector(s), alive = () => root.isConnected;
  const where = () => (st.src.id != null ? { id: st.src.id } : { dir: st.src.dir });
  const fail = err => { if (alive()) $('.mbx-state').innerHTML = `<div class="trunc">${esc(err.message || err)}</div>`; };

  const drawTools = () => {
    const d = st.d;
    if (!d || !tools) return;
    const what = d.kind === 'mbox' ? 'mbox' : 'Maildir';
    tools.innerHTML = `<span class="v-meta">${what} · ${plural(d.messages, 'message')}${d.done ? '' : '+'}</span>
      <div class="mbx-mode" role="group" aria-label="List by"><button class="btn sm" type="button" data-mode="threads" aria-pressed="${!st.flat}">Threads</button><button class="btn sm" type="button" data-mode="flat" aria-pressed="${st.flat}">Messages</button></div>`;
    tools.querySelectorAll('[data-mode]').forEach(b => b.onclick = () => {
      const flat = b.dataset.mode === 'flat';
      if (flat === st.flat) return;
      st.flat = flat; st.key = null;
      load({ open: true }).catch(fail);
    });
  };

  const people = t => (t.who.length ? t.who.join(', ') + (t.people > t.who.length ? ` +${t.people - t.who.length}` : '') : '(unknown sender)');
  const row = t => `<button type="button" class="mbx-row${t.unread ? ' unread' : ''}" data-key="${t.key}" aria-current="${t.key === st.key}">
      <span class="mbx-l1"><span class="mbx-subj">${esc(t.subject || '(no subject)')}</span>${t.count > 1 ? `<span class="mbx-n" title="${plural(t.count, 'message')}"><span class="sr">,</span>${num(t.count)}<span class="sr"> messages,</span></span>` : ''}</span>
      <span class="mbx-l2">${marks(t.flags, t.unread > 0)}<span class="mbx-who">${esc(people(t))}</span><span class="mbx-when" title="${esc(range(t.first, t.last))}">${short(t.last)}</span></span></button>`;

  const drawRows = () => {
    $('.mbx-rows').innerHTML = st.rows.length ? st.rows.map(row).join('')
      : `<p class="note pad">${st.q ? 'Nothing matches in what has been read.' : st.d?.done ? 'No messages in this mailbox.' : 'No messages found yet.'}</p>`;
  };

  const drawState = () => {
    const d = st.d;
    if (!d) return;
    const pct = Math.min(99, Math.floor((100 * d.read) / Math.max(1, d.size)));
    const what = d.kind === 'mbox' ? `${pct}% of the file` : `the newest ${num(d.read)} of ${plural(d.size, 'message')}`;
    let html = '';
    if (st.reading) {
      html = `<div class="mbx-note"><span class="mbx-spin" aria-hidden="true"></span><span>Reading… ${what} so far.</span><button class="btn sm" type="button" data-act="stop">Stop</button></div>`;
    } else if (d.capped) {
      html = `<div class="trunc">A very large mailbox: only its first ${num(d.messages)} messages are threaded.</div>`;
    } else if (!d.done) {
      html = `<div class="trunc">Partial list: BackDown has read ${what}${d.kind === 'mbox' ? ` (${plural(d.messages, 'message')})` : ''}, so threads may go on further in${st.q ? ' and the search covers only that' : ''}.
        <button class="btn sm" type="button" data-act="read">Keep reading</button></div>`;
    }
    if (d.unreadable) html += `<div class="trunc">${plural(d.unreadable, 'message')} couldn’t be read: their bytes aren’t on any indexed drive now.</div>`;
    $('.mbx-state').innerHTML = html;
    const more = st.rows.length < st.total;
    $('.mbox-more').innerHTML = `<span class="v-meta">${more ? `${num(st.rows.length)} of ` : ''}${plural(st.total, st.flat ? 'message' : 'thread')}${st.q ? (st.total === 1 ? ' matches' : ' match') : ''}</span>
      ${more ? '<button class="btn sm" type="button" data-act="page">Show more</button>' : ''}`;
  };

  const drawFolders = d => {
    if (!d.folders) return;
    const nav = $('.mbx-folders');
    nav.hidden = d.folders.length < 2;
    root.classList.toggle('with-folders', !nav.hidden);
    nav.innerHTML = `<div class="side-sec">Folders</div>${d.folders.map(f => `<button type="button" class="mbx-folder" data-dir="${esc(f.key)}" aria-current="${f.here}">
      <span class="n">${esc(f.name)}</span><span class="c">${f.new ? `${num(f.new)} new · ` : ''}${plural(f.count, 'message')}</span></button>`).join('')}`;
    const here = d.folders.find(f => f.here);
    if (title && here && d.folders.length > 1) title.textContent = `${d.name} · ${here.name}`;
  };

  // the list from its start: after a search, a switch of folder or mode, or reading further
  // (then as many rows as were shown)
  async function load({ more = false, keep = false, open = false } = {}) {
    const seq = ++st.seq;
    const limit = keep ? Math.max(PAGE, st.rows.length) : PAGE;
    const d = await api('threads', { ...where(), q: st.q, flat: st.flat ? 1 : '', limit, more: more ? 1 : '' });
    if (seq !== st.seq || !alive()) return null;
    Object.assign(st, { d, rows: d.threads, total: d.total });
    drawRows(); drawState(); drawFolders(d); drawTools();
    if (open) {
      if (st.rows.length) await openThread(st.rows[0].key);
      else $('.mbx-thread').innerHTML = '';
    }
    return d;
  }

  async function page() {
    const seq = st.seq;
    const d = await api('threads', { ...where(), q: st.q, flat: st.flat ? 1 : '', offset: st.rows.length, limit: PAGE });
    if (seq !== st.seq || !alive()) return;
    st.rows.push(...d.threads);
    st.total = d.total;
    drawRows(); drawState();
  }

  async function keepReading() {
    if (st.reading) return;
    Object.assign(st, { reading: true, stop: false });
    drawState();
    try {
      while (!st.stop && alive()) {
        const d = await load({ more: true, keep: true });
        if (!d || d.done || d.capped) break;
      }
    } finally {
      st.reading = false;
      if (alive()) drawState();
    }
  }

  // one thread as a conversation: replies indented under what they answer, each message's
  // header a button that opens the whole message in place
  async function openThread(key) {
    st.key = key;
    root.querySelectorAll('.mbx-row').forEach(b => b.setAttribute('aria-current', String(+b.dataset.key === key)));
    const pane = $('.mbx-thread'), seq = ++st.tseq;
    pane.innerHTML = '<div class="v-center"><p class="note">Loading…</p></div>';
    const d = await api('thread', { ...where(), key, q: st.q, flat: st.flat ? 1 : '' });
    if (seq !== st.tseq || !alive()) return;
    const t = d.thread, msgs = (st.msgs = d.messages), subject = t.subject || '(no subject)';
    pane.innerHTML = `<header class="mbx-head"><h3>${esc(subject)}</h3>
        <div class="m">${plural(t.count, 'message')} · ${esc(d.who.join(', ') || '(unknown sender)')}${t.first || t.last ? ` · ${range(t.first, t.last)}` : ''}</div>
        ${msgs.length > 1 ? '<div class="mbx-acts"><button class="btn sm" type="button" data-act="all">Expand all</button></div>' : ''}</header>
      ${t.count > msgs.length ? `<div class="trunc">Showing the first ${num(msgs.length)} of ${plural(t.count, 'message')}.</div>` : ''}
      <ol class="mbx-conv">${msgs.map((m, i) => `<li class="mbx-msg${m.unread ? ' unread' : ''}${m.hit ? ' hit' : ''}${m.flags.includes('T') ? ' trashed' : ''}" style="--d:${Math.min(m.depth, 10)}">
        <button type="button" class="mbx-mh" aria-expanded="false" aria-controls="mbx-b${seq}-${i}" data-i="${i}">
          <span class="mbx-from">${esc(nameOf(m.from))}</span>${marks(m.flags, m.unread)}${m.dup ? '<span class="bdg" title="Another copy of a message that is already here">copy</span>' : ''}
          <span class="mbx-sub">${bare(m.subject) !== bare(t.subject) ? esc(m.subject) : ''}</span><span class="mbx-when">${m.date ? esc(when(m.date)) : ''}</span></button>
        ${m.orphan && i === 0 ? '<div class="mbx-orphan">In reply to a message that isn’t in this mailbox</div>' : ''}
        <div class="mbx-body" id="mbx-b${seq}-${i}" hidden></div></li>`).join('')}</ol>`;
    pane.scrollTop = 0;
    const items = [...pane.querySelectorAll('.mbx-msg')];
    // open the search hits, or else the newest message
    let first = items.filter((_, i) => msgs[i].hit).slice(0, 5);
    if (!first.length) {
      let n = 0;
      msgs.forEach((m, i) => { if ((m.date ?? 0) >= (msgs[n].date ?? 0)) n = i; });
      first = [items[n]];
    }
    first.forEach(li => toggle(li, true));
    if (first[0] !== items[0]) first[0].scrollIntoView({ block: 'nearest' });
  }

  async function toggle(li, open) {
    const btn = li.querySelector('.mbx-mh'), body = li.querySelector('.mbx-body');
    open ??= btn.getAttribute('aria-expanded') !== 'true';
    btn.setAttribute('aria-expanded', String(open));
    body.hidden = !open;
    if (!open || body.dataset.loaded) return;
    body.dataset.loaded = '1';
    body.innerHTML = '<p class="note pad">Loading…</p>';
    const m = st.msgs[+btn.dataset.i], mbox = st.src.id != null;
    try {
      const mail = await api('mail', mbox ? { id: st.src.id, at: m.key } : { id: m.key });
      if (body.isConnected) body.innerHTML = mbox ? render(st.src.id, mail, m.key) : render(m.key, mail, null);
    } catch (err) {
      delete body.dataset.loaded;
      if (body.isConnected) body.innerHTML = `<p class="note pad">${esc(err.message || err)}</p>`;
    }
  }

  root.addEventListener('click', ev => {
    const t = ev.target;
    const act = t.closest('[data-act]')?.dataset.act;
    if (act === 'read') return void keepReading().catch(fail);
    if (act === 'stop') { st.stop = true; return; }
    if (act === 'page') return void page().catch(fail);
    if (act === 'all') {
      const items = [...root.querySelectorAll('.mbx-msg')];
      const open = items.some(li => li.querySelector('.mbx-mh').getAttribute('aria-expanded') !== 'true');
      items.forEach(li => toggle(li, open));
      t.closest('[data-act]').textContent = open ? 'Collapse all' : 'Expand all';
      return;
    }
    const r = t.closest('.mbx-row');
    if (r) return void openThread(+r.dataset.key).catch(fail);
    const h = t.closest('.mbx-mh');
    if (h) return void toggle(h.closest('.mbx-msg'));
    const f = t.closest('.mbx-folder');
    if (f && f.getAttribute('aria-current') !== 'true') {
      st.src = { dir: f.dataset.dir };
      st.key = null;
      load({ open: true }).catch(fail);
    }
  });

  // arrows move through a list (the threads also open as they're reached), Home/End to its ends
  let timer;
  root.addEventListener('keydown', ev => {
    const t = ev.target;
    if (t.matches('input[type=search]')) {
      if (ev.key === 'ArrowDown' || ev.key === 'Enter') {
        const first = $('.mbx-row');
        if (first) { ev.preventDefault(); first.focus(); if (ev.key === 'Enter') first.click(); }
      }
      return;
    }
    const el = t.closest('.mbx-row, .mbx-mh, .mbx-folder');
    if (!el || !['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(ev.key)) return;
    const cls = ['mbx-row', 'mbx-mh', 'mbx-folder'].find(c => el.classList.contains(c));
    const all = [...root.querySelectorAll(`.${cls}`)], i = all.indexOf(el);
    const j = ev.key === 'Home' ? 0 : ev.key === 'End' ? all.length - 1 : i + (ev.key === 'ArrowDown' ? 1 : -1);
    const next = all[Math.max(0, Math.min(all.length - 1, j))];
    ev.preventDefault();
    if (!next || next === el) return;
    next.focus();
    if (cls === 'mbx-row') {
      clearTimeout(timer);
      timer = setTimeout(() => { if (document.activeElement === next) openThread(+next.dataset.key).catch(fail); }, 180);
    }
  });

  let qTimer;
  $('.mbox-search input').addEventListener('input', ev => {
    clearTimeout(qTimer);
    qTimer = setTimeout(() => { st.q = ev.target.value.trim(); load().catch(fail); }, 300);
  });

  await load({ open: true });
}
