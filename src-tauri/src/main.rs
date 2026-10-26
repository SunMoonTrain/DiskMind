#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod elevate;
mod history;
mod mcp;
mod mft;
mod progress;
mod report;
mod sys;
mod tree;
mod walker;

fn main() {
    // `diskmind --bench <path> [report.md]`: headless scan timing (and AI report), for profiling.
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 4 && args[1] == elevate::FLAG {
        std::process::exit(elevate::helper_main(&args[2], &args[3]));
    }
    if args.len() >= 3 && args[1] == "--bench" {
        bench(&args[2], args.get(3).map(String::as_str));
        return;
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(commands::AppState::default())
        .setup(|app| {
            mcp::start(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_drives,
            commands::is_admin,
            commands::start_scan,
            commands::cancel_scan,
            commands::get_layout,
            commands::get_children,
            commands::get_info,
            commands::get_changes,
            commands::get_top_files,
            commands::get_extensions,
            commands::get_history,
            commands::set_baseline,
            commands::get_path,
            commands::reveal_path,
            commands::export_report,
            commands::write_text_file,
            commands::mcp_url,
            commands::resolve_delete,
        ])
        .run(tauri::generate_context!())
        .expect("error while running DiskMind");
}

fn bench(path: &str, report_out: Option<&str>) {
    use std::sync::atomic::AtomicBool;
    let c = progress::Counters::default();
    let cancel = AtomicBool::new(false);
    let t0 = std::time::Instant::now();
    let use_mft = path.len() == 3 && path.ends_with(":\\") && sys::is_admin();
    let raw = if use_mft {
        match mft::scan(path.chars().next().unwrap(), &c, &cancel) {
            Ok(r) => r,
            Err(e) => {
                println!("mft failed: {e}");
                walker::scan(path, &c, &cancel)
            }
        }
    } else {
        walker::scan(path, &c, &cancel)
    };
    let t1 = t0.elapsed();
    let mut t = tree::Tree::build(raw, path.to_string());
    let t2 = t0.elapsed();
    if let Some(out) = report_out {
        t.mode = if use_mft { "MFT" } else { "遍历" }.into();
        t.scan_time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
        let opts = report::ReportOpts { scope: t.root, anonymize: true, tz_offset_min: -480 };
        let r0 = std::time::Instant::now();
        let md = report::build(&t, &opts, sys::drives().iter().find(|d| d.root.eq_ignore_ascii_case(&path[..3.min(path.len())])));
        println!("report: {} chars, {} lines in {:?}", md.chars().count(), md.lines().count(), r0.elapsed());
        std::fs::write(out, md).unwrap();
    }
    println!(
        "mode={} files={} dirs={} total={:.2} GB scan={:?} build={:?}",
        if use_mft { "mft" } else { "walk" },
        t.file_count,
        t.dir_count,
        t.size[t.root as usize] as f64 / (1u64 << 30) as f64,
        t1,
        t2 - t1
    );
    for &c in t.children(t.root).iter().take(8) {
        println!("  {:>10.2} GB  {}", t.size[c as usize] as f64 / (1u64 << 30) as f64, t.names[c as usize]);
    }
    let snap = history::build(&t, 0);
    println!("snapshot nodes: {}", snap.nodes.len());
}
