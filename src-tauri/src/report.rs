//! Markdown report meant to be pasted into an AI chat: a pruned size tree, file types,
//! largest files, long-untouched items and growth since the baseline scan.

use crate::sys::DriveInfo;
use crate::tree::{Tree, NONE};
use serde::Deserialize;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::fmt::Write;

const MB: u64 = 1 << 20;
const GB: u64 = 1 << 30;
const DAY: i64 = 86_400;
/// Expanded nodes in the tree section (each may add one "others" line).
const TREE_NODES: usize = 140;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportOpts {
    /// Node to report on (scan root or the folder being viewed).
    pub scope: u32,
    /// Hide user names and the contents of personal folders.
    pub anonymize: bool,
    /// `Date.getTimezoneOffset()` from the UI, for local dates.
    pub tz_offset_min: i32,
}

pub fn fmt_size(b: u64) -> String {
    if b < 1024 {
        return format!("{b} B");
    }
    let units = ["KB", "MB", "GB", "TB", "PB"];
    let mut v = b as f64 / 1024.0;
    let mut i = 0;
    while v >= 1024.0 && i < units.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    let n = if v >= 100.0 { format!("{v:.0}") } else if v >= 10.0 { format!("{v:.1}") } else { format!("{v:.2}") };
    format!("{n} {}", units[i])
}

fn fmt_delta(d: i64) -> String {
    match d {
        0 => "±0".into(),
        d if d > 0 => format!("+{}", fmt_size(d as u64)),
        d => format!("−{}", fmt_size(d.unsigned_abs())),
    }
}

fn fmt_pct(f: f64) -> String {
    let p = f * 100.0;
    if p >= 10.0 { format!("{p:.0}%") } else if p >= 1.0 { format!("{p:.1}%") } else { format!("{p:.2}%") }
}

fn fmt_num(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Days since 1970-01-01 -> (year, month, day). Howard Hinnant's civil_from_days.
fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + (m <= 2) as i64, m, d)
}

struct Clock {
    /// Scan time, Unix seconds.
    now: i64,
    tz_offset_min: i32,
}

impl Clock {
    fn local(&self, secs: i64) -> i64 {
        secs - self.tz_offset_min as i64 * 60
    }
    fn date(&self, secs: u32) -> String {
        if secs == 0 {
            return "未知".into();
        }
        let (y, m, d) = civil(self.local(secs as i64).div_euclid(DAY));
        format!("{y}-{m:02}-{d:02}")
    }
    fn datetime_ms(&self, ms: i64) -> String {
        let t = self.local(ms.div_euclid(1000));
        let (y, m, d) = civil(t.div_euclid(DAY));
        let s = t.rem_euclid(DAY);
        format!("{y}-{m:02}-{d:02} {:02}:{:02}", s / 3600, s % 3600 / 60)
    }
    fn age(&self, secs: u32) -> String {
        if secs == 0 {
            return "未知".into();
        }
        let days = (self.now - secs as i64) / DAY;
        match days {
            d if d < 1 => "今天".into(),
            d if d < 60 => format!("{d} 天前"),
            d if d < 365 => format!("{} 个月前", d / 30),
            d => format!("{:.1} 年前", d as f64 / 365.0),
        }
    }
}

/// Replaces user names and the contents of personal folders with stable per-report aliases.
struct Masker {
    on: bool,
    user: String,
    aliases: HashMap<String, String>,
    salt: u64,
}

const PUBLIC_PROFILES: [&str; 5] = ["public", "default", "default user", "all users", "desktop.ini"];
const PERSONAL: [&str; 11] = [
    "desktop", "documents", "downloads", "pictures", "videos", "music", "favorites", "contacts", "links",
    "searches", "saved games",
];

impl Masker {
    fn new(on: bool) -> Self {
        let user = std::env::var("USERNAME").unwrap_or_default().to_lowercase();
        let salt = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(7, |d| d.as_nanos() as u64);
        Masker { on, user, aliases: HashMap::new(), salt }
    }

    fn is_personal(name: &str) -> bool {
        let l = name.to_lowercase();
        PERSONAL.contains(&l.as_str()) || l.starts_with("onedrive")
    }

    fn user_alias(&mut self, name: &str) -> String {
        let l = name.to_lowercase();
        if l == self.user {
            return "<当前用户>".into();
        }
        let n = self.aliases.len() + 1;
        self.aliases.entry(l).or_insert_with(|| format!("<用户{n}>")).clone()
    }

    fn scrub_user(&self, name: &str) -> String {
        if self.user.chars().count() >= 3 {
            let l = name.to_lowercase();
            if l.contains(&self.user) {
                return l.replace(&self.user, "<当前用户>");
            }
        }
        name.to_string()
    }

    fn pseudonym(&self, name: &str, is_file: bool) -> String {
        let mut h: u64 = 0xcbf29ce484222325 ^ self.salt;
        for b in name.to_lowercase().bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        let tag = format!("{:04x}", (h >> 16) & 0xffff);
        if !is_file {
            return format!("文件夹-{tag}");
        }
        match name.rfind('.') {
            Some(p) if p > 0 && name.len() - p <= 9 => format!("文件-{tag}{}", &name[p..]),
            _ => format!("文件-{tag}"),
        }
    }

    /// Account-like names: WeChat ids, QQ numbers, e-mail addresses.
    fn is_account(name: &str) -> bool {
        let l = name.to_lowercase();
        l.starts_with("wxid_") || l.contains('@') || ((5..=12).contains(&l.len()) && l.bytes().all(|b| b.is_ascii_digit()))
    }

    /// Masks a full path. `last_is_dir` tells whether the final component is a folder.
    /// - `X:\Users\<name>` -> user alias
    /// - everything inside a personal folder (Documents, Desktop, Downloads, …, OneDrive),
    ///   at any depth on any drive -> random per-report pseudonyms, extension kept
    /// - account-like names anywhere -> pseudonyms
    /// - the current user name inside any other name -> alias
    fn path(&mut self, path: &str, last_is_dir: bool) -> String {
        if !self.on {
            return path.to_string();
        }
        let comps: Vec<&str> = path.split('\\').collect();
        let n = comps.len();
        let under_users = n > 1 && comps[1].eq_ignore_ascii_case("users");
        let personal_at = comps.iter().enumerate().skip(1).find(|(_, c)| Self::is_personal(c)).map(|(i, _)| i);
        let mut out = Vec::with_capacity(n);
        for (i, c) in comps.iter().enumerate() {
            let s = if c.is_empty() {
                String::new()
            } else if under_users && i == 2 && !PUBLIC_PROFILES.contains(&c.to_lowercase().as_str()) {
                self.user_alias(c)
            } else if personal_at == Some(i) && c.to_lowercase().starts_with("onedrive") {
                "OneDrive".into() // "OneDrive - Company" would leak the employer
            } else if personal_at.is_some_and(|p| i > p) || (i > 0 && Self::is_account(c)) {
                self.pseudonym(c, i == n - 1 && !last_is_dir)
            } else {
                self.scrub_user(c)
            };
            out.push(s);
        }
        out.join("\\")
    }

    fn name(&mut self, t: &Tree, x: u32) -> String {
        let p = self.path(&t.path_of(x), t.is_dir[x as usize]);
        p.rsplit('\\').find(|s| !s.is_empty()).unwrap_or(&p).to_string()
    }
}

fn is_within(t: &Tree, mut x: u32, scope: u32) -> bool {
    loop {
        if x == scope {
            return true;
        }
        if x == t.root || x == NONE {
            return false;
        }
        x = t.parent[x as usize];
    }
}

pub fn build(t: &Tree, opts: &ReportOpts, drive: Option<&DriveInfo>) -> String {
    let scope = if t.valid(opts.scope) { opts.scope } else { t.root };
    let si = scope as usize;
    let total = t.size[si].max(1);
    let clock = Clock { now: t.scan_time / 1000, tz_offset_min: opts.tz_offset_min };
    let mut m = Masker::new(opts.anonymize);
    let mut o = String::new();

    // ------------------------------------------------------------ overview
    let scope_path = m.path(&t.path_of(scope), true);
    let mut dirs = 0u64;
    t.for_each_desc(scope, |x| dirs += t.is_dir[x] as u64);
    let _ = writeln!(o, "# 磁盘占用分析数据（DiskMind 导出）\n");
    let _ = writeln!(o, "- 分析范围：`{scope_path}`");
    let mode = if t.mode == "MFT" {
        "MFT 直读（管理员权限，包含系统目录）"
    } else {
        "目录遍历（无管理员权限，System Volume Information 等系统目录可能未计入）"
    };
    let _ = writeln!(o, "- 扫描时间：{}，扫描方式：{mode}", clock.datetime_ms(t.scan_time));
    if let Some(d) = drive {
        let _ = writeln!(
            o,
            "- 所在磁盘 {}：总容量 {}，已用 {}，可用 {}（{}）",
            d.root,
            fmt_size(d.total),
            fmt_size(d.total - d.free),
            fmt_size(d.free),
            if d.fs.is_empty() { "未知文件系统" } else { &d.fs }
        );
    }
    let _ = writeln!(o, "- 范围内占用：{}，{} 个文件，{} 个文件夹", fmt_size(t.size[si]), fmt_num(t.files[si] as u64), fmt_num(dirs.saturating_sub(1)));
    let _ = write!(o, "- 说明：大小为文件的逻辑大小；文件夹的「最近修改」是其中最新文件的修改时间。");
    if opts.anonymize {
        let _ = write!(o, "用户名和个人文件夹（桌面、文档、下载、图片、视频、音乐、OneDrive 等）内的名称已替换为代号，扩展名保留。");
    }
    o.push_str("\n\n");

    // ------------------------------------------------------------ 1. tree
    let _ = writeln!(o, "## 1. 目录树（只展开大头）\n");
    let _ = writeln!(o, "展开条件：≥ 1 GB，或占父文件夹 ≥ 5% 且 ≥ 100 MB；其余合并为「其他」。百分比相对于分析范围总量。\n");
    let qualifies = |x: u32, parent_size: u64| {
        let s = t.size[x as usize];
        s >= GB || (s >= 100 * MB && s * 20 >= parent_size)
    };
    let mut shown: HashSet<u32> = HashSet::new();
    let mut heap = BinaryHeap::new();
    let push_kids = |x: u32, heap: &mut BinaryHeap<(u64, u32)>| {
        for &c in t.children(x) {
            if !qualifies(c, t.size[x as usize]) {
                break; // children are sorted by size, so the rest won't qualify either
            }
            heap.push((t.size[c as usize], c));
        }
    };
    push_kids(scope, &mut heap);
    while let Some((_, x)) = heap.pop() {
        if shown.len() >= TREE_NODES {
            break;
        }
        shown.insert(x);
        if t.is_dir[x as usize] {
            push_kids(x, &mut heap);
        }
    }
    o.push_str("```text\n");
    let _ = writeln!(o, "{:>9}  {:>5}  {scope_path}", fmt_size(t.size[si]), "100%");
    tree_lines(t, &mut m, &clock, &shown, scope, 1, total, &mut o);
    o.push_str("```\n\n");

    // ------------------------------------------------------------ 2. extensions
    let _ = writeln!(o, "## 2. 按扩展名汇总（前 25）\n");
    o.push_str("| 扩展名 | 总大小 | 占比 | 文件数 |\n|---|---:|---:|---:|\n");
    for e in t.extensions(scope, 25) {
        let name = if e.ext.is_empty() { "(无扩展名)".to_string() } else { format!(".{}", e.ext) };
        let _ = writeln!(o, "| {name} | {} | {} | {} |", fmt_size(e.size), fmt_pct(e.size as f64 / total as f64), fmt_num(e.count));
    }
    o.push('\n');

    // ------------------------------------------------------------ 3. top files
    let _ = writeln!(o, "## 3. 最大的 50 个文件\n");
    o.push_str("| # | 大小 | 修改时间 | 路径 |\n|---:|---:|---|---|\n");
    for (i, f) in t.top_files(scope, 50).iter().enumerate() {
        let path = m.path(&f.path, false);
        let _ = writeln!(o, "| {} | {} | {} | `{path}` |", i + 1, fmt_size(f.size), clock.date(t.mtime[f.id as usize]));
    }
    o.push('\n');

    // ------------------------------------------------------------ 4. stale
    stale_section(t, &mut m, &clock, scope, total, &mut o);

    // ------------------------------------------------------------ 5. growth
    growth_section(t, &mut m, &clock, scope, &mut o);

    // ------------------------------------------------------------ ask
    let _ = writeln!(o, "## 请帮我分析\n");
    o.push_str("1. 哪些占用可以安全清理或迁移到其他磁盘？请按可释放空间从大到小列出，说明每一项是什么、怎么清理、有什么风险。\n");
    o.push_str("2. 结合第 5 节的增长数据，空间为什么在持续增长？主要来源是哪个程序或目录，有没有办法限制它（例如修改缓存位置或上限）？\n");
    o.push_str("3. 哪些是系统或程序运行必需、不应手动删除的？如果确实想缩小它们，正确的做法是什么？\n");
    o
}

#[allow(clippy::too_many_arguments)]
fn tree_lines(t: &Tree, m: &mut Masker, clock: &Clock, shown: &HashSet<u32>, x: u32, depth: usize, total: u64, o: &mut String) {
    let indent = "  ".repeat(depth);
    let (mut other_n, mut other_files, mut other_size) = (0u64, 0u64, 0u64);
    let mut any = false;
    for &c in t.children(x) {
        let ci = c as usize;
        if !shown.contains(&c) {
            other_n += 1;
            other_files += t.files[ci] as u64;
            other_size += t.size[ci];
            continue;
        }
        any = true;
        let name = m.name(t, c);
        let pct = fmt_pct(t.size[ci] as f64 / total as f64);
        let meta = if t.is_dir[ci] {
            format!("{} 个文件 · 最近修改 {}", fmt_num(t.files[ci] as u64), clock.age(t.mtime[ci]))
        } else {
            format!("修改于 {}", clock.date(t.mtime[ci]))
        };
        let slash = if t.is_dir[ci] { "\\" } else { "" };
        let _ = writeln!(o, "{:>9}  {pct:>5}{indent}{name}{slash}  ({meta})", fmt_size(t.size[ci]));
        if t.is_dir[ci] {
            tree_lines(t, m, clock, shown, c, depth + 1, total, o);
        }
    }
    // Fold away negligible remainders ("24 B in 3 items") to keep the tree compact.
    if any && other_size > 0 && (other_size >= 50 * MB || other_size * 10 >= t.size[x as usize]) {
        let _ = writeln!(
            o,
            "{:>9}  {:>5}{indent}[其他 {} 项，含 {} 个文件]",
            fmt_size(other_size),
            fmt_pct(other_size as f64 / total as f64),
            fmt_num(other_n),
            fmt_num(other_files)
        );
    }
}

fn stale_section(t: &Tree, m: &mut Masker, clock: &Clock, scope: u32, total: u64, o: &mut String) {
    let cutoff = clock.now - 365 * DAY;
    let stale = |x: usize| t.mtime[x] != 0 && (t.mtime[x] as i64) < cutoff;
    let dir_min = (500 * MB).max(total / 200);
    let mut dirs = Vec::new();
    t.for_each_desc(scope, |x| {
        if x as u32 != scope && t.is_dir[x] && t.size[x] >= dir_min && stale(x) {
            let p = t.parent[x] as usize;
            // Only the outermost stale folder: skip if the parent is itself stale.
            if p as u32 == scope || !stale(p) {
                dirs.push(x as u32);
            }
        }
    });
    dirs.sort_unstable_by(|a, b| t.size[*b as usize].cmp(&t.size[*a as usize]));
    dirs.truncate(25);
    let in_dirs: HashSet<u32> = dirs.iter().copied().collect();
    let mut files = Vec::new();
    t.for_each_desc(scope, |x| {
        if !t.is_dir[x] && t.size[x] >= 100 * MB && stale(x) {
            files.push(x as u32);
        }
    });
    files.retain(|&f| {
        let mut a = t.parent[f as usize];
        while a != NONE && a != scope {
            if in_dirs.contains(&a) {
                return false;
            }
            a = t.parent[a as usize];
        }
        true
    });
    files.sort_unstable_by(|a, b| t.size[*b as usize].cmp(&t.size[*a as usize]));
    files.truncate(25);

    let _ = writeln!(o, "## 4. 很大但超过一年没修改过\n");
    if dirs.is_empty() && files.is_empty() {
        o.push_str("没有找到超过一年未修改的大文件夹（≥ 500 MB）或大文件（≥ 100 MB）。\n\n");
        return;
    }
    let sum: u64 = dirs.iter().chain(&files).map(|&x| t.size[x as usize]).sum();
    let _ = writeln!(o, "合计 {}。文件夹只列最外层（其中所有文件都超过一年未修改）。注意：系统目录即使很久没修改也可能是必需的。\n", fmt_size(sum));
    o.push_str("| 类型 | 大小 | 最近修改 | 路径 |\n|---|---:|---|---|\n");
    for &x in dirs.iter().chain(&files) {
        let xi = x as usize;
        let path = m.path(&t.path_of(x), t.is_dir[xi]);
        let kind = if t.is_dir[xi] { "文件夹" } else { "文件" };
        let _ = writeln!(o, "| {kind} | {} | {}（{}） | `{path}` |", fmt_size(t.size[xi]), clock.date(t.mtime[xi]), clock.age(t.mtime[xi]));
    }
    o.push('\n');
}

fn growth_section(t: &Tree, m: &mut Masker, clock: &Clock, scope: u32, o: &mut String) {
    let _ = writeln!(o, "## 5. 与上次扫描相比的变化\n");
    let Some(b) = &t.baseline else {
        o.push_str("暂无可对比的历史扫描。下次扫描后再导出，即可看到哪些文件夹在增长。\n\n");
        return;
    };
    let days = (t.scan_time - b.time) as f64 / 86_400_000.0;
    let per_week = |d: i64| {
        if days >= 1.0 { fmt_delta((d as f64 / days * 7.0) as i64) } else { "—".into() }
    };
    let ch = t.changes(500);
    let scope_prev = t.prev[scope as usize];
    let _ = writeln!(
        o,
        "- 基线：{}（距本次 {}），扫描方式：{}",
        clock.datetime_ms(b.time),
        if days >= 1.0 { format!("{days:.1} 天") } else { format!("{:.1} 小时", days * 24.0) },
        if b.mode.is_empty() { "未知" } else { &b.mode }
    );
    if scope_prev != crate::tree::UNKNOWN {
        let d = t.size[scope as usize] as i64 - scope_prev as i64;
        let _ = writeln!(o, "- 范围内总变化：{}（{} → {}），折合每周 {}", fmt_delta(d), fmt_size(scope_prev), fmt_size(t.size[scope as usize]), per_week(d));
    }
    if !b.mode.is_empty() && b.mode != t.mode {
        o.push_str("- 注意：基线和本次的扫描方式不同，目录遍历读不到部分系统目录，这些目录会显示为新增或删除，并非真实变化。\n");
    }
    if days < 1.0 {
        o.push_str("- 两次扫描间隔不足一天，增长速度参考意义有限。\n");
    }
    o.push_str("- 列表只列最具体的位置：如果某个子文件夹贡献了父文件夹 70% 以上的变化，只列子文件夹。\n\n");

    let in_scope = |c: &crate::tree::Change| {
        if c.id >= 0 { is_within(t, c.id as u32, scope) && c.id as u32 != scope } else { c.parent >= 0 && is_within(t, c.parent as u32, scope) }
    };
    let grown: Vec<_> = ch.grown.iter().filter(|c| in_scope(c)).take(20).collect();
    let shrunk: Vec<_> = ch.shrunk.iter().filter(|c| in_scope(c)).take(10).collect();
    let _ = writeln!(o, "### 增长最多的 {} 处\n", grown.len());
    if grown.is_empty() {
        o.push_str("没有超过 1 MB 的增长。\n\n");
    } else {
        o.push_str("| 变化 | 折合每周 | 之前 → 现在 | 路径 |\n|---:|---:|---|---|\n");
        for c in grown {
            let tag = if c.kind == "new" { "（新出现）" } else { "" };
            let path = m.path(&c.path, c.dir);
            let _ = writeln!(o, "| {}{tag} | {} | {} → {} | `{path}` |", fmt_delta(c.delta), per_week(c.delta), fmt_size(c.prev), fmt_size(c.size));
        }
        o.push('\n');
    }
    if !shrunk.is_empty() {
        let _ = writeln!(o, "### 减少最多的 {} 处\n", shrunk.len());
        o.push_str("| 变化 | 之前 → 现在 | 路径 |\n|---:|---|---|\n");
        for c in shrunk {
            let tag = if c.kind == "removed" { "（已删除）" } else { "" };
            let path = m.path(&c.path, c.dir);
            let _ = writeln!(o, "| {}{tag} | {} → {} | `{path}` |", fmt_delta(c.delta), fmt_size(c.prev), fmt_size(c.size));
        }
        o.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(19_723), (2024, 1, 1));
        assert_eq!(civil(20_512), (2026, 2, 28));
    }

    #[test]
    fn masks_user_and_personal_folders() {
        let mut m = Masker { on: true, user: "sunmoontrain".into(), aliases: HashMap::new(), salt: 1 };
        assert_eq!(m.path(r"C:\Users\SunMoonTrain\AppData\Local\Tencent", true), r"C:\Users\<当前用户>\AppData\Local\Tencent");
        assert_eq!(m.path(r"C:\Users\Public\Videos", true), r"C:\Users\Public\Videos");
        assert_eq!(m.path(r"C:\Users\bob\AppData", true), r"C:\Users\<用户1>\AppData");
        let p = m.path(r"C:\Users\SunMoonTrain\Documents\简历\offer.pdf", false);
        assert!(p.starts_with(r"C:\Users\<当前用户>\Documents\文件夹-"), "{p}");
        assert!(p.ends_with(".pdf") && !p.contains("简历") && !p.contains("offer"), "{p}");
        // Same name -> same alias within a report.
        assert_eq!(m.path(r"C:\Users\SunMoonTrain\Desktop\简历", true).rsplit('\\').next(), p.split('\\').nth(4));
        let od = m.path(r"C:\Users\SunMoonTrain\OneDrive - Contoso Ltd\Plans", true);
        assert!(od.contains(r"\OneDrive\文件夹-") && !od.contains("Contoso"), "{od}");
        assert_eq!(m.path(r"D:\Backup\SunMoonTrain-old", true), r"D:\Backup\<当前用户>-old");
        // Personal folders on other drives; account ids anywhere.
        let d = m.path(r"D:\Documents\龙息堂\图鉴.zip", false);
        assert!(d.starts_with(r"D:\Documents\文件夹-") && d.ends_with(".zip") && !d.contains("龙息堂"), "{d}");
        let w = m.path(r"D:\Software\WeChat\xwechat_files\wxid_ysnml00ou5d121_d82c\msg", true);
        assert!(w.starts_with(r"D:\Software\WeChat\xwechat_files\文件夹-") && w.ends_with(r"\msg"), "{w}");
        assert!(!m.path(r"C:\Tencent Files\123456789\Image", true).contains("123456789"));
        assert!(!m.path(r"C:\Mail\me@example.com", true).contains("example"));
        assert_eq!(m.path(r"D:\Steam\steamapps\common\00", true), r"D:\Steam\steamapps\common\00");
        // Off: untouched.
        let mut off = Masker { on: false, user: "sunmoontrain".into(), aliases: HashMap::new(), salt: 1 };
        assert_eq!(off.path(r"C:\Users\SunMoonTrain\Documents\a.txt", false), r"C:\Users\SunMoonTrain\Documents\a.txt");
    }

    #[test]
    fn report_sections() {
        use crate::tree::RawTree;
        let mut raw = RawTree { names: vec![], parent: vec![], size: vec![], is_dir: vec![], mtime: vec![], root: 0 };
        let mut add = |n: &str, p: u32, s: u64, d: bool, mt: u32| {
            raw.names.push(n.into());
            raw.parent.push(p);
            raw.size.push(s);
            raw.is_dir.push(d);
            raw.mtime.push(mt);
            raw.names.len() as u32 - 1
        };
        let now = 1_790_000_000u32;
        let old = now - 400 * 86_400;
        let r = add("C:\\", NONE, 0, true, 0);
        let users = add("Users", r, 0, true, 0);
        let me = add("SunMoonTrain", users, 0, true, 0);
        let docs = add("Documents", me, 0, true, 0);
        add("工资单.xlsx", docs, 3 * GB, false, old);
        let games = add("Games", r, 0, true, 0);
        add("old.iso", games, 5 * GB, false, old);
        for i in 0..300 {
            add(&format!("tiny{i}.log"), games, 10 * MB, false, now);
        }
        let mut t = Tree::build(raw, "C:\\".into());
        t.scan_time = now as i64 * 1000;
        t.mode = "MFT".into();
        let opts = ReportOpts { scope: t.root, anonymize: true, tz_offset_min: -480 };
        let s = build(&t, &opts, None);
        assert!(s.contains("## 1. 目录树"), "{s}");
        assert!(s.contains("[其他 300 项"), "{s}");
        assert!(s.contains("| .log | 2.93 GB |"), "{s}");
        assert!(s.contains("old.iso"), "{s}");
        assert!(s.contains("## 4. 很大但超过一年没修改过"), "{s}");
        assert!(s.contains("暂无可对比的历史扫描"), "{s}");
        assert!(!s.contains("工资单") && !s.contains("SunMoonTrain"), "{s}");
        assert!(s.contains(".xlsx"), "{s}");
    }
}
