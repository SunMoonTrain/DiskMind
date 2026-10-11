import { invoke } from '@tauri-apps/api/core';
import { listen, UnlistenFn } from '@tauri-apps/api/event';

export interface Item {
  id: number;
  parent: number;
  name: string;
  size: number;
  files: number;
  dir: boolean;
  prev: number | null;
  other: number;
  hasChildren: boolean;
}

export interface DriveInfo {
  root: string;
  label: string;
  fs: string;
  total: number;
  free: number;
  kind: string;
  lastScan: number | null;
  lastTotal: number | null;
}

export interface Crumb { id: number; name: string }

export interface NodeInfo {
  id: number;
  parent: number;
  name: string;
  path: string;
  size: number;
  files: number;
  dir: boolean;
  prev: number | null;
  crumbs: Crumb[];
}

export interface Baseline { file: string; time: number; total: number; mode: string }

export interface Change {
  id: number;
  parent: number;
  name: string;
  path: string;
  size: number;
  prev: number;
  delta: number;
  dir: boolean;
  kind: 'grown' | 'new' | 'shrunk' | 'removed';
}

export interface Changes {
  baseline: Baseline | null;
  totalDelta: number | null;
  grown: Change[];
  shrunk: Change[];
}

export interface SnapMeta {
  file: string;
  time: number;
  total: number;
  files: number;
  dirs: number;
  mode: string;
  elapsedMs: number;
}

export interface FileItem { id: number; parent: number; name: string; path: string; size: number; prev: number | null }
export interface ExtItem { ext: string; size: number; count: number }

export interface Progress {
  path: string;
  files: number;
  dirs: number;
  bytes: number;
  elapsedMs: number;
  percent: number;
  phase: string;
  mode: string;
}

export interface ScanDone {
  ok: boolean;
  error: string | null;
  warning: string | null;
  root: number;
  rootPath: string;
  mode: string;
  elapsedMs: number;
  files: number;
  dirs: number;
  total: number;
  baseline: Baseline | null;
}

const realApi = {
  listDrives: () => invoke<DriveInfo[]>('list_drives'),
  isAdmin: () => invoke<boolean>('is_admin'),
  startScan: (path: string, useMft: boolean) => invoke<void>('start_scan', { path, useMft }),
  cancelScan: () => invoke<void>('cancel_scan'),
  getLayout: (id: number, budget: number, minFrac: number) => invoke<Item[]>('get_layout', { id, budget, minFrac }),
  getChildren: (id: number, limit: number) => invoke<{ items: Item[]; total: number }>('get_children', { id, limit }),
  getInfo: (id: number) => invoke<NodeInfo>('get_info', { id }),
  getChanges: (limit: number) => invoke<Changes>('get_changes', { limit }),
  getTopFiles: (id: number, limit: number) => invoke<FileItem[]>('get_top_files', { id, limit }),
  getExtensions: (id: number, limit: number) => invoke<ExtItem[]>('get_extensions', { id, limit }),
  getHistory: () => invoke<SnapMeta[]>('get_history'),
  setBaseline: (file: string) => invoke<Baseline | null>('set_baseline', { file }),
  getPath: (id: number) => invoke<string>('get_path', { id }),
  revealPath: (path: string, select: boolean) => invoke<void>('reveal_path', { path, select }),
};

// Outside Tauri (plain browser via `npm run dev`) fall back to a fake backend for UI work.
const mock = !('__TAURI_INTERNALS__' in window) && import.meta.env.DEV ? await import('./mock') : null;

export const api: typeof realApi = mock ? (mock.mockApi as typeof realApi) : realApi;

export const onProgress = (f: (p: Progress) => void): Promise<UnlistenFn> =>
  mock ? mock.mockListen('scan-progress', f) : listen<Progress>('scan-progress', (e) => f(e.payload));
export const onDone = (f: (d: ScanDone) => void): Promise<UnlistenFn> =>
  mock ? mock.mockListen('scan-done', f) : listen<ScanDone>('scan-done', (e) => f(e.payload));
