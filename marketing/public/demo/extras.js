// Pieces of the browser that don't need its state: the size map's layout, comparing a folder at two
// scans, a popup menu and a toast.

// ---------- size map (squarified treemap: Bruls, Huizing & van Wijk)

// Lay items ({size > 0}) out in rect {x, y, w, h}, largest first, keeping tiles close to square.
// Returns [{item, x, y, w, h}].
export function squarify(items, rect) {
  const total = items.reduce((s, i) => s + i.size, 0);
  if (!total || rect.w <= 0 || rect.h <= 0) return [];
  const scale = (rect.w * rect.h) / total;
  const nodes = items.map(item => ({ item, area: item.size * scale })).sort((a, b) => b.area - a.area);
  const out = [];
  let { x, y, w, h } = rect;
  // the worst aspect ratio in a row laid along a side of this length
  const worst = (row, side) => {
    const s = row.reduce((a, n) => a + n.area, 0);
    let max = 0, min = Infinity;
    for (const n of row) { max = Math.max(max, n.area); min = Math.min(min, n.area); }
    return Math.max((side * side * max) / (s * s), (s * s) / (side * side * min));
  };
  const place = row => {
    const s = row.reduce((a, n) => a + n.area, 0);
    if (w >= h) { // a column down the left edge
      const cw = s / h;
      let cy = y;
      for (const n of row) { const nh = n.area / cw; out.push({ item: n.item, x, y: cy, w: cw, h: nh }); cy += nh; }
      x += cw; w -= cw;
    } else { // a row along the top
      const rh = s / w;
      let cx = x;
      for (const n of row) { const nw = n.area / rh; out.push({ item: n.item, x: cx, y, w: nw, h: rh }); cx += nw; }
      y += rh; h -= rh;
    }
  };
  let row = [];
  for (const n of nodes) {
    const side = Math.min(w, h);
    if (!row.length || worst([...row, n], side) <= worst(row, side)) row.push(n);
    else { place(row); row = [n]; }
  }
  if (row.length) place(row);
  return out;
}

// ---------- one folder at two scans

// Entries of the same folder at scan A and scan B (listing entries, keyed by path key) ->
// [{status: added|removed|changed|same, a, b}]
export function compareListings(a, b) {
  const A = new Map(a.map(e => [e.key, e]));
  const B = new Map(b.map(e => [e.key, e]));
  const rows = [];
  for (const [k, eb] of B) {
    const ea = A.get(k);
    rows.push(ea ? { status: same(ea, eb) ? 'same' : 'changed', a: ea, b: eb } : { status: 'added', a: null, b: eb });
  }
  for (const [k, ea] of A) if (!B.has(k)) rows.push({ status: 'removed', a: ea, b: null });
  return rows;
}

function same(a, b) {
  if (a.kind !== b.kind) return false;
  if (a.kind === 'dir') return a.size === b.size && (a.files ?? 0) === (b.files ?? 0);
  if (a.sha256 && b.sha256) return a.sha256 === b.sha256;
  return a.size === b.size && a.mtime === b.mtime;
}

// ---------- popup menu: [{label (HTML), hint, act, disabled, checked, sep}], arrows / Enter / Esc

let open = null;

export function menu(x, y, items) {
  closeMenu();
  const prev = document.activeElement;
  const el = document.createElement('div');
  el.className = 'menu';
  el.setAttribute('role', 'menu');
  el.innerHTML = items.map((it, i) => it.sep ? '<div class="menu-sep" role="separator"></div>'
    : `<button type="button" role="${it.checked === undefined ? 'menuitem' : 'menuitemcheckbox'}" data-i="${i}"${it.disabled ? ' disabled' : ''}${it.checked === undefined ? '' : ` aria-checked="${!!it.checked}"`}>`
      + `<span class="mi-check" aria-hidden="true"></span><span class="mi-label">${it.label}</span>${it.hint ? `<span class="mi-hint">${it.hint}</span>` : ''}</button>`).join('');
  document.body.append(el);
  const r = el.getBoundingClientRect();
  el.style.left = `${Math.max(4, Math.min(x, innerWidth - r.width - 4))}px`;
  el.style.top = `${Math.max(4, Math.min(y, innerHeight - r.height - 4))}px`;
  const buttons = [...el.querySelectorAll('button:not([disabled])')];
  buttons[0]?.focus();
  el.addEventListener('click', e => {
    const b = e.target.closest('button[data-i]');
    if (!b) return;
    const it = items[+b.dataset.i];
    closeMenu(true);
    it.act?.();
  });
  el.addEventListener('keydown', e => {
    const i = buttons.indexOf(document.activeElement);
    const to = j => { e.preventDefault(); buttons[(j + buttons.length) % buttons.length]?.focus(); };
    if (e.key === 'ArrowDown') to(i + 1);
    else if (e.key === 'ArrowUp') to(i - 1);
    else if (e.key === 'Home') to(0);
    else if (e.key === 'End') to(buttons.length - 1);
    else if (e.key === 'Escape' || e.key === 'Tab') { e.preventDefault(); closeMenu(true); }
    e.stopPropagation();
  });
  open = { el, prev };
}

export function closeMenu(refocus = false) {
  if (!open) return;
  open.el.remove();
  if (refocus && open.prev?.isConnected) open.prev.focus();
  open = null;
}

export const menuOpen = () => !!open;

document.addEventListener('pointerdown', e => { if (open && !open.el.contains(e.target)) closeMenu(); }, true);
addEventListener('blur', () => closeMenu());
addEventListener('resize', () => closeMenu());
document.addEventListener('scroll', () => closeMenu(), true);

// ---------- a short message at the bottom of the window

let toastTimer;
export function toast(msg) {
  let el = document.getElementById('toast');
  if (!el) {
    el = document.createElement('div');
    el.id = 'toast';
    el.setAttribute('role', 'status');
    document.body.append(el);
  }
  el.textContent = msg;
  el.classList.add('on');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => el.classList.remove('on'), 2600);
}
