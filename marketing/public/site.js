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

// Underline the nav link for the section in view.
const links = new Map([...document.querySelectorAll('.hdr nav a[data-s]')].map(a => [a.dataset.s, a]));
if (links.size && 'IntersectionObserver' in window) {
  const io = new IntersectionObserver(entries => {
    for (const e of entries) if (e.isIntersecting) links.forEach((a, id) => a.classList.toggle('active', id === e.target.id));
  }, { rootMargin: '-45% 0px -50% 0px' });
  links.forEach((_, id) => { const s = document.getElementById(id); if (s) io.observe(s); });
}
