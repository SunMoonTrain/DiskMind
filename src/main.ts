import './style.css';
import { api, onDone, onEvent, onProgress } from './api';
import type { Change, Changes, CleanupRequest, DeleteItem, DeleteRequest, DriveInfo, Item, NodeInfo, Progress, ScanDone, SnapMeta, Suggestion } from './api';
import { ChartHost, ColorMode, ViewMode } from './chart';
import { esc, extOf, fmtAgo, fmtDelta, fmtDuration, fmtNum, fmtPct, fmtShortTime, fmtSize, fmtTime } from './format';

const $ = <T extends HTMLElement = HTMLElement>(s: string) => document.querySelector(s) as T;

const ICON = {
  dir: '<svg viewBox="0 0 24 24"><path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/></svg>',
  file: '<svg viewBox="0 0 24 24"><path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"/><path d="M14 3v5h5"/></svg>',
  other: '<svg viewBox="0 0 24 24"><circle cx="6" cy="12" r="1.5"/><circle cx="12" cy="12" r="1.5"/><circle cx="18" cy="12" r="1.5"/></svg>',
  open: '<svg viewBox="0 0 24 24"><path d="M9 18l6-6-6-6"/></svg>',
  explorer: '<svg viewBox="0 0 24 24"><path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/><path d="M12 11v5M9.5 13.5h5"/></svg>',
  copy: '<svg viewBox="0 0 24 24"><rect x="8" y="8" width="12" height="12" rx="2"/><path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2"/></svg>',
  scan: '<svg viewBox="0 0 24 24"><path d="M20 12a8 8 0 1 1-2.3-5.7M20 4v5h-5"/></svg>',
};
const TOP_HUES = [192, 266, 328, 158, 34, 214, 292, 96, 8, 178, 244, 52];

type Tab = 'changes' | 'files' | 'types' | 'history' | 'ai';

const st = {
  admin: false,
  mft: loadMftPref(),
  rootId: -1,
  rootPath: '',
  mode: '',
  lastDone: null as ScanDone | null,
  cur: -1,
  info: null as NodeInfo | null,
  children: [] as Item[],
  childTotal: 0,
  sel: -1,
  view: 'treemap' as ViewMode,
  color: 'dir' as ColorMode,
  tab: 'changes' as Tab,
  sort: 'size' as 'size' | 'delta',
  changes: null as Changes | null,
  changeSide: 'grown' as 'grown' | 'shrunk',
  history: [] as SnapMeta[],
  navSeq: 0,
  suggestions: [] as Suggestion[],
};

// ------------------------------------------------------------------ chart

const chart = new ChartHost($('#chartWrap'), {
  hover: (it, ev) => showTip(it, ev),
  select: (it) => select(it.id, false),
  open: (it) => navigate(it.id, 'in'),
  context: (it, ev) => openMenu(it.id, it.dir, ev.clientX, ev.clientY),
  up: () => goUp(),
});

function chartQuery(): [number, number] {
  const { w, h } = chart.size;
  if (st.view === 'sunburst') return [3500, 0.0006];
  return [6000, 14 / Math.max(1, w * h)];
}

// ------------------------------------------------------------------ toast / tooltip

let toastTimer = 0;
/** `ms` 0 keeps the toast up until the next one. */
function toast(msg: string, err = false, ms = err ? 5000 : 2600) {
  const t = $('#toast');
  t.textContent = msg;
  t.className = `toast show${err ? ' err' : ''}`;
  clearTimeout(toastTimer);
  if (ms) toastTimer = window.setTimeout(() => (t.className = 'toast'), ms);
}

function deltaBadge(size: number, prev: number | null, removed = false): string {
  if (removed) return `<span class="delta gone">已删除</span>`;
  if (prev === null) return '';
  if (prev === 0 && size > 0) return `<span class="delta new">新增</span>`;
  const d = size - prev;
  if (Math.abs(d) < 1 << 20) return '';
  return `<span class="delta ${d > 0 ? 'up' : 'down'}">${d > 0 ? '▲' : '▼'} ${fmtSize(Math.abs(d))}</span>`;
}

function showTip(it: Item | null, ev: MouseEvent | null) {
  const tip = $('#tooltip');
  if (!it || !ev || !st.info) return tip.classList.add('hidden');
  const share = st.info.size > 0 ? it.size / st.info.size : 0;
  let delta = '';
  if (it.prev !== null) {
    const d = it.size - it.prev;
    const cls = it.prev === 0 ? 'new' : d > 0 ? 'up' : d < 0 ? 'down' : '';
    delta = `<div class="kv"><span>较基线</span><b class="${cls}" style="color:var(--${cls || 'text'})">${it.prev === 0 ? '新增' : fmtDelta(d)}</b></div>`;
  }
  tip.innerHTML = `
    <div class="t">${esc(it.name)}</div>
    <div class="kv"><span>大小</span><b>${fmtSize(it.size)}</b></div>
    <div class="kv"><span>占当前</span><b>${fmtPct(share)}</b></div>
    ${it.dir || it.other ? `<div class="kv"><span>文件数</span><b>${fmtNum(it.files)}</b></div>` : ''}
    ${it.mtime ? `<div class="kv"><span>${it.dir || it.other ? '最近修改' : '修改时间'}</span><b>${fmtTime(it.mtime * 1000).slice(0, 10)} · ${fmtAgo(it.mtime * 1000)}</b></div>` : ''}
    ${delta}
    <div class="hint">${it.dir ? '双击进入 · 右键更多' : it.other ? `已合并 ${fmtNum(it.other)} 个较小项目` : '右键可在资源管理器中显示'}</div>`;
  tip.classList.remove('hidden');
  const r = tip.getBoundingClientRect();
  let x = ev.clientX + 16;
  let y = ev.clientY + 16;
  if (x + r.width > innerWidth - 8) x = ev.clientX - r.width - 16;
  if (y + r.height > innerHeight - 8) y = ev.clientY - r.height - 16;
  tip.style.left = `${x}px`;
  tip.style.top = `${y}px`;
}

// ------------------------------------------------------------------ context menu

async function openMenu(id: number, dir: boolean, x: number, y: number) {
  const path = await api.getPath(id).catch(() => '');
  if (!path) return;
  const m = $('#ctxMenu');
  m.innerHTML = `
    <div class="ttl">${esc(path)}</div>
    ${dir && id !== st.cur ? `<button data-a="open">${ICON.open}进入此文件夹</button>` : ''}
    <button data-a="reveal">${ICON.explorer}在资源管理器中显示</button>
    ${dir ? `<button data-a="explore">${ICON.dir}用资源管理器打开</button>` : ''}
    <button data-a="copy">${ICON.copy}复制路径</button>
    ${dir ? `<hr><button data-a="rescan">${ICON.scan}单独扫描此文件夹</button>` : ''}`;
  m.classList.remove('hidden');
  const r = m.getBoundingClientRect();
  m.style.left = `${Math.min(x, innerWidth - r.width - 8)}px`;
  m.style.top = `${Math.min(y, innerHeight - r.height - 8)}px`;
  m.onclick = async (e) => {
    const a = (e.target as HTMLElement).closest('button')?.dataset.a;
    if (!a) return;
    closeMenu();
    if (a === 'open') navigate(id, 'in');
    else if (a === 'reveal') api.revealPath(path, true).catch((err) => toast(String(err), true));
    else if (a === 'explore') api.revealPath(path, false).catch((err) => toast(String(err), true));
    else if (a === 'copy') {
      await navigator.clipboard.writeText(path).catch(() => {});
      toast('已复制路径');
    } else if (a === 'rescan') startScan(path);
  };
}
const closeMenu = () => $('#ctxMenu').classList.add('hidden');
document.addEventListener('mousedown', (e) => {
  if (!(e.target as HTMLElement).closest('#ctxMenu')) closeMenu();
});

// ------------------------------------------------------------------ MFT toggle

function loadMftPref(): boolean {
  try {
    return localStorage.getItem('mft') !== '0';
  } catch {
    return true;
  }
}

function renderMftToggle() {
  const b = $('#mftToggle');
  b.className = `chip ${st.mft ? 'ok' : 'off'}`;
  $('#mftLabel').textContent = `MFT 极速：${st.mft ? '开' : '关'}`;
  b.title = st.mft
    ? st.admin
      ? '扫描整个 NTFS 磁盘时直接读取主文件表（当前已是管理员，无需授权）。点击关闭'
      : '扫描整个 NTFS 磁盘时直接读取主文件表，速度快 10 倍以上。\n每次扫描会弹出一次管理员授权，只授权给扫描子进程；界面本身不以管理员运行，不影响其他软件的全局快捷键。\n点击关闭'
    : '已关闭：使用普通目录遍历，不会弹出管理员授权。点击开启';
}

// ------------------------------------------------------------------ welcome

async function loadWelcome() {
  const drives = await api.listDrives().catch(() => [] as DriveInfo[]);
  const box = $('#drives');
  box.innerHTML = drives
    .map((d, i) => {
      const used = d.total - d.free;
      const pct = d.total ? used / d.total : 0;
      const C = 2 * Math.PI * 30;
      const last = d.lastScan
        ? `<span>上次 ${fmtAgo(d.lastScan)} · ${fmtSize(d.lastTotal ?? 0)}</span>`
        : `<span>尚未扫描</span>`;
      return `
      <div class="drive${pct > 0.9 ? ' hot' : ''}" data-root="${esc(d.root)}" style="animation-delay:${i * 60}ms">
        <div class="top">
          <div class="ringbox">
            <svg viewBox="0 0 72 72"><circle class="bg" cx="36" cy="36" r="30"/><circle class="fg" cx="36" cy="36" r="30" stroke-dasharray="${C}" stroke-dashoffset="${C}" data-off="${C * (1 - pct)}"/></svg>
            <div class="pct">${Math.round(pct * 100)}%</div>
          </div>
          <div>
            <div class="letter">${esc(d.root.slice(0, 2))}</div>
            <div class="meta">${esc(d.label || (d.kind === 'removable' ? '可移动磁盘' : '本地磁盘'))} · ${esc(d.fs || '—')}</div>
          </div>
        </div>
        <div class="usage"><span>已用 <b>${fmtSize(used)}</b></span><span>共 ${fmtSize(d.total)}</span></div>
        <div class="usage" style="margin-top:4px"><span>可用 ${fmtSize(d.free)}</span><span>${d.fs === 'NTFS' && st.mft ? '⚡ MFT 极速' : ''}</span></div>
        <div class="last">${last}<span class="go">扫描 →</span></div>
      </div>`;
    })
    .join('');
  requestAnimationFrame(() =>
    box.querySelectorAll<SVGCircleElement>('.fg').forEach((c) => c.setAttribute('stroke-dashoffset', c.dataset.off!)),
  );
  box.querySelectorAll<HTMLElement>('.drive').forEach((el) => {
    el.onclick = () => startScan(el.dataset.root!);
    el.onmousemove = (e) => el.style.setProperty('--mx', `${e.offsetX}px`);
  });
}

function showWelcome() {
  $('#main').classList.add('hidden');
  $('#welcome').classList.remove('hidden');
  loadWelcome();
}

// ------------------------------------------------------------------ scanning

async function startScan(path: string) {
  closeMenu();
  try {
    resetOverlay(path);
    $('#scanOverlay').classList.remove('hidden');
    await api.startScan(path, st.mft);
  } catch (e) {
    $('#scanOverlay').classList.add('hidden');
    toast(String(e), true);
  }
}

function resetOverlay(path: string) {
  $('#scanPath').textContent = path;
  $('#scanBig').textContent = '0';
  $('#sDirs').textContent = '0';
  $('#sBytes').textContent = '0 B';
  $('#sTime').textContent = '0 s';
  $('#scanPhase').textContent = '准备中…';
  $('#pctRing').style.strokeDashoffset = '578';
}

function onScanProgress(p: Progress) {
  $('#scanPath').textContent = p.path;
  $('#scanBig').textContent = fmtNum(p.files);
  $('#sDirs').textContent = fmtNum(p.dirs);
  $('#sBytes').textContent = fmtSize(p.bytes);
  $('#sTime').textContent = fmtDuration(p.elapsedMs);
  const pct = p.percent >= 0 ? ` ${Math.round(p.percent * 100)}%` : '';
  $('#scanPhase').textContent = `${p.mode === 'MFT' ? '⚡ MFT 直读' : '目录遍历'} · ${p.phase}${pct}`;
  $('#pctRing').style.strokeDashoffset = String(578 * (1 - (p.percent >= 0 ? p.percent : 0)));
}

async function onScanDone(d: ScanDone) {
  $('#scanOverlay').classList.add('hidden');
  if (!d.ok) {
    if (d.error !== '已取消') toast(d.error ?? '扫描失败', true);
    return;
  }
  if (d.warning) toast(d.warning, true);
  st.lastDone = d;
  st.rootId = d.root;
  st.rootPath = d.rootPath;
  st.mode = d.mode;
  st.sel = -1;
  st.changeSide = 'grown';
  $('#welcome').classList.add('hidden');
  $('#main').classList.remove('hidden');
  await new Promise((r) => requestAnimationFrame(r));
  chart.resize();
  await Promise.all([loadChanges(), loadHistory()]);
  await navigate(d.root, null);
  renderStatus();
  if (d.baseline && st.changes) {
    const td = st.changes.totalDelta ?? 0;
    toast(`与 ${fmtAgo(d.baseline.time)} 的扫描相比：${fmtDelta(td)}`);
  }
}

// ------------------------------------------------------------------ navigation

async function navigate(id: number, anim: 'in' | 'out' | null, selectAfter = -1) {
  closeMenu();
  const seq = ++st.navSeq;
  const prev = st.cur;
  const rect = anim === 'in' ? chart.rectOf(id) : null;
  const [budget, minFrac] = chartQuery();
  try {
    const [info, kids, items] = await Promise.all([
      api.getInfo(id),
      api.getChildren(id, 600),
      api.getLayout(id, budget, minFrac),
    ]);
    if (seq !== st.navSeq) return;
    st.cur = id;
    st.info = info;
    st.children = kids.items;
    st.childTotal = kids.total;
    st.sel = selectAfter;
    renderCrumbs();
    renderHead();
    renderList();
    chart.setSelected(selectAfter);
    chart.setData(items, anim ? { kind: anim, rect, childId: prev } : null);
    if (st.tab === 'files' || st.tab === 'types') renderTab();
    if (selectAfter >= 0) {
      chart.flash(selectAfter);
      setTimeout(() => chart.flash(NaN), 1400);
      $(`.row[data-id="${selectAfter}"]`)?.scrollIntoView({ block: 'center' });
    }
  } catch (e) {
    toast(String(e), true);
  }
}

function goUp() {
  if (!st.info || st.info.parent < 0 || st.cur === st.rootId) return;
  navigate(st.info.parent, 'out');
}

/** Jump to a node: open its parent folder and highlight it. */
async function reveal(id: number, parent: number) {
  if (id < 0) return;
  const info = await api.getInfo(id).catch(() => null);
  if (!info) return;
  if (info.dir && info.files > 0) {
    // For folders, open the folder itself for a closer look, selecting nothing.
    if (st.cur === parent || parent < 0) return navigate(id, 'in');
    return navigate(id, null);
  }
  navigate(parent >= 0 ? parent : st.rootId, null, id);
}

function select(id: number, scroll = true) {
  st.sel = id;
  chart.setSelected(id);
  document.querySelectorAll('.row.sel').forEach((r) => r.classList.remove('sel'));
  const row = $(`.row[data-id="${id}"]`);
  if (row) {
    row.classList.add('sel');
    if (scroll) row.scrollIntoView({ block: 'nearest' });
  }
}

// ------------------------------------------------------------------ rendering

function renderCrumbs() {
  const c = st.info!.crumbs;
  $('#crumbs').innerHTML = c
    .map(
      (b, i) =>
        `${i ? '<span class="sep">›</span>' : ''}<button data-id="${b.id}" class="${i === c.length - 1 ? 'last' : ''}" title="${esc(b.name)}">${esc(b.name)}</button>`,
    )
    .join('');
  $('#crumbs').querySelectorAll<HTMLElement>('button').forEach((b) => {
    b.onclick = () => {
      const id = +b.dataset.id!;
      if (id !== st.cur) navigate(id, 'out');
    };
  });
  ($('#upBtn') as HTMLButtonElement).disabled = st.cur === st.rootId;
}

function renderHead() {
  const i = st.info!;
  const rootShare = st.lastDone && st.lastDone.total ? i.size / st.lastDone.total : 1;
  $('#curHead').innerHTML = `
    <div class="nm" title="${esc(i.path)}">${esc(i.name)}</div>
    <div class="sz"><b>${fmtSize(i.size)}</b>${deltaBadge(i.size, i.prev)}</div>
    <div class="sub">${fmtNum(i.files)} 个文件 · ${fmtNum(st.childTotal)} 个子项${st.cur !== st.rootId ? ` · 占全部 ${fmtPct(rootShare)}` : ''}</div>`;
}

function renderList() {
  const total = st.info?.size || 1;
  let items = st.children;
  if (st.sort === 'delta') {
    const dv = (x: Item) => (x.prev === null ? -Infinity : x.size - x.prev);
    items = [...items].sort((a, b) => dv(b) - dv(a));
  }
  const max = items.reduce((m, x) => Math.max(m, x.size), 1);
  $('#list').innerHTML =
    items
      .map((it, k) => {
        const hue = TOP_HUES[st.children.indexOf(it) % TOP_HUES.length];
        const color = it.dir ? `hsl(${hue},75%,60%)` : `hsl(${hashHue(extOf(it.name))},55%,60%)`;
        return `<div class="row${it.dir ? ' dirrow' : ''}${it.id === st.sel ? ' sel' : ''}" data-id="${it.id}" data-k="${k}" style="--c:${color}">
        <div class="bar" style="width:${((it.size / max) * 100).toFixed(1)}%;background:linear-gradient(90deg,${color},transparent)"></div>
        <span class="ico${it.dir ? ' dir' : ''}">${it.dir ? ICON.dir : ICON.file}</span>
        <span class="nm" title="${esc(it.name)}">${esc(it.name)}</span>
        ${deltaBadge(it.size, it.prev)}
        <span class="sz">${fmtSize(it.size)}</span>
        <span class="pc">${fmtPct(it.size / total)}</span>
      </div>`;
      })
      .join('') + (st.childTotal > items.length ? `<div class="more">还有 ${fmtNum(st.childTotal - items.length)} 个较小项目</div>` : '');
  const list = $('#list');
  list.onclick = (e) => {
    const row = (e.target as HTMLElement).closest<HTMLElement>('.row');
    if (row) select(+row.dataset.id!, false);
  };
  list.ondblclick = (e) => {
    const row = (e.target as HTMLElement).closest<HTMLElement>('.row');
    if (!row) return;
    const it = items[+row.dataset.k!];
    if (it.dir && it.hasChildren) navigate(it.id, 'in');
  };
  list.oncontextmenu = (e) => {
    e.preventDefault();
    const row = (e.target as HTMLElement).closest<HTMLElement>('.row');
    if (!row) return;
    const it = items[+row.dataset.k!];
    select(it.id, false);
    openMenu(it.id, it.dir, e.clientX, e.clientY);
  };
  list.onmouseover = (e) => {
    const row = (e.target as HTMLElement).closest<HTMLElement>('.row');
    chart.flash(row ? +row.dataset.id! : NaN);
  };
  list.onmouseleave = () => chart.flash(NaN);
}

function hashHue(s: string): number {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619);
  return (h >>> 0) % 360;
}

function renderLegend() {
  const lg = $('#legend');
  if (st.color === 'delta') {
    lg.innerHTML = st.changes?.baseline
      ? `<span>缩小</span><div class="grad"></div><span>增长</span><span><i class="sw" style="background:#d65cf0"></i>新增</span>`
      : `<span>暂无历史基线 —— 下次扫描后即可按变化着色</span>`;
    lg.classList.remove('hidden');
  } else lg.classList.add('hidden');
}

function renderStatus() {
  const d = st.lastDone;
  if (!d) return;
  const base = st.changes?.baseline;
  $('#status').innerHTML = `
    <span><i class="dot${d.mode === 'MFT' ? '' : ' walk'}"></i>${d.mode === 'MFT' ? 'MFT 直读' : '目录遍历'}</span>
    <span>扫描用时 <b>${fmtDuration(d.elapsedMs)}</b></span>
    <span><b>${fmtNum(d.files)}</b> 个文件</span>
    <span><b>${fmtNum(d.dirs)}</b> 个文件夹</span>
    <span>总计 <b>${fmtSize(d.total)}</b></span>
    <span class="sp"></span>
    <span>${base ? `对比基线：<b>${fmtTime(base.time)}</b>` : '首次扫描，已保存为基线'}</span>`;
}

// ------------------------------------------------------------------ right panel

async function loadChanges() {
  st.changes = await api.getChanges(150).catch(() => null);
}

async function loadHistory() {
  st.history = await api.getHistory().catch(() => []);
}

async function renderTab() {
  const body = $('#tabBody');
  const tab = st.tab;
  const cur = st.cur;
  if (tab === 'changes') return renderChanges(body);
  if (tab === 'history') return renderHistory(body);
  if (tab === 'ai') return renderSuggestions(body);
  if (tab === 'files') {
    const files = await api.getTopFiles(cur, 150).catch(() => []);
    if (st.tab !== tab || st.cur !== cur) return;
    const max = files[0]?.size || 1;
    body.innerHTML = files.length
      ? files
          .map(
            (f) => `<div class="crow" data-id="${f.id}" data-p="${f.parent}">
          <div class="l1"><span class="nm" title="${esc(f.name)}">${esc(f.name)}</span>${deltaBadge(f.size, f.prev)}<b>${fmtSize(f.size)}</b></div>
          <div class="l2"><span class="p" title="${esc(f.path)}"><bdi>${esc(f.path)}</bdi></span></div>
          <div class="meter"><i style="width:${(f.size / max) * 100}%;background:linear-gradient(90deg,#4cc9f0,#9b5cff)"></i></div>
        </div>`,
          )
          .join('')
      : `<div class="empty">没有文件</div>`;
    bindRevealRows(body);
    return;
  }
  if (tab === 'types') {
    const exts = await api.getExtensions(cur, 40).catch(() => []);
    if (st.tab !== tab || st.cur !== cur) return;
    const total = exts.reduce((s, e) => s + e.size, 0) || 1;
    const col = (e: string) => (e ? `hsl(${hashHue(e)},68%,56%)` : 'hsl(220,8%,50%)');
    const top = exts.slice(0, 10);
    const rest = total - top.reduce((s, e) => s + e.size, 0);
    const segs = [...top.map((e) => ({ v: e.size, c: col(e.ext) })), { v: rest, c: '#2a3346' }];
    let a = -Math.PI / 2;
    const arcs = segs
      .filter((s) => s.v > 0)
      .map((s) => {
        const span = (s.v / total) * Math.PI * 2;
        const p = donutArc(95, 95, 88, 58, a, a + Math.min(span, Math.PI * 2 - 0.0001));
        a += span;
        return `<path d="${p}" fill="${s.c}" stroke="#0d121c" stroke-width="1.5"/>`;
      })
      .join('');
    body.innerHTML = `
      <div class="donut"><svg viewBox="0 0 190 190">${arcs}
        <text x="95" y="90" text-anchor="middle" fill="#e6ecf6" font-size="17" font-weight="700">${fmtSize(total)}</text>
        <text x="95" y="110" text-anchor="middle" fill="#8792a8" font-size="11">${exts.length >= 40 ? '40+' : exts.length} 种类型</text></svg></div>
      ${exts
        .map(
          (e) => `<div class="erow"><i class="sw" style="background:${col(e.ext)}"></i>
          <span class="nm">${e.ext ? '.' + esc(e.ext) : '(无扩展名)'}</span>
          <span class="ct">${fmtNum(e.count)} 个</span>
          <span class="sz">${fmtSize(e.size)}</span></div>`,
        )
        .join('')}`;
  }
}

function donutArc(cx: number, cy: number, ro: number, ri: number, a0: number, a1: number): string {
  const p = (r: number, a: number) => `${(cx + r * Math.cos(a)).toFixed(2)},${(cy + r * Math.sin(a)).toFixed(2)}`;
  const large = a1 - a0 > Math.PI ? 1 : 0;
  return `M${p(ro, a0)} A${ro},${ro} 0 ${large} 1 ${p(ro, a1)} L${p(ri, a1)} A${ri},${ri} 0 ${large} 0 ${p(ri, a0)} Z`;
}

function bindRevealRows(body: HTMLElement) {
  body.querySelectorAll<HTMLElement>('.crow[data-id]').forEach((r) => {
    r.onclick = () => reveal(+r.dataset.id!, +r.dataset.p!);
    r.oncontextmenu = (e) => {
      e.preventDefault();
      if (+r.dataset.id! >= 0) openMenu(+r.dataset.id!, r.dataset.dir === '1', e.clientX, e.clientY);
    };
  });
}

function renderChanges(body: HTMLElement) {
  const ch = st.changes;
  if (!ch || !ch.baseline) {
    body.innerHTML = `<div class="empty"><div class="big">◔</div>这是第一次扫描这个位置。<br>本次结果已保存为基线，<br>下次扫描时会自动标出哪些文件夹变大了。</div>`;
    return;
  }
  const td = ch.totalDelta ?? 0;
  const options = st.history
    .slice(1)
    .map(
      (m) =>
        `<option value="${esc(m.file)}" ${m.file === ch.baseline!.file ? 'selected' : ''}>${fmtTime(m.time)} · ${fmtSize(m.total)}</option>`,
    )
    .join('');
  const list = st.changeSide === 'grown' ? ch.grown : ch.shrunk;
  const maxAbs = list.reduce((m, c) => Math.max(m, Math.abs(c.delta)), 1);
  body.innerHTML = `
    <div class="cmp-head">
      <div class="lbl"><span>对比基线（${fmtAgo(ch.baseline.time)}）</span></div>
      <select id="baseSel">${options}</select>
      <div class="tot"><b class="${td > 0 ? 'up' : td < 0 ? 'down' : ''}">${fmtDelta(td)}</b><span>${fmtSize(ch.baseline.total)} → ${fmtSize(st.lastDone?.total ?? 0)}</span></div>
      ${modeNote(ch.baseline.mode)}
    </div>
    <div class="subtabs">
      <button data-s="grown" class="g ${st.changeSide === 'grown' ? 'on' : ''}">▲ 变大 ${ch.grown.length}</button>
      <button data-s="shrunk" class="s ${st.changeSide === 'shrunk' ? 'on' : ''}">▼ 变小 / 删除 ${ch.shrunk.length}</button>
    </div>
    ${list.length ? list.map((c) => changeRow(c, maxAbs)).join('') : `<div class="empty">没有超过 1 MB 的变化</div>`}`;
  body.querySelectorAll<HTMLElement>('.subtabs button').forEach((b) => {
    b.onclick = () => {
      st.changeSide = b.dataset.s as 'grown' | 'shrunk';
      renderChanges(body);
    };
  });
  ($('#baseSel') as HTMLSelectElement).onchange = async (e) => {
    const file = (e.target as HTMLSelectElement).value;
    try {
      await api.setBaseline(file);
      await loadChanges();
      renderChanges(body);
      renderStatus();
      renderLegend();
      if (st.cur >= 0) navigate(st.cur, null);
    } catch (err) {
      toast(String(err), true);
    }
  };
  bindRevealRows(body);
}

function modeNote(baseMode: string): string {
  const cur = st.lastDone?.mode;
  if (!baseMode || !cur || baseMode === cur) return '';
  const name = (m: string) => (m === 'MFT' ? 'MFT 直读' : '目录遍历');
  return `<div class="mode-note">基线用的是${name(baseMode)}，本次是${name(cur)}。目录遍历读不到 System Volume Information 等系统目录，这部分会显示为新增或删除。</div>`;
}

function changeRow(c: Change, maxAbs: number): string {
  const up = c.delta > 0;
  const color = c.kind === 'new' ? 'var(--new)' : up ? 'var(--up)' : 'var(--down)';
  const badge =
    c.kind === 'removed'
      ? `<span class="delta gone">已删除</span>`
      : c.kind === 'new'
        ? `<span class="delta new">新增</span>`
        : '';
  return `<div class="crow${c.kind === 'removed' ? ' gone' : ''}" ${c.id >= 0 ? `data-id="${c.id}" data-p="${c.parent}" data-dir="${c.dir ? 1 : 0}"` : ''}>
    <div class="l1">
      <span class="ico" style="width:14px;color:${color}">${c.dir ? ICON.dir : ICON.file}</span>
      <span class="nm" title="${esc(c.name)}">${esc(c.name)}</span>${badge}
      <b style="color:${color};font-variant-numeric:tabular-nums">${fmtDelta(c.delta)}</b>
    </div>
    <div class="l2"><span class="p" title="${esc(c.path)}"><bdi>${esc(c.path)}</bdi></span><span class="ft">${fmtSize(c.prev)} → ${fmtSize(c.size)}</span></div>
    <div class="meter"><i style="width:${(Math.abs(c.delta) / maxAbs) * 100}%;background:${color};box-shadow:0 0 8px ${color}"></i></div>
  </div>`;
}

function renderHistory(body: HTMLElement) {
  const h = st.history;
  if (!h.length) {
    body.innerHTML = `<div class="empty">暂无历史记录</div>`;
    return;
  }
  const pts = [...h].reverse();
  const W = 300;
  const H = 150;
  const pad = { l: 8, r: 8, t: 18, b: 22 };
  const vals = pts.map((p) => p.total);
  let lo = Math.min(...vals);
  let hi = Math.max(...vals);
  if (hi - lo < 1) {
    lo -= 1 << 30;
    hi += 1 << 30;
  }
  const span = hi - lo;
  lo -= span * 0.15;
  hi += span * 0.15;
  const X = (i: number) => pad.l + (pts.length === 1 ? (W - pad.l - pad.r) / 2 : (i / (pts.length - 1)) * (W - pad.l - pad.r));
  const Y = (v: number) => pad.t + (1 - (v - lo) / (hi - lo)) * (H - pad.t - pad.b);
  const line = pts.map((p, i) => `${i ? 'L' : 'M'}${X(i).toFixed(1)},${Y(p.total).toFixed(1)}`).join('');
  const area = `${line}L${X(pts.length - 1).toFixed(1)},${H - pad.b}L${X(0).toFixed(1)},${H - pad.b}Z`;
  const baseFile = st.changes?.baseline?.file;
  const dots = pts
    .map((p, i) => {
      const isBase = p.file === baseFile;
      const isCur = i === pts.length - 1;
      return `<circle cx="${X(i).toFixed(1)}" cy="${Y(p.total).toFixed(1)}" r="${isCur || isBase ? 4 : 2.5}" fill="${isCur ? '#5ee7ff' : isBase ? '#b98cff' : '#0d121c'}" stroke="${isBase ? '#b98cff' : '#5ee7ff'}" stroke-width="1.5"><title>${fmtTime(p.time)}  ${fmtSize(p.total)}</title></circle>`;
    })
    .join('');
  body.innerHTML = `
    <div class="hist-chart"><svg viewBox="0 0 ${W} ${H}" preserveAspectRatio="none">
      <defs><linearGradient id="ha" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#4cc9f0" stop-opacity=".35"/><stop offset="1" stop-color="#4cc9f0" stop-opacity="0"/></linearGradient></defs>
      <path d="${area}" fill="url(#ha)"/>
      <path d="${line}" fill="none" stroke="#5ee7ff" stroke-width="1.8" style="filter:drop-shadow(0 0 4px rgba(94,231,255,.7))"/>
      ${dots}
      <text x="${pad.l}" y="12" fill="#8792a8" font-size="10">${fmtSize(Math.max(...vals))}</text>
      <text x="${pad.l}" y="${H - 6}" fill="#5b6579" font-size="10">${fmtShortTime(pts[0].time)}</text>
      <text x="${W - pad.r}" y="${H - 6}" fill="#5b6579" font-size="10" text-anchor="end">${fmtShortTime(pts[pts.length - 1].time)}</text>
    </svg></div>
    ${h
      .map((m, i) => {
        const older = h[i + 1];
        const d = older ? m.total - older.total : null;
        const tag =
          i === 0
            ? `<span class="tag cur">本次</span>`
            : m.file === baseFile
              ? `<span class="tag base">基线</span>`
              : `<button class="tag" data-f="${esc(m.file)}">设为基线</button>`;
        return `<div class="hrow">
          <div class="t"><div>${fmtTime(m.time)}</div><div>${fmtNum(m.files)} 个文件 · ${m.mode || '—'} · ${fmtDuration(m.elapsedMs)}</div></div>
          <div class="v"><div>${fmtSize(m.total)}</div>${d !== null ? `<div class="delta ${d > 0 ? 'up' : d < 0 ? 'down' : ''}" style="display:inline-block">${fmtDelta(d)}</div>` : ''}</div>
          ${tag}</div>`;
      })
      .join('')}`;
  body.querySelectorAll<HTMLElement>('button.tag[data-f]').forEach((b) => {
    b.onclick = async () => {
      try {
        await api.setBaseline(b.dataset.f!);
        await loadChanges();
        renderHistory(body);
        renderStatus();
        renderLegend();
        if (st.cur >= 0) navigate(st.cur, null);
        toast('已切换对比基线');
      } catch (err) {
        toast(String(err), true);
      }
    };
  });
}

// ------------------------------------------------------------------ export for AI

const ex = { scope: 'root' as 'root' | 'cur', text: '', seq: 0 };

function loadAnonPref(): boolean {
  try {
    return localStorage.getItem('exAnon') !== '0';
  } catch {
    return true;
  }
}

/** Rough token estimate: CJK ≈ 1 token per char, other text ≈ 1 per 3.5 chars. */
function estimateTokens(s: string): number {
  let cjk = 0;
  for (const ch of s) if (ch.charCodeAt(0) > 0x2e80) cjk++;
  return Math.round(cjk + (s.length - cjk) / 3.5);
}

async function refreshExport() {
  const seq = ++ex.seq;
  const pre = $('#exPreview');
  pre.classList.add('loading');
  try {
    const scope = ex.scope === 'cur' && st.cur >= 0 ? st.cur : st.rootId;
    const anonymize = ($('#exAnon') as HTMLInputElement).checked;
    const text = await api.exportReport({ scope, anonymize, tzOffsetMin: new Date().getTimezoneOffset() });
    if (seq !== ex.seq) return;
    ex.text = text;
    pre.textContent = text;
    pre.scrollTop = 0;
    const k = (n: number) => (n >= 1000 ? `${(n / 1000).toFixed(1)} 千` : String(n));
    $('#exStats').textContent = `${fmtNum(text.split('\n').length)} 行 · ${k(text.length)}字符 · 约 ${k(estimateTokens(text))} tokens`;
  } catch (e) {
    pre.textContent = String(e);
  } finally {
    if (seq === ex.seq) pre.classList.remove('loading');
  }
}

function openExport() {
  if (st.rootId < 0) return;
  closeMenu();
  const curBtn = $('#exScopeCur');
  const atRoot = st.cur === st.rootId || st.cur < 0;
  curBtn.classList.toggle('hidden', atRoot);
  curBtn.textContent = `当前文件夹：${st.info?.name ?? ''}`;
  if (atRoot) ex.scope = 'root';
  $('#exScope').querySelectorAll<HTMLElement>('button').forEach((b) => b.classList.toggle('on', b.dataset.s === ex.scope));
  ($('#exAnon') as HTMLInputElement).checked = loadAnonPref();
  $('#exPreview').textContent = '生成中…';
  $('#exStats').textContent = '';
  $('#exportModal').classList.remove('hidden');
  refreshExport();
}

function closeExport() {
  $('#exportModal').classList.add('hidden');
}

function bindExport() {
  $('#exportBtn').onclick = openExport;
  api.mcpUrl().then(
    (url) => {
      const hint = $('#mcpHint');
      hint.innerHTML = `也可以让 AI 直接分析和清理（删除前会在这里弹窗确认）：MCP 地址 <code>${esc(url)}</code><button class="link" id="mcpCopy">复制 Claude Code 添加命令</button>`;
      hint.classList.remove('hidden');
      $('#mcpCopy').onclick = () =>
        navigator.clipboard.writeText(`claude mcp add --transport http diskmind ${url}`).then(
          () => toast('已复制，在终端里运行即可'),
          (e) => toast(`复制失败：${e}`, true),
        );
    },
    () => {},
  );
  $('#exClose').onclick = closeExport;
  $('#exportModal').onmousedown = (e) => {
    if (e.target === e.currentTarget) closeExport();
  };
  $('#exScope').querySelectorAll<HTMLElement>('button').forEach((b) => {
    b.onclick = () => {
      ex.scope = b.dataset.s as 'root' | 'cur';
      $('#exScope').querySelectorAll('button').forEach((x) => x.classList.toggle('on', x === b));
      refreshExport();
    };
  });
  ($('#exAnon') as HTMLInputElement).onchange = (e) => {
    try {
      localStorage.setItem('exAnon', (e.target as HTMLInputElement).checked ? '1' : '0');
    } catch {}
    refreshExport();
  };
  $('#exCopy').onclick = async () => {
    if (!ex.text) return;
    try {
      await navigator.clipboard.writeText(ex.text);
      toast('已复制，直接粘贴给 AI 即可');
    } catch (e) {
      toast(`复制失败：${e}`, true);
    }
  };
  $('#exSave').onclick = async () => {
    if (!ex.text) return;
    try {
      const { save } = await import('@tauri-apps/plugin-dialog');
      const d = new Date();
      const pad = (n: number) => String(n).padStart(2, '0');
      const drive = st.rootPath.slice(0, 1);
      const path = await save({
        title: '保存分析数据',
        defaultPath: `DiskMind-${drive}盘-${d.getFullYear()}${pad(d.getMonth() + 1)}${pad(d.getDate())}.md`,
        filters: [{ name: 'Markdown', extensions: ['md'] }],
      });
      if (!path) return;
      await api.writeTextFile(path, ex.text);
      toast('已保存');
    } catch (e) {
      toast(String(e), true);
    }
  };
}

// ------------------------------------------------------------------ AI cleanup (MCP)

type Confirm = { kind: 'delete'; r: DeleteRequest } | { kind: 'cleanup'; r: CleanupRequest };

/** MCP confirmations, shown one at a time. */
const confirm = { cur: null as Confirm | null, queue: [] as Confirm[] };

function enqueueConfirm(c: Confirm) {
  if (confirm.cur) confirm.queue.push(c);
  else showConfirm(c);
}

function showConfirm(c: Confirm) {
  confirm.cur = c;
  closeMenu();
  closeExport();
  if (c.kind === 'delete') openDelete(c.r);
  else openCleanup(c.r);
}

function answerConfirm(answer: string[]) {
  const c = confirm.cur;
  if (!c) return;
  api.resolveDelete(c.r.id, answer).catch((e) => toast(String(e), true));
  if (c.kind === 'delete') toast(answer.length ? `正在移到回收站：${answer.length} 项…` : '已拒绝 AI 的删除请求');
  else if (!answer.length) toast('已拒绝 AI 的清理请求');
  closeConfirm();
}

function closeConfirm() {
  $('#delModal').classList.add('hidden');
  $('#runModal').classList.add('hidden');
  confirm.cur = null;
  const next = confirm.queue.shift();
  if (next) showConfirm(next);
}

function openDelete(r: DeleteRequest) {
  $('#delReason').textContent = r.reason;
  $('#delList').innerHTML = r.items
    .map(
      (it, i) => `
      <div class="del-row${it.error ? ' bad' : ''}">
        <input type="checkbox" data-i="${i}" ${it.error ? 'disabled' : 'checked'} />
        <span class="ico">${it.dir ? ICON.dir : ICON.file}</span>
        <span class="p">${esc(it.path)}</span>
        ${
          it.error
            ? `<span class="err">${esc(it.error)}</span>`
            : `<span class="s">${it.size == null ? '—' : fmtSize(it.size)}</span>
               <button class="icon-btn" data-r="${i}" title="在资源管理器中显示">${ICON.explorer}</button>`
        }
      </div>`,
    )
    .join('');
  renderDeleteStats();
  $('#delModal').classList.remove('hidden');
}

function deleteReq(): DeleteRequest | null {
  return confirm.cur?.kind === 'delete' ? confirm.cur.r : null;
}

function checkedDelete(): DeleteItem[] {
  const r = deleteReq();
  if (!r) return [];
  return [...$('#delList').querySelectorAll<HTMLInputElement>('input:checked')].map((c) => r.items[Number(c.dataset.i)]);
}

function renderDeleteStats() {
  const r = deleteReq();
  if (!r) return;
  const sel = checkedDelete();
  const ok = r.items.filter((x) => !x.error).length;
  const size = `${fmtSize(sel.reduce((s, x) => s + (x.size ?? 0), 0))}${sel.some((x) => x.size == null) ? '+' : ''}`;
  $('#delStats').textContent = `已选 ${sel.length} / ${ok} 项`;
  const yes = $<HTMLButtonElement>('#delYes');
  yes.disabled = sel.length === 0;
  yes.textContent = sel.length ? `移到回收站（${size}）` : '移到回收站';
  const all = $<HTMLInputElement>('#delAll');
  all.checked = sel.length === ok;
  all.indeterminate = sel.length > 0 && sel.length < ok;
}

function openCleanup(r: CleanupRequest) {
  const a = r.action;
  $('#runReason').textContent = r.reason;
  $('#runAction').innerHTML = `
    <b>${esc(a.title)}</b>
    <p>${esc(a.desc)}</p>
    <code class="${a.shell ? '' : 'prose'}">${esc(a.command)}</code>
    <div class="meta">
      ${a.estimate != null ? `<span class="pill good">预计释放 ${fmtSize(a.estimate)}</span>` : ''}
      ${a.admin && !r.elevated ? '<span class="pill warn">需要管理员权限，确认后 Windows 会弹出授权提示</span>' : ''}
    </div>`;
  $('#runModal').classList.remove('hidden');
}

/** The backend pruned recycled items from the loaded tree; redraw without rescanning. */
async function refreshAfterDelete(t: { total: number; files: number; dirs: number; freed: number } | null) {
  if (!t || st.rootId < 0) return;
  if (st.lastDone) Object.assign(st.lastDone, { total: t.total, files: t.files, dirs: t.dirs });
  const alive = st.cur >= 0 && (await api.getInfo(st.cur).then(() => true, () => false));
  await loadChanges();
  await navigate(alive ? st.cur : st.rootId, null);
  renderStatus();
  renderTab();
  toast(`已移到回收站：${fmtSize(t.freed)}，清空回收站后释放空间`);
}

function renderSuggestions(body: HTMLElement) {
  const list = st.suggestions;
  if (!list.length) {
    body.innerHTML = `<div class="empty">还没有建议<br>通过 MCP 连接的 AI 会把需要你自己处理的建议<br>（迁移游戏库、修改软件缓存位置、卸载软件等）发到这里</div>`;
    return;
  }
  body.innerHTML =
    `<div class="sug-foot"><button class="link" id="sugClear">全部清除</button></div>` +
    list
      .slice()
      .reverse()
      .map(
        (g) => `<div class="sug" data-id="${g.id}">
        <div class="sug-h"><b>${esc(g.title)}</b>${g.savings ? `<span class="pill good">${esc(g.savings)}</span>` : ''}
          <button class="icon-btn" data-x title="忽略"><svg viewBox="0 0 24 24"><path d="M6 6l12 12M18 6L6 18" /></svg></button></div>
        <div class="sug-d">${esc(g.detail)}</div>
        ${g.command ? `<div class="sug-cmd"><code>${esc(g.command)}</code><button class="btn sm" data-copy>复制</button></div>` : ''}
        ${g.path ? `<div><button class="link" data-open title="${esc(g.path)}">${ICON.explorer} 打开 ${esc(g.path)}</button></div>` : ''}
      </div>`,
      )
      .join('');
  const dismiss = (id: number | null) =>
    api.dismissSuggestion(id).then(setSuggestions, (e) => toast(String(e), true));
  $('#sugClear').onclick = () => dismiss(null);
  body.querySelectorAll<HTMLElement>('.sug').forEach((el) => {
    const g = list.find((x) => x.id === Number(el.dataset.id))!;
    el.querySelector<HTMLElement>('[data-x]')!.onclick = () => dismiss(g.id);
    const copy = el.querySelector<HTMLElement>('[data-copy]');
    if (copy) copy.onclick = () => navigator.clipboard.writeText(g.command ?? '').then(() => toast('已复制命令'), (e) => toast(`复制失败：${e}`, true));
    const open = el.querySelector<HTMLElement>('[data-open]');
    if (open) open.onclick = () => api.revealPath(g.path ?? '', true).catch((e) => toast(String(e), true));
  });
}

function setSuggestions(list: Suggestion[]) {
  st.suggestions = list;
  const badge = $('#aiBadge');
  badge.textContent = String(list.length);
  badge.classList.toggle('hidden', !list.length);
  if (st.tab === 'ai') renderTab();
}

function bindMcp() {
  $('#delList').onchange = renderDeleteStats;
  $('#delList').onclick = (e) => {
    const b = (e.target as HTMLElement).closest<HTMLElement>('[data-r]');
    const r = deleteReq();
    if (b && r) api.revealPath(r.items[Number(b.dataset.r)].path, true).catch((err) => toast(String(err), true));
  };
  $<HTMLInputElement>('#delAll').onchange = (e) => {
    const on = (e.target as HTMLInputElement).checked;
    $('#delList').querySelectorAll<HTMLInputElement>('input:not(:disabled)').forEach((c) => (c.checked = on));
    renderDeleteStats();
  };
  $('#delNo').onclick = () => answerConfirm([]);
  $('#delYes').onclick = () => answerConfirm(checkedDelete().map((x) => x.path));
  $('#runNo').onclick = () => answerConfirm([]);
  $('#runYes').onclick = () => {
    const c = confirm.cur;
    if (c?.kind === 'cleanup') answerConfirm([c.r.action.id]);
  };
  onEvent<DeleteRequest>('mcp-delete-request', (r) => enqueueConfirm({ kind: 'delete', r }));
  onEvent<CleanupRequest>('mcp-cleanup-request', (r) => enqueueConfirm({ kind: 'cleanup', r }));
  onEvent<number>('mcp-confirm-cancel', (id) => {
    if (confirm.cur?.r.id === id) {
      closeConfirm();
      toast('AI 的请求已超时，没有做任何操作', true);
    } else confirm.queue = confirm.queue.filter((c) => c.r.id !== id);
  });
  onEvent('tree-changed', refreshAfterDelete);
  onEvent<string>('mcp-cleanup-started', (title) => toast(`正在运行：${title}…`, false, 0));
  onEvent<{ title: string; ok: boolean; freed?: number; error?: string }>('mcp-cleanup-done', (d) =>
    d.ok
      ? toast(`${d.title}完成，可用空间增加 ${fmtSize(d.freed ?? 0)}${st.rootId >= 0 ? '，按 F5 重新扫描查看变化' : ''}`, false, 6000)
      : toast(`${d.title}失败：${d.error}`, true),
  );
  onEvent<Suggestion[]>('suggestions-changed', (list) => {
    const added = list.length > st.suggestions.length;
    setSuggestions(list);
    if (added && st.tab !== 'ai') toast('AI 提出了新建议，见右侧“AI 建议”');
  });
  api.getSuggestions().then(setSuggestions, () => {});
  // Scans started over MCP show the same progress overlay.
  onEvent<string>('scan-start', (path) => {
    if (!$('#scanOverlay').classList.contains('hidden')) return;
    closeMenu();
    resetOverlay(path);
    $('#scanOverlay').classList.remove('hidden');
  });
}

// ------------------------------------------------------------------ wiring

function bindUi() {
  document.body.insertAdjacentHTML(
    'beforeend',
    `<svg width="0" height="0" style="position:absolute"><defs>
      <linearGradient id="ringGrad" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#5ee7ff"/><stop offset="1" stop-color="#9b5cff"/></linearGradient>
      <linearGradient id="ringHot" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#ffb347"/><stop offset="1" stop-color="#ff3d6e"/></linearGradient>
    </defs></svg>`,
  );

  import('@tauri-apps/api/window')
    .then(({ getCurrentWindow }) => {
      const w = getCurrentWindow();
      $('#wMin').onclick = () => w.minimize();
      $('#wMax').onclick = () => w.toggleMaximize();
      $('#wClose').onclick = () => w.close();
    })
    .catch(() => {});

  $('#mftToggle').onclick = () => {
    st.mft = !st.mft;
    try {
      localStorage.setItem('mft', st.mft ? '1' : '0');
    } catch {}
    renderMftToggle();
    if (!$('#welcome').classList.contains('hidden')) loadWelcome();
  };
  $('#pickFolder').onclick = async () => {
    try {
      const { open } = await import('@tauri-apps/plugin-dialog');
      const dir = await open({ directory: true, multiple: false, title: '选择要扫描的文件夹' });
      if (typeof dir === 'string') startScan(dir);
    } catch (e) {
      toast(String(e), true);
    }
  };
  $('#homeBtn').onclick = () => showWelcome();
  $('#upBtn').onclick = () => goUp();
  $('#rescanBtn').onclick = () => st.rootPath && startScan(st.rootPath);
  bindExport();
  $('#cancelScan').onclick = () => api.cancelScan();
  $('#sortBtn').onclick = () => {
    st.sort = st.sort === 'size' ? 'delta' : 'size';
    $('#sortBtn').textContent = st.sort === 'size' ? '按大小 ▾' : '按变化 ▾';
    renderList();
  };

  $('#viewSeg').querySelectorAll<HTMLElement>('button').forEach((b) => {
    b.onclick = async () => {
      if (b.dataset.v === st.view) return;
      st.view = b.dataset.v as ViewMode;
      $('#viewSeg').querySelectorAll('button').forEach((x) => x.classList.toggle('on', x === b));
      // Different views want different detail budgets: fetch first, then transition once.
      let items = null;
      if (st.cur >= 0) {
        const [budget, minFrac] = chartQuery();
        items = await api.getLayout(st.cur, budget, minFrac).catch(() => null);
      }
      chart.switchView(st.view, items);
    };
  });
  $('#colorSeg').querySelectorAll<HTMLElement>('button').forEach((b) => {
    b.onclick = () => {
      st.color = b.dataset.c as ColorMode;
      $('#colorSeg').querySelectorAll('button').forEach((x) => x.classList.toggle('on', x === b));
      chart.setColor(st.color);
      renderLegend();
    };
  });
  $('#tabs').querySelectorAll<HTMLElement>('button').forEach((b) => {
    b.onclick = () => {
      st.tab = b.dataset.t as Tab;
      $('#tabs').querySelectorAll('button').forEach((x) => x.classList.toggle('on', x === b));
      renderTab();
    };
  });

  document.addEventListener('keydown', (e) => {
    if ((e.target as HTMLElement).tagName === 'SELECT') return;
    if (confirm.cur) {
      if (e.key === 'Escape') answerConfirm([]);
      return;
    }
    if (e.key === 'Escape') {
      closeMenu();
      closeExport();
    }
    if (!$('#exportModal').classList.contains('hidden')) return;
    if ($('#main').classList.contains('hidden')) return;
    if (e.key === 'Backspace' || (e.altKey && e.key === 'ArrowLeft')) {
      e.preventDefault();
      goUp();
    } else if (e.key === 'Enter' && st.sel >= 0) {
      const it = st.children.find((x) => x.id === st.sel);
      if (it?.dir && it.hasChildren) navigate(it.id, 'in');
    } else if (e.key === 'F5') {
      e.preventDefault();
      if (st.rootPath) startScan(st.rootPath);
    }
  });
  document.addEventListener('contextmenu', (e) => {
    if (!(e.target as HTMLElement).closest('.row, .crow, canvas')) e.preventDefault();
  });
}

async function init() {
  bindUi();
  bindMcp();
  onProgress(onScanProgress);
  onDone(async (d) => {
    await onScanDone(d);
    renderTab();
    renderLegend();
  });
  st.admin = await api.isAdmin().catch(() => false);
  renderMftToggle();
  await loadWelcome();
}

init();
