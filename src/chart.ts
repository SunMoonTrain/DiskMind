import type { Item } from './api';
import { extOf, fmtSize } from './format';

export type ColorMode = 'dir' | 'type' | 'delta';
export type ViewMode = 'treemap' | 'sunburst';
export interface Rect { x: number; y: number; w: number; h: number }

interface TNode {
  it: Item;
  kids: TNode[];
  parent: TNode | null;
  depth: number;
  /** index of the depth-1 ancestor, used for hue */
  top: number;
  /** size change ratio of self or nearest tracked ancestor */
  ratio: number | null;
  inherited: boolean;
  r?: Rect;
  header?: boolean;
  framed?: boolean;
  a0?: number;
  a1?: number;
  ri?: number;
  ro?: number;
}

export interface ChartEvents {
  hover(it: Item | null, ev: MouseEvent | null): void;
  select(it: Item): void;
  open(it: Item): void;
  context(it: Item, ev: MouseEvent): void;
  up(): void;
}

type HSL = [number, number, number];
const TOP_HUES = [192, 266, 328, 158, 34, 214, 292, 96, 8, 178, 244, 52];
const TAU = Math.PI * 2;
// system-ui follows the user's Windows UI font (the message font), whatever it is set to.
const FONT = 'system-ui, "Segoe UI", "Microsoft YaHei UI", sans-serif';

const hsl = (c: HSL, a = 1) => `hsla(${c[0]},${c[1]}%,${c[2]}%,${a})`;

function hashHue(s: string): number {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619);
  return ((h >>> 0) % 360);
}

export class ChartHost {
  view: ViewMode = 'treemap';
  color: ColorMode = 'dir';
  private base: HTMLCanvasElement;
  private over: HTMLCanvasElement;
  private bctx: CanvasRenderingContext2D;
  private octx: CanvasRenderingContext2D;
  private w = 0;
  private h = 0;
  private dpr = 1;
  private root: TNode | null = null;
  private byId = new Map<number, TNode>();
  private drawn: TNode[] = [];
  private hoverId = NaN;
  private selId = NaN;
  private flashId = NaN;
  private anim = 0;
  private stripe: CanvasPattern | null = null;
  private sb = { cx: 0, cy: 0, r0: 0, R: 0 };

  constructor(private wrap: HTMLElement, private ev: ChartEvents) {
    this.base = document.createElement('canvas');
    this.over = document.createElement('canvas');
    this.base.className = 'c-base';
    this.over.className = 'c-over';
    wrap.append(this.base, this.over);
    this.bctx = this.base.getContext('2d')!;
    this.octx = this.over.getContext('2d')!;
    new ResizeObserver(() => this.resize()).observe(wrap);
    this.bindEvents();
  }

  get size() { return { w: this.w, h: this.h }; }

  private bindEvents() {
    const o = this.over;
    o.addEventListener('mousemove', (e) => {
      if (this.anim) return;
      const n = this.hit(e.offsetX, e.offsetY);
      const id = n ? n.it.id : NaN;
      if (id !== this.hoverId && !(isNaN(id) && isNaN(this.hoverId))) {
        this.hoverId = id;
        this.drawOverlay();
      }
      o.style.cursor = n && (n.it.dir || n.depth > 1 || (this.view === 'sunburst' && n.depth === 0)) ? 'pointer' : 'default';
      this.ev.hover(n ? n.it : null, e);
    });
    o.addEventListener('mouseleave', () => {
      this.hoverId = NaN;
      this.drawOverlay();
      this.ev.hover(null, null);
    });
    o.addEventListener('click', (e) => {
      const n = this.hit(e.offsetX, e.offsetY);
      if (!n) return;
      if (this.view === 'sunburst' && n.depth === 0) return this.ev.up();
      this.ev.select(n.it);
    });
    o.addEventListener('dblclick', (e) => {
      const n = this.hit(e.offsetX, e.offsetY);
      if (!n || n.depth === 0) return;
      // Files / aggregates open their containing folder (unless that is the current root).
      let t: TNode | null = n;
      if (!t.it.dir || t.it.other) t = t.parent && t.parent.depth > 0 ? t.parent : null;
      if (t) this.ev.open(t.it);
    });
    o.addEventListener('contextmenu', (e) => {
      e.preventDefault();
      const n = this.hit(e.offsetX, e.offsetY);
      if (n && n.it.id >= 0) this.ev.context(n.it, e);
    });
    o.addEventListener('mouseup', (e) => {
      if (e.button === 3) this.ev.up();
    });
  }

  resize() {
    const r = this.wrap.getBoundingClientRect();
    this.dpr = window.devicePixelRatio || 1;
    this.w = Math.max(1, Math.floor(r.width));
    this.h = Math.max(1, Math.floor(r.height));
    for (const c of [this.base, this.over]) {
      c.width = Math.floor(this.w * this.dpr);
      c.height = Math.floor(this.h * this.dpr);
      c.style.width = `${this.w}px`;
      c.style.height = `${this.h}px`;
    }
    this.bctx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
    this.octx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
    if (this.root) {
      this.layout();
      this.renderBase();
      this.drawOverlay();
    }
  }

  // ---------------------------------------------------------------- data

  setData(items: Item[], anim: { kind: 'in' | 'out'; rect?: Rect | null; childId?: number } | null = null) {
    if (!anim) this.stopAnim();
    const old = this.root && anim ? this.snapshot() : null;
    this.build(items);
    this.layout();
    this.renderBase();
    this.hoverId = NaN;
    if (old && anim) {
      const rect = anim.kind === 'in' ? anim.rect : anim.childId !== undefined ? this.rectOf(anim.childId) : null;
      this.animate(old, this.snapshot(), rect ?? this.centerRect(), anim.kind);
    } else {
      this.drawOverlay();
    }
  }

  setColor(color: ColorMode) {
    this.color = color;
    if (!this.root) return;
    this.computeRatios();
    this.renderBase();
    this.drawOverlay();
  }

  /** Switch view with the data fetched for the new view, so there is a single transition. */
  switchView(view: ViewMode, items: Item[] | null) {
    const old = this.root ? this.snapshot() : null;
    this.view = view;
    if (items) this.build(items);
    if (!this.root) return;
    this.layout();
    this.renderBase();
    this.hoverId = NaN;
    if (old) this.crossfade(old);
    else this.drawOverlay();
  }

  setSelected(id: number) {
    this.selId = id;
    this.drawOverlay();
  }

  flash(id: number) {
    this.flashId = id;
    this.drawOverlay();
  }

  rectOf(id: number): Rect | null {
    const n = this.byId.get(id);
    if (!n) return null;
    if (this.view === 'treemap') return n.r ?? null;
    if (n.a0 === undefined) return null;
    const m = (n.a0 + n.a1!) / 2;
    const rr = (n.ri! + n.ro!) / 2;
    const x = this.sb.cx + Math.cos(m) * rr;
    const y = this.sb.cy + Math.sin(m) * rr;
    return { x: x - 30, y: y - 20, w: 60, h: 40 };
  }

  private centerRect(): Rect {
    return { x: this.w * 0.4, y: this.h * 0.4, w: this.w * 0.2, h: this.h * 0.2 };
  }

  private build(items: Item[]) {
    this.byId.clear();
    this.root = null;
    for (const it of items) {
      const p = it.parent >= 0 ? this.byId.get(it.parent) ?? null : null;
      const n: TNode = { it, kids: [], parent: p, depth: p ? p.depth + 1 : 0, top: 0, ratio: null, inherited: false };
      if (!p) {
        if (!this.root) this.root = n;
        else continue;
      } else {
        p.kids.push(n);
        n.top = n.depth === 1 ? p.kids.length - 1 : p.top;
      }
      this.byId.set(it.id, n);
    }
    for (const n of this.byId.values()) n.kids.sort((a, b) => b.it.size - a.it.size);
    this.computeRatios();
  }

  private computeRatios() {
    const walk = (n: TNode, inh: number | null) => {
      const it = n.it;
      if (it.prev !== null) {
        n.ratio = it.prev === 0 ? (it.size > 0 ? Infinity : 0) : (it.size - it.prev) / Math.max(it.prev, it.size);
        n.inherited = false;
      } else {
        n.ratio = inh;
        n.inherited = true;
      }
      for (const k of n.kids) walk(k, n.ratio);
    };
    if (this.root) walk(this.root, null);
  }

  // ---------------------------------------------------------------- colors

  private colorOf(n: TNode): HSL {
    const it = n.it;
    if (it.other) return [220, 12, 30];
    switch (this.color) {
      case 'dir': {
        const h = TOP_HUES[n.top % TOP_HUES.length];
        if (n.depth === 0) return [210, 30, 40];
        return [h, 72 - Math.min(n.depth, 6) * 3, Math.max(30, 56 - (n.depth - 1) * 5)];
      }
      case 'type': {
        if (it.dir) return [218, 14, 34];
        const e = extOf(it.name);
        if (!e) return [220, 8, 42];
        return [hashHue(e), 68, 52];
      }
      case 'delta': {
        const r = n.ratio;
        if (r === null) return [218, 10, 26];
        const dim = n.inherited ? 0.7 : 1;
        if (r === Infinity) return [300, 78 * dim, 56 * dim];
        const a = Math.min(1, Math.abs(r) * 4);
        if (a < 0.02) return [215, 14, 32];
        return r > 0 ? [352, (40 + 50 * a) * dim, (30 + 26 * a) * dim] : [152, (35 + 45 * a) * dim, (26 + 22 * a) * dim];
      }
    }
  }

  private getStripe(): CanvasPattern | null {
    if (this.stripe) return this.stripe;
    const c = document.createElement('canvas');
    c.width = c.height = 8;
    const g = c.getContext('2d')!;
    g.fillStyle = '#1a2030';
    g.fillRect(0, 0, 8, 8);
    g.strokeStyle = '#2a3348';
    g.lineWidth = 2;
    g.beginPath();
    g.moveTo(-2, 10);
    g.lineTo(10, -2);
    g.stroke();
    this.stripe = this.bctx.createPattern(c, 'repeat');
    return this.stripe;
  }

  // ---------------------------------------------------------------- layout

  private layout() {
    this.drawn = [];
    if (!this.root) return;
    for (const n of this.byId.values()) {
      n.r = undefined;
      n.a0 = undefined;
    }
    if (this.view === 'treemap') this.layoutTM(this.root, { x: 0, y: 0, w: this.w, h: this.h });
    else this.layoutSB();
  }

  private layoutTM(n: TNode, r: Rect) {
    n.r = r;
    n.header = false;
    n.framed = false;
    this.drawn.push(n);
    const kids = n.kids.filter((k) => k.it.size > 0);
    if (!kids.length || r.w < 8 || r.h < 8) return;
    let inner: Rect;
    if (n.depth === 0) inner = { x: r.x + 2, y: r.y + 2, w: r.w - 4, h: r.h - 4 };
    else if (r.w > 64 && r.h > 40) {
      n.header = true;
      inner = { x: r.x + 3, y: r.y + 18, w: r.w - 6, h: r.h - 21 };
    } else inner = { x: r.x + 2, y: r.y + 2, w: r.w - 4, h: r.h - 4 };
    if (inner.w < 2 || inner.h < 2) return;
    n.framed = true;
    squarify(kids, inner, (k, rr) => this.layoutTM(k, rr));
  }

  private layoutSB() {
    const root = this.root!;
    let maxD = 0;
    for (const n of this.byId.values()) maxD = Math.max(maxD, n.depth);
    maxD = Math.max(1, Math.min(maxD, 7));
    const cx = this.w / 2;
    const cy = this.h / 2;
    const R = Math.max(40, Math.min(this.w, this.h) / 2 - 16);
    const r0 = R * 0.22;
    const ring = (R - r0) / maxD;
    this.sb = { cx, cy, r0, R };
    root.a0 = -Math.PI / 2;
    root.a1 = root.a0 + TAU;
    root.ri = 0;
    root.ro = r0;
    this.drawn.push(root);
    const rec = (n: TNode) => {
      const total = n.it.size;
      if (total <= 0) return;
      let a = n.a0!;
      const span = n.a1! - n.a0!;
      for (const k of n.kids) {
        const s = (k.it.size / total) * span;
        if (k.depth <= maxD && s * (r0 + k.depth * ring) > 1.2) {
          k.a0 = a;
          k.a1 = a + s;
          k.ri = r0 + (k.depth - 1) * ring + 1;
          k.ro = r0 + k.depth * ring;
          this.drawn.push(k);
          rec(k);
        } else {
          k.a0 = undefined;
        }
        a += s;
      }
    };
    rec(root);
  }

  // ---------------------------------------------------------------- render

  private renderBase() {
    const c = this.bctx;
    c.clearRect(0, 0, this.w, this.h);
    if (!this.root) return;
    if (this.view === 'treemap') this.renderTM(c);
    else this.renderSB(c);
  }

  private renderTM(c: CanvasRenderingContext2D) {
    c.textBaseline = 'middle';
    for (const n of this.drawn) {
      const r = n.r!;
      if (n.depth === 0) continue;
      const col = this.colorOf(n);
      const isFrame = n.framed;
      if (isFrame) {
        c.fillStyle = hsl([col[0], Math.min(40, col[1]), 9]);
        c.fillRect(r.x + 0.5, r.y + 0.5, r.w - 1, r.h - 1);
        c.strokeStyle = hsl([col[0], col[1], Math.max(28, col[2] - 10)], 0.55);
        c.lineWidth = 1;
        c.strokeRect(r.x + 0.5, r.y + 0.5, r.w - 1, r.h - 1);
        if (n.header) this.label(c, n, r.x + 6, r.y + 9.5, r.w - 12, hsl([col[0], 85, 78]), true);
      } else if (n.it.other) {
        c.fillStyle = this.getStripe() ?? '#1a2030';
        c.fillRect(r.x + 0.5, r.y + 0.5, Math.max(0, r.w - 1), Math.max(0, r.h - 1));
        if (r.w > 70 && r.h > 20) this.label(c, n, r.x + 6, r.y + Math.min(r.h / 2, 12), r.w - 12, 'rgba(180,190,210,.7)', false);
      } else {
        cushion(c, r, col);
        if (r.w > 46 && r.h > 18) {
          const two = r.h > 34;
          this.label(c, n, r.x + 6, r.y + (two ? 12 : r.h / 2), r.w - 12, 'rgba(255,255,255,.93)', false, two);
        }
      }
    }
  }

  private label(c: CanvasRenderingContext2D, n: TNode, x: number, y: number, maxW: number, color: string, header: boolean, twoLine = false) {
    c.font = `${header ? 600 : 500} 11px ${FONT}`;
    c.fillStyle = color;
    const size = fmtSize(n.it.size);
    if (header) {
      const sw = c.measureText(size).width;
      const name = fit(c, n.it.name, maxW - sw - 8);
      c.fillText(name, x, y);
      if (maxW > sw + 30) {
        c.font = `400 10.5px ${FONT}`;
        c.fillStyle = 'rgba(200,215,240,.55)';
        c.fillText(size, x + maxW - sw, y);
      }
      return;
    }
    c.shadowColor = 'rgba(0,0,0,.6)';
    c.shadowBlur = 3;
    c.fillText(fit(c, n.it.name, maxW), x, y);
    if (twoLine) {
      c.font = `400 10.5px ${FONT}`;
      c.fillStyle = 'rgba(255,255,255,.7)';
      c.fillText(fit(c, size, maxW), x, y + 14);
    }
    c.shadowBlur = 0;
  }

  private renderSB(c: CanvasRenderingContext2D) {
    const { cx, cy, r0, R } = this.sb;
    // ambient glow
    const g = c.createRadialGradient(cx, cy, r0 * 0.5, cx, cy, R * 1.1);
    g.addColorStop(0, 'rgba(76,201,240,.10)');
    g.addColorStop(1, 'rgba(76,201,240,0)');
    c.fillStyle = g;
    c.fillRect(0, 0, this.w, this.h);
    for (const n of this.drawn) {
      if (n.depth === 0) continue;
      const col = this.colorOf(n);
      arcPath(c, cx, cy, n.ri!, n.ro!, n.a0!, n.a1!);
      const gr = c.createRadialGradient(cx, cy, n.ri!, cx, cy, n.ro!);
      gr.addColorStop(0, hsl([col[0], col[1], Math.min(80, col[2] + 10)]));
      gr.addColorStop(1, hsl([col[0], col[1], Math.max(10, col[2] - 10)]));
      c.fillStyle = n.it.other ? this.getStripe() ?? '#1a2030' : gr;
      c.fill();
      c.strokeStyle = '#06080d';
      c.lineWidth = 1;
      c.stroke();
    }
    // labels on wide arcs
    c.textBaseline = 'middle';
    c.font = `500 11px ${FONT}`;
    for (const n of this.drawn) {
      if (n.depth === 0 || n.it.other) continue;
      const mid = (n.a0! + n.a1!) / 2;
      const rm = (n.ri! + n.ro!) / 2;
      const arcLen = (n.a1! - n.a0!) * rm;
      const ringW = n.ro! - n.ri! - 8;
      if (arcLen < 14 || ringW < 30) continue;
      c.save();
      c.translate(cx + Math.cos(mid) * rm, cy + Math.sin(mid) * rm);
      let rot = mid;
      const flip = Math.cos(mid) < 0;
      if (flip) rot += Math.PI;
      c.rotate(rot);
      c.textAlign = 'center';
      c.fillStyle = 'rgba(255,255,255,.92)';
      c.shadowColor = 'rgba(0,0,0,.7)';
      c.shadowBlur = 3;
      c.fillText(fit(c, n.it.name, ringW), 0, 0);
      c.restore();
    }
    // center disc
    const root = this.root!;
    const cg = c.createRadialGradient(cx, cy - r0 * 0.3, 0, cx, cy, r0);
    cg.addColorStop(0, '#1b2438');
    cg.addColorStop(1, '#0a0e17');
    c.beginPath();
    c.arc(cx, cy, r0 - 2, 0, TAU);
    c.fillStyle = cg;
    c.fill();
    c.strokeStyle = 'rgba(76,201,240,.45)';
    c.lineWidth = 1.5;
    c.stroke();
    c.textAlign = 'center';
    c.fillStyle = '#e8eef8';
    c.font = `600 ${Math.max(14, Math.min(26, r0 / 3.4))}px ${FONT}`;
    c.fillText(fmtSize(root.it.size), cx, cy + 2);
    c.font = `500 12px ${FONT}`;
    c.fillStyle = 'rgba(180,195,220,.75)';
    c.fillText(fit(c, root.it.name, r0 * 1.6), cx, cy - r0 * 0.36);
    c.font = `400 11px ${FONT}`;
    c.fillStyle = 'rgba(140,155,180,.7)';
    c.fillText('点击返回上级', cx, cy + r0 * 0.38);
    c.textAlign = 'left';
  }

  drawOverlay() {
    if (this.anim) return;
    const c = this.octx;
    c.clearRect(0, 0, this.w, this.h);
    if (!this.root) return;
    const mark = (id: number, kind: 'hover' | 'sel' | 'flash') => {
      const n = this.byId.get(id);
      if (!n || n.depth === 0) return;
      c.save();
      if (kind === 'sel') {
        c.strokeStyle = '#ffffff';
        c.setLineDash([5, 3]);
        c.lineWidth = 1.5;
      } else {
        c.strokeStyle = kind === 'flash' ? '#ffd166' : '#5ee7ff';
        c.shadowColor = c.strokeStyle;
        c.shadowBlur = 14;
        c.lineWidth = 2;
        c.fillStyle = 'rgba(255,255,255,.10)';
      }
      if (this.view === 'treemap') {
        const r = n.r;
        if (!r) return c.restore();
        if (kind !== 'sel') c.fillRect(r.x, r.y, r.w, r.h);
        c.strokeRect(r.x + 1, r.y + 1, r.w - 2, r.h - 2);
      } else {
        if (n.a0 === undefined) return c.restore();
        arcPath(c, this.sb.cx, this.sb.cy, n.ri!, n.ro!, n.a0, n.a1!);
        if (kind !== 'sel') c.fill();
        c.stroke();
      }
      c.restore();
    };
    if (!isNaN(this.selId)) mark(this.selId, 'sel');
    if (!isNaN(this.flashId)) mark(this.flashId, 'flash');
    if (!isNaN(this.hoverId)) mark(this.hoverId, 'hover');
  }

  // ---------------------------------------------------------------- hit test

  private hit(x: number, y: number): TNode | null {
    if (this.view === 'treemap') {
      for (let i = this.drawn.length - 1; i >= 0; i--) {
        const n = this.drawn[i];
        const r = n.r!;
        if (n.depth > 0 && x >= r.x && y >= r.y && x < r.x + r.w && y < r.y + r.h) return n;
      }
      return null;
    }
    const { cx, cy, r0 } = this.sb;
    const dx = x - cx;
    const dy = y - cy;
    const rr = Math.hypot(dx, dy);
    if (rr < r0) return this.root;
    let a = Math.atan2(dy, dx);
    if (a < -Math.PI / 2) a += TAU;
    for (let i = this.drawn.length - 1; i >= 1; i--) {
      const n = this.drawn[i];
      if (rr >= n.ri! - 1 && rr <= n.ro! && a >= n.a0! && a < n.a1!) return n;
    }
    return null;
  }

  // ---------------------------------------------------------------- animation

  private snapshot(): HTMLCanvasElement {
    const s = document.createElement('canvas');
    s.width = this.base.width;
    s.height = this.base.height;
    s.getContext('2d')!.drawImage(this.base, 0, 0);
    return s;
  }

  private stopAnim() {
    if (!this.anim) return;
    cancelAnimationFrame(this.anim);
    this.anim = 0;
    this.base.style.opacity = '1';
  }

  private run(D: number, frame: (e: number) => void, hideBase = true) {
    cancelAnimationFrame(this.anim);
    const t0 = performance.now();
    // Paint the first frame before hiding the base so no blank frame is shown in between.
    this.octx.clearRect(0, 0, this.w, this.h);
    frame(0);
    if (hideBase) this.base.style.opacity = '0';
    const step = (now: number) => {
      const t = Math.min(1, (now - t0) / D);
      const e = 1 - Math.pow(1 - t, 3);
      this.octx.clearRect(0, 0, this.w, this.h);
      frame(e);
      if (t < 1) this.anim = requestAnimationFrame(step);
      else {
        this.anim = 0;
        this.base.style.opacity = '1';
        this.drawOverlay();
      }
    };
    this.anim = requestAnimationFrame(step);
  }

  private animate(oldImg: HTMLCanvasElement, newImg: HTMLCanvasElement, r: Rect, kind: 'in' | 'out') {
    if (r.w < 1 || r.h < 1) r = this.centerRect();
    this.run(380, (e) => {
      if (kind === 'in') this.zoomFrame(oldImg, newImg, r, e);
      else this.zoomFrame(newImg, oldImg, r, 1 - e);
    });
  }

  /** `outer`'s rect r expands toward full view while `inner` is drawn into that moving rect. */
  private zoomFrame(outer: HTMLCanvasElement, inner: HTMLCanvasElement, r: Rect, e: number) {
    const c = this.octx;
    const W = this.w;
    const H = this.h;
    const R = { x: r.x * (1 - e), y: r.y * (1 - e), w: r.w + (W - r.w) * e, h: r.h + (H - r.h) * e };
    const sx = R.w / r.w;
    const sy = R.h / r.h;
    c.globalAlpha = Math.max(0, 1 - e * 1.2);
    c.drawImage(outer, 0, 0, outer.width, outer.height, R.x - r.x * sx, R.y - r.y * sy, W * sx, H * sy);
    c.globalAlpha = Math.min(1, e * 1.4);
    c.drawImage(inner, 0, 0, inner.width, inner.height, R.x, R.y, R.w, R.h);
    c.globalAlpha = 1;
  }

  /**
   * The new view is already on the (visible) base canvas; only the old frame fades out
   * on top. No snapshot of the freshly drawn view is needed, so a slow GPU rasterization
   * of a heavy treemap can't surface as blank or stale frames.
   */
  private crossfade(oldImg: HTMLCanvasElement) {
    const bg = getComputedStyle(this.wrap).backgroundColor;
    this.run(
      260,
      (e) => {
        const c = this.octx;
        const s = 1 + 0.04 * e;
        c.globalAlpha = 1 - e;
        c.fillStyle = bg;
        c.fillRect(0, 0, this.w, this.h);
        c.drawImage(oldImg, 0, 0, oldImg.width, oldImg.height, (this.w * (1 - s)) / 2, (this.h * (1 - s)) / 2, this.w * s, this.h * s);
        c.globalAlpha = 1;
      },
      false,
    );
  }
}

// ------------------------------------------------------------------ helpers

function cushion(c: CanvasRenderingContext2D, r: Rect, col: HSL) {
  if (r.w < 4 || r.h < 4) {
    c.fillStyle = hsl(col);
    c.fillRect(r.x, r.y, Math.max(1, r.w - 0.5), Math.max(1, r.h - 0.5));
    return;
  }
  const g = c.createRadialGradient(r.x + r.w * 0.32, r.y + r.h * 0.28, 0, r.x + r.w * 0.5, r.y + r.h * 0.5, Math.hypot(r.w, r.h) * 0.62);
  g.addColorStop(0, hsl([col[0], col[1], Math.min(84, col[2] + 20)]));
  g.addColorStop(0.5, hsl(col));
  g.addColorStop(1, hsl([col[0], Math.min(100, col[1] + 6), Math.max(6, col[2] - 24)]));
  c.fillStyle = g;
  c.fillRect(r.x + 0.5, r.y + 0.5, r.w - 1, r.h - 1);
}

function arcPath(c: CanvasRenderingContext2D, cx: number, cy: number, ri: number, ro: number, a0: number, a1: number) {
  c.beginPath();
  c.arc(cx, cy, ro, a0, a1);
  c.arc(cx, cy, ri, a1, a0, true);
  c.closePath();
}

function fit(c: CanvasRenderingContext2D, s: string, maxW: number): string {
  if (maxW <= 8) return '';
  if (c.measureText(s).width <= maxW) return s;
  let lo = 0;
  let hi = s.length;
  while (lo < hi) {
    const m = (lo + hi + 1) >> 1;
    if (c.measureText(s.slice(0, m) + '…').width <= maxW) lo = m;
    else hi = m - 1;
  }
  return lo > 0 ? s.slice(0, lo) + '…' : '';
}

/** Squarified treemap (Bruls et al.). `nodes` must be sorted by size desc. */
function squarify(nodes: TNode[], rect: Rect, place: (n: TNode, r: Rect) => void) {
  let { x, y, w, h } = rect;
  const total = nodes.reduce((s, n) => s + n.it.size, 0);
  if (total <= 0) return;
  const scale = (w * h) / total;
  let i = 0;
  while (i < nodes.length) {
    const short = Math.min(w, h);
    if (short <= 0) break;
    let rowSum = 0;
    let best = Infinity;
    let j = i;
    const maxA = nodes[i].it.size * scale;
    while (j < nodes.length) {
      const a = nodes[j].it.size * scale;
      const s = rowSum + a;
      const worst = Math.max((short * short * maxA) / (s * s), (s * s) / (short * short * a));
      if (j > i && worst > best) break;
      best = worst;
      rowSum = s;
      j++;
    }
    const thick = rowSum / short;
    if (w >= h) {
      let yy = y;
      for (let k = i; k < j; k++) {
        const hh = (nodes[k].it.size * scale) / thick;
        place(nodes[k], { x, y: yy, w: thick, h: hh });
        yy += hh;
      }
      x += thick;
      w -= thick;
    } else {
      let xx = x;
      for (let k = i; k < j; k++) {
        const ww = (nodes[k].it.size * scale) / thick;
        place(nodes[k], { x: xx, y, w: ww, h: thick });
        xx += ww;
      }
      y += thick;
      h -= thick;
    }
    i = j;
  }
}
