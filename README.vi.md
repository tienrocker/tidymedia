<div align="center">

# 🗂️ TidyMedia

**Trình quản lý media siêu nhanh + gom thư viện cho Windows.**
Index cả ổ cứng, gõ là ra kết quả, dọn 15 năm ảnh vứt lung tung - an toàn tuyệt đối.

[![CI](https://github.com/tienrocker/tidymedia/actions/workflows/ci.yml/badge.svg)](https://github.com/tienrocker/tidymedia/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/tienrocker/tidymedia?include_prereleases&label=release)](https://github.com/tienrocker/tidymedia/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
![Platform](https://img.shields.io/badge/platform-Windows%2010%2F11%20x64-0078d6)
![Built with](https://img.shields.io/badge/built%20with-Tauri%202%20%2B%20Rust-orange)

[English](README.md) · **Tiếng Việt** · [中文](README.zh.md)

</div>

---

Ảnh và video tích tụ qua năm tháng - sync iPhone qua cả chục app khác nhau, máy ảnh, app chat - đến lúc ổ cứng đầy bản trùng lặp và tên file đụng nhau (`IMG_1234.JPG` từ ba đời điện thoại). TidyMedia coi **nội dung là danh tính**, không bao giờ tin tên file, và xây quanh ba trụ:

1. **Duyệt & tìm kiếm cả ổ đĩa, tức thì** - kiến trúc index-first; UI không bao giờ đụng filesystem khi duyệt.
2. **Dedup an toàn** - hash phân tầng (size → xxh3 → BLAKE3 đầy đủ) + so trùng perceptual, UI so ảnh cạnh nhau, chỉ xóa vào Thùng rác. *(đang làm - M4)*
3. **Gom thư viện** - dồn tất cả về thư mục do chính bạn chọn, cấu trúc theo ngày tùy chỉnh được (mặc định `YYYY\YYYY-MM`), tên file theo ngày chụp chống đụng độ, kèm nhật ký undo đầy đủ. *(đang làm - M5)*

## ✨ Điểm nổi bật

- ⚡ **Nhanh thật sự** - 218.000 file trên ổ cứng thật index trong **chưa tới 10 giây**; tìm kiếm trả lời trong **20-40 ms** *ngay trong lúc đang scan*.
- 🔎 **Tìm kiếm dễ tính** - substring, không phân biệt dấu (`anh tet` ra `Ảnh Tết`), lọc theo loại / dung lượng / ngày / độ phân giải.
- 🖼️ **Lưới thumbnail & lightbox** - grid ảo hóa hoàn toàn, cuộn mượt qua cả triệu item, cache thumbnail WebP (LRU 2 GB), zoom/pan quanh con trỏ, panel EXIF (ngày chụp, máy ảnh, kích thước). HEIC/AVIF qua ffmpeg.
- 🌥️ **An toàn với cloud** - placeholder OneDrive/iCloud được index nhưng **không bao giờ bị hydrate**: app không âm thầm kéo hàng GB từ cloud về.
- 🕐 **Đúng múi giờ** - chọn múi giờ ngay lần chạy đầu; filter ngày và (sắp tới) tên file thư viện theo đúng múi giờ đó.
- 🌍 **English · Tiếng Việt · 中文** sẵn trong app.
- 📦 **App native gọn nhẹ** - Tauri 2 + Rust, không Electron. Installer NSIS, MSI, và bản portable ZIP lưu data ngay cạnh exe.

## 🚀 Bắt đầu

1. Tải **setup .exe**, **.msi**, hoặc **portable .zip** mới nhất từ [Releases](https://github.com/tienrocker/tidymedia/releases).
2. Windows 10/11 x64. Thiếu WebView2 thì installer tự cài; không cần runtime nào khác.
3. Lần chạy đầu: chọn **ngôn ngữ** và **múi giờ** → bấm **+ Thêm** và chọn thư mục hoặc cả ổ (`D:\`) → vừa scan vừa duyệt/tìm bình thường.

> Thumbnail HEIC/AVIF hiện dùng `ffmpeg` trên `PATH` nếu có - bản cập nhật video (M3) sẽ bundle sẵn.
> Lần cài đầu Windows SmartScreen sẽ cảnh báo (chưa mua chứng chỉ Authenticode).

## 🛡️ An toàn dữ liệu là thiết kế, không phải lời hứa

Năm bất biến mà mọi đường code phá hủy đều phải qua - luật cứng, không phải khuyến nghị:

1. Không bao giờ xóa khi chưa có bản sống được **verify bằng BLAKE3** - hash nhanh chỉ để *lọc* ứng viên, không bao giờ là căn cứ xóa.
2. Re-check size+mtime ngay trước mọi thao tác xóa/ghi đè (chống TOCTOU) - lệch là hủy.
3. Cùng ổ ⇒ **rename atomic**; khác ổ ⇒ copy → flush → verify → xong hết mới xóa nguồn.
4. Mọi lệnh xóa đều vào **Thùng rác** (folder cách ly làm dự phòng) - không bao giờ hard-delete thẳng.
5. Mọi thao tác phá hủy đều **ghi nhật ký**, có dry-run xem trước, và undo được.

## 🗺️ Lộ trình

| Milestone | Phạm vi | Trạng thái |
|---|---|---|
| M1 | Index cả ổ, tìm kiếm tức thì, duyệt ảo hóa | ✅ |
| M2 | Lưới thumbnail, lightbox + EXIF, HEIC, job metadata | ✅ |
| M3 | Video: thumb keyframe, phát trong app, ghép cặp Live Photo, bundle ffmpeg | 🔜 |
| M4 | Dedup exact: hash phân tầng, UI so 2-4 ảnh cạnh nhau, review bằng bàn phím | ⏳ |
| M5 | Gom thư viện: thư mục đích tự chọn, format ngày tùy chỉnh, move atomic, nhật ký undo | ⏳ |
| M6 | Import iPhone / SD qua WPD/MTP, nhớ "đã import" incremental | ⏳ |
| M7 | So trùng perceptual, tag, album, phân tích dung lượng | ⏳ |
| M8 | Scan nhanh NTFS MFT/USN - rescan cả ổ trong vài giây | ⏳ |

## ⚙️ Hiệu năng

Đo trên corpus synthetic 200k file (sinh bằng `devtool gen-tree` kèm theo repo) cộng ổ cứng thật 218k file:

| Thao tác | Kết quả |
|---|---|
| Scan lạnh, 200k file | ~12-15 s |
| Ổ thật, 218k file | ~10 s |
| Tìm-là-ra (FTS5 trigram) | 20-40 ms |
| Fetch một cửa sổ kết quả | < 1 ms |
| UI trong lúc scan | cuộn & tìm bình thường |

**Stack:** Tauri 2 + Rust · React 18 + Vite + TanStack Virtual · SQLite (WAL, FTS5 trigram, single-writer)

## 🛠️ Build từ source

Yêu cầu: Rust (stable, MSVC), Node.js 20+.

```powershell
npm install
npm run tauri dev      # dev (data dir riêng biệt - không đụng bản cài thật)
npm run tauri build    # build release: bundle NSIS + MSI
cargo test --workspace # test core headless, không cần GUI
```

Sinh corpus test / benchmark:

```powershell
cargo run -p devtool --release -- gen-tree --root D:\.testdata --files 200000 --dupe-sets 500
cargo run -p devtool --release -- bench-scan --root <path> --db <dir>
```

## 🌍 i18n & đóng góp

Mọi chuỗi UI nằm trong `src/locales/{en,vi,zh}.json` (react-i18next). Chuỗi mới phải có đủ cả ba file - hoan nghênh PR thêm ngôn ngữ mới và bug report kèm bước tái hiện.

## 📄 Giấy phép

[MIT](LICENSE) © [tienrocker](https://github.com/tienrocker)
