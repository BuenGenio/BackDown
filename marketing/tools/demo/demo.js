// Runs before the app in the docs' live demo (public/demo/index.html): says what this is, opens the view or
// folder the docs page asked for (?view=places, ?dir=/media/alex/Old%20Laptop/home/alex/Pictures, ?mode=gallery),
// and keeps the demo inside its frame. Everything else is BackDown's own UI, answered from recorded responses.
(() => {
  const q = new URLSearchParams(location.search);
  const b64 = s => btoa(unescape(encodeURIComponent(s))).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
  try {
    if (q.get('dir')) sessionStorage.setItem('bd.dir', b64(q.get('dir')));
    if (q.get('mode')) localStorage.setItem('bd.view', q.get('mode'));
    sessionStorage.removeItem('bd.tabs');
  } catch {}
  const VIEWS = { dupes: '#dupes-btn', activity: '#activity-btn', entities: '#entities-btn', places: '#places-btn', swarm: '#swarm-btn',
    plan: '#plan-btn', wipe: '#wipe-btn', rescue: '#rescue-btn', settings: '#settings-btn', machine: '#machines .place[data-mc]',
    search: '#search-btn' };
  const framed = window.top !== window.self;
  const lang = q.get('lang') === 'uk' ? 'uk' : 'en';
  const T = {
    en: { what: 'Demo', body: 'BackDown’s own interface on an invented set of drives. Look around; nothing here is yours and nothing can be changed.',
          open: 'Open in a window of its own', install: 'Install BackDown' },
    uk: { what: 'Демо', body: 'Власний інтерфейс BackDown на вигаданому наборі дисків. Роздивляйтеся; тут немає ваших даних і нічого не можна змінити.',
          open: 'Відкрити в окремому вікні', install: 'Встановити BackDown' },
  }[lang];
  document.addEventListener('DOMContentLoaded', () => {
    const bar = document.createElement('div');
    bar.id = 'demo-bar';
    bar.innerHTML = `<b>${T.what}</b> <span>${T.body}</span>`
      + (framed ? ` <a href="${location.pathname}${location.search}" target="_blank" rel="noopener">${T.open} ↗</a>` : '')
      + ` <a href="${lang === 'uk' ? '/uk' : ''}/download/" target="_top">${T.install} ↗</a>`;
    document.body.prepend(bar);
    const style = document.createElement('style');
    style.textContent = `#demo-bar{position:sticky;top:0;z-index:1000;display:flex;gap:10px;align-items:center;flex-wrap:wrap;padding:6px 14px;font:13px/1.4 system-ui,sans-serif;background:#1d4ed8;color:#fff}
#demo-bar b{text-transform:uppercase;letter-spacing:.08em;font-size:11px;background:rgba(255,255,255,.18);padding:2px 7px;border-radius:4px}
#demo-bar span{flex:1;min-width:200px;opacity:.95}#demo-bar a{color:#fff;font-weight:600;white-space:nowrap}
@media (prefers-color-scheme:dark){#demo-bar{background:#1e3a8a}}`;
    document.head.append(style);
    // the view the docs asked for, once the app has its drives
    const view = q.get('view');
    if (view && VIEWS[view]) {
      let tries = 0;
      const t = setInterval(() => {
        const ready = document.querySelector('#places .place');
        if (ready || ++tries > 60) {
          clearInterval(t);
          const btn = document.querySelector(VIEWS[view]);
          if (btn) setTimeout(() => btn.click(), 120);
          // and a tab or section inside it (?tab=Attestations): the first control whose text starts with it
          const tab = q.get('tab');
          if (tab) {
            let n = 0;
            const tt = setInterval(() => {
              const el = [...document.querySelectorAll('#view [role="tab"], #view nav button, #view .set-nav button, #view .tabs button')]
                .find(e => e.textContent.trim().toLowerCase().startsWith(tab.toLowerCase()));
              if (el || ++n > 40) { clearInterval(tt); el?.click(); }
            }, 150);
          }
        }
      }, 100);
    }
  });
  // the app long-polls /api/daemon every second or two while a daemon runs; the recorded daemon never changes,
  // so answer those polls from the recording every half minute instead of hammering the site
  const realFetch = window.fetch.bind(window);
  window.fetch = (input, init) => {
    const url = typeof input === 'string' ? input : input?.url || '';
    if (url.startsWith('/demo/api/daemon?') && /[?&]since=/.test(url)) {
      return new Promise(r => setTimeout(r, 30000)).then(() => realFetch('/demo/api/daemon', init));
    }
    return realFetch(input, init);
  };
  // the app's "open in the system" and other things that can't work here: swallow quietly
  window.addEventListener('error', e => { if (/demo/.test(e.message || '')) e.preventDefault(); });
})();
