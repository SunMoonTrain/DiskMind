//! Local MCP endpoint (Streamable HTTP, plain JSON responses, no SSE) served by the running
//! GUI, so an AI client can look at the current scan and ask to recycle files. Nothing is
//! deleted until the user approves it in a dialog; approved items go to the Recycle Bin.

use crate::commands::{self, AppState};
use crate::report::{self, fmt_size};
use crate::sys;
use crate::tree::Tree;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering::*};
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

pub const PORT: u16 = 47_821;
const PROTOCOLS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const SCAN_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const MAX_DELETE: usize = 200;

/// One confirmation dialog at a time.
static DELETE_LOCK: Mutex<()> = Mutex::new(());
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

const INSTRUCTIONS: &str = "DiskMind is a disk space analyzer running on the user's Windows PC. \
Tools work on the scan currently loaded in its window; call `status` first and `scan` if nothing is loaded. \
Explore with `list_dir`, `top_files`, `extensions`, `changes` or `report`, explain what is safe to remove, \
then call `recycle` with the exact paths. The user confirms every deletion in the DiskMind window and can untick items; \
approved items go to the Recycle Bin (space is freed once it is emptied). Write `reason` in the user's language. \
Never propose deleting personal documents, photos, projects or anything you are unsure about.";

pub fn start(app: AppHandle) {
    let port = std::env::var("DISKMIND_MCP_PORT").ok().and_then(|s| s.parse().ok()).unwrap_or(PORT);
    let state = app.state::<AppState>();
    let server = match tiny_http::Server::http(("127.0.0.1", port)) {
        Ok(s) => s,
        Err(e) => {
            *state.mcp.lock().unwrap() = Err(format!("端口 {port} 不可用：{e}"));
            return;
        }
    };
    *state.mcp.lock().unwrap() = Ok(format!("http://127.0.0.1:{port}/mcp"));
    std::thread::Builder::new()
        .name("mcp".into())
        .spawn(move || {
            for req in server.incoming_requests() {
                let app = app.clone();
                // Tool calls can block for minutes (scans, confirmation), so one thread each.
                std::thread::spawn(move || handle(&app, req, port));
            }
        })
        .ok();
}

fn header(req: &tiny_http::Request, name: &'static str) -> Option<String> {
    req.headers().iter().find(|h| h.field.equiv(name)).map(|h| h.value.as_str().to_string())
}

/// Host or Origin authority must be loopback; this blocks DNS-rebinding requests from web pages.
fn is_local(authority: &str, port: u16) -> bool {
    let a = authority.trim_start_matches("http://").trim_end_matches('/');
    let host = a.rsplit_once(':').map_or(a, |(h, p)| if p == port.to_string() { h } else { "" });
    matches!(host, "127.0.0.1" | "localhost" | "[::1]")
}

fn handle(app: &AppHandle, mut req: tiny_http::Request, port: u16) {
    use tiny_http::{Header, Method, Response};
    let local = header(&req, "Host").map_or(true, |h| is_local(&h, port))
        && header(&req, "Origin").map_or(true, |o| is_local(&o, port));
    let path = req.url().split('?').next().unwrap_or("").to_string();
    let reply = |req: tiny_http::Request, code: u16, body: Option<Value>| {
        let mut r = Response::from_string(body.map(|b| b.to_string()).unwrap_or_default()).with_status_code(code);
        if code == 200 {
            r.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
        }
        let _ = req.respond(r);
    };
    if !local {
        return reply(req, 403, None);
    }
    if path != "/mcp" && path != "/" {
        return reply(req, 404, None);
    }
    if *req.method() != Method::Post {
        // No server-initiated stream and no sessions to end.
        return reply(req, 405, None);
    }
    let mut body = String::new();
    if req.as_reader().take(4 << 20).read_to_string(&mut body).is_err() {
        return reply(req, 400, None);
    }
    let msg: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => return reply(req, 400, Some(rpc_error(Value::Null, -32700, &e.to_string()))),
    };
    let out = match msg {
        Value::Array(batch) => {
            let v: Vec<Value> = batch.into_iter().filter_map(|m| dispatch(app, m)).collect();
            (!v.is_empty()).then(|| Value::Array(v))
        }
        m => dispatch(app, m),
    };
    match out {
        Some(v) => reply(req, 200, Some(v)),
        None => reply(req, 202, None),
    }
}

fn rpc_error(id: Value, code: i64, msg: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": msg } })
}

/// Handles one JSON-RPC message; notifications and responses get no reply.
fn dispatch(app: &AppHandle, m: Value) -> Option<Value> {
    let id = m.get("id").cloned()?;
    let method = m.get("method").and_then(Value::as_str)?;
    let params = m.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
            let version = PROTOCOLS.iter().find(|&&v| v == asked).unwrap_or(&PROTOCOLS[0]);
            json!({
                "protocolVersion": version,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "diskmind", "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS,
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tool_defs() }),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            let (text, is_error) = match call_tool(app, name, &args) {
                Ok(t) => (t, false),
                Err(e) => (e, true),
            };
            json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
        }
        _ => return Some(rpc_error(id, -32601, &format!("method not found: {method}"))),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn tool_defs() -> Value {
    let path = json!({ "type": "string", "description": "Absolute folder path inside the loaded scan. Defaults to the scan root." });
    let limit = |d: u32, max: u32| json!({ "type": "integer", "minimum": 1, "maximum": max, "default": d });
    let ro = json!({ "readOnlyHint": true });
    json!([
        {
            "name": "status",
            "description": "Drives with capacity and free space, whether a scan is running, and a summary of the scan loaded in DiskMind.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": ro,
        },
        {
            "name": "scan",
            "description": "Scan a drive (e.g. \"C:\\\\\") or folder and load it in DiskMind, replacing the current scan. Waits until done. NTFS drive roots are read from the MFT (fast; Windows may ask the user for admin rights) unless use_mft is false.",
            "inputSchema": {
                "type": "object",
                "properties": { "path": { "type": "string" }, "use_mft": { "type": "boolean", "default": true } },
                "required": ["path"],
            },
        },
        {
            "name": "list_dir",
            "description": "Immediate children of a folder, largest first, with size, file count, newest modification date and growth since the baseline scan.",
            "inputSchema": { "type": "object", "properties": { "path": path, "limit": limit(50, 1000) } },
            "annotations": ro,
        },
        {
            "name": "top_files",
            "description": "Largest files anywhere under a folder.",
            "inputSchema": { "type": "object", "properties": { "path": path, "limit": limit(50, 1000) } },
            "annotations": ro,
        },
        {
            "name": "extensions",
            "description": "Total size and count per file extension under a folder.",
            "inputSchema": { "type": "object", "properties": { "path": path, "limit": limit(30, 500) } },
            "annotations": ro,
        },
        {
            "name": "changes",
            "description": "Folders and files that grew, appeared, shrank or disappeared since the baseline (previous) scan.",
            "inputSchema": { "type": "object", "properties": { "limit": limit(30, 500) } },
            "annotations": ro,
        },
        {
            "name": "report",
            "description": "Markdown overview of a folder: pruned size tree, totals by extension, largest files, large items untouched for over a year, and growth since the baseline. A good first call.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": path,
                    "anonymize": { "type": "boolean", "default": false, "description": "Replace user names and personal folder contents with aliases." },
                },
            },
            "annotations": ro,
        },
        {
            "name": "recycle",
            "description": "Ask the user to move files or folders to the Recycle Bin. DiskMind shows a confirmation dialog listing every path with its size and your reason; the user can untick items or decline. Blocks until they answer (up to 10 minutes). System folders, Program Files, drive roots and profile folders are refused.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "paths": { "type": "array", "items": { "type": "string" }, "minItems": 1, "maxItems": MAX_DELETE, "description": "Absolute paths." },
                    "reason": { "type": "string", "description": "Why these are safe to remove, shown to the user. Use the user's language." },
                },
                "required": ["paths", "reason"],
            },
            "annotations": { "destructiveHint": true },
        },
    ])
}

fn arg_str<'a>(a: &'a Value, k: &str) -> Option<&'a str> {
    a.get(k).and_then(Value::as_str).filter(|s| !s.trim().is_empty())
}

fn arg_limit(a: &Value, d: usize, max: usize) -> usize {
    a.get("limit").and_then(Value::as_u64).map_or(d, |n| n as usize).clamp(1, max)
}

fn pretty(v: Value) -> String {
    serde_json::to_string_pretty(&v).unwrap_or_default()
}

fn date(unix: u32) -> Value {
    if unix == 0 {
        return Value::Null;
    }
    let (y, m, d) = report::civil(unix as i64 / 86_400);
    json!(format!("{y:04}-{m:02}-{d:02}"))
}

fn delta(size: u64, prev: Option<u64>) -> Value {
    prev.map_or(Value::Null, |p| json!(fmt_delta(size as i64 - p as i64)))
}

fn fmt_delta(d: i64) -> String {
    format!("{}{}", if d < 0 { "-" } else { "+" }, fmt_size(d.unsigned_abs()))
}

/// Runs `f` on the loaded tree with `path` resolved to a node id.
fn on_tree<T>(app: &AppHandle, path: Option<&str>, f: impl FnOnce(&Tree, u32) -> T) -> Result<T, String> {
    let state = app.state::<AppState>();
    let g = state.tree.lock().map_err(|e| e.to_string())?;
    let t = g.as_ref().ok_or("No scan loaded in DiskMind yet; call `scan` first.")?;
    let id = match path {
        None => t.root,
        Some(p) => t.find(p).ok_or_else(|| format!("{p} is not in the loaded scan of {}", t.root_path))?,
    };
    Ok(f(t, id))
}

fn call_tool(app: &AppHandle, name: &str, a: &Value) -> Result<String, String> {
    let path = arg_str(a, "path");
    match name {
        "status" => Ok(status(app)),
        "scan" => scan(app, path.ok_or("path is required")?, a.get("use_mft").and_then(Value::as_bool).unwrap_or(true)),
        "list_dir" => on_tree(app, path, |t, id| {
            let (items, total) = t.children_items(id, arg_limit(a, 50, 1000));
            let base = t.path_of(id);
            let rows: Vec<Value> = items
                .iter()
                .map(|it| {
                    json!({
                        "name": it.name,
                        "dir": it.dir,
                        "size": fmt_size(it.size),
                        "bytes": it.size,
                        "files": it.files,
                        "modified": date(it.mtime),
                        "sinceBaseline": delta(it.size, it.prev),
                    })
                })
                .collect();
            pretty(json!({ "path": base, "size": fmt_size(t.size[id as usize]), "children": total, "items": rows }))
        }),
        "top_files" => on_tree(app, path, |t, id| {
            let rows: Vec<Value> = t
                .top_files(id, arg_limit(a, 50, 1000))
                .iter()
                .map(|f| json!({ "path": f.path, "size": fmt_size(f.size), "bytes": f.size, "modified": date(t.mtime[f.id as usize]) }))
                .collect();
            pretty(json!(rows))
        }),
        "extensions" => on_tree(app, path, |t, id| {
            let rows: Vec<Value> = t
                .extensions(id, arg_limit(a, 30, 500))
                .iter()
                .map(|e| json!({ "ext": e.ext, "size": fmt_size(e.size), "bytes": e.size, "count": e.count }))
                .collect();
            pretty(json!(rows))
        }),
        "changes" => on_tree(app, None, |t, _| {
            let ch = t.changes(arg_limit(a, 30, 500));
            let rows = |v: &[crate::tree::Change]| -> Vec<Value> {
                v.iter()
                    .map(|c| json!({ "path": c.path, "kind": c.kind, "delta": fmt_delta(c.delta), "size": fmt_size(c.size), "dir": c.dir }))
                    .collect()
            };
            match &ch.baseline {
                None => "No earlier scan of this location to compare with.".to_string(),
                Some(b) => pretty(json!({
                    "baselineTime": date((b.time / 1000) as u32),
                    "totalDelta": ch.total_delta.map(fmt_delta),
                    "grown": rows(&ch.grown),
                    "shrunk": rows(&ch.shrunk),
                })),
            }
        }),
        "report" => {
            let drives = sys::drives();
            on_tree(app, path, |t, id| {
                let opts = report::ReportOpts {
                    scope: id,
                    anonymize: a.get("anonymize").and_then(Value::as_bool).unwrap_or(false),
                    tz_offset_min: local_tz_offset_min(),
                };
                let letter = t.root_path.get(..3).unwrap_or_default();
                report::build(t, &opts, drives.iter().find(|d| d.root.eq_ignore_ascii_case(letter)))
            })
        }
        "recycle" => recycle(app, a),
        _ => Err(format!("unknown tool: {name}")),
    }
}

/// Minutes to add to local time to get UTC, like JS `getTimezoneOffset()`.
fn local_tz_offset_min() -> i32 {
    use windows_sys::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
    let mut tz: TIME_ZONE_INFORMATION = unsafe { std::mem::zeroed() };
    let r = unsafe { GetTimeZoneInformation(&mut tz) };
    tz.Bias + if r == 2 { tz.DaylightBias } else { 0 }
}

fn status(app: &AppHandle) -> String {
    let state = app.state::<AppState>();
    let drives: Vec<Value> = sys::drives()
        .iter()
        .map(|d| json!({ "root": d.root, "label": d.label, "fs": d.fs, "kind": d.kind, "total": fmt_size(d.total), "free": fmt_size(d.free) }))
        .collect();
    let scan = state.tree.lock().ok().and_then(|g| {
        g.as_ref().map(|t| {
            json!({
                "root": t.root_path,
                "mode": t.mode,
                "scannedAt": date((t.scan_time / 1000) as u32),
                "size": fmt_size(t.size[t.root as usize]),
                "files": t.file_count,
                "dirs": t.dir_count,
                "baseline": t.baseline.as_ref().map(|b| date((b.time / 1000) as u32)),
            })
        })
    });
    pretty(json!({ "scanning": state.scanning.load(SeqCst), "loadedScan": scan, "drives": drives }))
}

fn scan(app: &AppHandle, path: &str, use_mft: bool) -> Result<String, String> {
    let state = app.state::<AppState>();
    *state.last_done.lock().unwrap() = None;
    commands::begin_scan(app, &state, path, use_mft)?;
    let t0 = Instant::now();
    while state.scanning.load(SeqCst) {
        if t0.elapsed() > SCAN_TIMEOUT {
            return Err("Scan is still running; check `status` later.".into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let d = state.last_done.lock().unwrap().clone().ok_or("scan finished without a result")?;
    if !d.ok {
        return Err(d.error.unwrap_or_else(|| "scan failed".into()));
    }
    Ok(pretty(json!({
        "root": d.root_path,
        "mode": d.mode,
        "seconds": d.elapsed_ms as f64 / 1000.0,
        "size": fmt_size(d.total),
        "files": d.files,
        "dirs": d.dirs,
        "warning": d.warning,
        "hasBaseline": d.baseline.is_some(),
    })))
}

/// Cleanup must never touch the OS, installed programs or the profile skeleton.
fn protected(p: &str) -> Option<&'static str> {
    let l = p.to_lowercase();
    if l.len() <= 3 {
        return Some("不能删除整个磁盘");
    }
    let env = |k: &str| std::env::var(k).ok().map(|v| v.trim_end_matches('\\').to_lowercase()).filter(|v| v.len() > 3);
    let under = |root: &str| l == root || l.starts_with(&format!("{root}\\"));
    if let Some(win) = env("SystemRoot") {
        // Windows\Temp is the one part of the OS folder that is fair game.
        if under(&win) && !l.starts_with(&format!("{win}\\temp\\")) {
            return Some("Windows 系统目录");
        }
    }
    for k in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
        if env(k).is_some_and(|r| under(&r)) {
            return Some("程序安装目录，请用“应用和功能”卸载");
        }
    }
    let first = l[3..].split('\\').next().unwrap_or("");
    if first.starts_with('$')
        || first == "system volume information"
        || (l[3..] == *first && ["pagefile.sys", "hiberfil.sys", "swapfile.sys", "bootmgr"].contains(&first))
    {
        return Some("系统文件");
    }
    if l[3..] == *"users" {
        return Some("用户目录");
    }
    let mut exact: Vec<String> = ["ProgramData", "APPDATA", "LOCALAPPDATA", "PUBLIC"].iter().filter_map(|k| env(k)).collect();
    if let Some(home) = env("USERPROFILE") {
        for sub in ["", "\\appdata", "\\appdata\\locallow", "\\desktop", "\\documents", "\\downloads", "\\pictures", "\\videos", "\\music", "\\onedrive"] {
            exact.push(format!("{home}{sub}"));
        }
    }
    exact.iter().any(|e| *e == l).then_some("系统或个人文件夹本身（可以删除其中的内容）")
}

fn check_path(raw: &str) -> Result<String, String> {
    let mut p = raw.trim().replace('/', "\\");
    while p.len() > 3 && p.ends_with('\\') {
        p.pop();
    }
    let b = p.as_bytes();
    if b.len() < 3 || !b[0].is_ascii_alphabetic() || b[1] != b':' || b[2] != b'\\' {
        return Err("需要以盘符开头的绝对路径".into());
    }
    p = p[..1].to_uppercase() + &p[1..];
    if p.len() > 3 && p[3..].split('\\').any(|c| c.is_empty() || c == "." || c == "..") {
        return Err("路径格式不正确".into());
    }
    if let Some(why) = protected(&p) {
        return Err(format!("受保护：{why}"));
    }
    if std::fs::symlink_metadata(&p).is_err() {
        return Err("文件不存在".into());
    }
    Ok(p)
}

fn recycle(app: &AppHandle, a: &Value) -> Result<String, String> {
    let reason = arg_str(a, "reason").ok_or("reason is required")?.trim().to_string();
    let raw: Vec<&str> = a.get("paths").and_then(Value::as_array).ok_or("paths is required")?.iter().filter_map(Value::as_str).collect();
    if raw.is_empty() || raw.len() > MAX_DELETE {
        return Err(format!("paths must hold 1 to {MAX_DELETE} entries"));
    }
    let _one = DELETE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let state = app.state::<AppState>();

    // Validate, drop duplicates and anything already covered by a listed parent folder.
    let mut ok: Vec<String> = Vec::new();
    let mut rejected: Vec<(String, String)> = Vec::new();
    for r in &raw {
        match check_path(r) {
            Ok(p) => ok.push(p),
            Err(e) => rejected.push((r.to_string(), e)),
        }
    }
    ok.sort_by_key(|p| p.to_lowercase());
    ok.dedup_by_key(|p| p.to_lowercase());
    let lower: Vec<String> = ok.iter().map(|p| p.to_lowercase()).collect();
    ok.retain(|p| {
        let l = p.to_lowercase();
        !lower.iter().any(|o| l.len() > o.len() && l.starts_with(o.as_str()) && l.as_bytes()[o.len()] == b'\\')
    });

    let items: Vec<Value> = {
        let g = state.tree.lock().map_err(|e| e.to_string())?;
        let mut v: Vec<Value> = ok
            .iter()
            .map(|p| {
                let meta = std::fs::symlink_metadata(p).ok();
                let dir = meta.as_ref().is_some_and(|m| m.is_dir());
                let node = g.as_ref().and_then(|t| t.find(p).map(|id| (t, id)));
                let size = node.map(|(t, id)| t.size[id as usize]).or_else(|| meta.filter(|m| m.is_file()).map(|m| m.len()));
                let files = node.map(|(t, id)| t.files[id as usize]);
                json!({ "path": p, "dir": dir, "size": size, "files": files, "error": null })
            })
            .collect();
        v.extend(rejected.iter().map(|(p, e)| json!({ "path": p, "dir": false, "size": null, "files": null, "error": e })));
        v
    };
    if ok.is_empty() {
        let why: Vec<String> = rejected.iter().map(|(p, e)| format!("{p}: {e}")).collect();
        return Err(format!("Nothing to delete.\n{}", why.join("\n")));
    }

    let id = NEXT_ID.fetch_add(1, SeqCst);
    let (tx, rx) = mpsc::channel();
    state.pending_deletes.lock().unwrap().insert(id, tx);
    let _ = app.emit("mcp-delete-request", json!({ "id": id, "reason": reason, "items": items }));
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
    let approved = match rx.recv_timeout(CONFIRM_TIMEOUT) {
        Ok(v) => v,
        Err(_) => {
            state.pending_deletes.lock().unwrap().remove(&id);
            let _ = app.emit("mcp-delete-cancel", id);
            return Err("The user did not answer within 10 minutes; nothing was deleted.".into());
        }
    };
    // Only paths that were validated and shown may be deleted, whatever the UI sends back.
    let approved: HashSet<String> = approved.iter().map(|p| p.to_lowercase()).collect();
    if approved.is_empty() {
        return Ok("The user declined; nothing was deleted.".into());
    }

    let (mut done, mut failed, mut skipped) = (Vec::new(), Vec::new(), Vec::new());
    let mut freed = 0u64;
    for p in &ok {
        if !approved.contains(&p.to_lowercase()) {
            skipped.push(p.clone());
            continue;
        }
        match sys::recycle(p) {
            Ok(()) => {
                let mut g = state.tree.lock().unwrap();
                let size = g.as_mut().and_then(|t| {
                    let id = t.find(p)?;
                    let s = t.size[id as usize];
                    t.remove(id);
                    Some(s)
                });
                freed += size.unwrap_or(0);
                done.push(json!({ "path": p, "size": size.map(fmt_size) }));
            }
            Err(e) => failed.push(json!({ "path": p, "error": e })),
        }
    }
    if !done.is_empty() {
        let totals = state.tree.lock().unwrap().as_ref().map(|t| {
            json!({ "total": t.size[t.root as usize], "files": t.file_count, "dirs": t.dir_count, "freed": freed })
        });
        let _ = app.emit("tree-changed", totals);
    }
    Ok(pretty(json!({
        "recycled": done,
        "total": fmt_size(freed),
        "notApprovedByUser": skipped,
        "failed": failed,
        "rejected": rejected.iter().map(|(p, e)| json!({ "path": p, "error": e })).collect::<Vec<_>>(),
        "note": "Items are in the Recycle Bin; the space is freed once it is emptied.",
    })))
}
