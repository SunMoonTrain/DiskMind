//! Direct NTFS Master File Table reader (requires administrator rights).
//!
//! Reads `$MFT` sequentially from the raw volume and parses every FILE record for its
//! name, parent directory and data size — the same trick WizTree uses to avoid walking
//! the directory hierarchy through the filesystem API.

use crate::progress::Counters;
use crate::tree::{RawTree, NONE};
use rayon::prelude::*;
use std::fs::File;
use std::os::windows::fs::FileExt;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc::sync_channel;

const CHUNK: u64 = 8 << 20;
const ROOT_RECORD: u32 = 5;
const SPARSE: u64 = u64::MAX;

#[inline]
fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
#[inline]
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
#[inline]
fn u64le(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

fn read_full(f: &File, buf: &mut [u8], mut off: u64) -> std::io::Result<()> {
    let mut done = 0;
    while done < buf.len() {
        let n = f.seek_read(&mut buf[done..], off)?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        done += n;
        off += n as u64;
    }
    Ok(())
}

/// Apply the update-sequence-array fixups (512-byte stride). Returns false if the
/// record is not a valid FILE record.
fn fixup(rec: &mut [u8]) -> bool {
    if rec.len() < 48 || &rec[0..4] != b"FILE" {
        return false;
    }
    let usa = u16le(rec, 4) as usize;
    let cnt = u16le(rec, 6) as usize;
    if cnt == 0 || usa + cnt * 2 > rec.len() {
        return false;
    }
    let (u0, u1) = (rec[usa], rec[usa + 1]);
    for i in 1..cnt {
        let end = i * 512;
        if end > rec.len() {
            break;
        }
        if rec[end - 2] != u0 || rec[end - 1] != u1 {
            return false;
        }
        rec[end - 2] = rec[usa + i * 2];
        rec[end - 1] = rec[usa + i * 2 + 1];
    }
    true
}

fn decode_runs(b: &[u8]) -> Vec<(u64, u64)> {
    let mut v = Vec::new();
    let mut p = 0;
    let mut lcn: i64 = 0;
    while p < b.len() {
        let h = b[p];
        if h == 0 {
            break;
        }
        p += 1;
        let ls = (h & 0xF) as usize;
        let os = (h >> 4) as usize;
        if ls > 8 || os > 8 || p + ls + os > b.len() {
            break;
        }
        let mut len = 0u64;
        for i in 0..ls {
            len |= (b[p + i] as u64) << (8 * i);
        }
        p += ls;
        if os == 0 {
            v.push((SPARSE, len));
        } else {
            let mut off: i64 = 0;
            for i in 0..os {
                off |= (b[p + i] as i64) << (8 * i);
            }
            if os < 8 && b[p + os - 1] & 0x80 != 0 {
                off |= -1i64 << (8 * os);
            }
            lcn += off;
            v.push((lcn as u64, len));
        }
        p += os;
    }
    v
}

struct Parsed {
    idx: u32,
    base: u32,
    dir: bool,
    name: Option<Box<str>>,
    parent: u32,
    size: u64,
}

fn parse_record(rec: &mut [u8], idx: u32) -> Option<Parsed> {
    if !fixup(rec) {
        return None;
    }
    let flags = u16le(rec, 0x16);
    if flags & 1 == 0 {
        return None;
    }
    let base = (u64le(rec, 0x20) & 0xFFFF_FFFF_FFFF) as u32;
    let mut off = u16le(rec, 0x14) as usize;
    let mut name = None;
    let mut parent = NONE;
    let mut size = 0u64;
    while off + 16 <= rec.len() {
        let ty = u32le(rec, off);
        if ty == 0xFFFF_FFFF {
            break;
        }
        let len = u32le(rec, off + 4) as usize;
        if len < 16 || off + len > rec.len() {
            break;
        }
        let end = off + len;
        let nonres = rec[off + 8] != 0;
        let attr_name_len = rec[off + 9];
        match ty {
            // $FILE_NAME: always resident. Skip DOS 8.3 names (namespace 2).
            0x30 if !nonres && name.is_none() && len >= 0x18 => {
                let v = off + u16le(rec, off + 0x14) as usize;
                if v + 0x42 <= end && rec[v + 0x41] != 2 {
                    let nl = rec[v + 0x40] as usize;
                    if v + 0x42 + nl * 2 <= end {
                        parent = (u64le(rec, v) & 0xFFFF_FFFF_FFFF) as u32;
                        let u: Vec<u16> = (0..nl).map(|i| u16le(rec, v + 0x42 + i * 2)).collect();
                        name = Some(String::from_utf16_lossy(&u).into_boxed_str());
                    }
                }
            }
            // $DATA: count every stream. Named streams (e.g. $UsnJrnl:$J) are often
            // sparse, so use their allocated size.
            0x80 => {
                if nonres {
                    if len >= 0x40 && u64le(rec, off + 0x10) == 0 {
                        let alloc = u64le(rec, off + 0x28);
                        let real = u64le(rec, off + 0x30);
                        size += if attr_name_len == 0 { real } else { alloc.min(real) };
                    }
                } else if len >= 0x18 {
                    size += u32le(rec, off + 0x10) as u64;
                }
            }
            _ => {}
        }
        off = end;
    }
    Some(Parsed { idx, base: if base == 0 { NONE } else { base }, dir: flags & 2 != 0, name, parent, size })
}

pub fn scan(letter: char, c: &Counters, cancel: &AtomicBool) -> Result<RawTree, String> {
    let path = format!(r"\\.\{}:", letter);
    let vol = File::open(&path).map_err(|e| format!("无法打开卷 {path}: {e}"))?;

    let mut boot = [0u8; 512];
    read_full(&vol, &mut boot, 0).map_err(|e| format!("读取引导扇区失败: {e}"))?;
    if &boot[3..11] != b"NTFS    " {
        return Err("不是 NTFS 卷".into());
    }
    let bps = u16le(&boot, 0x0B) as u64;
    let mut spc = boot[0x0D] as u64;
    if spc > 0x80 {
        spc = 1u64 << (256 - spc);
    }
    let cluster = bps * spc;
    let mft_lcn = u64le(&boot, 0x30);
    let cpr = boot[0x40] as i8;
    let rec_size = if cpr > 0 { cpr as u64 * cluster } else { 1u64 << (-(cpr as i32)) as u32 };
    if cluster == 0 || rec_size < 512 || rec_size > 65536 {
        return Err("引导扇区参数异常".into());
    }

    // Record 0 is $MFT itself; its unnamed $DATA runs describe where the table lives.
    let mut first = vec![0u8; rec_size.max(cluster) as usize];
    read_full(&vol, &mut first, mft_lcn * cluster).map_err(|e| format!("读取 $MFT 失败: {e}"))?;
    let rec0 = &mut first[..rec_size as usize];
    if !fixup(rec0) {
        return Err("$MFT 记录损坏".into());
    }
    let (runs, mft_bytes) = mft_runs(rec0).ok_or("无法解析 $MFT 数据位置")?;
    let total = (mft_bytes / rec_size) as usize;
    c.total.store(total as u64, Relaxed);

    let mut names: Vec<Box<str>> = vec![Box::default(); total];
    let mut parent = vec![NONE; total];
    let mut size = vec![0u64; total];
    let mut is_dir = vec![false; total];
    let rs = rec_size as usize;

    std::thread::scope(|s| -> Result<(), String> {
        let (tx, rx) = sync_channel::<std::io::Result<Vec<u8>>>(4);
        let vol = &vol;
        s.spawn(move || {
            for &(lcn, len) in &runs {
                let mut pos = 0u64;
                let bytes = len * cluster;
                while pos < bytes {
                    if cancel.load(Relaxed) {
                        return;
                    }
                    let n = (bytes - pos).min(CHUNK);
                    let mut b = vec![0u8; n as usize];
                    let r = if lcn == SPARSE { Ok(()) } else { read_full(vol, &mut b, lcn * cluster + pos) };
                    let failed = r.is_err();
                    if tx.send(r.map(|_| b)).is_err() || failed {
                        return;
                    }
                    pos += n;
                }
            }
        });

        let mut idx = 0usize;
        let mut pending: Vec<u8> = Vec::new();
        for chunk in rx {
            let chunk = chunk.map_err(|e| format!("读取 MFT 数据失败: {e}"))?;
            let mut data = if pending.is_empty() {
                chunk
            } else {
                pending.extend_from_slice(&chunk);
                std::mem::take(&mut pending)
            };
            let nrec = (data.len() / rs).min(total - idx);
            let base_idx = idx;
            let parsed: Vec<Parsed> = data[..nrec * rs]
                .par_chunks_mut(rs)
                .enumerate()
                .filter_map(|(i, r)| parse_record(r, (base_idx + i) as u32))
                .collect();
            let (mut nf, mut nd, mut nb) = (0u64, 0u64, 0u64);
            for p in parsed {
                let i = p.idx as usize;
                if p.base == NONE {
                    is_dir[i] = p.dir;
                    size[i] += p.size;
                    if let Some(n) = p.name {
                        names[i] = n;
                        parent[i] = p.parent;
                    }
                    if p.dir { nd += 1 } else { nf += 1 }
                } else if (p.base as usize) < total {
                    // Extension record: fold its attributes into the base record.
                    let b = p.base as usize;
                    size[b] += p.size;
                    if names[b].is_empty() {
                        if let Some(n) = p.name {
                            names[b] = n;
                            parent[b] = p.parent;
                        }
                    }
                }
                nb += p.size;
            }
            c.files.fetch_add(nf, Relaxed);
            c.dirs.fetch_add(nd, Relaxed);
            c.bytes.fetch_add(nb, Relaxed);
            idx += nrec;
            c.done.store(idx as u64, Relaxed);
            if idx >= total {
                break;
            }
            pending = data.split_off(nrec * rs);
        }
        Ok(())
    })?;

    if cancel.load(Relaxed) {
        return Err("已取消".into());
    }
    if (ROOT_RECORD as usize) >= total {
        return Err("MFT 过小".into());
    }
    names[ROOT_RECORD as usize] = format!("{letter}:\\").into_boxed_str();
    Ok(RawTree { names, parent, size, is_dir, root: ROOT_RECORD })
}

fn mft_runs(rec: &[u8]) -> Option<(Vec<(u64, u64)>, u64)> {
    let mut off = u16le(rec, 0x14) as usize;
    while off + 16 <= rec.len() {
        let ty = u32le(rec, off);
        if ty == 0xFFFF_FFFF {
            break;
        }
        let len = u32le(rec, off + 4) as usize;
        if len < 16 || off + len > rec.len() {
            break;
        }
        if ty == 0x80 && rec[off + 8] != 0 && rec[off + 9] == 0 && len >= 0x40 {
            let real = u64le(rec, off + 0x30);
            let ro = off + u16le(rec, off + 0x20) as usize;
            return Some((decode_runs(&rec[ro..off + len]), real));
        }
        off += len;
    }
    None
}
