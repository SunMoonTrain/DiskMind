//! Parallel directory walker — the fallback when the MFT can't be read
//! (no admin rights, non-NTFS volume, or scanning a sub-folder).

use crate::progress::Counters;
use crate::tree::{RawTree, NONE};
use rayon::prelude::*;
use std::os::windows::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

struct DirRes {
    name: Box<str>,
    files: Vec<(Box<str>, u64)>,
    dirs: Vec<DirRes>,
}

fn walk(path: PathBuf, name: Box<str>, c: &Counters, cancel: &AtomicBool) -> DirRes {
    let mut files = Vec::new();
    let mut sub = Vec::new();
    if !cancel.load(Relaxed) {
        if let Ok(rd) = std::fs::read_dir(&path) {
            let mut bytes = 0u64;
            for e in rd.flatten() {
                // On Windows DirEntry::metadata comes from the FindNextFile data: no extra syscall.
                let Ok(md) = e.metadata() else { continue };
                // Skip junctions / symlinks so nothing is counted twice.
                if md.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                    continue;
                }
                let nm: Box<str> = e.file_name().to_string_lossy().into();
                if md.is_dir() {
                    sub.push((e.path(), nm));
                } else {
                    bytes += md.len();
                    files.push((nm, md.len()));
                }
            }
            c.files.fetch_add(files.len() as u64, Relaxed);
            c.bytes.fetch_add(bytes, Relaxed);
        }
        c.dirs.fetch_add(1, Relaxed);
    }
    let dirs = sub.into_par_iter().map(|(p, n)| walk(p, n, c, cancel)).collect();
    DirRes { name, files, dirs }
}

pub fn scan(root: &str, c: &Counters, cancel: &AtomicBool) -> RawTree {
    let threads = std::thread::available_parallelism().map_or(8, |n| n.get()).clamp(4, 16) * 2;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .stack_size(8 << 20)
        .build()
        .expect("thread pool");
    let res = pool.install(|| walk(PathBuf::from(root), root.into(), c, cancel));

    let mut raw = RawTree { names: Vec::new(), parent: Vec::new(), size: Vec::new(), is_dir: Vec::new(), root: 0 };
    // Iterative flatten (deep trees would otherwise risk stack overflow).
    let mut stack = vec![(res, NONE)];
    while let Some((d, p)) = stack.pop() {
        let id = raw.names.len() as u32;
        raw.names.push(d.name);
        raw.parent.push(p);
        raw.size.push(0);
        raw.is_dir.push(true);
        for (n, s) in d.files {
            raw.names.push(n);
            raw.parent.push(id);
            raw.size.push(s);
            raw.is_dir.push(false);
        }
        stack.extend(d.dirs.into_iter().map(|c| (c, id)));
    }
    raw
}
