//! Runs the MFT reader in a short-lived elevated child process, so the UI itself never
//! runs as administrator. An elevated foreground window blocks keyboard hooks of
//! non-elevated apps (UIPI), which breaks global hotkeys of tools like PixPin.
//!
//! Protocol (all files next to `<out>` in %TEMP%):
//! - `<out>`           bincode RawTree, written by the child on success
//! - `<out>.progress`  5 × u64 LE (files, dirs, bytes, done, total), rewritten every 80 ms
//! - `<out>.cancel`    created by the parent to request cancellation
//! - `<out>.err`       error message, written by the child on failure

use crate::mft;
use crate::progress::Counters;
use crate::tree::RawTree;
use std::fs;
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::Duration;
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};

pub const FLAG: &str = "--mft-helper";
const WAIT_OBJECT_0: u32 = 0;
const WAIT_TIMEOUT: u32 = 258;
const ERROR_CANCELLED: i32 = 1223;
pub const DENIED: &str = "已取消管理员授权";

fn side(out: &Path, ext: &str) -> PathBuf {
    let mut s = out.as_os_str().to_owned();
    s.push(ext);
    s.into()
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Child entry point: `diskmind.exe --mft-helper <letter> <out>`. Returns the exit code.
pub fn helper_main(letter: &str, out: &str) -> i32 {
    let out = Path::new(out);
    let Some(letter) = letter.chars().next().filter(|c| c.is_ascii_alphabetic()) else { return 2 };
    let c = Counters::default();
    let cancel = AtomicBool::new(false);
    let stop = AtomicBool::new(false);
    let res = std::thread::scope(|s| {
        s.spawn(|| {
            let (pf, cf) = (side(out, ".progress"), side(out, ".cancel"));
            while !stop.load(Relaxed) {
                if cf.exists() {
                    cancel.store(true, Relaxed);
                }
                let mut b = Vec::with_capacity(40);
                for v in [&c.files, &c.dirs, &c.bytes, &c.done, &c.total] {
                    b.extend_from_slice(&v.load(Relaxed).to_le_bytes());
                }
                let _ = fs::write(&pf, b);
                std::thread::sleep(Duration::from_millis(80));
            }
        });
        let r = mft::scan(letter, &c, &cancel).and_then(|raw| {
            let f = fs::File::create(out).map_err(|e| e.to_string())?;
            let mut w = BufWriter::with_capacity(1 << 20, f);
            bincode::serialize_into(&mut w, &raw).map_err(|e| e.to_string())?;
            w.flush().map_err(|e| e.to_string())
        });
        stop.store(true, Relaxed);
        r
    });
    match res {
        Ok(()) => 0,
        Err(e) => {
            let _ = fs::write(side(out, ".err"), e);
            1
        }
    }
}

/// Parent side: launch the helper with a UAC prompt, mirror its progress into `c`,
/// and load the resulting tree.
pub fn run_elevated(letter: char, c: &Counters, cancel: &AtomicBool) -> Result<RawTree, String> {
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let out = std::env::temp_dir().join(format!("diskmind-mft-{}-{nonce}.bin", std::process::id()));
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let verb = wide("runas");
    let file = wide(&exe.to_string_lossy());
    let params = wide(&format!("{FLAG} {letter} \"{}\"", out.display()));

    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = 0; // SW_HIDE
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        let e = std::io::Error::last_os_error();
        return Err(if e.raw_os_error() == Some(ERROR_CANCELLED) {
            DENIED.into()
        } else {
            format!("无法启动 MFT 扫描进程: {e}")
        });
    }
    let h = info.hProcess;
    if h.is_null() {
        return Err("无法启动 MFT 扫描进程".into());
    }

    let pf = side(&out, ".progress");
    let mut cancel_sent = false;
    let wait = loop {
        let w = unsafe { WaitForSingleObject(h, 80) };
        if let Ok(b) = fs::read(&pf) {
            if b.len() == 40 {
                let v = |i: usize| u64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().unwrap());
                for (i, a) in [&c.files, &c.dirs, &c.bytes, &c.done, &c.total].into_iter().enumerate() {
                    a.store(v(i), Relaxed);
                }
                if v(4) > 0 {
                    c.set_phase("读取 MFT");
                }
            }
        }
        if cancel.load(Relaxed) && !cancel_sent {
            let _ = fs::write(side(&out, ".cancel"), b"");
            cancel_sent = true;
        }
        if w != WAIT_TIMEOUT {
            break w;
        }
    };
    let mut code = 1u32;
    unsafe {
        GetExitCodeProcess(h, &mut code);
        CloseHandle(h);
    }

    let res = if cancel.load(Relaxed) {
        Err("已取消".into())
    } else if wait != WAIT_OBJECT_0 {
        Err("等待 MFT 扫描进程失败".into())
    } else if code == 0 {
        fs::File::open(&out)
            .map_err(|e| e.to_string())
            .and_then(|f| bincode::deserialize_from(BufReader::with_capacity(1 << 20, f)).map_err(|e| e.to_string()))
    } else {
        Err(fs::read_to_string(side(&out, ".err")).unwrap_or_else(|_| format!("MFT 扫描进程退出码 {code}")))
    };
    for ext in ["", ".progress", ".cancel", ".err"] {
        let _ = fs::remove_file(side(&out, ext));
    }
    res
}

#[cfg(test)]
mod tests {
    use crate::tree::{RawTree, NONE};

    #[test]
    fn raw_tree_roundtrip() {
        let raw = RawTree {
            names: vec!["X:\\".into(), "目录".into(), "".into(), "a.bin".into()],
            parent: vec![NONE, 0, NONE, 1],
            size: vec![0, 0, 0, 123_456_789_012],
            is_dir: vec![true, true, false, false],
            mtime: vec![0, 0, 0, 1_700_000_000],
            root: 0,
        };
        let bytes = bincode::serialize(&raw).unwrap();
        let back: RawTree = bincode::deserialize(&bytes).unwrap();
        assert_eq!(back.names, raw.names);
        assert_eq!(back.parent, raw.parent);
        assert_eq!(back.size, raw.size);
        assert_eq!(back.is_dir, raw.is_dir);
        assert_eq!(back.mtime, raw.mtime);
        assert_eq!(back.root, 0);
    }
}
