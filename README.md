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
- 🐌 **Kind to slow drives** - thumbnails are requested only for what is actually on screen and served newest-first, so a fast scroll on a spinning disk never queues thousands of stale reads; a background warm job fills the cache at the lowest priority and steps aside for anything you do.
- 🎬 **Video, first-class** - keyframe thumbnails, duration/codec filters, in-app playback with instant seek, Live Photos (HEIC+MOV) paired as one item everywhere.
- ♻️ **Dedup you can trust** - groups sorted by wasted bytes, 2-4-up compare with **synchronized zoom** (inspect the same pixels on every copy), keyboard-first review, auto-select rules you can override per file. Every deletion is re-verified on disk at the last millisecond and goes to the Recycle Bin.
- ☑️ **Clean thousands of groups without clicking thousands of times** - explicit checkboxes (never "click a row and it's marked"), select-all by rule, Gmail-style Shift+click ranges, `Ctrl+A` / `Ctrl+D` / `Del` shortcuts, and a mass delete that runs as a job with live progress and a Stop button. Duplicate groups also appear *while* the scan is still hashing.
- 🗂️ **Organize on your terms** - any folder can be your library (one per drive, any name), folder layout and file names are **templates** (`{YYYY}\{YYYY}-{MM}`, `{YYYYMMDD}_{hhmmss}_{hash4}`, `{camera}`…), dry-run is mandatory, same-drive moves are atomic renames, and every batch is journaled and undoable.
- 📁 **Keeps the sorting you already did by hand** - `{relpath}`, `{folder}` and `{name}` tokens (with one-click presets) consolidate scattered drives *without* flattening the folders you curated over the years, and re-running organize moves nothing.
- 🧯 **Interruption-proof** - the dry-run is a frozen snapshot, so nothing is decided twice; content fingerprints are prepared by a separate cancellable job instead of stalling the preview; and if the app dies mid-move, the next launch replays the journal as a visible, cancellable recovery job that verifies by hash before touching anything.
- 🌥️ **Cloud-safe** - OneDrive/iCloud placeholders are indexed but **never hydrated**: TidyMedia will not silently pull gigabytes down from the cloud.
- 🕐 **Timezone-aware** - you pick your timezone on first run; date filters and library file names respect it, not whatever the OS guesses.
- 🌍 **English · Tiếng Việt · 中文** out of the box.
- 🚚 **Take your index with you** - Export writes a single self-contained copy of the database (plus the thumbnail cache) into a folder you pick; Import swaps it in on the next launch, keeping one `.bak` generation. Moving between the dev build and the installed one costs a folder copy instead of a full rescan.
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
| M4 | Exact dedup: tiered hashing, 2-4-up compare UI, bulk checkbox selection, keyboard-first review, cancellable mass delete | ✅ |
| M5 | Organize: user-chosen library folder, template naming (incl. keeping your existing folder structure), atomic moves, undo journal | ✅ |
| M6 | iPhone / SD import via WPD/MTP, incremental "already imported" | 🔜 |
| M7 | Perceptual similarity (same photo, re-encoded or resized), manual albums + tags, storage analytics | ⏳ |
| M8 | NTFS MFT/USN fast scan - full drive rescan in seconds | ⏳ |
| M9 | Places: GPS index, offline world map, photo clusters per place, zoom to filter the library | ⏳ |
| M10 | Trips: auto-group a journey from time gaps + GPS distance (plain algorithm, no ML) | ⏳ |
| — | People / face grouping: under consideration - needs a ~50-100 MB ONNX model and a full-library CPU pass, so no milestone promised | 🔬 |

> Why M7 needs perceptual hashing, measured on a real 34k-file library: the same photo exported
> three ways - `icloud\IMG_1463.JPEG` (3024², 1.90 MB), `iphone\2018_08_02_…JPG` (3024², 0.79 MB)
> and a resized `anh\2019\IMG_1463.JPEG` (1772², 0.87 MB) - is **the same picture** (PSNR 42.6 and
> 38.7 dB) yet **no two share a single byte**, so exact dedup correctly leaves all three alone.
> Matching by file name is not a shortcut either: of 8 sampled `IMG_####` name collisions between
> two folders, 4 were the same photo (32-40 dB) and 4 were completely unrelated shots (9-11 dB).
>
> Perceptual distance alone is not enough either, and that is the more dangerous half. A burst of
> shots of one scene is near-identical to a 64-bit hash - at an 8×8 luma grid the person who moved
> occupies one or two cells - so 25 genuinely different photos landed in a single "similar" group.
> No threshold separates them: tightening it loses the real re-encodes long before it splits the
> burst. Capture time does separate them, exactly: those 25 carry 25 distinct EXIF timestamps,
> while the four copies of `IMG_1463` all carry `18:02:24.802` down to the millisecond. Two files
> can only be the same picture if they came from the same shutter press, so groups now require an
> identical capture time whenever both files have one. On the 21.5k photos of that library the
> largest group fell from 138 files to 5, and 3,084 files left the deletion candidate list - while
> every genuine duplicate set, including a re-export stripped of EXIF, stayed intact.
>
> That guard cannot help the 2,081 photos in that library carrying no EXIF at all - messaging apps
> strip it - so for those the hash is the only check, and a 64-bit one left just 3 bits of headroom.
> The hash is therefore 256-bit. It does **not** pull burst frames apart (measured: still 0-8 bits,
> because a moving subject covers few cells even on a 16×16 grid - that is the capture-time guard's
> job, not the hash's). What it buys is a better trade-off everywhere: judged against 9,456 pairs
> that provably share one shutter press, at an equal miss rate the wider hash mis-groups about half
> as often - 11.4 % versus 20.6 % - and clustering the whole library still takes 272 ms.

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
