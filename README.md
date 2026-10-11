# DiskMind

A fast disk space analyzer (Rust + Tauri 2 / WebView2). It remembers every scan and shows which folders grew since last time.

## Features

- **MFT direct read**: with admin rights, on an NTFS drive root, it reads `$MFT` from the raw volume (the same approach as WizTree). Without admin it falls back to a parallel rayon walk of the directories.
- **Visualization**: cushion-shaded treemap and sunburst on canvas, double-click to zoom in, color by folder / file type / change.
- **History diff**: after each scan it saves a compressed snapshot (folders ≥ 1 MB, files ≥ 16 MB). The next scan diffs against it automatically and lists folders that grew, shrank, are new, or were deleted. Any older snapshot can be chosen as the baseline.
- Large files, file-type breakdown, history trend, right-click to show in Explorer.

## Development

```bash
npm install
npm run tauri dev      # run the app
npm run dev            # frontend only, in a plain browser with mock data (src/mock.ts)
npm run tauri build    # release build + NSIS installer
```

Headless scan benchmark:

```bash
src-tauri/target/release/diskmind.exe --bench "D:\\"
```

Snapshots are stored in `%APPDATA%\com.diskmind.desktop\history\`.

## Layout

| File | Purpose |
| --- | --- |
| `src-tauri/src/mft.rs` | NTFS MFT parsing (fixups, data runs, $FILE_NAME / $DATA, extension records) |
| `src-tauri/src/walker.rs` | Parallel directory walk (skips reparse points) |
| `src-tauri/src/tree.rs` | CSR tree, size aggregation, layout trimming, baseline matching and change detection |
| `src-tauri/src/history.rs` | Snapshot storage (bincode + gzip) |
| `src/chart.ts` | Treemap / sunburst rendering, hit testing, zoom animation |
| `src/main.ts` | UI controller |
