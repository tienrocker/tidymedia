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
2. **Dedup an toàn** - hash phân tầng (size → xxh3 → BLAKE3 đầy đủ), UI so ảnh cạnh nhau với zoom đồng bộ, chỉ xóa vào Thùng rác. *(so trùng perceptual sẽ tới ở M7)*
3. **Gom thư viện** - dồn tất cả về thư mục do chính bạn chọn, cấu trúc + tên file là template tùy chỉnh được (mặc định `YYYY\YYYY-MM`), tên theo ngày chụp chống đụng độ, dry-run bắt buộc, undo được từng đợt.

## ✨ Điểm nổi bật

- ⚡ **Nhanh thật sự** - 218.000 file trên ổ cứng thật index trong **chưa tới 10 giây**; tìm kiếm trả lời trong **20-40 ms** *ngay trong lúc đang scan*.
- 🔎 **Tìm kiếm dễ tính** - substring, không phân biệt dấu (`anh tet` ra `Ảnh Tết`), lọc theo loại / dung lượng / ngày / độ phân giải.
- 🖼️ **Lưới thumbnail & lightbox** - grid ảo hóa hoàn toàn, cuộn mượt qua cả triệu item, cache thumbnail WebP (LRU 2 GB), zoom/pan quanh con trỏ, panel EXIF (ngày chụp, máy ảnh, kích thước). HEIC/AVIF qua ffmpeg.
- 🐌 **Tử tế với ổ cứng chậm** - chỉ xin thumb cho đúng phần đang nhìn thấy và xử lý cái mới nhất trước, nên cuộn nhanh trên ổ HDD không để lại hàng nghìn request rác; job warm nền chạy ở ưu tiên thấp nhất và tự nhường mọi thao tác của bạn.
- 🎬 **Video hạng nhất** - thumbnail keyframe, lọc theo thời lượng/codec, phát ngay trong app seek tức thì, Live Photo (HEIC+MOV) là một đơn vị ở mọi nơi.
- ♻️ **Dedup đáng tin** - nhóm sort theo dung lượng lãng phí, so 2-4 bản cạnh nhau với **zoom đồng bộ** (soi cùng một vùng pixel trên mọi bản), review thuần bàn phím, rule tự chọn bản giữ override được từng ô. Mọi lệnh xóa re-verify trên đĩa ở mili-giây cuối và vào Thùng rác.
- ☑️ **Dọn vài nghìn nhóm không phải bấm vài nghìn lần** - checkbox tường minh (bấm vào row không bao giờ tự đánh dấu xóa), chọn tất cả theo rule, Shift+click chọn cả dải kiểu Gmail, phím tắt `Ctrl+A` / `Ctrl+D` / `Del`, và lệnh xóa hàng loạt chạy như một job có tiến độ + nút Dừng. Nhóm trùng còn hiện dần **ngay trong lúc** đang quét.
- 🗂️ **Gom kho theo ý bạn** - thư mục nào cũng làm kho được (mỗi ổ một cái, tên tùy ý), cấu trúc thư mục + tên file là **template** (`{YYYY}\{YYYY}-{MM}`, `{YYYYMMDD}_{hhmmss}_{hash4}`, `{camera}`…), dry-run bắt buộc, move cùng ổ là rename atomic, mọi đợt đều ghi sổ và undo được.
- 📁 **Giữ nguyên phân loại bạn đã làm bằng tay** - token `{relpath}`, `{folder}`, `{name}` (kèm preset 1 click) gom nhiều ổ về một kho mà **không** làm phẳng những thư mục bạn phân loại bao năm; chạy lại lần nữa thì không file nào bị di chuyển.
- 🌥️ **An toàn với cloud** - placeholder OneDrive/iCloud được index nhưng **không bao giờ bị hydrate**: app không âm thầm kéo hàng GB từ cloud về.
- 🕐 **Đúng múi giờ** - chọn múi giờ ngay lần chạy đầu; filter ngày và tên file thư viện theo đúng múi giờ đó.
- 🌍 **English · Tiếng Việt · 中文** sẵn trong app.
- 📦 **App native gọn nhẹ** - Tauri 2 + Rust, không Electron. Installer NSIS, MSI, và bản portable ZIP lưu data ngay cạnh exe.

## 🚀 Bắt đầu

1. Tải **setup .exe**, **.msi**, hoặc **portable .zip** mới nhất từ [Releases](https://github.com/tienrocker/tidymedia/releases).
2. Windows 10/11 x64. Thiếu WebView2 thì installer tự cài; không cần runtime nào khác.
3. Lần chạy đầu: chọn **ngôn ngữ** và **múi giờ** → bấm **+ Thêm** và chọn thư mục hoặc cả ổ (`D:\`) → vừa scan vừa duyệt/tìm bình thường.

> Thumbnail HEIC/AVIF và mọi tính năng video dùng `ffmpeg`/`ffprobe` bundle sẵn (bản GPLv3 từ [gyan.dev](https://www.gyan.dev/ffmpeg/builds/), pin version + verify SHA256 lúc build; license đi kèm tại `binaries/ffmpeg-LICENSE.txt`).
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
| M3 | Video: thumb keyframe, phát trong app, ghép cặp Live Photo, bundle ffmpeg | ✅ |
| M4 | Dedup exact: hash phân tầng, UI so 2-4 ảnh cạnh nhau, chọn hàng loạt bằng checkbox, review bằng bàn phím, xóa hàng loạt hủy được | ✅ |
| M5 | Gom thư viện: thư mục đích tự chọn, đặt tên theo template (giữ được cấu trúc thư mục sẵn có), move atomic, nhật ký undo | ✅ |
| M6 | Import iPhone / SD qua WPD/MTP, nhớ "đã import" incremental | 🔜 |
| M7 | So trùng perceptual (cùng ảnh nhưng nén lại / thu nhỏ), album + tag thủ công, phân tích dung lượng | ⏳ |
| M8 | Scan nhanh NTFS MFT/USN - rescan cả ổ trong vài giây | ⏳ |
| M9 | Places: index GPS, bản đồ thế giới offline, cụm ảnh theo địa điểm, zoom để lọc thư viện | ⏳ |
| M10 | Trips: tự gom chuyến đi theo khoảng trống thời gian + khoảng cách GPS (thuật toán thuần, không AI) | ⏳ |
| — | People / gom theo khuôn mặt: đang cân nhắc - cần model ONNX ~50-100 MB và một lượt quét CPU toàn kho, nên không hứa mốc | 🔬 |

> Vì sao M7 phải dùng perceptual hash, đo trên kho thật 34k file: cùng một tấm ảnh xuất ba đường -
> `icloud\IMG_1463.JPEG` (3024², 1.90 MB), `iphone\2018_08_02_…JPG` (3024², 0.79 MB) và bản thu nhỏ
> `anh\2019\IMG_1463.JPEG` (1772², 0.87 MB) - **đúng là một ảnh** (PSNR 42.6 và 38.7 dB) nhưng
> **không cặp nào trùng byte**, nên dedup tuyệt đối bỏ qua cả ba là đúng. Gom theo tên file cũng
> không phải lối tắt: 8 cặp trùng tên `IMG_####` lấy mẫu giữa hai thư mục thì 4 là cùng ảnh
> (32-40 dB), 4 là ảnh hoàn toàn khác nhau (9-11 dB).
>
> Chỉ dựa khoảng cách tri giác cũng chưa đủ, và đây mới là nửa nguy hiểm. Một loạt ảnh chụp liên
> tiếp cùng cảnh gần như y hệt nhau dưới hash 64-bit - ở lưới 8×8 luma thì người vừa đổi tư thế chỉ
> chiếm một hai ô - nên 25 tấm ảnh thật sự khác nhau rơi chung vào một nhóm "gần giống". Không
> ngưỡng nào tách được: siết chặt lại thì mất các bản nén lại thật từ lâu trước khi loạt ảnh kia
> chịu rời nhau. Giờ bấm máy thì tách được, tuyệt đối: 25 tấm đó mang 25 mốc EXIF khác nhau, còn
> bốn bản của `IMG_1463` cùng mang đúng `18:02:24.802` tới từng mili-giây. Hai file chỉ có thể là
> cùng một tấm ảnh nếu ra từ cùng một lần bấm máy, nên nhóm giờ bắt buộc trùng giờ bấm máy mỗi khi
> cả hai file đều có. Trên 21,5k ảnh của kho đó, nhóm to nhất tụt từ 138 file xuống 5, và 3.084
> file rời khỏi danh sách ứng viên xóa - trong khi mọi bộ trùng thật, kể cả bản Telegram xuất ra đã
> mất sạch EXIF, vẫn còn nguyên.

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
