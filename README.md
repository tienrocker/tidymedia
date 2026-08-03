# media-dedup

Media manager + library consolidator cho Windows — index cả ổ đĩa trong nền, search tức thì,
tìm trùng lặp (exact + perceptual), gom media rải rác về thư viện chuẩn theo ngày chụp,
import trực tiếp từ iPhone/thẻ SD.

Stack: Tauri 2 + Rust · React 18 + Vite + TanStack Virtual · SQLite (FTS5 trigram).

## Dev

```powershell
npm install
npm run tauri dev
```

- Dev build dùng data dir riêng (`%APPDATA%\com.polyvn.mediadedup\dev\`) — không đụng dữ liệu bản cài thật.
- Test core crates (không cần mở app): `cargo test --workspace`
- Sinh corpus test: `cargo run -p devtool -- gen-tree --root D:\.testdata --files 200000 --dupe-sets 500`

## Release

```powershell
.\scripts\release.ps1 minor   # bump + changelog + commit + tag
git push --follow-tags        # CI build NSIS installer -> draft GitHub Release
```

Cài đặt lần đầu sẽ có cảnh báo SmartScreen (chưa mua cert Authenticode — app cá nhân).
