// Dev-only fake backend so the UI can be developed in a plain browser (`npm run dev`).
import type { Change, Changes, DriveInfo, ExtItem, FileItem, Item, NodeInfo, Progress, ScanDone, SnapMeta } from './api';

interface N { id: number; parent: number; name: string; own: number; size: number; files: number; dir: boolean; kids: number[]; prev: number | null }

const MB = 1 << 20;
const nodes: N[] = [];
let seed = 7;
const rnd = () => ((seed = (seed * 16807) % 2147483647) / 2147483647);
const EXTS = ['dll', 'exe', 'png', 'mp4', 'pak', 'json', 'log', 'zip', 'iso', 'jpg', 'txt', 'vhdx', 'db', 'bin', 'wav'];
const DIRS = ['Windows', 'Users', 'Program Files', 'Games', 'Steam', 'Docker', 'Projects', 'Videos', 'node_modules', 'System32', 'AppData', 'Local', 'Temp', 'Cache', 'Packages', 'Downloads', 'Music', 'WinSxS', 'Adobe', 'JetBrains'];

function add(parent: number, name: string, dir: boolean, own = 0): number {
  const id = nodes.length;
  nodes.push({ id, parent, name, own, size: own, files: dir ? 0 : 1, dir, kids: [], prev: null });
  if (parent >= 0) nodes[parent].kids.push(id);
  return id;
}

function gen(p: number, depth: number) {
  const nd = depth === 0 ? 9 : Math.floor(rnd() * (6 - depth));
  for (let i = 0; i < nd; i++) {
    const d = add(p, DIRS[Math.floor(rnd() * DIRS.length)] + (depth ? '' : ''), true);
    gen(d, depth + 1);
  }
  const nf = Math.floor(rnd() * 14) + 1;
  for (let i = 0; i < nf; i++) {
    const big = rnd() < 0.08;
    const s = Math.floor(Math.pow(rnd(), 3) * (big ? 8000 : 300) * MB) + 4096;
    add(p, `file_${depth}_${i}.${EXTS[Math.floor(rnd() * EXTS.length)]}`, false, s);
  }
}

add(-1, 'C:\\', true);
gen(0, 0);
const agg = (i: number): void => {
  const n = nodes[i];
  for (const k of n.kids) {
    agg(k);
    n.size += nodes[k].size;
    n.files += nodes[k].files;
  }
  n.kids.sort((a, b) => nodes[b].size - nodes[a].size);
};
agg(0);
for (const n of nodes) if (n.dir || n.size > 16 * MB) n.prev = Math.max(0, Math.round(n.size * (1 - (rnd() - 0.6) * 0.5)));
nodes[0].prev = nodes[0].size - 3200 * MB;

const item = (n: N): Item => ({ id: n.id, parent: n.parent, name: n.name, size: n.size, files: n.files, dir: n.dir, prev: n.prev, other: 0, hasChildren: n.kids.length > 0, mtime: Math.floor(Date.now() / 1000) - ((n.id * 7919) % 900) * 86400 });
const pathOf = (i: number): string => {
  const parts: string[] = [];
  for (let x = i; x > 0; x = nodes[x].parent) parts.push(nodes[x].name);
  return 'C:\\' + parts.reverse().join('\\');
};
const desc = (i: number, f: (n: N) => void) => {
  f(nodes[i]);
  nodes[i].kids.forEach((k) => desc(k, f));
};
const now = Date.now();
const history: SnapMeta[] = [0, 1, 3, 8, 15].map((d, i) => ({
  file: `${now - d * 86400000}.dmsnap`, time: now - d * 86400000 - i * 3600000, total: nodes[0].size - i * 2900 * MB + (i % 2) * 900 * MB,
  files: 912345 - i * 3000, dirs: 120000, mode: i % 2 ? '遍历' : 'MFT', elapsedMs: 1800 + i * 400,
}));

type Handler = (p: any) => void;
const handlers: Record<string, Handler[]> = {};
export const mockListen = (ev: string, f: Handler) => {
  (handlers[ev] ||= []).push(f);
  return Promise.resolve(() => {});
};
const emit = (ev: string, p: unknown) => (handlers[ev] || []).forEach((f) => f(p));

export const mockApi = {
  listDrives: async (): Promise<DriveInfo[]> => [
    { root: 'C:\\', label: '系统', fs: 'NTFS', total: 476 * 1024 * MB, free: 88 * 1024 * MB, kind: 'fixed', lastScan: now - 3 * 86400000, lastTotal: 371 * 1024 * MB },
    { root: 'D:\\', label: '数据', fs: 'NTFS', total: 1863 * 1024 * MB, free: 1120 * 1024 * MB, kind: 'fixed', lastScan: null, lastTotal: null },
    { root: 'E:\\', label: '游戏', fs: 'NTFS', total: 953 * 1024 * MB, free: 40 * 1024 * MB, kind: 'fixed', lastScan: now - 86400000 * 12, lastTotal: 900 * 1024 * MB },
  ],
  isAdmin: async () => false,
  startScan: async (path: string, _useMft: boolean) => {
    let t = 0;
    const iv = setInterval(() => {
      t += 80;
      const p: Progress = { path, files: t * 410, dirs: t * 40, bytes: t * 120 * MB, elapsedMs: t, percent: Math.min(1, t / 1600), phase: '读取 MFT', mode: 'MFT' };
      emit('scan-progress', p);
      if (t >= 1600) {
        clearInterval(iv);
        const d: ScanDone = { ok: true, error: null, warning: null, root: 0, rootPath: 'C:\\', mode: 'MFT', elapsedMs: 1642, files: nodes[0].files, dirs: nodes.filter((n) => n.dir).length, total: nodes[0].size, baseline: { file: history[1].file, time: history[1].time, total: nodes[0].prev!, mode: 'MFT' } };
        emit('scan-done', d);
      }
    }, 80);
  },
  cancelScan: async () => {},
  getLayout: async (id: number, budget: number, minFrac: number): Promise<Item[]> => {
    const out: Item[] = [item(nodes[id])];
    const min = nodes[id].size * minFrac;
    const q = [id];
    while (q.length && out.length < budget) {
      const x = q.shift()!;
      let rest = 0;
      let rc = 0;
      for (const k of nodes[x].kids) {
        if (nodes[k].size >= min) {
          out.push(item(nodes[k]));
          if (nodes[k].kids.length) q.push(k);
        } else {
          rest += nodes[k].size;
          rc++;
        }
      }
      if (rc) out.push({ id: -x - 1, parent: x, name: `${rc} 个小项目`, size: rest, files: rc, dir: false, prev: null, other: rc, hasChildren: false, mtime: 0 });
    }
    return out;
  },
  getChildren: async (id: number, limit: number) => ({ items: nodes[id].kids.slice(0, limit).map((k) => item(nodes[k])), total: nodes[id].kids.length }),
  getInfo: async (id: number): Promise<NodeInfo> => {
    const crumbs = [];
    for (let x = id; x >= 0; x = nodes[x].parent) crumbs.unshift({ id: x, name: nodes[x].name });
    const n = nodes[id];
    return { id, parent: n.parent, name: n.name, path: pathOf(id), size: n.size, files: n.files, dir: n.dir, prev: n.prev, crumbs };
  },
  getChanges: async (): Promise<Changes> => {
    const all: Change[] = nodes
      .filter((n) => n.prev !== null && n.id > 0 && Math.abs(n.size - n.prev) > 50 * MB)
      .map((n) => ({ id: n.id, parent: n.parent, name: n.name, path: pathOf(n.id), size: n.size, prev: n.prev!, delta: n.size - n.prev!, dir: n.dir, kind: n.prev === 0 ? 'new' : n.size > n.prev! ? 'grown' : 'shrunk' }));
    const shrunk = all.filter((c) => c.delta < 0).sort((a, b) => a.delta - b.delta).slice(0, 40);
    shrunk.splice(2, 0, { id: -1, parent: 0, name: 'OldBackups', path: 'C:\\Users\\me\\OldBackups', size: 0, prev: 2400 * MB, delta: -2400 * MB, dir: true, kind: 'removed' });
    return { baseline: { file: history[1].file, time: history[1].time, total: nodes[0].prev!, mode: '遍历' }, totalDelta: nodes[0].size - nodes[0].prev!, grown: all.filter((c) => c.delta > 0).sort((a, b) => b.delta - a.delta).slice(0, 40), shrunk };
  },
  getTopFiles: async (id: number, limit: number): Promise<FileItem[]> => {
    const fs: N[] = [];
    desc(id, (n) => !n.dir && fs.push(n));
    return fs.sort((a, b) => b.size - a.size).slice(0, limit).map((n) => ({ id: n.id, parent: n.parent, name: n.name, path: pathOf(n.id), size: n.size, prev: n.prev }));
  },
  getExtensions: async (id: number, limit: number): Promise<ExtItem[]> => {
    const m = new Map<string, ExtItem>();
    desc(id, (n) => {
      if (n.dir) return;
      const e = n.name.split('.').pop()!;
      const x = m.get(e) ?? { ext: e, size: 0, count: 0 };
      x.size += n.size;
      x.count++;
      m.set(e, x);
    });
    return [...m.values()].sort((a, b) => b.size - a.size).slice(0, limit);
  },
  getHistory: async () => history,
  setBaseline: async () => null,
  getPath: async (id: number) => pathOf(id),
  revealPath: async () => {},
  exportReport: async (opts: { scope: number; anonymize: boolean; tzOffsetMin: number }) => {
    const lines = [`# 磁盘占用分析数据（DiskMind 导出）`, '', `- 分析范围：\`${opts.anonymize ? 'C:\\Users\\<当前用户>' : pathOf(opts.scope)}\``, '', '## 1. 目录树（只展开大头）', '', '```text'];
    desc(opts.scope, (n) => {
      if (n.size > 2 * 1024 * MB) lines.push(`${(n.size / 1024 / MB).toFixed(1).padStart(7)} GB  ${pathOf(n.id)}`);
    });
    lines.push('```', '', '## 请帮我分析', '', '1. 哪些占用可以安全清理或迁移到其他磁盘？');
    return lines.join('\n');
  },
  writeTextFile: async () => {},
};
