//! Thin wrappers over Win32: drive enumeration, elevation, Explorer.

use serde::Serialize;
use std::os::windows::process::CommandExt;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
};
use windows_sys::Win32::UI::Shell::{IsUserAnAdmin, ShellExecuteW};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveInfo {
    pub root: String,
    pub label: String,
    pub fs: String,
    pub total: u64,
    pub free: u64,
    pub kind: String,
    pub last_scan: Option<i64>,
    pub last_total: Option<u64>,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_wide(b: &[u16]) -> String {
    let n = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf16_lossy(&b[..n])
}

fn vol_info(root_w: &[u16]) -> (String, String) {
    let mut name = [0u16; 261];
    let mut fsn = [0u16; 261];
    let (mut serial, mut maxc, mut flags) = (0u32, 0u32, 0u32);
    let ok = unsafe {
        GetVolumeInformationW(
            root_w.as_ptr(),
            name.as_mut_ptr(),
            name.len() as u32,
            &mut serial,
            &mut maxc,
            &mut flags,
            fsn.as_mut_ptr(),
            fsn.len() as u32,
        )
    };
    if ok == 0 {
        return (String::new(), String::new());
    }
    (from_wide(&name), from_wide(&fsn))
}

pub fn drives() -> Vec<DriveInfo> {
    let mask = unsafe { GetLogicalDrives() };
    let mut v = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let root = format!("{}:\\", (b'A' + i as u8) as char);
        let w = wide(&root);
        let kind = match unsafe { GetDriveTypeW(w.as_ptr()) } {
            2 => "removable",
            3 => "fixed",
            4 => "network",
            6 => "ram",
            _ => continue,
        };
        let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
        if unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut avail, &mut total, &mut free) } == 0 {
            continue;
        }
        let (label, fs) = vol_info(&w);
        v.push(DriveInfo { root, label, fs, total, free, kind: kind.into(), last_scan: None, last_total: None });
    }
    v
}

pub fn fs_name(root: &str) -> String {
    vol_info(&wide(root)).1
}

pub fn is_admin() -> bool {
    unsafe { IsUserAnAdmin() != 0 }
}

pub fn relaunch_admin() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let op = wide("runas");
    let file = wide(&exe.to_string_lossy());
    let r = unsafe { ShellExecuteW(null_mut(), op.as_ptr(), file.as_ptr(), null(), null(), 1) };
    if r as isize > 32 {
        Ok(())
    } else {
        Err("已取消或启动失败".into())
    }
}

pub fn reveal(path: &str, select: bool) -> Result<(), String> {
    let mut cmd = std::process::Command::new("explorer.exe");
    if select {
        cmd.raw_arg(format!("/select,\"{path}\""));
    } else {
        cmd.raw_arg(format!("\"{path}\""));
    }
    cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
}
