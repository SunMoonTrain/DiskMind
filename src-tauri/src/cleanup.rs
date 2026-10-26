//! Built-in cleanup actions an MCP client may ask for. The AI only picks an action by id;
//! what runs is fixed here, and the user confirms it in the GUI first. Actions that need
//! administrator rights run in a short-lived elevated helper (see `elevate`).

use crate::{elevate, sys};
use serde::{Deserialize, Serialize};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Temp files younger than this may belong to running programs and are kept.
const TEMP_MIN_AGE: Duration = Duration::from_secs(7 * 86_400);

enum Run {
    /// A native implementation in this file.
    Builtin(fn() -> Result<String, String>),
    /// An external program with fixed arguments.
    Exec(&'static str, &'static [&'static str]),
}

pub struct Action {
    pub id: &'static str,
    pub title: &'static str,
    pub desc: &'static str,
    /// What the user sees in the confirmation dialog.
    pub command: &'static str,
    pub admin: bool,
    run: Run,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionInfo {
    pub id: &'static str,
    pub title: &'static str,
    pub desc: &'static str,
    pub command: &'static str,
    pub admin: bool,
    /// Bytes this is expected to free, when it can be measured up front.
    pub estimate: Option<u64>,
}

#[derive(Serialize, Deserialize)]
pub struct Outcome {
    /// Growth of free space across fixed drives while the action ran.
    pub freed: u64,
    pub detail: String,
}

const ACTIONS: &[Action] = &[
    Action {
        id: "empty_recycle_bin",
        title: "清空回收站",
        desc: "永久删除所有磁盘回收站里的文件，之前移到回收站的项目将无法还原。",
        command: "SHEmptyRecycleBin（所有磁盘）",
        admin: false,
        run: Run::Builtin(empty_recycle_bin),
    },
    Action {
        id: "user_temp",
        title: "清理用户临时文件",
        desc: "删除当前用户临时文件夹中 7 天前的文件，正在使用的会自动跳过。",
        command: "删除 %TEMP% 中 7 天未修改的文件",
        admin: false,
        run: Run::Builtin(user_temp),
    },
    Action {
        id: "windows_temp",
        title: "清理 Windows 临时文件和更新缓存",
        desc: "删除 Windows\\Temp 中 7 天前的文件，以及已下载的 Windows 更新安装包（需要时会重新下载）。",
        command: "删除 %SystemRoot%\\Temp 中 7 天未修改的文件和 %SystemRoot%\\SoftwareDistribution\\Download 的内容",
        admin: true,
        run: Run::Builtin(windows_temp),
    },
    Action {
        id: "component_cleanup",
        title: "清理 Windows 组件存储",
        desc: "删除已被更新替代的旧版系统组件（WinSxS），可能需要几分钟到半小时。之后无法卸载已安装的更新。",
        command: "dism.exe /Online /Cleanup-Image /StartComponentCleanup",
        admin: true,
        run: Run::Exec("dism.exe", &["/Online", "/Cleanup-Image", "/StartComponentCleanup"]),
    },
    Action {
        id: "hibernation_off",
        title: "关闭休眠",
        desc: "删除 hiberfil.sys。关闭后无法使用休眠，快速启动也会失效；可以用 powercfg /hibernate on 恢复。",
        command: "powercfg.exe /hibernate off",
        admin: true,
        run: Run::Exec("powercfg.exe", &["/hibernate", "off"]),
    },
    Action {
        id: "npm_cache",
        title: "清理 npm 缓存",
        desc: "删除 npm 下载缓存，之后安装依赖时会重新下载。",
        command: "npm cache clean --force",
        admin: false,
        run: Run::Exec("npm", &["cache", "clean", "--force"]),
    },
    Action {
        id: "pip_cache",
        title: "清理 pip 缓存",
        desc: "删除 pip 下载和构建缓存，之后安装包时会重新下载。",
        command: "pip cache purge",
        admin: false,
        run: Run::Exec("pip", &["cache", "purge"]),
    },
    Action {
        id: "nuget_cache",
        title: "清理 NuGet 缓存",
        desc: "删除 NuGet 全局包和 HTTP 缓存，之后还原 .NET 项目时会重新下载。",
        command: "dotnet nuget locals all --clear",
        admin: false,
        run: Run::Exec("dotnet", &["nuget", "locals", "all", "--clear"]),
    },
    Action {
        id: "docker_prune",
        title: "清理 Docker 无用数据",
        desc: "删除已停止的容器、未使用的网络、悬空镜像和构建缓存，不删除数据卷。Docker Desktop 的数据在虚拟磁盘里，宿主机空间要压缩虚拟磁盘后才会变多。",
        command: "docker system prune -f",
        admin: false,
        run: Run::Exec("docker", &["system", "prune", "-f"]),
    },
];

pub fn find(id: &str) -> Option<&'static Action> {
    ACTIONS.iter().find(|a| a.id == id)
}

fn env_path(k: &str) -> Option<PathBuf> {
    std::env::var_os(k).map(PathBuf::from)
}

fn hiberfil() -> Option<PathBuf> {
    let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
    let p = PathBuf::from(format!("{drive}\\hiberfil.sys"));
    // metadata falls back to FindFirstFile for in-use files like this one.
    std::fs::metadata(&p).is_ok().then_some(p)
}

/// `where` resolves .exe/.cmd shims on PATH the same way cmd does.
fn on_path(program: &str) -> bool {
    Command::new("where.exe").arg(program).creation_flags(CREATE_NO_WINDOW).output().is_ok_and(|o| o.status.success())
}

/// Bytes that will be freed (None: unknown up front), or Err when the action does not apply.
fn estimate(a: &Action) -> Result<Option<u64>, ()> {
    let local = env_path("LOCALAPPDATA");
    let dir = |p: Option<PathBuf>| p.filter(|p| p.is_dir()).map(|p| old_files(&p, Duration::ZERO).0);
    match a.id {
        "empty_recycle_bin" => match recycle_bin_size() {
            0 => Err(()),
            n => Ok(Some(n)),
        },
        "user_temp" => Ok(Some(old_files(&std::env::temp_dir(), TEMP_MIN_AGE).0)),
        "windows_temp" => Ok(None),
        "component_cleanup" => Ok(None),
        "hibernation_off" => hiberfil().map(|p| std::fs::metadata(p).ok().map(|m| m.len())).ok_or(()),
        "npm_cache" if on_path("npm") => Ok(dir(local.map(|l| l.join("npm-cache")))),
        "pip_cache" if on_path("pip") => Ok(dir(local.map(|l| l.join("pip").join("cache")))),
        "nuget_cache" if on_path("dotnet") => Ok(dir(env_path("USERPROFILE").map(|h| h.join(".nuget").join("packages")))),
        "docker_prune" if on_path("docker") => Ok(None),
        _ => Err(()),
    }
}

/// Actions that apply to this machine, with what they are expected to free.
pub fn available() -> Vec<ActionInfo> {
    std::thread::scope(|s| {
        let jobs: Vec<_> = ACTIONS.iter().map(|a| (a, s.spawn(move || estimate(a)))).collect();
        jobs.into_iter()
            .filter_map(|(a, j)| {
                let estimate = j.join().ok()?.ok()?;
                Some(ActionInfo { id: a.id, title: a.title, desc: a.desc, command: a.command, admin: a.admin, estimate })
            })
            .collect()
    })
}

/// Runs the action, elevating through a helper process when it needs admin rights.
pub fn run(id: &str) -> Result<Outcome, String> {
    let a = find(id).ok_or("未知的清理操作")?;
    if a.admin && !sys::is_admin() {
        elevate::run_cleanup(id)
    } else {
        run_here(id)
    }
}

/// Runs the action in this process.
pub fn run_here(id: &str) -> Result<Outcome, String> {
    let a = find(id).ok_or("未知的清理操作")?;
    let before = total_free();
    let detail = match a.run {
        Run::Builtin(f) => f()?,
        Run::Exec(program, args) => exec(program, args)?,
    };
    Ok(Outcome { freed: total_free().saturating_sub(before), detail })
}

fn total_free() -> u64 {
    sys::drives().iter().filter(|d| d.kind == "fixed").map(|d| d.free).sum()
}

fn exec(program: &str, args: &[&str]) -> Result<String, String> {
    // cmd /c so that npm.cmd-style shims resolve like they do in a terminal.
    let out = Command::new("cmd.exe")
        .arg("/d")
        .arg("/c")
        .arg(program)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("无法运行 {program}：{e}"))?;
    let mut text = decode(&out.stdout);
    text.push_str(&decode(&out.stderr));
    // Keep the end, where tools print their summary.
    let lines: Vec<&str> = text.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
    let tail = lines[lines.len().saturating_sub(15)..].join("\n");
    if out.status.success() {
        Ok(tail)
    } else {
        Err(format!("{program} 退出码 {}\n{tail}", out.status.code().unwrap_or(-1)))
    }
}

/// Console tools print UTF-8 or the OEM code page (GBK on Chinese Windows).
fn decode(b: &[u8]) -> String {
    use windows_sys::Win32::Globalization::{MultiByteToWideChar, CP_OEMCP};
    if let Ok(s) = std::str::from_utf8(b) {
        return s.to_string();
    }
    let n = unsafe { MultiByteToWideChar(CP_OEMCP, 0, b.as_ptr(), b.len() as i32, std::ptr::null_mut(), 0) };
    if n <= 0 {
        return String::from_utf8_lossy(b).into_owned();
    }
    let mut w = vec![0u16; n as usize];
    unsafe { MultiByteToWideChar(CP_OEMCP, 0, b.as_ptr(), b.len() as i32, w.as_mut_ptr(), n) };
    String::from_utf16_lossy(&w)
}

fn recycle_bin_size() -> u64 {
    use windows_sys::Win32::UI::Shell::{SHQueryRecycleBinW, SHQUERYRBINFO};
    let mut info: SHQUERYRBINFO = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHQUERYRBINFO>() as u32;
    if unsafe { SHQueryRecycleBinW(std::ptr::null(), &mut info) } != 0 {
        return 0;
    }
    info.i64Size.max(0) as u64
}

fn empty_recycle_bin() -> Result<String, String> {
    use windows_sys::Win32::UI::Shell::{SHEmptyRecycleBinW, SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI, SHERB_NOSOUND};
    let size = recycle_bin_size();
    let r = unsafe {
        SHEmptyRecycleBinW(std::ptr::null_mut(), std::ptr::null(), SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND)
    };
    // S_FALSE-style codes when the bin was already empty are not failures.
    if r < 0 && recycle_bin_size() > 0 {
        return Err(format!("清空回收站失败（0x{:X}）", r as u32));
    }
    Ok(format!("已清空回收站，原有 {}", crate::report::fmt_size(size)))
}

/// Files under `root` not modified for `min_age`: (bytes, paths).
fn old_files(root: &Path, min_age: Duration) -> (u64, Vec<PathBuf>) {
    let now = SystemTime::now();
    let (mut bytes, mut files) = (0u64, Vec::new());
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(m) = e.metadata() else { continue };
            // Never follow junctions or symlinks out of the folder.
            if m.file_type().is_symlink() || sys::is_reparse(&m) {
                continue;
            }
            if m.is_dir() {
                stack.push(e.path());
            } else if m.modified().ok().and_then(|t| now.duration_since(t).ok()).is_some_and(|age| age >= min_age) {
                bytes += m.len();
                files.push(e.path());
            }
        }
    }
    (bytes, files)
}

/// Deletes old files under `root` (in-use ones are skipped), then empty old folders.
fn purge(root: &Path, min_age: Duration) -> (u64, usize, usize) {
    let (_, files) = old_files(root, min_age);
    let (mut freed, mut done, mut skipped) = (0u64, 0usize, 0usize);
    for f in &files {
        let len = std::fs::metadata(f).map_or(0, |m| m.len());
        match std::fs::remove_file(f) {
            Ok(()) => {
                freed += len;
                done += 1;
            }
            Err(_) => skipped += 1,
        }
    }
    remove_empty_dirs(root, min_age, true);
    (freed, done, skipped)
}

fn remove_empty_dirs(dir: &Path, min_age: Duration, is_root: bool) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        if e.metadata().is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink() && !sys::is_reparse(&m)) {
            remove_empty_dirs(&e.path(), min_age, false);
        }
    }
    let old = std::fs::metadata(dir).ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()).is_some_and(|a| a >= min_age);
    if !is_root && old {
        let _ = std::fs::remove_dir(dir); // fails unless empty
    }
}

fn purge_summary(label: &str, (freed, done, skipped): (u64, usize, usize)) -> String {
    let mut s = format!("{label}：删除 {done} 个文件，{}", crate::report::fmt_size(freed));
    if skipped > 0 {
        s.push_str(&format!("，{skipped} 个正在使用已跳过"));
    }
    s
}

fn user_temp() -> Result<String, String> {
    Ok(purge_summary("%TEMP%", purge(&std::env::temp_dir(), TEMP_MIN_AGE)))
}

fn windows_temp() -> Result<String, String> {
    let win = env_path("SystemRoot").ok_or("找不到 Windows 目录")?;
    let a = purge_summary("Windows\\Temp", purge(&win.join("Temp"), TEMP_MIN_AGE));
    let b = purge_summary("更新缓存", purge(&win.join("SoftwareDistribution").join("Download"), Duration::ZERO));
    Ok(format!("{a}\n{b}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purge_keeps_recent_files() {
        let root = std::env::temp_dir().join(format!("diskmind-purge-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub").join("new.tmp"), b"12345").unwrap();
        std::fs::write(root.join("x.tmp"), b"123").unwrap();

        // Nothing is a week old yet.
        assert_eq!(purge(&root, TEMP_MIN_AGE), (0, 0, 0));
        assert!(root.join("sub").join("new.tmp").exists());

        let (freed, done, _) = purge(&root, Duration::ZERO);
        assert_eq!((freed, done), (8, 2));
        assert!(!root.join("sub").exists(), "emptied folders past the age limit go too");
        assert!(root.exists(), "the root itself is kept");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn exec_reports_output_and_failure() {
        assert!(exec("where.exe", &["cmd"]).unwrap().to_lowercase().contains("cmd.exe"));
        let e = exec("where.exe", &["diskmind-no-such-program"]).unwrap_err();
        assert!(e.contains("退出码"), "{e}");
    }

    #[test]
    fn decodes_utf8_and_ids_are_unique() {
        assert_eq!(decode("清理完成".as_bytes()), "清理完成");
        let mut ids: Vec<_> = ACTIONS.iter().map(|a| a.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), ACTIONS.len());
    }
}
