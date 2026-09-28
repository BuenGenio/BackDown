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
