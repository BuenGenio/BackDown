// Early-access forms post in place; without JS they fall back to a normal post and the worker redirects to #joined.
// Messages come from the form's data-* attributes so each language page carries its own.
document.querySelectorAll('form.join').forEach(form => {
  const msg = form.parentElement.querySelector('.join-msg');
  const btn = form.querySelector('button');
  const say = (cls, text) => { msg.className = `join-msg ${cls}`; msg.textContent = text; };
  form.addEventListener('submit', async e => {
    e.preventDefault();
    btn.disabled = true;
    say('', form.dataset.sending);
    try {
      const r = await fetch(form.action, { method: 'POST', body: new FormData(form), headers: { Accept: 'application/json' } });
      if (r.ok) { say('ok', form.dataset.ok); form.reset(); track('Early access', 'join'); }
      else say('err', r.status === 422 ? form.dataset.invalid : form.dataset.error);
    } catch {
      say('err', form.dataset.offline);
    } finally {
      btn.disabled = false;
    }
  });
});

// Narrow screens: the tabs row scrolls when the page names don't fit. Keep the current page's tab in view, and fade
// the edge where there's more.
const tabs = document.querySelector('.hdr nav.tabs');
if (tabs) {
  const edges = () => {
    const over = tabs.scrollWidth - tabs.clientWidth > 1;
    tabs.classList.toggle('more-l', over && tabs.scrollLeft > 1);
    tabs.classList.toggle('more-r', over && tabs.scrollLeft < tabs.scrollWidth - tabs.clientWidth - 1);
  };
  const cur = tabs.querySelector('[aria-current=page]');
  if (cur && tabs.scrollWidth > tabs.clientWidth) {
    tabs.scrollLeft += cur.getBoundingClientRect().left - tabs.getBoundingClientRect().left - (tabs.clientWidth - cur.offsetWidth) / 2;
  }
  tabs.addEventListener('scroll', edges, { passive: true });
  addEventListener('resize', edges);
  edges();
}

// Language menu: close on outside click or Escape.
const langMenu = document.querySelector('details.lang');
if (langMenu) {
  document.addEventListener('click', e => { if (!langMenu.contains(e.target)) langMenu.open = false; });
  document.addEventListener('keydown', e => { if (e.key === 'Escape' && langMenu.open) { langMenu.open = false; langMenu.querySelector('summary').focus(); } });
}

// Product screens: the track scroll-snaps on its own (swipe/scroll works without JS); this adds arrows, dots and
// autoplay. Autoplay pauses on hover/focus, off-screen or in a hidden tab, stops for good once someone takes over,
// and never starts under prefers-reduced-motion.
const show = document.querySelector('.showcase');
if (show) {
  const track = show.querySelector('.track'), slides = [...track.children], ctl = show.querySelector('.show-ctl');
  const dots = [...ctl.querySelectorAll('.sdots button')], play = ctl.querySelector('.splay');
  const reduce = matchMedia('(prefers-reduced-motion: reduce)').matches;
  let i = 0, timer, stopped = reduce, held = false, visible = false;
  const mark = () => dots.forEach((d, k) => d.setAttribute('aria-current', String(k === i)));
  const go = n => { i = (n + slides.length) % slides.length; mark(); track.scrollTo({ left: slides[i].offsetLeft, behavior: reduce ? 'auto' : 'smooth' }); };
  const schedule = () => { clearTimeout(timer); if (!stopped && !held && visible && !document.hidden) timer = setTimeout(() => { go(i + 1); schedule(); }, 6000); };
  const setStopped = v => { stopped = v; play.setAttribute('aria-pressed', String(v)); play.setAttribute('aria-label', v ? play.dataset.play : play.dataset.pause); schedule(); };
  const hold = v => () => { held = v; schedule(); };

  ctl.hidden = false;
  track.addEventListener('scroll', () => { const n = Math.round(track.scrollLeft / track.clientWidth); if (n !== i) { i = n; mark(); } }, { passive: true });
  // arrow buttons under the slideshow and the click halves over each screenshot
  show.addEventListener('click', e => { const b = e.target.closest('[data-step]'); if (b) { setStopped(true); go(i + Number(b.dataset.step)); } });
  dots.forEach((d, k) => d.addEventListener('click', () => { setStopped(true); go(k); }));
  play.addEventListener('click', () => setStopped(!stopped));
  track.addEventListener('pointerdown', () => setStopped(true));
  // ← / → anywhere on the page while the slideshow is on screen, unless the visitor is typing or using a modifier
  document.addEventListener('keydown', e => {
    if ((e.key !== 'ArrowRight' && e.key !== 'ArrowLeft') || !visible || e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return;
    if (e.target.closest('input, textarea, select, [contenteditable]')) return;
    e.preventDefault(); setStopped(true); go(i + (e.key === 'ArrowRight' ? 1 : -1));
  });
  show.addEventListener('mouseenter', hold(true)); show.addEventListener('mouseleave', hold(false));
  // per the WAI-ARIA carousel pattern, keyboard focus inside stops rotation until the play button restarts it
  show.addEventListener('focusin', e => { if (e.target !== play) setStopped(true); });
  document.addEventListener('visibilitychange', schedule);
  new IntersectionObserver(([e]) => { visible = e.isIntersecting; schedule(); }, { threshold: 0.4 }).observe(show);
  mark(); setStopped(stopped);
}

// Features: each tile is an icon and a name, and turns over to what the feature does. It turns in chunks: the first
// time, copies of both sides are cut into rows of uneven pieces (the way BackDown cuts a file into chunks), and the
// pieces flip, lifting towards the viewer, in a wave that spreads from where the pointer came in, then back from
// where it left. The copies only show while it turns; at rest the tile is its own text again, selectable and sharp.
// Mouse: hover. Touch: tap (one tile open at a time). Keyboard: focus, Enter or Space, Escape. Under
// prefers-reduced-motion the sides cross-fade instead; without JS, hover and focus fade the description in (site.css).
const feats = document.querySelector('.feats');
if (feats) {
  const still = matchMedia('(prefers-reduced-motion: reduce)');
  const LIFT = 46, SPEED = 0.8, EASE = 'cubic-bezier(.5,.05,.2,1)';  // px towards the viewer mid-turn; px/ms the wave spreads
  const state = new Map();
  const pose = (c, a, z) => `translateZ(${z}px) rotate3d(${c.ax},${c.ay},0,${a}deg)`;
  const shade = a => Math.abs(Math.sin(a * Math.PI / 180)).toFixed(3);  // darkest edge-on
  const sync = () => feats.classList.toggle('still', still.matches);
  feats.classList.add('fx'); sync(); still.addEventListener?.('change', sync);

  // the pieces, cut to the tile's current size; cut again when that has changed. Each piece is two flat cards, its
  // front and its back, turning together half a turn apart: whichever faces away is hidden (backface-visibility), so
  // no two sides share a plane (where some browsers would draw slivers of both)
  function cut(tile) {
    const box = tile.getBoundingClientRect(), old = state.get(tile);
    const W = box.width - tile.clientLeft * 2, H = box.height - tile.clientTop * 2;  // fractional, so the copies wrap as the tile does
    if (old?.layer && old.W === W && old.H === H) return old;
    old?.layer?.remove();
    const face = tile.querySelector(':scope > .feat-face'), back = tile.querySelector(':scope > .feat-back');
    const r = Math.max(0, parseFloat(getComputedStyle(tile).borderTopLeftRadius) - tile.clientTop) + 'px';
    const layer = document.createElement('div');
    layer.className = 'chunks'; layer.setAttribute('aria-hidden', 'true'); layer.inert = true;
    let seed = [...tile.dataset.feat].reduce((h, ch) => (h * 31 + ch.charCodeAt(0)) % 2147483647, 11);
    const rnd = () => (seed = seed * 16807 % 2147483647) / 2147483647;
    const rows = Math.max(4, Math.round(H / 46)), list = [], at = old?.open ? 180 : 0;
    for (let row = 0; row < rows; row++) {
      // whole pixels, each piece 1px over its right and lower neighbours, so no seam shows while they lie flat
      const y = Math.round(row * H / rows), rh = row === rows - 1 ? H - y : Math.round((row + 1) * H / rows) - y;
      for (let x = 0; x < W - 0.5;) {
        let w = Math.round(34 + rnd() * 54);
        if (W - x - w < 30) w = W - x;
        const last = x + w >= W - 0.5, top = row === 0, bottom = row === rows - 1, left = x === 0;
        const [f, b] = [face, back].map(src => {
          const el = document.createElement('div'), copy = src.cloneNode(true);
          el.className = src === back ? 'ch ch-b' : 'ch';
          el.style.cssText = `left:${x}px;top:${y}px;width:${w + (last ? 0 : 1)}px;height:${rh + (bottom ? 0 : 1)}px`;
          copy.style.cssText = `left:${-x}px;top:${-y}px;width:${W}px;height:${H}px`;
          el.append(copy, document.createElement('i'));
          // the tile's rounded corners
          if (top && left) el.style.borderTopLeftRadius = r;
          if (top && last) el.style.borderTopRightRadius = r;
          if (bottom && left) el.style.borderBottomLeftRadius = r;
          if (bottom && last) el.style.borderBottomRightRadius = r;
          return el;
        });
        const c = { f, b, cx: x + w / 2, cy: y + rh / 2, ax: 0, ay: 1, angle: at };
        place(c, at);
        list.push(c);
        x += w;
      }
    }
    layer.append(...list.flatMap(c => [c.f, c.b]));
    tile.append(layer);
    const s = { W, H, layer, list, open: !!old?.open, run: 0 };
    state.set(tile, s);
    return s;
  }
  const place = (c, a) => { c.f.style.transform = pose(c, a, 0); c.b.style.transform = pose(c, a - 180, 0); };

  // where a piece is now: part-way through a turn, or at rest
  const angleOf = c => {
    const p = c.anims?.[0].effect.getComputedTiming().progress;
    return p == null ? c.angle : c.from + (c.to - c.from) * p;
  };
  const halt = c => { c.anims?.forEach(a => a.cancel()); c.anims = null; };

  function turn(tile, open, ox, oy) {
    if (tile.classList.contains('open') === open) return;
    tile.classList.toggle('open', open);
    if (still.matches) { state.set(tile, { ...state.get(tile), open }); return; }
    const s = cut(tile), run = ++s.run;
    s.open = open;
    // at rest, every piece is aimed afresh, to tip away from the pointer
    if (s.list.every(c => !c.anims)) for (const c of s.list) {
      const dx = c.cx - ox, dy = c.cy - oy, d = Math.hypot(dx, dy);
      [c.ax, c.ay] = d < 1 ? [0, 1] : [(-dy / d).toFixed(4), (dx / d).toFixed(4)];
      place(c, c.angle);
    }
    tile.classList.add('turning');
    let left = 0;
    const done = () => { if (--left === 0 && run === s.run) tile.classList.remove('turning'); };
    for (const c of s.list) {
      const moving = !!c.anims, from = angleOf(c), want = open ? 180 : 0;
      halt(c);
      // from rest, on over (always the same way round, away from the pointer); mid-turn, back the shortest way
      const to = moving ? want + 360 * Math.round((from - want) / 360) : from + 180;
      const span = Math.abs(to - from);
      if (span < 1) { c.angle = ((to % 360) + 360) % 360; place(c, c.angle); continue; }
      const timing = { duration: (open ? 640 : 520) * Math.max(span / 180, 0.4), delay: Math.hypot(c.cx - ox, c.cy - oy) / SPEED,
                       easing: EASE, fill: 'both' };
      const mid = (from + to) / 2, z = LIFT * span / 180;
      const moves = (el, k) => el.animate([{ transform: pose(c, from + k, 0) }, { transform: pose(c, mid + k, z) }, { transform: pose(c, to + k, 0) }], timing);
      const light = el => el.animate([{ opacity: shade(from) }, { opacity: shade(mid) }, { opacity: shade(to) }], timing);
      c.from = from; c.to = to; left++;
      c.anims = [moves(c.f, 0), moves(c.b, -180), light(c.f.lastChild), light(c.b.lastChild)];
      c.anims[0].onfinish = () => {
        c.angle = ((to % 360) + 360) % 360;
        place(c, c.angle);
        halt(c); done();
      };
    }
    if (!left) tile.classList.remove('turning');
  }

  let touch = false, pressed = 0;
  feats.addEventListener('pointerdown', e => { touch = e.pointerType !== 'mouse'; pressed = performance.now(); });
  for (const tile of feats.querySelectorAll('.feat')) {
    const at = e => { const b = tile.getBoundingClientRect(); return [e.clientX - b.left - tile.clientLeft, e.clientY - b.top - tile.clientTop]; };
    let intent;
    // a pointer only passing over, on its way somewhere else, doesn't set every tile turning
    tile.addEventListener('pointerenter', e => {
      if (e.pointerType !== 'mouse') return;
      const p = at(e); clearTimeout(intent); intent = setTimeout(() => turn(tile, true, ...p), 70);
    });
    tile.addEventListener('pointerleave', e => {
      if (e.pointerType !== 'mouse') return;
      clearTimeout(intent);
      if (!tile.matches(':focus-visible')) turn(tile, false, ...at(e));
    });
    tile.addEventListener('click', e => {
      if (!touch) return;
      const open = !tile.classList.contains('open');
      if (open) for (const other of feats.querySelectorAll('.feat.open')) if (other !== tile) turn(other, false, other.clientWidth / 2, other.clientHeight / 2);
      turn(tile, open, ...at(e));
    });
    // focus from the keyboard; a tap or click that focuses the tile is handled above
    tile.addEventListener('focus', () => {
      if (tile.matches(':focus-visible') && performance.now() - pressed > 500) turn(tile, true, tile.clientWidth / 2, 0);
    });
    tile.addEventListener('blur', () => { if (!tile.matches(':hover')) turn(tile, false, tile.clientWidth / 2, tile.clientHeight); });
    tile.addEventListener('keydown', e => {
      if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); turn(tile, !tile.classList.contains('open'), tile.clientWidth / 2, tile.clientHeight / 2); }
      else if (e.key === 'Escape') turn(tile, false, tile.clientWidth / 2, tile.clientHeight / 2);
    });
  }
}


// Visit counting (only when build.py put a Matomo <meta> in the page): cookieless, and not loaded at all for
// Do Not Track or Global Privacy Control.
// (matomo.js takes over window._paq when it loads, so always go through window)
function track(category, action, name) { (window._paq = window._paq || []).push(['trackEvent', category, action, name]); }
window._paq = window._paq || [];
const analytics = document.querySelector('meta[name=analytics][content=matomo]');
if (analytics && navigator.doNotTrack !== '1' && !navigator.globalPrivacyControl) {
  const base = analytics.dataset.url;
  // the thanks page's ?session_id= unlocks a licence key: never send it anywhere
  if (document.documentElement.dataset.page === 'thanks') window._paq.push(['setCustomUrl', location.origin + location.pathname]);
  // titles as "backdown.io/Page", as in Matomo's own snippet
  window._paq.push(['disableCookies'], ['setDoNotTrack', true], ['setDocumentTitle', `${document.domain}/${document.title}`],
    ['trackPageView'], ['enableLinkTracking'],
    ['setTrackerUrl', `${base}matomo.php`], ['setSiteId', analytics.dataset.site]);
  const s = document.createElement('script');
  s.async = true; s.src = `${base}matomo.js`;
  document.head.append(s);
}

// Moving between pages fades: natively where cross-document view transitions exist (site.css), and here, by fading
// the page out before following the link, everywhere else. The new page fades itself in (site.css).
if (!window.CSSViewTransitionRule && !matchMedia('(prefers-reduced-motion: reduce)').matches) {
  document.addEventListener('click', e => {
    const a = e.target.closest('a[href]');
    if (!a || e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
    if (a.target && a.target !== '_self' || a.hasAttribute('download')) return;
    const to = new URL(a.href, location.href);
    if (to.origin !== location.origin || to.pathname.startsWith('/api/')) return;
    if (to.pathname === location.pathname && to.search === location.search) return; // a jump within this page
    e.preventDefault();
    document.documentElement.classList.add('leaving');
    setTimeout(() => { location.href = to.href; }, 160);
  });
  // coming back through the back/forward cache: show the page again
  addEventListener('pageshow', () => document.documentElement.classList.remove('leaving'));
}

const lang = document.documentElement.lang;
const ago = iso => {
  const s = (Date.parse(iso) - Date.now()) / 1000, rtf = new Intl.RelativeTimeFormat(lang, { numeric: 'auto' });
  for (const [unit, n] of [['day', 86400], ['hour', 3600], ['minute', 60]]) if (Math.abs(s) >= n) return rtf.format(Math.round(s / n), unit);
  return rtf.format(0, 'minute');
};

// Status page: the live checks come from /api/status, which the server refreshes hourly. Asked once on load, then
// every 10 minutes only while the page is visible: nothing runs in a background tab.
const live = document.querySelector('.livebox[data-api]');
if (live) {
  const states = JSON.parse(live.dataset.states), checked = live.querySelector('.checked');
  let last = 0, timer;
  const paint = d => {
    for (const [svc, st] of Object.entries(d.services || {})) {
      const li = live.querySelector(`[data-svc="${svc}"]`);
      if (!li) continue;
      li.querySelector('.sdot').className = `sdot ${st}`;
      li.querySelector('.st').textContent = states[st] || st;
    }
    if (d.release) {
      const a = live.querySelector('[data-rel]'), t = live.querySelector('[data-rel-date]');
      // a release newer than this page's build: show it, with the date in the browser's format for this language
      if (a.textContent !== d.release.tag) {
        a.textContent = d.release.tag;
        if (d.release.date) { t.dateTime = d.release.date; t.textContent = new Date(d.release.date).toLocaleDateString(lang === 'en' ? 'en-GB' : lang, { day: 'numeric', month: 'long', year: 'numeric' }); }
      }
      a.href = d.release.url;
    }
    checked.textContent = live.dataset.ago.replace('{ago}', ago(d.checked));
    checked.title = new Date(d.checked).toLocaleString(lang);
  };
  const load = async () => {
    clearTimeout(timer);
    if (document.hidden) return;
    if (Date.now() - last > 60e3) {
      try { const r = await fetch(live.dataset.api); if (r.ok) { paint(await r.json()); last = Date.now(); } } catch {}
    }
    timer = setTimeout(load, 600e3);
  };
  document.addEventListener('visibilitychange', load);
  load();
}

// Pre-orders of Pro, Team and Forensic: the box, and each edition's button on its pricing card, stay hidden unless
// /api/preorder reports a live Stripe price for that edition; the checkout form is a plain POST (the server answers
// with a redirect to Stripe), so this only fills in the prices, picks the edition and shows a busy state.
const po = document.getElementById('preorder');
if (po) {
  const form = po.querySelector('form.checkout'), msg = form.querySelector('.join-msg'), button = form.querySelector('button[type=submit]');
  const option = edition => form.querySelector(`.po-ed[data-edition="${edition}"]`);
  const pick = edition => {
    const opt = option(edition);
    if (!opt || opt.hidden) return;
    opt.querySelector('input').checked = true;
    button.textContent = opt.dataset.cta;
  };
  fetch('/api/preorder').then(r => r.ok ? r.json() : null).then(d => {
    // (an answer cached from before there were three editions describes Pro alone)
    const editions = d?.editions || { pro: d || {} };
    const open = Object.keys(editions).filter(e => editions[e].open && option(e));
    if (!open.length) return;
    for (const e of open) {
      const { amount, currency } = editions[e], opt = option(e);
      opt.querySelector('[data-price]').textContent = new Intl.NumberFormat(lang, { style: 'currency', currency: currency.toUpperCase(), minimumFractionDigits: amount % 100 ? 2 : 0 }).format(amount / 100);
      opt.hidden = false;
      document.querySelectorAll(`.po-link[data-edition="${e}"]`).forEach(a => { a.hidden = false; });
    }
    form.querySelector('.po-eds').classList.toggle('single', open.length === 1);
    pick(open[0]);
    po.hidden = false;
    if (location.hash === '#preorder') po.scrollIntoView();
  }).catch(() => {});
  document.querySelectorAll('.po-link[data-edition]').forEach(a => a.addEventListener('click', () => pick(a.dataset.edition)));
  form.addEventListener('change', e => { if (e.target.name === 'edition') pick(e.target.value); });
  if (new URLSearchParams(location.search).get('checkout')) { msg.className = 'join-msg err'; msg.textContent = form.dataset.error; }
  form.addEventListener('submit', () => {
    button.disabled = true;
    msg.className = 'join-msg'; msg.textContent = form.dataset.busy;
    track('Pre-order', 'checkout', form.elements.edition.value);
  });
  // back from Stripe via the browser's back button: the page comes from the back/forward cache as it was left
  addEventListener('pageshow', e => { if (e.persisted) { button.disabled = false; msg.textContent = ''; } });
}

// Thanks page: look the order up by the session id Stripe put in the URL. While the payment is still clearing, ask
// again with a growing gap (3 s, 6 s, … up to a minute), and stop after ten minutes or when the tab is hidden.
const order = document.querySelector('.order[data-api]');
if (order) {
  const states = JSON.parse(order.dataset.states), say = order.querySelector('.order-state'), box = order.querySelector('.key');
  // which edition was bought: in the card's heading and the "when it ships" line (the names aren't translated)
  const EDITIONS = { pro: 'Pro', team: 'Team', forensic: 'Forensic' };
  const named = edition => order.querySelectorAll('[data-tpl]').forEach(el => {
    el.textContent = el.dataset.tpl.replace('{edition}', EDITIONS[edition] || EDITIONS.pro);
    el.hidden = false;
  });
  const id = new URLSearchParams(location.search).get('session_id');
  const started = Date.now();
  let gap = 3e3, timer;
  const check = async () => {
    clearTimeout(timer);
    if (!id) { say.textContent = states.unknown; return; }
    if (document.hidden) return;
    let d;
    try { const r = await fetch(`${order.dataset.api}?session_id=${encodeURIComponent(id)}`); d = await r.json(); } catch { d = { status: 'error' }; }
    if (d.edition) named(d.edition);
    if (d.status === 'paid') {
      say.textContent = (d.emailed ? states.paid : states.paid_noemail).replace('{email}', d.email || '');
      box.querySelector('textarea').value = d.licence;
      box.querySelector('[data-cmd]').textContent = `backdown license add ${d.licence}`;
      box.hidden = false;
      document.removeEventListener('visibilitychange', check);
      if (!sessionStorage.getItem(`tracked:${id}`)) { track('Pre-order', 'paid', d.edition); try { sessionStorage.setItem(`tracked:${id}`, '1'); } catch {} }
      return;
    }
    say.textContent = states[d.status] || states.error;
    if ((d.status === 'pending' || d.status === 'error') && Date.now() - started < 600e3) { timer = setTimeout(check, gap); gap = Math.min(gap * 2, 60e3); }
  };
  document.addEventListener('visibilitychange', check);
  check();
  order.querySelector('[data-copy]').addEventListener('click', async e => {
    const btn = e.currentTarget, area = document.querySelector(btn.dataset.copy), label = btn.textContent;
    try { await navigator.clipboard.writeText(area.value); } catch { area.select(); document.execCommand('copy'); }
    btn.textContent = btn.dataset.done; setTimeout(() => { btn.textContent = label; }, 2000);
  });
}

// Copy buttons: code blocks in the docs, and the press kit's texts (data-copy points at what to copy). They're in
// the page hidden, so there's no dead button without JS. The docs page (.dsearch) carries the labels in its own
// language; the press kit (English only) falls back to the defaults.
const copyLabels = document.querySelector('.dsearch')?.dataset ?? {};
document.querySelectorAll('.cbtn, button[data-copy]:not(.order [data-copy])').forEach(btn => {
  btn.hidden = false;
  btn.addEventListener('click', async () => {
    const src = btn.dataset.copy ? document.querySelector(btn.dataset.copy) : btn.parentElement.querySelector('code');
    const text = src.textContent.replace(/\n$/, '');
    let ok = true;
    try { await navigator.clipboard.writeText(text); } catch {
      const area = Object.assign(document.createElement('textarea'), { value: text });
      area.style.cssText = 'position:fixed;opacity:0';
      document.body.append(area); area.select();
      try { ok = document.execCommand('copy'); } catch { ok = false; }
      area.remove();
    }
    btn.dataset.label ||= btn.textContent;
    btn.textContent = ok ? (copyLabels.copied || 'Copied') : (copyLabels.select || 'Select and copy');
    btn.classList.toggle('done', ok);
    clearTimeout(btn.timer);
    btn.timer = setTimeout(() => { btn.textContent = btn.dataset.label; btn.classList.remove('done'); }, 1800);
  });
});

// Docs search: the index (url, page, section, text) is fetched the first time the box is used, then filtered here.
// Every word has to match; a match in the page title counts most, then the section heading, then the text.
const ds = document.querySelector('.dsearch');
if (ds) {
  const input = ds.querySelector('input'), box = ds.querySelector('.dresults');
  let index = null, loading = null, sel = -1;
  ds.hidden = false;
  const load = () => loading ||= fetch(ds.dataset.index).then(r => r.json())
    .then(d => { index = d.map(([u, p, h, x]) => ({ u, p, h, x, k: `${p}\n${h}\n${x}`.toLowerCase() })); })
    .catch(() => { index = []; loading = null; });
  const escHTML = s => s.replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
  const escRe = s => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const hi = (s, re) => escHTML(s).replace(re, '<mark>$&</mark>');
  const snippet = (x, terms) => {
    const lx = x.toLowerCase();
    const at = Math.min(...terms.map(t => lx.indexOf(t)).filter(i => i >= 0), x.length);
    const from = Math.max(0, at - 50), to = Math.min(x.length, from + 170);
    return (from > 0 ? '…' : '') + x.slice(from, to).trim() + (to < x.length ? '…' : '');
  };
  const items = () => [...box.querySelectorAll('a')];
  const select = n => {
    const all = items();
    sel = all.length && n >= 0 ? n % all.length : -1;
    all.forEach((a, k) => a.setAttribute('aria-selected', String(k === sel)));
    if (sel >= 0) { input.setAttribute('aria-activedescendant', all[sel].id); all[sel].scrollIntoView({ block: 'nearest' }); }
    else input.removeAttribute('aria-activedescendant');
  };
  const show = open => { box.hidden = !open; input.setAttribute('aria-expanded', String(open)); if (!open) select(-1); };
  const search = async () => {
    const q = input.value.trim().toLowerCase();
    if (!q) { show(false); return; }
    await load();
    if (input.value.trim().toLowerCase() !== q) return; // typed on meanwhile
    const terms = [...new Set(q.split(/\s+/).filter(Boolean))].slice(0, 8);
    const re = new RegExp(terms.map(escRe).sort((a, b) => b.length - a.length).join('|'), 'gi');
    const perPage = {};
    const hits = index.filter(e => terms.every(t => e.k.includes(t)))
      .map(e => {
        const p = e.p.toLowerCase(), h = e.h.toLowerCase();
        let score = 0;
        for (const t of terms) score += (p.includes(t) ? 8 : 0) + (h.includes(t) ? 5 : 0) + Math.min(3, e.k.split(t).length - 1);
        if ((h || p).includes(q)) score += 6;
        if (!e.h && p.includes(q)) score += 4;
        return { e, score };
      })
      .sort((a, b) => b.score - a.score)
      .filter(({ e }) => (perPage[e.p] = (perPage[e.p] || 0) + 1) <= 3)
      .slice(0, 12);
    const noneMsg = (ds.dataset.none || 'Nothing in the docs matches “{q}”.').replace('{q}', escHTML(input.value.trim()));
    box.innerHTML = hits.length
      ? hits.map(({ e }, k) => `<a href="${escHTML(e.u)}" role="option" id="dr${k}" aria-selected="false">${e.h ? `<small>${hi(e.p, re)}</small><b>${hi(e.h, re)}</b>` : `<b>${hi(e.p, re)}</b>`}${hi(snippet(e.x, terms), re)}</a>`).join('')
      : `<p class="none">${noneMsg}</p>`;
    show(true);
    select(hits.length ? 0 : -1);
  };
  let timer;
  input.addEventListener('focus', () => { load(); if (input.value.trim()) search(); });
  input.addEventListener('input', () => { clearTimeout(timer); timer = setTimeout(search, 120); });
  input.addEventListener('keydown', e => {
    const n = items().length;
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      if (box.hidden) search(); else if (n) select((sel + (e.key === 'ArrowDown' ? 1 : n - 1)) % n);
    } else if (e.key === 'Enter') {
      const a = items()[sel >= 0 ? sel : 0];
      if (a) { e.preventDefault(); show(false); location.href = a.href; }
    } else if (e.key === 'Escape') {
      if (!box.hidden) show(false); else { input.value = ''; input.blur(); }
    }
  });
  box.addEventListener('click', e => { if (e.target.closest('a')) show(false); });
  document.addEventListener('click', e => { if (!ds.contains(e.target)) show(false); });
  // "/" anywhere searches, unless typing somewhere else
  document.addEventListener('keydown', e => {
    if (e.key !== '/' || e.ctrlKey || e.metaKey || e.altKey || e.target.closest('input, textarea, select, [contenteditable]')) return;
    e.preventDefault();
    input.focus();
  });
}

// Docs: "on this page" marks the section being read.
const toc = document.querySelector('.doc-toc');
if (toc) {
  const links = new Map([...toc.querySelectorAll('a')].map(a => [decodeURIComponent(a.hash.slice(1)), a]));
  const heads = [...document.querySelectorAll('.doc-body h2[id], .doc-body h3[id]')].filter(h => links.has(h.id));
  let queued = false;
  const mark = () => {
    queued = false;
    const top = document.querySelector('.hdr').offsetHeight + 24;
    let cur = heads[0];
    for (const h of heads) if (h.getBoundingClientRect().top <= top) cur = h;
    // at the very bottom, the last section is the one being read
    if (innerHeight + scrollY >= document.documentElement.scrollHeight - 4) cur = heads[heads.length - 1];
    links.forEach((a, id) => { if (id === cur?.id) a.setAttribute('aria-current', 'true'); else a.removeAttribute('aria-current'); });
  };
  addEventListener('scroll', () => { if (!queued) { queued = true; requestAnimationFrame(mark); } }, { passive: true });
  mark();
}

// Docs: the sidebar keeps its scroll position from page to page, and always shows the current page.
const side = document.querySelector('.docs-nav');
if (side) {
  try { const y = sessionStorage.getItem('docs-side'); if (y !== null) side.scrollTop = Number(y); } catch {}
  const cur = side.querySelector('[aria-current=page]');
  if (cur && side.scrollHeight > side.clientHeight) {
    const r = cur.getBoundingClientRect(), s = side.getBoundingClientRect();
    if (r.top < s.top || r.bottom > s.bottom) side.scrollTop += r.top - s.top - s.height / 3;
  }
  addEventListener('pagehide', () => { try { sessionStorage.setItem('docs-side', String(side.scrollTop)); } catch {} });
}
