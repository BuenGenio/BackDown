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
      if (r.ok) { say('ok', form.dataset.ok); form.reset(); }
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

// Underline the nav link for the section in view.
const links = new Map([...document.querySelectorAll('.hdr nav a[data-s]')].map(a => [a.dataset.s, a]));
if (links.size && 'IntersectionObserver' in window) {
  const io = new IntersectionObserver(entries => {
    for (const e of entries) if (e.isIntersecting) links.forEach((a, id) => a.classList.toggle('active', id === e.target.id));
  }, { rootMargin: '-45% 0px -50% 0px' });
  links.forEach((_, id) => { const s = document.getElementById(id); if (s) io.observe(s); });
}
