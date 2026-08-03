# TidyMedia

Fast media manager + library consolidator for Windows.

Index entire drives in the background, search instantly as you type, find duplicates
(exact + perceptual), consolidate scattered photos/videos into a clean date-based
library, and import directly from iPhone / SD cards — without ever trusting
inconsistent file names.

**Stack:** Tauri 2 + Rust · React 18 + Vite + TanStack Virtual · SQLite (WAL, FTS5 trigram)

## Why

Media collected over 15 years — iPhone syncs through many different apps, cameras,
chat apps — ends up scattered across drives with duplicate copies and colliding
names (`IMG_1234.JPG` from three different phones). TidyMedia treats **content as
identity** (BLAKE3 + EXIF taken-date, never file names) and is built around three pillars:

1. **Browse & search a whole drive instantly** — NTFS-aware scanning, index-first
   architecture, virtualized UI that scrolls smoothly through 1M+ items.
2. **Deduplicate safely** — tiered hashing (size → xxh3 → full BLAKE3), perceptual
   matching for re-encoded copies, side-by-side review UI, Recycle Bin only.
3. **Consolidate** — organize everything into `Library\YYYY\YYYY-MM\` with
   collision-proof date-based names; atomic same-volume moves; full undo journal.

Current status: **M1** (index + instant search) — scan of a 200k-file corpus in ~15s,
search-as-you-type in 20–30 ms. See the milestone plan in the repo issues/docs.

## i18n

UI ships in **English, Tiếng Việt, 中文** (react-i18next, `src/locales/*.json`).
PRs adding locales are welcome.

## Development

```powershell
npm install
npm run tauri dev
```

- Dev builds use an isolated data dir (`…\com.polyvn.tidymedia\dev\`) so they never
  touch a real installation's index or library.
- Core crates are Tauri-free and test headlessly: `cargo test --workspace`
- Generate a synthetic test corpus:
  `cargo run -p devtool --release -- gen-tree --root D:\.testdata --files 200000 --dupe-sets 500`
- Benchmark scan/search: `cargo run -p devtool --release -- bench-scan --root <path> --db <dir>`

## Release

```powershell
.\scripts\release.ps1 minor   # bump version + changelog + commit + tag
git push --follow-tags        # CI builds the NSIS installer -> draft GitHub Release
```

First-run installs show a SmartScreen warning (no Authenticode certificate yet).
