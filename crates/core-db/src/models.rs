use serde::{Deserialize, Serialize};

/// One row rendered in the browse list/grid.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileRow {
    pub id: i64,
    pub name: String,
    pub dir: String,
    pub ext: Option<String>,
    pub kind: i64,
    pub size: i64,
    pub mtime: i64,
    pub status: i64,
    /// Dimensions hiển thị từ media_meta (None = meta job chưa chạy tới).
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub taken_at: Option<i64>,
    pub duration_ms: Option<i64>,
    /// true = ảnh có MOV Live Photo đi kèm (MOV bị ẩn khỏi list, đi theo cặp).
    pub is_live: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RootInfo {
    pub id: i64,
    pub volume_id: i64,
    pub path: String,
    pub last_scan_at: Option<i64>,
    pub file_count: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobRow {
    pub id: i64,
    pub kind: String,
    pub state: String,
    pub done: i64,
    pub total: Option<i64>,
    pub message: Option<String>,
    pub created_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub error: Option<String>,
}

/// Filter đến từ frontend (camelCase qua IPC).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FileFilter {
    pub text: Option<String>,
    pub kind: Option<i64>,
    pub exts: Option<Vec<String>>,
    pub size_min: Option<i64>,
    pub size_max: Option<i64>,
    /// Khoảng ngày, áp lên trường ngày đang chọn (`date_field`).
    pub date_from: Option<i64>,
    pub date_to: Option<i64>,
    /// "taken" (mặc định) = ngày chụp EXIF, thiếu thì lùi về mtime; "mtime" =
    /// ngày file. Kho bị copy qua lại nhiều lần thì mtime là NGÀY COPY, sắp
    /// theo nó là sai thứ tự thật của thư viện.
    pub date_field: Option<String>,
    pub root_path: Option<String>,
    pub sort: Option<String>,
    pub include_missing: Option<bool>,
    /// Lọc theo tổng pixel (width*height >= min_px). Chỉ khớp file đã có meta.
    pub min_px: Option<i64>,
    /// Lọc thời lượng video (ms). Chỉ khớp file đã có meta duration.
    pub dur_min_ms: Option<i64>,
    pub dur_max_ms: Option<i64>,
    /// Lọc theo thiết bị (`media_meta.camera`), so KHỚP ĐÚNG chuỗi lấy từ
    /// [`list_cameras`](crate::query::list_cameras) chứ không phải user gõ tay.
    /// Chỉ khớp file đã có meta.
    pub camera: Option<String>,
    /// Chỉ file mang nhãn này.
    pub tag_id: Option<i64>,
    /// Chỉ file nằm trong album này. Kèm `sort = "album"` để giữ THỨ TỰ user đã
    /// thêm vào album thay vì sắp lại theo ngày.
    pub album_id: Option<i64>,
}

/// Một thiết bị trong thư viện + số file của nó, cho dropdown lọc.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraCount {
    pub camera: String,
    pub count: i64,
}

/// File chờ trích metadata (meta job M2/M3).
#[derive(Debug, Clone)]
pub struct PendingMeta {
    pub file_id: i64,
    /// Full path đã ghép sẵn dir + name.
    pub path: String,
    /// 0 = image (header-only), 1 = video (ffprobe).
    pub kind: i64,
    /// Snapshot lúc select — upsert guard theo cặp này (file đổi giữa chừng
    /// thì meta vừa trích là rác, phải bị bỏ).
    pub mtime: i64,
    pub size: i64,
}

/// Kết quả trích meta của 1 file, chờ ghi vào media_meta.
#[derive(Debug, Clone, Default)]
pub struct MetaUpsert {
    pub file_id: i64,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub taken_at: Option<i64>,
    pub date_source: Option<i64>,
    pub camera: Option<String>,
    pub orientation: Option<i64>,
    pub duration_ms: Option<i64>,
    pub vcodec: Option<String>,
    pub acodec: Option<String>,
    pub bitrate: Option<i64>,
    pub fps: Option<f64>,
    /// Toạ độ nơi chụp, độ thập phân. `meta_ver` KHÔNG nằm ở đây: upsert luôn
    /// đóng dấu `ops::META_VERSION` hiện tại, caller không có quyền chọn.
    pub gps_lat: Option<f64>,
    pub gps_lon: Option<f64>,
    /// 1 = done, 2 = failed (không đọc nổi dimensions).
    pub meta_state: i64,
    /// Snapshot từ PendingMeta — upsert chỉ ghi khi files.mtime/size còn khớp
    /// (file đổi trong lúc extract → bỏ, trigger đã dọn meta rồi, job sau làm lại).
    pub src_mtime: i64,
    pub src_size: i64,
}

/// Row phục vụ protocol thumb:// / media:// — lookup theo file id.
#[derive(Debug, Clone)]
pub struct MediaSrc {
    pub path: String,
    pub ext: Option<String>,
    pub kind: i64,
    pub size: i64,
    pub mtime: i64,
    pub status: i64,
    /// Từ media_meta (video): điểm seek keyframe thumb.
    pub duration_ms: Option<i64>,
}

/// Chi tiết 1 file cho panel info của lightbox.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDetail {
    pub id: i64,
    pub name: String,
    pub dir: String,
    pub kind: i64,
    pub status: i64,
    pub size: i64,
    pub mtime: i64,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub taken_at: Option<i64>,
    pub camera: Option<String>,
    pub orientation: Option<i64>,
    pub duration_ms: Option<i64>,
    pub vcodec: Option<String>,
    pub acodec: Option<String>,
    pub fps: Option<f64>,
    pub meta_state: Option<i64>,
    pub gps_lat: Option<f64>,
    pub gps_lon: Option<f64>,
    /// Tên nơi chụp để HIỂN THỊ — GIỮ NGUYÊN DẤU ("Phường Lý Thái Tổ, Hà Nội"),
    /// khác hẳn tên dùng đặt thư mục.
    ///
    /// core-db KHÔNG điền field này (luôn `None`): tên tra từ toạ độ bằng
    /// `core-geo`, mà tra tên là việc của tầng lệnh chứ không phải tầng lưu
    /// trữ. Và cố ý KHÔNG lưu vào DB — bộ dữ liệu địa điểm sẽ cập nhật, tên đã
    /// lưu thì đứng yên rồi lệch dần với tên đang hiện ở chỗ khác.
    pub place: Option<String>,
}

/// File chờ hash (quick hoặc full) — M4 dedup.
#[derive(Debug, Clone)]
pub struct PendingHash {
    pub file_id: i64,
    pub path: String,
    /// Snapshot lúc select — upsert guard theo cặp này.
    pub mtime: i64,
    pub size: i64,
}

/// Kết quả hash chờ ghi. `quick64`/`full` chỉ ghi khi Some — file không đọc
/// được thì skip (job sau retry), như meta job.
#[derive(Debug, Clone)]
pub struct HashUpsert {
    pub file_id: i64,
    pub quick64: Option<i64>,
    pub full: Option<Vec<u8>>,
    pub src_mtime: i64,
    pub src_size: i64,
}

/// 1 nhóm trùng exact cho list bên trái màn Dedup.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DupGroupRow {
    pub id: i64,
    pub count: i64,
    pub size: i64,
    /// (count-1) * size — bytes giải phóng được nếu giữ đúng 1 bản.
    pub waste: i64,
    /// Tối đa 3 (file_id, mtime) đầu tiên — UI ghép thumb URL.
    pub samples: Vec<(i64, i64)>,
}

/// 1 bản trong nhóm trùng cho compare view.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DupMemberRow {
    pub file_id: i64,
    pub name: String,
    pub dir: String,
    pub size: i64,
    pub mtime: i64,
    pub status: i64,
    pub is_live: bool,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub taken_at: Option<i64>,
    pub camera: Option<String>,
}

/// 1 file chờ tính perceptual hash (job phash M7).
#[derive(Debug, Clone)]
pub struct PendingPhash {
    pub file_id: i64,
    pub path: String,
    pub ext: Option<String>,
    /// Snapshot lúc select — upsert chỉ ghi khi file chưa đổi.
    pub mtime: i64,
    pub size: i64,
}

/// Kết quả dhash 1 file. `hash = None` = ảnh phẳng/không decode được: vẫn ghi
/// một row "bia" (kind = PHASH_KIND_NONE) để job không chọn lại file đó mãi,
/// nhưng không bao giờ tham gia gom nhóm.
#[derive(Debug, Clone)]
pub struct PhashUpsert {
    pub file_id: i64,
    /// 256 bit chia thành 4 word; ghi thành 4 dòng `seq 0..3`.
    pub hash: Option<[i64; 4]>,
    pub src_mtime: i64,
    pub src_size: i64,
}

/// 1 ứng viên vào [`crate::ops::cluster_similar`]. Tách struct thay vì tuple 5
/// phần tử vì mỗi field là một chốt chặn riêng, đọc `it.taken_at` rõ hơn `it.4`.
#[derive(Debug, Clone, Copy)]
pub struct ClusterItem {
    pub file_id: i64,
    /// dhash 256-bit, 4 word lưu thành 4 dòng `seq 0..3` trong `phashes`.
    pub hash: [i64; 4],
    pub width: Option<i64>,
    pub height: Option<i64>,
    /// Giờ bấm máy theo EXIF, epoch ms CÓ sub-second (xem core-media). Chốt
    /// chặn mạnh nhất: hai bản của cùng một tấm ảnh luôn mang đúng mốc này.
    ///
    /// CHỈ được nhận giờ CHỤP thật — hiện là EXIF `DateTimeOriginal`
    /// (date_source 0) hoặc QuickTime creation time (1). ĐỪNG BAO GIỜ đổ ngày
    /// suy từ TÊN FILE (date_source 2) hay mtime (3) vào đây: tên file mang
    /// giờ LƯU chứ không phải giờ bấm máy, ví dụ thật trong kho là
    /// `photo_2018-12-02_10-50-05.jpg` — bản sao đã bị lột EXIF của một tấm
    /// chụp lúc 10:49, lệch 60 giây. Gán mốc giả đó vào sẽ khiến nó KHÁC mốc
    /// với bản gốc và bị đá khỏi nhóm, tức là mất đúng ca mà M7 sinh ra để bắt.
    /// Hai tầng suy đoán kia chỉ sống trong `core_ingest::date::resolve_taken`
    /// cho organize, không bao giờ ghi xuống `media_meta`.
    pub taken_at: Option<i64>,
}

/// Bản rút gọn của MỌI member trong MỌI nhóm trùng — vừa đủ field để UI chạy
/// rule "giữ bản nào" hàng loạt mà không phải mở từng nhóm (không kèm
/// name/dir/camera: 13k dòng thì mấy chuỗi đó mới là phần nặng của payload).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DupMemberBrief {
    pub group_id: i64,
    pub file_id: i64,
    pub size: i64,
    pub mtime: i64,
    pub status: i64,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub taken_at: Option<i64>,
}

/// Context verify trước khi xóa: mọi member của mọi group dính tới đợt xóa.
#[derive(Debug, Clone)]
pub struct DeleteContextRow {
    pub group_id: i64,
    pub file_id: i64,
    pub path: String,
    /// 0 = image, 1 = video — pair expansion CHỈ đi từ ảnh sang MOV, không
    /// bao giờ chiều ngược (MOV.live_pair_id trỏ về ảnh gốc!).
    pub kind: i64,
    pub size: i64,
    pub mtime: i64,
    pub status: i64,
    pub live_pair_id: Option<i64>,
    pub full_hash: Option<Vec<u8>>,
    pub hashed_size: Option<i64>,
    pub hashed_mtime: Option<i64>,
}

/// Library root: thư mục ĐÍCH organize do user tự chỉ định, tối đa 1/volume.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryRootRow {
    pub id: i64,
    pub volume_id: i64,
    /// "D" — chữ cái ổ của volume
    pub letter: Option<String>,
    pub path: String,
    pub is_primary: bool,
}

/// Ứng viên organize (1 file, MOV của Live Photo gắn ở `pair` chứ không là row riêng).
#[derive(Debug, Clone)]
pub struct OrgCandidateRow {
    pub file_id: i64,
    pub path: String,
    /// thư mục chứa file (dirs.path) — nguồn tính token {relpath}/{folder}
    pub dir_path: String,
    /// tên trước khi organize (files.original_name) — nguồn token {name}
    pub original_name: Option<String>,
    pub ext: String,
    pub kind: i64,
    pub size: i64,
    pub mtime: i64,
    pub status: i64,
    pub taken_at: Option<i64>,
    pub date_source: Option<i64>,
    pub camera: Option<String>,
    /// Toạ độ nơi chụp cho token {place}… — chỉ Some khi có ĐỦ cả lat lẫn lon.
    pub gps: Option<(f64, f64)>,
    pub full_hash: Option<Vec<u8>>,
    pub hashed_size: Option<i64>,
    pub hashed_mtime: Option<i64>,
    /// Library root TẠI THỜI ĐIỂM organize đặt file vào chỗ hiện tại (từ op
    /// active khớp vị trí). {relpath} của file đã organize phải tính theo gốc
    /// này — sau khi đổi thư mục kho, gốc hiện tại không còn chứa file nên suy
    /// từ nó ra là lồng/flatten cây. None = file chưa từng qua organize.
    pub managed_lib_root: Option<String>,
    pub pair: Option<OrgPairRow>,
}

#[derive(Debug, Clone)]
pub struct OrgPairRow {
    pub file_id: i64,
    pub path: String,
    pub ext: String,
    pub status: i64,
    pub size: i64,
    pub mtime: i64,
    pub full_hash: Option<Vec<u8>>,
    pub hashed_size: Option<i64>,
    pub hashed_mtime: Option<i64>,
}

/// 1 dòng journal org_ops (write-ahead: insert trước khi đụng fs).
#[derive(Debug, Clone)]
pub struct OrgOpRow {
    pub id: i64,
    pub batch_id: i64,
    pub file_id: i64,
    pub old_path: String,
    pub new_path: String,
    /// Some(id op gốc) = đây là intent của lượt UNDO đảo op đó. Recovery dùng
    /// để chốt nốt trạng thái op gốc khi crash giữa undo.
    pub reverses_op_id: Option<i64>,
}

/// Batch organize đã chạy (batch_id = job id) — cho UI undo.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgBatchRow {
    pub batch_id: i64,
    pub finished_at: Option<i64>,
    /// số op đã done và CHƯA undo
    pub moved: i64,
    pub undone: i64,
}

/// Một file do scanner tìm thấy, chờ ghi vào index.
#[derive(Debug, Clone)]
pub struct ScanEntry {
    pub dir_path: String,
    pub name: String,
    pub ext: String,
    pub kind: i64,
    pub size: i64,
    pub mtime: i64,
    pub attrs: u32,
    /// 0 = present, 2 = cloud placeholder (OFFLINE / RECALL_* attrs).
    pub status: i64,
}
