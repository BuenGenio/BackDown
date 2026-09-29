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
  window._paq.push(['disableCookies'], ['setDoNotTrack', true], ['trackPageView'], ['enableLinkTracking'],
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

// Pro pre-order: the box stays hidden unless /api/preorder reports a live Stripe price; the checkout form is a
// plain POST (the server answers with a redirect to Stripe), so this only fills in the price and a busy state.
const po = document.getElementById('preorder');
if (po) {
  fetch('/api/preorder').then(r => r.ok ? r.json() : null).then(d => {
    if (!d?.open) return;
    const price = new Intl.NumberFormat(lang, { style: 'currency', currency: d.currency.toUpperCase(), minimumFractionDigits: d.amount % 100 ? 2 : 0 }).format(d.amount / 100);
    po.querySelectorAll('[data-price]').forEach(el => { el.textContent = price; });
    po.hidden = false;
    document.querySelectorAll('.po-link').forEach(a => { a.hidden = false; });
    if (location.hash === '#preorder') po.scrollIntoView();
  }).catch(() => {});
  const form = po.querySelector('form.checkout'), msg = form.querySelector('.join-msg');
  if (new URLSearchParams(location.search).get('checkout')) { msg.className = 'join-msg err'; msg.textContent = form.dataset.error; }
  form.addEventListener('submit', () => {
    form.querySelector('button').disabled = true;
    msg.className = 'join-msg'; msg.textContent = form.dataset.busy;
    track('Pre-order', 'checkout');
  });
  // back from Stripe via the browser's back button: the page may come from the back/forward cache
  addEventListener('pageshow', () => { form.querySelector('button').disabled = false; msg.textContent = ''; });
}

// Thanks page: look the order up by the session id Stripe put in the URL. While the payment is still clearing, ask
// again with a growing gap (3 s, 6 s, … up to a minute), and stop after ten minutes or when the tab is hidden.
const order = document.querySelector('.order[data-api]');
if (order) {
  const states = JSON.parse(order.dataset.states), say = order.querySelector('.order-state'), box = order.querySelector('.key');
  const id = new URLSearchParams(location.search).get('session_id');
  const started = Date.now();
  let gap = 3e3, timer;
  const check = async () => {
    clearTimeout(timer);
    if (!id) { say.textContent = states.unknown; return; }
    if (document.hidden) return;
    let d;
    try { const r = await fetch(`${order.dataset.api}?session_id=${encodeURIComponent(id)}`); d = await r.json(); } catch { d = { status: 'error' }; }
    if (d.status === 'paid') {
      say.textContent = (d.emailed ? states.paid : states.paid_noemail).replace('{email}', d.email || '');
      box.querySelector('textarea').value = d.licence;
      box.querySelector('[data-cmd]').textContent = `backdown license add ${d.licence}`;
      box.hidden = false;
      document.removeEventListener('visibilitychange', check);
      if (!sessionStorage.getItem(`tracked:${id}`)) { track('Pre-order', 'paid'); try { sessionStorage.setItem(`tracked:${id}`, '1'); } catch {} }
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
// the page hidden, so there's no dead button without JS.
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
    btn.textContent = ok ? 'Copied' : 'Select and copy';
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
    box.innerHTML = hits.length
      ? hits.map(({ e }, k) => `<a href="${escHTML(e.u)}" role="option" id="dr${k}" aria-selected="false">${e.h ? `<small>${hi(e.p, re)}</small><b>${hi(e.h, re)}</b>` : `<b>${hi(e.p, re)}</b>`}${hi(snippet(e.x, terms), re)}</a>`).join('')
      : `<p class="none">Nothing in the docs matches “${escHTML(input.value.trim())}”.</p>`;
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
