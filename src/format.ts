const UNITS = ['KB', 'MB', 'GB', 'TB', 'PB'];

export function fmtSize(b: number): string {
  if (b < 1024) return `${b} B`;
  let v = b / 1024;
  let i = 0;
  while (v >= 1024 && i < UNITS.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v >= 100 ? v.toFixed(0) : v >= 10 ? v.toFixed(1) : v.toFixed(2)} ${UNITS[i]}`;
}

export function fmtDelta(d: number): string {
  if (d === 0) return '±0';
  return (d > 0 ? '+' : '−') + fmtSize(Math.abs(d));
}

export function fmtNum(n: number): string {
  return n.toLocaleString('zh-CN');
}

export function fmtPct(f: number): string {
  if (!isFinite(f)) return '—';
  const p = f * 100;
  return p >= 10 ? `${p.toFixed(0)}%` : p >= 1 ? `${p.toFixed(1)}%` : `${p.toFixed(2)}%`;
}

const pad = (n: number) => String(n).padStart(2, '0');

export function fmtTime(ms: number): string {
  const d = new Date(ms);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

export function fmtShortTime(ms: number): string {
  const d = new Date(ms);
  return `${pad(d.getMonth() + 1)}/${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

export function fmtAgo(ms: number): string {
  const s = (Date.now() - ms) / 1000;
  if (s < 60) return '刚刚';
  if (s < 3600) return `${Math.floor(s / 60)} 分钟前`;
  if (s < 86400) return `${Math.floor(s / 3600)} 小时前`;
  if (s < 86400 * 30) return `${Math.floor(s / 86400)} 天前`;
  return fmtTime(ms).slice(0, 10);
}

export function fmtDuration(ms: number): string {
  if (ms < 1000) return `${ms} ms`;
  if (ms < 60000) return `${(ms / 1000).toFixed(2)} s`;
  return `${Math.floor(ms / 60000)} 分 ${Math.round((ms % 60000) / 1000)} 秒`;
}

export function esc(s: string): string {
  return s.replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]!);
}

export function extOf(name: string): string {
  const p = name.lastIndexOf('.');
  if (p <= 0 || p === name.length - 1 || name.length - p > 12) return '';
  return name.slice(p + 1).toLowerCase();
}
