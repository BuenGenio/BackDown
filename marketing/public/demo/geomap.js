// The Places view's map, drawn without going online: Web Mercator on a canvas, its ground made of
// the 170,000 towns and cities built into BackDown (every place of 1,000 people or more, as a dot,
// so coasts and valleys show where people live), the photos' positions as numbered clusters, the
// circle being searched, and the names of the biggest places in view. The dots and the names come
// from this BackDown (/api/places-basemap and /api/places-labels, from geo.rs); nothing else is
// fetched.

const TILE = 256;
const MAX_Z = 16;
const R_KM = 6371.0088;
const CELL = 34; // px: photos closer than this on screen to a cluster's first are in it

const clampLat = lat => Math.max(-85.05, Math.min(85.05, lat));
// a latitude and longitude as 0..1 across and down the world
const mercX = lon => (lon + 180) / 360;
const mercY = lat => { const r = clampLat(lat) * Math.PI / 180; return (1 - Math.log(Math.tan(r) + 1 / Math.cos(r)) / Math.PI) / 2; };
const latOf = y => { const n = Math.PI - 2 * Math.PI * y; return 180 / Math.PI * Math.atan(0.5 * (Math.exp(n) - Math.exp(-n))); };
const lonOf = x => x * 360 - 180;

// the point `km` from (lat, lon) towards `deg`, on the sphere
function destination(lat, lon, km, deg) {
  const d = km / R_KM, b = deg * Math.PI / 180, p1 = lat * Math.PI / 180, l1 = lon * Math.PI / 180;
  const p2 = Math.asin(Math.sin(p1) * Math.cos(d) + Math.cos(p1) * Math.sin(d) * Math.cos(b));
  const l2 = l1 + Math.atan2(Math.sin(b) * Math.sin(d) * Math.cos(p1), Math.cos(d) - Math.sin(p1) * Math.sin(p2));
  return [p2 * 180 / Math.PI, ((l2 * 180 / Math.PI + 540) % 360) - 180];
}

export function haversine(la1, lo1, la2, lo2) {
  const r = Math.PI / 180, dl = (la2 - la1) * r, dn = (lo2 - lo1) * r;
  const a = Math.sin(dl / 2) ** 2 + Math.cos(la1 * r) * Math.cos(la2 * r) * Math.sin(dn / 2) ** 2;
  return 2 * R_KM * Math.asin(Math.min(1, Math.sqrt(a)));
}

// the box around a circle, as [south, west, north, east]
export function circleBounds(lat, lon, km) {
  const n = destination(lat, lon, km, 0)[0], s = destination(lat, lon, km, 180)[0];
  const e = destination(lat, lon, km, 90)[1], w = destination(lat, lon, km, 270)[1];
  return [Math.max(-85, s), w, Math.min(85, n), e];
}

// the dots, fetched once a session (the browser keeps them a day): per place, its position as
// 0..1 across and down the world and its size class
let basemap = null;
function loadBasemap() {
  basemap ||= fetch('/demo/api/places-basemap', { credentials: 'same-origin' })
    .then(r => r.ok ? r.arrayBuffer() : Promise.reject(new Error(r.statusText)))
    .then(buf => {
      const n = Math.floor(buf.byteLength / 5), dv = new DataView(buf);
      const x = new Float32Array(n), y = new Float32Array(n), cls = new Uint8Array(n);
      for (let i = 0; i < n; i++) {
        x[i] = mercX(dv.getInt16(i * 5 + 2, true) / 100);
        y[i] = mercY(dv.getInt16(i * 5, true) / 100);
        cls[i] = dv.getUint8(i * 5 + 4);
      }
      return { n, x, y, cls };
    });
  return basemap;
}

const fmtCount = n => n >= 1e4 ? `${Math.round(n / 1e3)}k` : n >= 1e3 ? `${(n / 1e3).toFixed(1)}k` : String(n);

export class GeoMap {
  // onPick(lat, lon, km?): search around a point (Shift+click, or a cluster that's one spot);
  // onArea(lat, lon, km): search the area in view ("Search this area")
  constructor(host, { onPick = () => {}, onArea = () => {} } = {}) {
    this.host = host;
    this.onPick = onPick; this.onArea = onArea;
    host.classList.add('gm');
    host.innerHTML = `<canvas class="gm-c" tabindex="0" aria-label="Map of where the photos were taken. Drag to move, scroll or press + and − to zoom, Shift+click to search around a point."></canvas>
      <div class="gm-tools" role="group" aria-label="Map">
        <button class="gm-b" type="button" data-gm="in" title="Zoom in (+)" aria-label="Zoom in"><svg viewBox="0 0 24 24"><path d="M12 5v14M5 12h14"/></svg></button>
        <button class="gm-b" type="button" data-gm="out" title="Zoom out (−)" aria-label="Zoom out"><svg viewBox="0 0 24 24"><path d="M5 12h14"/></svg></button>
        <button class="gm-b" type="button" data-gm="fit" title="Show every match" aria-label="Show every match"><svg viewBox="0 0 24 24"><path d="M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5"/></svg></button>
      </div>
      <button class="btn sm gm-here" type="button" data-gm="here" hidden>Search this area</button>
      <div class="gm-scale" aria-hidden="true"><i></i><span></span></div>
      <div class="gm-credit" title="Towns, cities and regions from GeoNames (CC BY 4.0); airports from OurAirports">Places © GeoNames</div>`;
    this.canvas = host.querySelector('canvas');
    this.ctx = this.canvas.getContext('2d');
    this.z = 1.5; this.cx = 0.5; this.cy = 0.42;
    this.points = []; this.circle = null; this.marks = []; this.labels = []; this.clusters = [];
    this.bounds = null; this.base = null; this.cache = null; this.drag = null; this.frame = 0; this.labelSeq = 0;
    this.w = 0; this.h = 0; this.dpr = 1;
    loadBasemap().then(b => { this.base = b; this.cache = null; this.redraw(); }).catch(() => {});
    this.wire();
    new ResizeObserver(() => this.resize()).observe(host);
    this.resize();
  }

  // ---------- what's shown

  // the located matches: [[lat, lon, n], …]
  setPoints(points) { this.points = [...(points || [])].sort((a, b) => b[2] - a[2]); this.redraw(); }
  // the circle searched ({lat, lon, km}), or null
  setCircle(c) { this.circle = c; this.redraw(); }
  // photos to ring (hovered or chosen): [[lat, lon], …]
  setMarks(marks) { this.marks = marks || []; this.redraw(); }
  // where "fit" goes: [south, west, north, east]
  setBounds(b) { this.bounds = b; }

  fit(b = this.bounds) {
    if (!b) return;
    if (!this.w) { this.pending = b; return; }
    let [s, w, n, e] = b;
    // a single spot gets a town's worth of map around it
    if (n - s < 0.02) { s -= 0.01; n += 0.01; }
    if (e - w < 0.02) { w -= 0.01; e += 0.01; }
    const x0 = mercX(w), x1 = mercX(e), y0 = mercY(n), y1 = mercY(s);
    const pad = 40;
    const zx = Math.log2(Math.max(1, this.w - pad * 2) / ((x1 - x0) * TILE));
    const zy = Math.log2(Math.max(1, this.h - pad * 2) / ((y1 - y0) * TILE));
    this.z = Math.max(this.minZ(), Math.min(MAX_Z - 2, zx, zy));
    this.cx = (x0 + x1) / 2; this.cy = (y0 + y1) / 2;
    this.host.querySelector('[data-gm=here]').hidden = true;
    this.clamp(); this.cache = null; this.redraw();
  }

  // ---------- geometry

  minZ() { return Math.max(0.5, Math.log2(Math.max(this.w, 1) / TILE)); }
  scale() { return TILE * 2 ** this.z; }
  toScreen(lat, lon) { const s = this.scale(); return [(mercX(lon) - this.cx) * s + this.w / 2, (mercY(lat) - this.cy) * s + this.h / 2]; }
  toLatLon(px, py) { const s = this.scale(); return [latOf(this.cy + (py - this.h / 2) / s), lonOf(this.cx + (px - this.w / 2) / s)]; }
  view() {
    const [n, w] = this.toLatLon(0, 0), [s, e] = this.toLatLon(this.w, this.h);
    return { s, w: Math.max(-180, w), n, e: Math.min(180, e) };
  }
  clamp() {
    this.z = Math.max(this.minZ(), Math.min(MAX_Z, this.z));
    const s = this.scale(), hx = this.w / 2 / s, hy = this.h / 2 / s;
    this.cx = hx >= 0.5 ? 0.5 : Math.max(hx, Math.min(1 - hx, this.cx));
    this.cy = hy >= 0.5 ? 0.5 : Math.max(hy, Math.min(1 - hy, this.cy));
  }
  zoomAt(dz, px = this.w / 2, py = this.h / 2) {
    const [lat, lon] = this.toLatLon(px, py);
    this.z = Math.max(this.minZ(), Math.min(MAX_Z, this.z + dz));
    // keep the point under the pointer where it is
    const s = this.scale();
    this.cx = mercX(lon) - (px - this.w / 2) / s;
    this.cy = mercY(lat) - (py - this.h / 2) / s;
    this.clamp(); this.cache = null; this.userMoved(); this.redraw();
  }
  pan(dx, dy) { const s = this.scale(); this.cx -= dx / s; this.cy -= dy / s; this.clamp(); this.userMoved(); this.redraw(); }
  userMoved() { this.host.querySelector('[data-gm=here]').hidden = false; }

  resize() {
    const r = this.host.getBoundingClientRect();
    if (!r.width || !r.height) return;
    const dpr = window.devicePixelRatio || 1;
    this.w = r.width; this.h = r.height; this.dpr = dpr;
    this.canvas.width = Math.round(r.width * dpr); this.canvas.height = Math.round(r.height * dpr);
    this.canvas.style.width = `${r.width}px`; this.canvas.style.height = `${r.height}px`;
    if (this.pending) { const b = this.pending; this.pending = null; return this.fit(b); }
    this.clamp(); this.cache = null; this.redraw();
  }

  // ---------- input

  wire() {
    const c = this.canvas;
    c.addEventListener('pointerdown', e => {
      if (e.button !== 0) return;
      c.setPointerCapture(e.pointerId);
      this.drag = { x: e.clientX, y: e.clientY, sx: e.clientX, sy: e.clientY, moved: false };
    });
    c.addEventListener('pointermove', e => {
      if (this.drag) {
        const dx = e.clientX - this.drag.x, dy = e.clientY - this.drag.y;
        if (Math.abs(e.clientX - this.drag.sx) + Math.abs(e.clientY - this.drag.sy) > 4) this.drag.moved = true;
        if (this.drag.moved) { this.drag.x = e.clientX; this.drag.y = e.clientY; this.pan(dx, dy); }
        return;
      }
      const hit = this.clusterAt(e.offsetX, e.offsetY);
      c.style.cursor = hit ? 'pointer' : 'grab';
      c.title = hit ? `${hit.n} ${hit.n === 1 ? 'photo' : 'photos'} here${hit.n > 1 && hit.spread > 0 ? ': click to zoom in' : ': click to search around it'}` : '';
    });
    const end = e => {
      if (!this.drag) return;
      const d = this.drag; this.drag = null;
      if (d.moved) { this.cache = null; this.redraw(); return; }
      this.click(e.offsetX, e.offsetY, e.shiftKey);
    };
    c.addEventListener('pointerup', end);
    c.addEventListener('pointercancel', () => { this.drag = null; });
    c.addEventListener('wheel', e => {
      e.preventDefault();
      const unit = e.deltaMode === 1 ? 3 : e.deltaMode === 2 ? 1 : 120;
      const dz = -Math.sign(e.deltaY) * Math.min(1, Math.abs(e.deltaY) / unit) * 0.6;
      this.zoomAt(dz, e.offsetX, e.offsetY);
    }, { passive: false });
    c.addEventListener('dblclick', e => { e.preventDefault(); this.zoomAt(1, e.offsetX, e.offsetY); });
    c.addEventListener('keydown', e => {
      const step = 80, k = e.key;
      if (k === '+' || k === '=') this.zoomAt(1);
      else if (k === '-' || k === '_') this.zoomAt(-1);
      else if (k === 'ArrowLeft') this.pan(step, 0);
      else if (k === 'ArrowRight') this.pan(-step, 0);
      else if (k === 'ArrowUp') this.pan(0, step);
      else if (k === 'ArrowDown') this.pan(0, -step);
      else return;
      e.preventDefault(); e.stopPropagation();
    });
    this.host.addEventListener('click', e => {
      const b = e.target.closest('[data-gm]');
      if (!b) return;
      const a = b.dataset.gm;
      if (a === 'in') this.zoomAt(1);
      else if (a === 'out') this.zoomAt(-1);
      else if (a === 'fit') this.fit();
      else if (a === 'here') {
        const [lat, lon] = this.toLatLon(this.w / 2, this.h / 2);
        // the circle that fits in the view
        const [, edge] = this.toLatLon(this.w / 2 + Math.min(this.w, this.h) * 0.45, this.h / 2);
        const km = Math.max(0.2, haversine(lat, lon, lat, edge));
        b.hidden = true;
        this.onArea(lat, lon, km >= 10 ? Math.round(km) : Math.round(km * 10) / 10);
      }
    });
  }

  click(px, py, shift) {
    const hit = this.clusterAt(px, py);
    if (hit && !shift) {
      // a cluster: closer, until it's one spot; then search around that spot
      if (hit.n > 1 && hit.spread > 0 && this.z < MAX_Z - 0.5) {
        const span = Math.max(hit.spread * this.scale(), 1);
        this.zoomAt(Math.min(4, Math.max(1, Math.log2(CELL * 3 / span))), hit.x, hit.y);
      } else {
        this.onPick(hit.lat, hit.lon, 1);
      }
      return;
    }
    if (shift) { const [lat, lon] = this.toLatLon(px, py); this.onPick(lat, lon); }
  }

  clusterAt(px, py) {
    let best = null, bd = Infinity;
    for (const c of this.clusters) {
      const d = Math.hypot(c.x - px, c.y - py);
      if (d <= c.r + 3 && d < bd) { best = c; bd = d; }
    }
    return best;
  }

  // ---------- drawing

  redraw() {
    if (this.frame) return;
    this.frame = requestAnimationFrame(() => { this.frame = 0; this.draw(); });
  }

  colours() {
    const cs = getComputedStyle(this.host);
    const v = n => cs.getPropertyValue(n).trim();
    return { bg: v('--gm-sea') || v('--view'), ink: v('--ink'), ink2: v('--ink-2'), dot: v('--gm-dot') || v('--ink-3'), accent: v('--accent'), view: v('--view'), line: v('--gm-grid') || v('--line'), font: cs.fontFamily };
  }

  // the dots at this zoom, drawn once into a canvas twice the view's size and moved with the map
  // while it's dragged; drawn again when it stops, zooms, or runs off the edge
  baseLayer(col) {
    const s = this.scale(), dpr = this.dpr;
    const W = this.w * 2, H = this.h * 2;
    const ox = this.cx * s - W / 2, oy = this.cy * s - H / 2;
    const c = this.cache;
    if (c && c.z === this.z && c.W === W && c.H === H && Math.abs(c.ox - ox) < this.w / 2 && Math.abs(c.oy - oy) < this.h / 2 && c.dpr === dpr && c.col === col.dot) {
      return { ...c, dx: c.ox - ox, dy: c.oy - oy };
    }
    const cv = c?.cv || document.createElement('canvas');
    cv.width = Math.round(W * dpr); cv.height = Math.round(H * dpr);
    const g = cv.getContext('2d');
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    g.clearRect(0, 0, W, H);
    if (this.base) {
      const { n, x, y, cls } = this.base;
      // a dot grows with the place and with the zoom
      const zf = Math.min(1.9, Math.max(0.6, (this.z - 1) / 5));
      const size = [0.9, 1.35, 2.1, 3.1].map(v => v * zf);
      const alpha = [0.42, 0.55, 0.7, 0.85];
      g.fillStyle = col.dot;
      for (let k = 0; k < 4; k++) {
        g.globalAlpha = alpha[k];
        const d = size[k], h = d / 2;
        for (let i = 0; i < n; i++) {
          if (cls[i] !== k) continue;
          const px = x[i] * s - ox; if (px < -3 || px > W + 3) continue;
          const py = y[i] * s - oy; if (py < -3 || py > H + 3) continue;
          g.fillRect(px - h, py - h, d, d);
        }
      }
      g.globalAlpha = 1;
    }
    this.cache = { cv, z: this.z, ox, oy, W, H, dpr, col: col.dot };
    return { ...this.cache, dx: 0, dy: 0 };
  }

  draw() {
    if (!this.w) return;
    const g = this.ctx, col = this.colours();
    g.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
    g.fillStyle = col.bg; g.fillRect(0, 0, this.w, this.h);
    this.graticule(g, col);
    const b = this.baseLayer(col);
    g.drawImage(b.cv, b.dx - this.w / 2, b.dy - this.h / 2, this.w * 2, this.h * 2);
    this.drawCircle(g, col);
    this.drawLabels(g, col);
    this.drawClusters(g, col);
    this.drawMarks(g, col);
    this.drawScale();
    if (!this.drag) this.wantLabels();
  }

  graticule(g, col) {
    // lines every 30°, 10°, 5° or 1° as the zoom allows: something to hold on to over the sea
    const step = this.z < 3 ? 30 : this.z < 5 ? 10 : this.z < 7 ? 5 : this.z < 9 ? 1 : 0;
    if (!step) return;
    const v = this.view();
    g.strokeStyle = col.line; g.lineWidth = 1;
    g.beginPath();
    for (let lon = Math.ceil(v.w / step) * step; lon <= v.e; lon += step) { const [x] = this.toScreen(0, lon); g.moveTo(Math.round(x) + 0.5, 0); g.lineTo(Math.round(x) + 0.5, this.h); }
    for (let lat = Math.ceil(v.s / step) * step; lat <= v.n; lat += step) { const [, y] = this.toScreen(lat, 0); g.moveTo(0, Math.round(y) + 0.5); g.lineTo(this.w, Math.round(y) + 0.5); }
    g.stroke();
  }

  drawCircle(g, col) {
    const c = this.circle;
    if (!c) return;
    const [cx, cy] = this.toScreen(c.lat, c.lon), world = this.scale();
    g.beginPath();
    for (let deg = 0; deg <= 360; deg += 3) {
      const [la, lo] = destination(c.lat, c.lon, c.km, deg);
      let [x, y] = this.toScreen(la, lo);
      // keep the ring whole across the antimeridian
      if (x - cx > world / 2) x -= world; else if (cx - x > world / 2) x += world;
      deg ? g.lineTo(x, y) : g.moveTo(x, y);
    }
    g.closePath();
    g.fillStyle = col.accent; g.globalAlpha = 0.07; g.fill();
    g.globalAlpha = 0.9; g.strokeStyle = col.accent; g.lineWidth = 1.6; g.setLineDash([6, 4]); g.stroke(); g.setLineDash([]);
    g.globalAlpha = 1;
    g.beginPath(); g.arc(cx, cy, 5, 0, Math.PI * 2); g.fillStyle = col.view; g.fill();
    g.lineWidth = 2.5; g.strokeStyle = col.accent; g.stroke();
  }

  drawClusters(g, col) {
    // greedy: the busiest spots first, each taking in what's within CELL of it (found through a
    // grid of CELL-sized buckets), so a cluster is where photos are, not where a grid line falls
    const grid = new Map(), list = [];
    for (const [lat, lon, n] of this.points) {
      const [x, y] = this.toScreen(lat, lon);
      if (x < -CELL || y < -CELL || x > this.w + CELL || y > this.h + CELL) continue;
      const gx = Math.floor(x / CELL), gy = Math.floor(y / CELL);
      let c = null, best = CELL;
      for (let dx = -1; dx <= 1; dx++) for (let dy = -1; dy <= 1; dy++) {
        for (const o of grid.get(`${gx + dx},${gy + dy}`) || []) {
          const d = Math.hypot(o.x0 - x, o.y0 - y);
          if (d < best) { best = d; c = o; }
        }
      }
      if (!c) {
        c = { x0: x, y0: y, n: 0, sx: 0, sy: 0, lat: 0, lon: 0, minx: Infinity, maxx: -Infinity, miny: Infinity, maxy: -Infinity };
        const k = `${gx},${gy}`;
        if (!grid.has(k)) grid.set(k, []);
        grid.get(k).push(c); list.push(c);
      }
      c.n += n; c.sx += x * n; c.sy += y * n; c.lat += lat * n; c.lon += lon * n;
      const mx = mercX(lon), my = mercY(lat);
      c.minx = Math.min(c.minx, mx); c.maxx = Math.max(c.maxx, mx); c.miny = Math.min(c.miny, my); c.maxy = Math.max(c.maxy, my);
    }
    // drawn where its first (busiest) spot is: those are at least CELL apart, so none overlap
    this.clusters = list.map(c => ({
      n: c.n, x: c.x0, y: c.y0, lat: c.lat / c.n, lon: c.lon / c.n,
      spread: Math.max(c.maxx - c.minx, c.maxy - c.miny), r: c.n === 1 ? 6 : Math.min(CELL / 2 - 1, 9 + Math.log2(c.n) * 1.9),
    }));
    g.font = `700 11px ${col.font}`;
    g.textAlign = 'center'; g.textBaseline = 'middle';
    for (const c of this.clusters) {
      g.beginPath(); g.arc(c.x, c.y, c.r + 3.5, 0, Math.PI * 2); g.fillStyle = col.accent; g.globalAlpha = 0.22; g.fill(); g.globalAlpha = 1;
      g.beginPath(); g.arc(c.x, c.y, c.r, 0, Math.PI * 2); g.fillStyle = col.accent; g.fill();
      g.lineWidth = 1.5; g.strokeStyle = col.view; g.stroke();
      if (c.n > 1) { g.fillStyle = '#fff'; g.fillText(fmtCount(c.n), c.x, c.y + 0.5); }
    }
  }

  drawMarks(g, col) {
    for (const [lat, lon] of this.marks) {
      if (lat == null || lon == null) continue;
      const [x, y] = this.toScreen(lat, lon);
      g.beginPath(); g.arc(x, y, 11, 0, Math.PI * 2); g.lineWidth = 4; g.strokeStyle = col.view; g.stroke();
      g.beginPath(); g.arc(x, y, 11, 0, Math.PI * 2); g.lineWidth = 2; g.strokeStyle = col.ink; g.stroke();
    }
  }

  drawLabels(g, col) {
    if (!this.labels.length) return;
    const placed = [];
    g.textAlign = 'left'; g.textBaseline = 'middle'; g.lineJoin = 'round';
    for (const l of this.labels) {
      const [x, y] = this.toScreen(l.lat, l.lon);
      if (x < 0 || y < 0 || x > this.w || y > this.h) continue;
      const big = l.population >= 1e6, air = l.kind === 'airport';
      g.font = `${big ? 650 : 500} ${big ? 12.5 : 11.5}px ${col.font}`;
      const text = air ? `✈ ${l.name}` : l.name;
      const tw = g.measureText(text).width, box = [x + 4, y - 8, x + 10 + tw, y + 8];
      if (placed.some(p => box[0] < p[2] && box[2] > p[0] && box[1] < p[3] && box[3] > p[1])) continue;
      placed.push(box);
      if (!air) { g.beginPath(); g.arc(x, y, big ? 2.6 : 2, 0, Math.PI * 2); g.fillStyle = col.ink2; g.fill(); }
      g.lineWidth = 3.5; g.strokeStyle = col.bg; g.strokeText(text, x + 7, y);
      g.fillStyle = air ? col.ink2 : col.ink; g.fillText(text, x + 7, y);
    }
  }

  drawScale() {
    const el = this.host.querySelector('.gm-scale');
    const [lat] = this.toLatLon(this.w / 2, this.h / 2);
    const kmPerPx = Math.cos(lat * Math.PI / 180) * 2 * Math.PI * R_KM / this.scale();
    const target = kmPerPx * 90;
    const pow = 10 ** Math.floor(Math.log10(target));
    const nice = [1, 2, 5, 10].map(m => m * pow).filter(v => v <= target).pop() || pow;
    el.querySelector('i').style.width = `${Math.round(nice / kmPerPx)}px`;
    el.querySelector('span').textContent = nice >= 1 ? `${nice.toLocaleString()} km` : `${Math.round(nice * 1000)} m`;
  }

  // the names in view, asked for once the map stops moving
  wantLabels() {
    clearTimeout(this.labelTimer);
    this.labelTimer = setTimeout(async () => {
      const v = this.view(), key = [v.s, v.w, v.n, v.e].map(x => x.toFixed(3)).join(',');
      if (key === this.labelKey) return;
      this.labelKey = key;
      const seq = ++this.labelSeq;
      const limit = Math.max(12, Math.min(60, Math.round(this.w * this.h / 9000)));
      const q = new URLSearchParams({ s: v.s, w: v.w, n: v.n, e: v.e, limit });
      try {
        const r = await fetch(`/demo/api/places-labels?${q}`, { credentials: 'same-origin' });
        const j = await r.json();
        if (seq !== this.labelSeq || !j.labels) return;
        this.labels = j.labels; this.redraw();
      } catch { /* names are a nicety */ }
    }, 220);
  }
}
