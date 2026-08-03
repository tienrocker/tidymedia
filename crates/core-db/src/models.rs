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
    pub mtime_from: Option<i64>,
    pub mtime_to: Option<i64>,
    pub root_path: Option<String>,
    pub sort: Option<String>,
    pub include_missing: Option<bool>,
    /// Lọc theo tổng pixel (width*height >= min_px). Chỉ khớp file đã có meta.
    pub min_px: Option<i64>,
    /// Lọc thời lượng video (ms). Chỉ khớp file đã có meta duration.
    pub dur_min_ms: Option<i64>,
    pub dur_max_ms: Option<i64>,
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
