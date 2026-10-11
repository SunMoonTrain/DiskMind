use crate::history::{SnapMeta, Snapshot};
use serde::Serialize;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

pub const NONE: u32 = u32::MAX;
pub const UNKNOWN: u64 = u64::MAX;
/// Directories at least this large are kept in history snapshots.
pub const SNAP_DIR_MIN: u64 = 1 << 20;
/// Files at least this large are kept in history snapshots.
pub const SNAP_FILE_MIN: u64 = 16 << 20;
/// Changes smaller than this are not reported.
const CHANGE_MIN: i64 = 1 << 20;

/// Scanner output: a flat parent-pointer forest. Entries with an empty name or
/// NONE parent are ignored (unused MFT records, etc.). `size` is the node's own size.
pub struct RawTree {
    pub names: Vec<Box<str>>,
    pub parent: Vec<u32>,
    pub size: Vec<u64>,
    pub is_dir: Vec<bool>,
    pub root: u32,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct BaselineInfo {
    pub file: String,
    pub time: i64,
    pub total: u64,
}

pub struct Removed {
    pub parent: u32,
    pub path: String,
    pub name: String,
    pub size: u64,
    pub dir: bool,
}

/// Finalized, query-ready tree. Children are stored CSR-style, sorted by size desc.
pub struct Tree {
    pub root_path: String,
    pub root: u32,
    pub names: Vec<Box<str>>,
    pub parent: Vec<u32>,
    pub size: Vec<u64>,
    pub files: Vec<u32>,
    pub is_dir: Vec<bool>,
    pub child_start: Vec<u32>,
    pub child_list: Vec<u32>,
    /// BFS order of every reachable node, root first.
    pub order: Vec<u32>,
    pub file_count: u64,
    pub dir_count: u64,
    /// Size at baseline (UNKNOWN when not tracked).
    pub prev: Vec<u64>,
    pub baseline: Option<BaselineInfo>,
    pub removed: Vec<Removed>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: i64,
    pub parent: i64,
    pub name: String,
    pub size: u64,
    pub files: u32,
    pub dir: bool,
    pub prev: Option<u64>,
    /// > 0 for an aggregate "N small items" entry.
    pub other: u32,
    pub has_children: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Crumb {
    pub id: u32,
    pub name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeInfo {
    pub id: u32,
    pub parent: i64,
    pub name: String,
    pub path: String,
    pub size: u64,
    pub files: u32,
    pub dir: bool,
    pub prev: Option<u64>,
    pub crumbs: Vec<Crumb>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    pub id: i64,
    pub parent: i64,
    pub name: String,
    pub path: String,
    pub size: u64,
    pub prev: u64,
    pub delta: i64,
    pub dir: bool,
    pub kind: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Changes {
    pub baseline: Option<BaselineInfo>,
    pub total_delta: Option<i64>,
    pub grown: Vec<Change>,
    pub shrunk: Vec<Change>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileItem {
    pub id: u32,
    pub parent: u32,
    pub name: String,
    pub path: String,
    pub size: u64,
    pub prev: Option<u64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtItem {
    pub ext: String,
    pub size: u64,
    pub count: u64,
}

impl Tree {
    pub fn build(raw: RawTree, root_path: String) -> Tree {
        let RawTree { mut names, parent, mut size, is_dir, root } = raw;
        let n = names.len();
        let valid = |i: usize| {
            i as u32 != root && (parent[i] as usize) < n && !names[i].is_empty()
        };

        // CSR children via counting sort on parent.
        let mut child_start = vec![0u32; n + 1];
        for i in 0..n {
            if valid(i) {
                child_start[parent[i] as usize + 1] += 1;
            }
        }
        for i in 0..n {
            child_start[i + 1] += child_start[i];
        }
        let mut fill = child_start.clone();
        let mut child_list = vec![0u32; child_start[n] as usize];
        for i in 0..n {
            if valid(i) {
                let p = parent[i] as usize;
                child_list[fill[p] as usize] = i as u32;
                fill[p] += 1;
            }
        }
        drop(fill);

        // BFS from root: reachability + topological order.
        let mut order = Vec::with_capacity(n);
        order.push(root);
        let mut h = 0;
        while h < order.len() {
            let x = order[h] as usize;
            h += 1;
            let (s, e) = (child_start[x] as usize, child_start[x + 1] as usize);
            order.extend_from_slice(&child_list[s..e]);
        }

        // Aggregate sizes bottom-up.
        let mut files = vec![0u32; n];
        let mut file_count = 0u64;
        for &x in &order {
            if !is_dir[x as usize] {
                files[x as usize] = 1;
                file_count += 1;
            }
        }
        for &x in order.iter().rev() {
            if x != root {
                let (x, p) = (x as usize, parent[x as usize] as usize);
                size[p] += size[x];
                files[p] += files[x];
            }
        }
        let dir_count = order.len() as u64 - file_count;

        for x in 0..n {
            let (s, e) = (child_start[x] as usize, child_start[x + 1] as usize);
            if e - s > 1 {
                child_list[s..e].sort_unstable_by(|a, b| size[*b as usize].cmp(&size[*a as usize]));
            }
        }

        names[root as usize] = root_path.clone().into_boxed_str();
        Tree {
            root_path,
            root,
            prev: vec![UNKNOWN; n],
            names,
            parent,
            size,
            files,
            is_dir,
            child_start,
            child_list,
            order,
            file_count,
            dir_count,
            baseline: None,
            removed: Vec::new(),
        }
    }

    #[inline]
    pub fn children(&self, x: u32) -> &[u32] {
        let x = x as usize;
        &self.child_list[self.child_start[x] as usize..self.child_start[x + 1] as usize]
    }

    pub fn valid(&self, id: u32) -> bool {
        (id as usize) < self.names.len()
            && (id == self.root || (self.parent[id as usize] != NONE && !self.names[id as usize].is_empty()))
    }

    fn prev_of(&self, i: usize) -> Option<u64> {
        match self.prev[i] {
            UNKNOWN => None,
            p => Some(p),
        }
    }

    fn parent_id(&self, x: u32) -> i64 {
        if x == self.root { -1 } else { self.parent[x as usize] as i64 }
    }

    fn item(&self, x: u32, parent: i64) -> Item {
        let i = x as usize;
        Item {
            id: x as i64,
            parent,
            name: self.names[i].to_string(),
            size: self.size[i],
            files: self.files[i],
            dir: self.is_dir[i],
            prev: self.prev_of(i),
            other: 0,
            has_children: self.child_start[i + 1] > self.child_start[i],
        }
    }

    pub fn path_of(&self, id: u32) -> String {
        let mut parts = Vec::new();
        let mut x = id;
        while x != self.root && x != NONE {
            parts.push(&*self.names[x as usize]);
            x = self.parent[x as usize];
        }
        let mut s = self.root_path.clone();
        for p in parts.iter().rev() {
            if !s.ends_with('\\') {
                s.push('\\');
            }
            s.push_str(p);
        }
        s
    }

    pub fn info(&self, id: u32) -> Option<NodeInfo> {
        if !self.valid(id) {
            return None;
        }
        let mut crumbs = Vec::new();
        let mut x = id;
        loop {
            crumbs.push(Crumb { id: x, name: self.names[x as usize].to_string() });
            if x == self.root || x == NONE {
                break;
            }
            x = self.parent[x as usize];
        }
        crumbs.reverse();
        let i = id as usize;
        Some(NodeInfo {
            id,
            parent: self.parent_id(id),
            name: self.names[i].to_string(),
            path: self.path_of(id),
            size: self.size[i],
            files: self.files[i],
            dir: self.is_dir[i],
            prev: self.prev_of(i),
            crumbs,
        })
    }

    /// Size-prioritized expansion of the subtree for chart rendering. Children smaller
    /// than `min_frac` of the root are folded into one aggregate item per parent.
    pub fn layout(&self, id: u32, budget: usize, min_frac: f64) -> Vec<Item> {
        let mut out = Vec::new();
        if !self.valid(id) {
            return out;
        }
        out.push(self.item(id, -1));
        let min = ((self.size[id as usize] as f64) * min_frac).max(1.0) as u64;
        let mut heap = BinaryHeap::new();
        heap.push((self.size[id as usize], id));
        while let Some((_, x)) = heap.pop() {
            if out.len() >= budget {
                break;
            }
            let ch = self.children(x);
            let mut k = 0;
            while k < ch.len() {
                let c = ch[k];
                let ci = c as usize;
                if self.size[ci] < min || out.len() >= budget {
                    break;
                }
                out.push(self.item(c, x as i64));
                if self.is_dir[ci] && self.child_start[ci + 1] > self.child_start[ci] {
                    heap.push((self.size[ci], c));
                }
                k += 1;
            }
            if k < ch.len() {
                let rest = &ch[k..];
                let s: u64 = rest.iter().map(|&c| self.size[c as usize]).sum();
                if s > 0 {
                    out.push(Item {
                        id: -(x as i64) - 1,
                        parent: x as i64,
                        name: format!("{} 个小项目", rest.len()),
                        size: s,
                        files: rest.iter().map(|&c| self.files[c as usize]).sum(),
                        dir: false,
                        prev: None,
                        other: rest.len() as u32,
                        has_children: false,
                    });
                }
            }
        }
        out
    }

    pub fn children_items(&self, id: u32, limit: usize) -> (Vec<Item>, usize) {
        if !self.valid(id) {
            return (Vec::new(), 0);
        }
        let ch = self.children(id);
        let items = ch.iter().take(limit).map(|&c| self.item(c, id as i64)).collect();
        (items, ch.len())
    }

    fn for_each_desc(&self, id: u32, mut f: impl FnMut(usize)) {
        if id == self.root {
            self.order.iter().for_each(|&x| f(x as usize));
            return;
        }
        let mut stack = vec![id];
        while let Some(x) = stack.pop() {
            f(x as usize);
            stack.extend_from_slice(self.children(x));
        }
    }

    pub fn top_files(&self, id: u32, limit: usize) -> Vec<FileItem> {
        if !self.valid(id) {
            return Vec::new();
        }
        let mut heap: BinaryHeap<Reverse<(u64, u32)>> = BinaryHeap::with_capacity(limit + 1);
        self.for_each_desc(id, |x| {
            if !self.is_dir[x] {
                let s = self.size[x];
                if heap.len() < limit {
                    heap.push(Reverse((s, x as u32)));
                } else if heap.peek().map_or(false, |r| r.0 .0 < s) {
                    heap.pop();
                    heap.push(Reverse((s, x as u32)));
                }
            }
        });
        let mut v: Vec<_> = heap.into_iter().map(|r| r.0).collect();
        v.sort_unstable_by(|a, b| b.cmp(a));
        v.into_iter()
            .map(|(s, x)| FileItem {
                id: x,
                parent: self.parent[x as usize],
                name: self.names[x as usize].to_string(),
                path: self.path_of(x),
                size: s,
                prev: self.prev_of(x as usize),
            })
            .collect()
    }

    pub fn extensions(&self, id: u32, limit: usize) -> Vec<ExtItem> {
        if !self.valid(id) {
            return Vec::new();
        }
        let mut map: HashMap<Box<str>, (u64, u64)> = HashMap::new();
        self.for_each_desc(id, |x| {
            if self.is_dir[x] {
                return;
            }
            let name = &*self.names[x];
            let ext = match name.rfind('.') {
                Some(p) if p > 0 && p + 1 < name.len() && name.len() - p <= 12 => &name[p + 1..],
                _ => "",
            };
            let key: Box<str> = if ext.bytes().any(|b| b.is_ascii_uppercase()) {
                ext.to_ascii_lowercase().into()
            } else {
                ext.into()
            };
            let e = map.entry(key).or_insert((0, 0));
            e.0 += self.size[x];
            e.1 += 1;
        });
        let mut v: Vec<ExtItem> =
            map.into_iter().map(|(k, (s, c))| ExtItem { ext: k.into(), size: s, count: c }).collect();
        v.sort_unstable_by(|a, b| b.size.cmp(&a.size));
        v.truncate(limit);
        v
    }

    /// Match the snapshot onto this tree by (parent, case-insensitive name) and fill `prev`.
    pub fn apply_baseline(&mut self, snap: Option<(&Snapshot, &SnapMeta)>) {
        let n = self.names.len();
        self.prev = vec![UNKNOWN; n];
        self.removed.clear();
        self.baseline = None;
        let Some((snap, meta)) = snap else { return };
        if snap.nodes.is_empty() {
            return;
        }
        let sn = &snap.nodes;
        let mut map: HashMap<(u32, String), u32> = HashMap::with_capacity(sn.len());
        let mut has_file_child = vec![false; sn.len()];
        for (i, s) in sn.iter().enumerate().skip(1) {
            map.insert((s.parent, s.name.to_lowercase()), i as u32);
            if !s.dir {
                has_file_child[s.parent as usize] = true;
            }
        }
        let mut pidx = vec![NONE; n];
        let mut matched = vec![false; sn.len()];
        let root = self.root as usize;
        pidx[root] = 0;
        matched[0] = true;
        self.prev[root] = sn[0].size;
        for k in 1..self.order.len() {
            let x = self.order[k] as usize;
            let sp = pidx[self.parent[x] as usize];
            if sp == NONE {
                continue;
            }
            let dir = self.is_dir[x];
            let big = dir || self.size[x] >= SNAP_FILE_MIN;
            if !big && !has_file_child[sp as usize] {
                continue;
            }
            match map.get(&(sp, self.names[x].to_lowercase())) {
                Some(&s) if sn[s as usize].dir == dir => {
                    pidx[x] = s;
                    matched[s as usize] = true;
                    self.prev[x] = sn[s as usize].size;
                }
                // Unmatched dirs were < 1 MB or didn't exist: ~0 is accurate. Unmatched files
                // may simply have been under the 16 MB snapshot threshold, so leave them unknown.
                _ if dir => self.prev[x] = 0,
                _ => {}
            }
        }
        // Map snapshot index -> current node id, for attaching removed entries.
        let mut cur_of = vec![NONE; sn.len()];
        for x in 0..n {
            if pidx[x] != NONE {
                cur_of[pidx[x] as usize] = x as u32;
            }
        }
        for (i, s) in sn.iter().enumerate().skip(1) {
            if !matched[i] && matched[s.parent as usize] {
                self.removed.push(Removed {
                    parent: cur_of[s.parent as usize],
                    path: snap_path(snap, i),
                    name: s.name.clone(),
                    size: s.size,
                    dir: s.dir,
                });
            }
        }
        self.baseline = Some(BaselineInfo { file: meta.file.clone(), time: meta.time, total: sn[0].size });
    }

    /// Most specific grown / shrunk entries: a directory is skipped when one child
    /// accounts for >= 70% of its change (the child is reported instead).
    pub fn changes(&self, limit: usize) -> Changes {
        let mut grown = Vec::new();
        let mut shrunk = Vec::new();
        if self.baseline.is_none() {
            return Changes { baseline: None, total_delta: None, grown, shrunk };
        }
        let mut removed_max: HashMap<u32, u64> = HashMap::new();
        for r in &self.removed {
            let e = removed_max.entry(r.parent).or_insert(0);
            *e = (*e).max(r.size);
        }
        for &x in &self.order {
            let xi = x as usize;
            if self.prev[xi] == UNKNOWN {
                continue;
            }
            let d = self.size[xi] as i64 - self.prev[xi] as i64;
            if d.abs() < CHANGE_MIN {
                continue;
            }
            if self.is_dir[xi] {
                let th = d as f64 * 0.7;
                let mut dominated = self.children(x).iter().any(|&c| {
                    let ci = c as usize;
                    if self.prev[ci] == UNKNOWN {
                        return false;
                    }
                    let dc = (self.size[ci] as i64 - self.prev[ci] as i64) as f64;
                    if d > 0 { dc >= th } else { dc <= th }
                });
                if d < 0 {
                    if let Some(&m) = removed_max.get(&x) {
                        dominated |= -(m as f64) <= th;
                    }
                }
                if dominated {
                    continue;
                }
            }
            let c = Change {
                id: x as i64,
                parent: self.parent_id(x),
                name: self.names[xi].to_string(),
                path: self.path_of(x),
                size: self.size[xi],
                prev: self.prev[xi],
                delta: d,
                dir: self.is_dir[xi],
                kind: if self.prev[xi] == 0 { "new" } else if d > 0 { "grown" } else { "shrunk" },
            };
            if d > 0 { grown.push(c) } else { shrunk.push(c) }
        }
        for r in &self.removed {
            if (r.size as i64) < CHANGE_MIN {
                continue;
            }
            shrunk.push(Change {
                id: -1,
                parent: if r.parent == NONE { -1 } else { r.parent as i64 },
                name: r.name.clone(),
                path: r.path.clone(),
                size: 0,
                prev: r.size,
                delta: -(r.size as i64),
                dir: r.dir,
                kind: "removed",
            });
        }
        grown.sort_unstable_by(|a, b| b.delta.cmp(&a.delta));
        shrunk.sort_unstable_by(|a, b| a.delta.cmp(&b.delta));
        grown.truncate(limit);
        shrunk.truncate(limit);
        let r = self.root as usize;
        Changes {
            baseline: self.baseline.clone(),
            total_delta: Some(self.size[r] as i64 - self.prev[r] as i64),
            grown,
            shrunk,
        }
    }
}

fn snap_path(snap: &Snapshot, mut i: usize) -> String {
    let mut parts = Vec::new();
    while i != 0 {
        parts.push(snap.nodes[i].name.as_str());
        i = snap.nodes[i].parent as usize;
    }
    let mut s = snap.root.clone();
    for p in parts.iter().rev() {
        if !s.ends_with('\\') {
            s.push('\\');
        }
        s.push_str(p);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history;

    const MB: u64 = 1 << 20;

    /// root ─┬─ A/ ─┬─ a1 (n MB)
    ///       │      └─ a2 (5 MB)
    ///       ├─ B/ ── b1 (20 MB)
    ///       └─ big.iso (size_iso)
    fn sample(a1: u64, with_b: bool, iso: u64) -> Tree {
        let mut raw = RawTree { names: vec![], parent: vec![], size: vec![], is_dir: vec![], root: 0 };
        let mut add = |n: &str, p: u32, s: u64, d: bool| {
            raw.names.push(n.into());
            raw.parent.push(p);
            raw.size.push(s);
            raw.is_dir.push(d);
            raw.names.len() as u32 - 1
        };
        let r = add("X:\\", NONE, 0, true);
        let a = add("A", r, 0, true);
        add("a1", a, a1 * MB, false);
        add("a2", a, 5 * MB, false);
        if with_b {
            let b = add("B", r, 0, true);
            add("b1", b, 20 * MB, false);
        }
        add("big.iso", r, iso * MB, false);
        Tree::build(raw, "X:\\".into())
    }

    #[test]
    fn aggregates_and_sorts() {
        let t = sample(10, true, 30);
        assert_eq!(t.size[t.root as usize], 65 * MB);
        assert_eq!(t.files[t.root as usize], 4);
        let names: Vec<_> = t.children(t.root).iter().map(|&c| &*t.names[c as usize]).collect();
        assert_eq!(names, ["big.iso", "B", "A"]);
    }

    #[test]
    fn baseline_diff() {
        let old = sample(10, true, 30);
        let snap = history::build(&old, 1);
        let meta = history::SnapMeta { file: "1.dmsnap".into(), time: 1, total: 0, files: 0, dirs: 0, mode: String::new(), elapsed_ms: 0 };

        // A grows by 40 MB (inside a1), B is deleted, iso shrinks to 20 MB.
        let mut cur = sample(50, false, 20);
        cur.apply_baseline(Some((&snap, &meta)));
        let ch = cur.changes(50);
        assert_eq!(ch.total_delta, Some(10 * MB as i64));
        // a1 was below the snapshot file threshold, so the growth is attributed to A.
        let g: Vec<_> = ch.grown.iter().map(|c| (c.name.as_str(), c.kind, c.delta / MB as i64)).collect();
        assert_eq!(g, [("A", "grown", 40)]);
        let s: Vec<_> = ch.shrunk.iter().map(|c| (c.name.as_str(), c.kind, c.delta / MB as i64)).collect();
        assert!(s.contains(&("B", "removed", -20)), "{s:?}");
        assert!(s.contains(&("big.iso", "shrunk", -10)), "{s:?}");
    }
}
