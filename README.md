<div align="center">

# 🗂️ TidyMedia

**Blazing-fast media manager & library consolidator for Windows.**
Index an entire drive, search as you type, and untangle 15 years of scattered photos - safely.

[![CI](https://github.com/tienrocker/tidymedia/actions/workflows/ci.yml/badge.svg)](https://github.com/tienrocker/tidymedia/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/tienrocker/tidymedia?include_prereleases&label=release)](https://github.com/tienrocker/tidymedia/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
![Platform](https://img.shields.io/badge/platform-Windows%2010%2F11%20x64-0078d6)
![Built with](https://img.shields.io/badge/built%20with-Tauri%202%20%2B%20Rust-orange)

**English** · [Tiếng Việt](README.vi.md) · [中文](README.zh.md)

</div>

---

Photos and videos pile up for years - iPhone syncs through a dozen different apps, cameras, chat apps - until your drives are full of duplicates and colliding names (`IMG_1234.JPG` from three different phones). TidyMedia treats **content as identity**, never file names, and is built around three pillars:

1. **Browse & search a whole drive, instantly** - index-first architecture; the UI never touches the filesystem while you browse.
2. **Deduplicate safely** - tiered hashing (size → xxh3 → full BLAKE3), side-by-side review with synchronized zoom, Recycle Bin only. *(perceptual similarity lands in M7)*
3. **Consolidate** - move everything into a library folder *you* choose, laid out by a naming template you control (default `YYYY\YYYY-MM`), with collision-proof date-based names, a mandatory dry-run, and per-batch undo.

## ✨ Highlights

- ⚡ **Fast for real** - 218,000 files on a real drive indexed in **under 10 seconds**; search answers in **20-40 ms** *while the scan is still running*.
- 🔎 **Search that forgives** - substring, accent-insensitive (`anh tet` finds `Ảnh Tết`), filters for kind / size / date / resolution.
- 🖼️ **Thumbnail grid & lightbox** - fully virtualized grid that scrolls smoothly through a million items, WebP thumbnail cache (2 GB LRU), cursor-anchored zoom & pan, EXIF info panel (taken date, camera, dimensions). HEIC/AVIF via ffmpeg.
- 🎬 **Video, first-class** - keyframe thumbnails, duration/codec filters, in-app playback with instant seek, Live Photos (HEIC+MOV) paired as one item everywhere.
- ♻️ **Dedup you can trust** - groups sorted by wasted bytes, 2-4-up compare with **synchronized zoom** (inspect the same pixels on every copy), keyboard-first review, auto-select rules you can override per file. Every deletion is re-verified on disk at the last millisecond and goes to the Recycle Bin.
- 🗂️ **Organize on your terms** - any folder can be your library (one per drive, any name), folder layout and file names are **templates** (`{YYYY}\{YYYY}-{MM}`, `{YYYYMMDD}_{hhmmss}_{hash4}`, `{camera}`…), dry-run is mandatory, same-drive moves are atomic renames, and every batch is journaled and undoable.
- 🧯 **Interruption-proof** - the dry-run is a frozen snapshot, so nothing is decided twice; content fingerprints are prepared by a separate cancellable job instead of stalling the preview; and if the app dies mid-move, the next launch replays the journal as a visible, cancellable recovery job that verifies by hash before touching anything.
- 🌥️ **Cloud-safe** - OneDrive/iCloud placeholders are indexed but **never hydrated**: TidyMedia will not silently pull gigabytes down from the cloud.
- 🕐 **Timezone-aware** - you pick your timezone on first run; date filters and library file names respect it, not whatever the OS guesses.
- 🌍 **English · Tiếng Việt · 中文** out of the box.
- 📦 **Tiny native app** - Tauri 2 + Rust, no Electron. NSIS installer, MSI, and a portable ZIP that keeps its data next to the exe.

## 🚀 Getting started

1. Grab the latest **setup .exe**, **.msi**, or **portable .zip** from [Releases](https://github.com/tienrocker/tidymedia/releases).
2. Windows 10/11 x64. WebView2 is installed automatically if missing; no other runtime needed.
3. First run: pick your **language** and **timezone** → click **+ Add** and select a folder or a whole drive (`D:\`) → browse and search while it scans.

> HEIC/AVIF thumbnails and all video features use bundled `ffmpeg`/`ffprobe` (GPLv3 builds from [gyan.dev](https://www.gyan.dev/ffmpeg/builds/), pinned + SHA256-verified at build time; license ships as `binaries/ffmpeg-LICENSE.txt`).
> First-run installs show a SmartScreen warning (no Authenticode certificate yet).

## 🛡️ Data safety, by design

Five invariants every destructive code path must pass - not guidelines, hard rules:

1. Nothing is ever deleted without a **BLAKE3-verified** surviving copy - quick hashes only *filter* candidates, they are never grounds for deletion.
2. Size+mtime re-checked immediately before any delete/overwrite (TOCTOU guard) - abort on mismatch.
3. Same volume ⇒ **atomic rename**; cross volume ⇒ copy → flush → verify → only then delete the source.
4. Every delete goes to the **Recycle Bin** (quarantine folder as fallback) - never a direct hard delete.
5. Every destructive operation is **journaled**, dry-run previewed, and undoable.

## 🗺️ Roadmap

| Milestone | Scope | Status |
|---|---|---|
| M1 | Whole-drive index, instant search, virtualized browse | ✅ |
| M2 | Thumbnail grid, lightbox + EXIF, HEIC, metadata jobs | ✅ |
| M3 | Video: keyframe thumbs, in-app playback, Live Photo pairing, bundled ffmpeg | ✅ |
| M4 | Exact dedup: tiered hashing, 2-4-up compare UI, keyboard-first review | ✅ |
| M5 | Organize: user-chosen library folder, template naming, atomic moves, undo journal | ✅ |
| M6 | iPhone / SD import via WPD/MTP, incremental "already imported" | 🔜 |
| M7 | Perceptual similarity, tags, albums, storage analytics | ⏳ |
| M8 | NTFS MFT/USN fast scan - full drive rescan in seconds | ⏳ |

## ⚙️ Performance

Measured on a 200k-file synthetic corpus (generated by the bundled `devtool gen-tree`) plus a real 218k-file consumer drive:

| Operation | Result |
|---|---|
| Cold scan, 200k files | ~12-15 s |
| Real drive, 218k files | ~10 s |
| Search-as-you-type (FTS5 trigram) | 20-40 ms |
| Fetch one result window | < 1 ms |
| UI while scanning | scrolls & searches normally |

**Stack:** Tauri 2 + Rust · React 18 + Vite + TanStack Virtual · SQLite (WAL, FTS5 trigram, single-writer)

## 🛠️ Building from source

Prerequisites: Rust (stable, MSVC), Node.js 20+.

```powershell
npm install
npm run tauri dev      # develop (isolated dev data dir - never touches a real install)
npm run tauri build    # release build: NSIS + MSI bundles
cargo test --workspace # headless core tests, no GUI needed
```

Generate a synthetic test corpus / benchmark:

```powershell
cargo run -p devtool --release -- gen-tree --root D:\.testdata --files 200000 --dupe-sets 500
cargo run -p devtool --release -- bench-scan --root <path> --db <dir>
```

## 🌍 i18n & contributing

All UI strings live in `src/locales/{en,vi,zh}.json` (react-i18next). New strings must land in all three files - PRs adding more locales are welcome, as are bug reports with reproduction steps.

## 📄 License

[MIT](LICENSE) © [tienrocker](https://github.com/tienrocker)
