//! Scan snapshots on disk, used to diff the current scan against earlier ones.
//!
//! Layout: `<app_data>/history/<fnv(root)>/index.json` + one gzip'd bincode file per scan.
//! Only directories >= 1 MB and files >= 16 MB are stored, which keeps a full-drive
//! snapshot to a few MB.

use crate::tree::{Tree, NONE, SNAP_DIR_MIN, SNAP_FILE_MIN};
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

const KEEP: usize = 30;
/// Snapshot format version. MFT snapshots below 2 over-counted sparse metadata streams
/// ($BadClus:$Bad, $UsnJrnl:$J) by up to the whole volume size and are discarded.
pub const VER: u32 = 2;

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SnapMeta {
    pub file: String,
    /// Unix time in milliseconds.
    pub time: i64,
    pub total: u64,
    pub files: u64,
    pub dirs: u64,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub elapsed_ms: u64,
    #[serde(default)]
    pub ver: u32,
}

impl SnapMeta {
    fn valid(&self) -> bool {
        !(self.mode == "MFT" && self.ver < 2)
    }
}

#[derive(Serialize, Deserialize)]
pub struct Snapshot {
    pub root: String,
    pub time: i64,
    /// nodes[0] is the root; `parent` indexes into this vec.
    pub nodes: Vec<SnapNode>,
}

#[derive(Serialize, Deserialize)]
pub struct SnapNode {
    pub parent: u32,
    pub name: String,
    pub size: u64,
    pub dir: bool,
}

fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn dir_for(base: &Path, root: &str) -> PathBuf {
    base.join("history").join(format!("{:016x}", fnv(&root.to_lowercase())))
}

fn read_index(base: &Path, root: &str) -> Vec<SnapMeta> {
    let p = dir_for(base, root).join("index.json");
    let mut v: Vec<SnapMeta> =
        fs::read(p).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    v.sort_by(|a, b| b.time.cmp(&a.time));
    v
}

pub fn list(base: &Path, root: &str) -> Vec<SnapMeta> {
    let mut v = read_index(base, root);
    v.retain(SnapMeta::valid);
    v
}

pub fn load(base: &Path, root: &str, file: &str) -> Option<Snapshot> {
    if file.contains(['/', '\\']) || file.contains("..") {
        return None;
    }
    let f = fs::File::open(dir_for(base, root).join(file)).ok()?;
    bincode::deserialize_from(GzDecoder::new(BufReader::new(f))).ok()
}

pub fn save(base: &Path, root: &str, snap: &Snapshot, mut meta: SnapMeta) -> Result<SnapMeta, String> {
    let e2s = |e: &dyn std::fmt::Display| e.to_string();
    let dir = dir_for(base, root);
    fs::create_dir_all(&dir).map_err(|e| e2s(&e))?;
    let file = format!("{}.dmsnap", snap.time);
    meta.file = file.clone();

    let f = fs::File::create(dir.join(&file)).map_err(|e| e2s(&e))?;
    let mut enc = GzEncoder::new(BufWriter::new(f), Compression::fast());
    bincode::serialize_into(&mut enc, snap).map_err(|e| e2s(&e))?;
    enc.finish().and_then(|mut w| w.flush()).map_err(|e| e2s(&e))?;

    let mut all = read_index(base, root);
    all.retain(|m| {
        let keep = m.valid();
        if !keep {
            let _ = fs::remove_file(dir.join(&m.file));
        }
        keep
    });
    all.retain(|m| m.file != file);
    all.insert(0, meta.clone());
    while all.len() > KEEP {
        if let Some(old) = all.pop() {
            let _ = fs::remove_file(dir.join(&old.file));
        }
    }
    fs::write(dir.join("index.json"), serde_json::to_vec(&all).map_err(|e| e2s(&e))?).map_err(|e| e2s(&e))?;
    Ok(meta)
}

pub fn build(tree: &Tree, time: i64) -> Snapshot {
    let mut nodes = Vec::new();
    let mut map = vec![NONE; tree.names.len()];
    let r = tree.root as usize;
    nodes.push(SnapNode { parent: NONE, name: String::new(), size: tree.size[r], dir: true });
    map[r] = 0;
    // BFS order guarantees parents are mapped before their children.
    for &x in tree.order.iter().skip(1) {
        let x = x as usize;
        let mp = map[tree.parent[x] as usize];
        if mp == NONE {
            continue;
        }
        let keep = if tree.is_dir[x] { tree.size[x] >= SNAP_DIR_MIN } else { tree.size[x] >= SNAP_FILE_MIN };
        if keep {
            map[x] = nodes.len() as u32;
            nodes.push(SnapNode { parent: mp, name: tree.names[x].to_string(), size: tree.size[x], dir: tree.is_dir[x] });
        }
    }
    Snapshot { root: tree.root_path.clone(), time, nodes }
}
