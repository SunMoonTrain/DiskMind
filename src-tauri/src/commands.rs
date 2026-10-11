use crate::history::{self, SnapMeta};
use crate::progress::Counters;
use crate::sys::{self, DriveInfo};
use crate::tree::{BaselineInfo, Changes, ExtItem, FileItem, Item, NodeInfo, RawTree, Tree};
use crate::{elevate, mft, walker};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::*};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, State};

#[derive(Default)]
pub struct AppState {
    pub tree: Mutex<Option<Tree>>,
    pub cancel: Arc<AtomicBool>,
    pub scanning: Arc<AtomicBool>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ProgressMsg {
    path: String,
    files: u64,
    dirs: u64,
    bytes: u64,
    elapsed_ms: u64,
    percent: f64,
    phase: String,
    mode: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ScanDone {
    ok: bool,
    error: Option<String>,
    warning: Option<String>,
    root: u32,
    root_path: String,
    mode: String,
    elapsed_ms: u64,
    files: u64,
    dirs: u64,
    total: u64,
    baseline: Option<BaselineInfo>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildList {
    items: Vec<Item>,
    total: usize,
}

type Res<T> = Result<T, String>;

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

fn data_dir(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_data_dir().ok()
}

fn normalize(p: &str) -> Res<String> {
    let mut s = p.trim().replace('/', "\\");
    if s.len() == 2 && s.ends_with(':') {
        s.push('\\');
    }
    while s.len() > 3 && s.ends_with('\\') {
        s.pop();
    }
    if s.len() >= 2 && s.as_bytes()[1] == b':' {
        s = s[..1].to_uppercase() + &s[1..];
    }
    if !Path::new(&s).is_dir() {
        return Err(format!("路径不存在: {s}"));
    }
    Ok(s)
}

fn is_drive_root(s: &str) -> bool {
    s.len() == 3 && s.ends_with(":\\")
}

fn with_tree<T>(state: &State<'_, AppState>, f: impl FnOnce(&Tree) -> T) -> Res<T> {
    let g = state.tree.lock().map_err(|e| e.to_string())?;
    g.as_ref().map(f).ok_or_else(|| "尚未扫描".to_string())
}

#[tauri::command]
pub fn list_drives(app: AppHandle) -> Vec<DriveInfo> {
    let base = data_dir(&app);
    let mut v = sys::drives();
    if let Some(base) = base {
        for d in &mut v {
            if let Some(m) = history::list(&base, &d.root).first() {
                d.last_scan = Some(m.time);
                d.last_total = Some(m.total);
            }
        }
    }
    v
}

#[tauri::command]
pub fn is_admin() -> bool {
    sys::is_admin()
}

#[tauri::command]
pub fn start_scan(app: AppHandle, state: State<'_, AppState>, path: String, use_mft: bool) -> Res<()> {
    let root = normalize(&path)?;
    if state.scanning.swap(true, SeqCst) {
        return Err("正在扫描中".into());
    }
    state.cancel.store(false, SeqCst);
    let app2 = app.clone();
    std::thread::Builder::new()
        .name("scan".into())
        .stack_size(16 << 20)
        .spawn(move || run_scan(app2, root, use_mft))
        .map_err(|e| {
            state.scanning.store(false, SeqCst);
            e.to_string()
        })?;
    Ok(())
}

#[tauri::command]
pub fn cancel_scan(state: State<'_, AppState>) {
    state.cancel.store(true, SeqCst);
}

fn run_scan(app: AppHandle, root: String, use_mft: bool) {
    let state = app.state::<AppState>();
    let c = Arc::new(Counters::default());
    let t0 = Instant::now();
    let stop = Arc::new(AtomicBool::new(false));
    {
        let (app, c, stop, path) = (app.clone(), c.clone(), stop.clone(), root.clone());
        std::thread::spawn(move || {
            while !stop.load(Relaxed) {
                let (done, total) = (c.done.load(Relaxed), c.total.load(Relaxed));
                let _ = app.emit(
                    "scan-progress",
                    ProgressMsg {
                        path: path.clone(),
                        files: c.files.load(Relaxed),
                        dirs: c.dirs.load(Relaxed),
                        bytes: c.bytes.load(Relaxed),
                        elapsed_ms: t0.elapsed().as_millis() as u64,
                        percent: if total > 0 { done as f64 / total as f64 } else { -1.0 },
                        phase: c.phase.lock().unwrap().clone(),
                        mode: c.mode.lock().unwrap().clone(),
                    },
                );
                std::thread::sleep(Duration::from_millis(80));
            }
        });
    }
    let done = scan_inner(&app, &state, &root, use_mft, &c, t0);
    stop.store(true, Relaxed);
    state.scanning.store(false, SeqCst);
    let _ = app.emit("scan-done", done);
}

fn scan_inner(app: &AppHandle, state: &State<'_, AppState>, root: &str, use_mft: bool, c: &Counters, t0: Instant) -> ScanDone {
    let cancel = &*state.cancel;
    let mut warning = None;
    let mut raw: Option<RawTree> = None;
    let mut mode = "遍历";
    let fail = |e: String| ScanDone {
        ok: false,
        error: Some(e),
        warning: None,
        root: 0,
        root_path: root.to_string(),
        mode: String::new(),
        elapsed_ms: t0.elapsed().as_millis() as u64,
        files: 0,
        dirs: 0,
        total: 0,
        baseline: None,
    };

    if use_mft && is_drive_root(root) && sys::fs_name(root).eq_ignore_ascii_case("NTFS") {
        c.set_mode("MFT");
        let letter = root.chars().next().unwrap();
        // The UI stays non-elevated; only a short-lived helper process gets admin rights.
        let r = if sys::is_admin() {
            c.set_phase("读取 MFT");
            mft::scan(letter, c, cancel)
        } else {
            c.set_phase("等待管理员授权");
            elevate::run_elevated(letter, c, cancel)
        };
        match r {
            Ok(r) => {
                raw = Some(r);
                mode = "MFT";
            }
            Err(e) if !cancel.load(Relaxed) => {
                warning = Some(if e == elevate::DENIED {
                    "未获得管理员授权，已改用目录遍历".to_string()
                } else {
                    format!("MFT 读取失败，已回退为目录遍历：{e}")
                });
                c.reset();
            }
            Err(_) => {}
        }
    }
    if cancel.load(Relaxed) {
        return fail("已取消".into());
    }
    let raw = match raw {
        Some(r) => r,
        None => {
            c.set_mode("遍历");
            c.set_phase("遍历目录");
            let r = walker::scan(root, c, cancel);
            if cancel.load(Relaxed) {
                return fail("已取消".into());
            }
            r
        }
    };

    c.set_phase("构建索引");
    c.total.store(0, Relaxed);
    let mut tree = Tree::build(raw, root.to_string());
    let elapsed_ms = t0.elapsed().as_millis() as u64;

    c.set_phase("对比历史");
    if let Some(base) = data_dir(app) {
        if let Some(m) = history::list(&base, root).first() {
            if let Some(s) = history::load(&base, root, &m.file) {
                tree.apply_baseline(Some((&s, m)));
            }
        }
        let now = now_ms();
        let snap = history::build(&tree, now);
        let meta = SnapMeta {
            file: String::new(),
            time: now,
            total: tree.size[tree.root as usize],
            files: tree.file_count,
            dirs: tree.dir_count,
            mode: mode.into(),
            elapsed_ms,
        };
        if let Err(e) = history::save(&base, root, &snap, meta) {
            warning = Some(format!("保存扫描快照失败：{e}"));
        }
    }

    let done = ScanDone {
        ok: true,
        error: None,
        warning,
        root: tree.root,
        root_path: root.to_string(),
        mode: mode.into(),
        elapsed_ms,
        files: tree.file_count,
        dirs: tree.dir_count,
        total: tree.size[tree.root as usize],
        baseline: tree.baseline.clone(),
    };
    *state.tree.lock().unwrap() = Some(tree);
    done
}

#[tauri::command]
pub async fn get_layout(state: State<'_, AppState>, id: u32, budget: usize, min_frac: f64) -> Res<Vec<Item>> {
    with_tree(&state, |t| t.layout(id, budget.clamp(50, 20000), min_frac))
}

#[tauri::command]
pub async fn get_children(state: State<'_, AppState>, id: u32, limit: usize) -> Res<ChildList> {
    with_tree(&state, |t| {
        let (items, total) = t.children_items(id, limit);
        ChildList { items, total }
    })
}

#[tauri::command]
pub async fn get_info(state: State<'_, AppState>, id: u32) -> Res<NodeInfo> {
    with_tree(&state, |t| t.info(id))?.ok_or_else(|| "节点不存在".into())
}

#[tauri::command]
pub async fn get_changes(state: State<'_, AppState>, limit: usize) -> Res<Changes> {
    with_tree(&state, |t| t.changes(limit))
}

#[tauri::command]
pub async fn get_top_files(state: State<'_, AppState>, id: u32, limit: usize) -> Res<Vec<FileItem>> {
    with_tree(&state, |t| t.top_files(id, limit.min(1000)))
}

#[tauri::command]
pub async fn get_extensions(state: State<'_, AppState>, id: u32, limit: usize) -> Res<Vec<ExtItem>> {
    with_tree(&state, |t| t.extensions(id, limit))
}

#[tauri::command]
pub async fn get_history(app: AppHandle, state: State<'_, AppState>) -> Res<Vec<SnapMeta>> {
    let root = with_tree(&state, |t| t.root_path.clone())?;
    Ok(data_dir(&app).map(|b| history::list(&b, &root)).unwrap_or_default())
}

#[tauri::command]
pub async fn set_baseline(app: AppHandle, state: State<'_, AppState>, file: String) -> Res<Option<BaselineInfo>> {
    let base = data_dir(&app).ok_or("无法定位数据目录")?;
    let mut g = state.tree.lock().map_err(|e| e.to_string())?;
    let tree = g.as_mut().ok_or("尚未扫描")?;
    let root = tree.root_path.clone();
    let metas = history::list(&base, &root);
    let m = metas.iter().find(|m| m.file == file).ok_or("快照不存在")?;
    let s = history::load(&base, &root, &file).ok_or("快照读取失败")?;
    tree.apply_baseline(Some((&s, m)));
    Ok(tree.baseline.clone())
}

#[tauri::command]
pub async fn get_path(state: State<'_, AppState>, id: u32) -> Res<String> {
    with_tree(&state, |t| t.path_of(id))
}

#[tauri::command]
pub fn reveal_path(path: String, select: bool) -> Res<()> {
    sys::reveal(&path, select)
}
