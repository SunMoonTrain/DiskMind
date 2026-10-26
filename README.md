# DiskMind

A fast disk space analyzer (Rust + Tauri 2 / WebView2). It remembers every scan and shows which folders grew since last time.

## Features

- **MFT direct read**: when scanning an NTFS drive root it reads `$MFT` from the raw volume (the same approach as WizTree). The UI never runs elevated: each scan starts a short-lived elevated helper (`diskmind.exe --mft-helper`, one UAC prompt) that hands the tree back through a temp file, so global hotkeys of other apps keep working. The title-bar toggle turns this off; otherwise, or if UAC is declined, it falls back to a parallel rayon walk of the directories.
- **Visualization**: cushion-shaded treemap and sunburst on canvas, double-click to zoom in, color by folder / file type / change.
- **History diff**: after each scan it saves a compressed snapshot (folders ≥ 1 MB, files ≥ 16 MB). The next scan diffs against it automatically and lists folders that grew, shrank, are new, or were deleted. Any older snapshot can be chosen as the baseline.
- Large files, file-type breakdown, history trend, right-click to show in Explorer.
- **Export for AI**: one click produces a Markdown report to paste into an AI chat: a pruned size tree (nodes ≥ 1 GB, or ≥ 5% of the parent and ≥ 100 MB), totals by extension, the 50 largest files, large items untouched for over a year, and growth since the baseline scan with a per-week rate. An optional anonymize mode replaces user names, account ids (WeChat / QQ / e-mail) and everything inside personal folders (Documents, Desktop, Downloads, …, OneDrive) with per-report random aliases.

- **AI cleanup over MCP**: while the window is open, DiskMind serves an MCP endpoint at `http://127.0.0.1:47821/mcp` (Streamable HTTP, loopback only; override the port with `DISKMIND_MCP_PORT`). Tools: `status`, `scan`, `list_dir`, `top_files`, `extensions`, `changes`, `report`, `recycle`, `list_cleanups`, `cleanup` and `suggest`. `recycle` never deletes on its own: DiskMind shows a confirmation dialog with the AI's reason and every path with its size, the user can untick items, and approved items go to the Recycle Bin. The OS folder (except `Windows\Temp`), Program Files, drive roots, `$` system files and profile folders themselves are refused. The loaded scan is updated in place, so no rescan is needed. `cleanup` runs one action from a fixed built-in list (empty the Recycle Bin, old files in %TEMP% or Windows\Temp plus the Windows Update download cache, DISM component cleanup, turning off hibernation, npm / pip / NuGet caches, `docker system prune`); only actions that apply to the PC are offered, each is confirmed in a dialog showing the exact command, and admin ones run in an elevated helper. DiskMind never runs arbitrary commands: anything else the AI recommends goes through `suggest` into the "AI 建议" tab, where the user can copy a command or open a folder. Add it to Claude Code with `claude mcp add --transport http diskmind http://127.0.0.1:47821/mcp`.

## Development

```bash
npm install
npm run tauri dev      # run the app
npm run dev            # frontend only, in a plain browser with mock data (src/mock.ts)
npm run tauri build    # release build + NSIS installer
```

Headless scan benchmark:

```bash
src-tauri/target/release/diskmind.exe --bench "D:\\" [report.md]
```

Snapshots are stored in `%APPDATA%\com.diskmind.desktop\history\`.

## Layout

| File | Purpose |
| --- | --- |
| `src-tauri/src/mft.rs` | NTFS MFT parsing (fixups, data runs, $FILE_NAME / $DATA, extension records) |
| `src-tauri/src/elevate.rs` | Elevated helper processes (MFT read, admin cleanup actions) and the parent-side launcher |
| `src-tauri/src/walker.rs` | Parallel directory walk (skips reparse points) |
| `src-tauri/src/tree.rs` | CSR tree, size aggregation, layout trimming, baseline matching and change detection |
| `src-tauri/src/report.rs` | AI export report and path anonymization |
| `src-tauri/src/mcp.rs` | Local MCP server, tools, and the GUI-confirmed recycle / cleanup flows |
| `src-tauri/src/cleanup.rs` | Built-in cleanup actions, their estimates and runners |
| `src-tauri/src/history.rs` | Snapshot storage (bincode + gzip) |
| `src/chart.ts` | Treemap / sunburst rendering, hit testing, zoom animation |
| `src/main.ts` | UI controller |
