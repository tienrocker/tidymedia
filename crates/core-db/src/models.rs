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
