#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod elevate;
mod history;
mod mft;
mod progress;
mod sys;
mod tree;
mod walker;

fn main() {
    // `diskmind --bench <path>`: headless scan timing, for profiling the scanners.
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 4 && args[1] == elevate::FLAG {
        std::process::exit(elevate::helper_main(&args[2], &args[3]));
    }
    if args.len() >= 3 && args[1] == "--bench" {
        bench(&args[2]);
        return;
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(commands::AppState::default())
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running DiskMind");
}

fn bench(path: &str) {
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
    let t = tree::Tree::build(raw, path.to_string());
    let t2 = t0.elapsed();
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
