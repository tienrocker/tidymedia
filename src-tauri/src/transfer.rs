//! Export / Import toàn bộ kho dữ liệu của app.
//!
//! Lý do tồn tại: bản dev ghi vào `%APPDATA%\...\dev`, bản cài ghi vào
//! `%APPDATA%\...` (cố ý tách để dev không phá data thật). Chuyển từ bản này
//! sang bản kia đồng nghĩa quét lại cả kho, hash lại, cày lại thumbnail —
//! hàng giờ cho một việc mà chỉ cần chép 2 file.
//!
//! BẤT BIẾN:
//! - Export **không bao giờ đè** file nào: đích đã có `index.db` là từ chối.
//!   `VACUUM INTO` gộp WAL nên bản xuất là một file tự đủ.
//! - Import **thay thế toàn bộ** index (thư mục theo dõi, hash, nhật ký
//!   organize). Nên nó (a) soi trước và trả về nội dung để user tự xác nhận,
//!   (b) từ chối khi còn job đang chạy, (c) giữ lại một đời `.bak`.
//! - Tráo file làm lúc KHỞI ĐỘNG, trước khi mở bất kỳ connection nào — đổi
//!   file SQLite dưới chân connection đang mở là hỏng dữ liệu.
//! - Nhập index mà KHÔNG kèm thumbs thì xoá cache thumbnail: cache khoá theo
//!   `file_id`, mà id của hai kho không hề ăn khớp — giữ lại là hiện nhầm ảnh
//!   của file khác.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::{AppHandle, State};

use crate::commands::{blocking, err, CmdResult};
use crate::state::AppState;

const INDEX_DB: &str = "index.db";
const THUMBS_DB: &str = "thumbs.db";
/// Đuôi file đã dàn sẵn, chờ khởi động lại mới tráo vào.
const INCOMING: &str = ".incoming";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    pub index_bytes: i64,
    pub thumbs_bytes: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportInfo {
    pub schema_version: i64,
    pub app_schema_version: i64,
    pub files: i64,
    pub roots: Vec<String>,
    pub index_bytes: i64,
    pub thumbs_bytes: i64,
    /// false = không nhập được; `reason` nói vì sao (đã là mã lỗi i18n).
    pub compatible: bool,
    pub reason: Option<String>,
}

fn size_of(p: &Path) -> i64 {
    std::fs::metadata(p).map(|m| m.len() as i64).unwrap_or(0)
}

// ---------- export ----------

#[tauri::command]
pub async fn export_data(
    state: State<'_, AppState>,
    dest: String,
    include_thumbs: bool,
) -> CmdResult<ExportResult> {
    let db = state.db.clone();
    let thumbs = state.thumbs.clone();
    let data_dir = state.data_dir.clone();
    blocking(move || {
        let dest = PathBuf::from(dest);
        if !dest.is_dir() {
            return Err("ERR_EXPORT_DEST|not a folder".into());
        }
        // Xuất đè lên chính thư mục dữ liệu đang chạy = tự bắn vào chân
        if same_dir(&dest, &data_dir) {
            return Err("ERR_EXPORT_SAME_DIR|".into());
        }
        let index_dest = dest.join(INDEX_DB);
        let thumbs_dest = dest.join(THUMBS_DB);
        if index_dest.exists() || (include_thumbs && thumbs_dest.exists()) {
            return Err("ERR_EXPORT_EXISTS|".into());
        }
        db.vacuum_into(&index_dest).map_err(err)?;
        if include_thumbs {
            // Thumb hỏng chỉ là mất cache — đừng để nó làm hỏng cả lượt xuất
            if let Err(e) = thumbs.vacuum_into(&thumbs_dest) {
                tracing::warn!("export thumbs failed: {e:#}");
                let _ = std::fs::remove_file(&thumbs_dest);
            }
        }
        Ok(ExportResult {
            index_bytes: size_of(&index_dest),
            thumbs_bytes: size_of(&thumbs_dest),
        })
    })
    .await
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

// ---------- import ----------

/// Soi bản xuất TRƯỚC khi đụng gì: mở read-only, không migrate, không ghi.
#[tauri::command]
pub async fn inspect_import(src: String) -> CmdResult<ImportInfo> {
    blocking(move || {
        let src = PathBuf::from(src);
        let index_src = src.join(INDEX_DB);
        if !index_src.is_file() {
            return Err("ERR_IMPORT_NO_INDEX|".into());
        }
        let conn = rusqlite::Connection::open_with_flags(
            &index_src,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(|e| format!("ERR_IMPORT_UNREADABLE|{e}"))?;
        let schema_version: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(|e| format!("ERR_IMPORT_UNREADABLE|{e}"))?;
        let app_schema_version = core_db::ops::SCHEMA_VERSION;

        // Bảng `files` thiếu = không phải index của TidyMedia (hoặc hỏng)
        let files: i64 = match conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0)) {
            Ok(n) => n,
            Err(_) => {
                return Ok(ImportInfo {
                    schema_version,
                    app_schema_version,
                    files: 0,
                    roots: Vec::new(),
                    index_bytes: size_of(&index_src),
                    thumbs_bytes: size_of(&src.join(THUMBS_DB)),
                    compatible: false,
                    reason: Some("ERR_IMPORT_NOT_TIDYMEDIA".into()),
                })
            }
        };
        let roots: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT path FROM roots ORDER BY path")
                .map_err(|e| format!("ERR_IMPORT_UNREADABLE|{e}"))?;
            let rows = stmt
                .query_map([], |r| r.get(0))
                .map_err(|e| format!("ERR_IMPORT_UNREADABLE|{e}"))?
                .collect::<Result<Vec<String>, _>>()
                .map_err(|e| format!("ERR_IMPORT_UNREADABLE|{e}"))?;
            rows
        };
        // Schema cũ hơn thì `ensure_schema` tự nâng cấp lúc mở. Mới hơn thì
        // không: app này không biết bảng của bản sau, mở vào là hỏng thật.
        let compatible = schema_version <= app_schema_version;
        Ok(ImportInfo {
            schema_version,
            app_schema_version,
            files,
            roots,
            index_bytes: size_of(&index_src),
            thumbs_bytes: size_of(&src.join(THUMBS_DB)),
            compatible,
            reason: (!compatible).then(|| "ERR_SCHEMA_TOO_NEW".into()),
        })
    })
    .await
}

/// Dàn file vào thư mục dữ liệu rồi khởi động lại app để tráo. KHÔNG tráo tại
/// chỗ: connection đang mở giữ file handle, đổi file dưới chân nó là hỏng DB.
#[tauri::command]
pub async fn apply_import(
    app: AppHandle,
    state: State<'_, AppState>,
    src: String,
) -> CmdResult<()> {
    if state
        .recovery_active
        .load(std::sync::atomic::Ordering::Acquire)
    {
        return Err("ERR_RECOVERY_BUSY|".into());
    }
    if !state.jobs.active_jobs().is_empty() {
        return Err("ERR_INDEX_BUSY|a job is running".into());
    }
    let data_dir = state.data_dir.clone();
    let staged = blocking(move || {
        let src = PathBuf::from(src);
        let index_src = src.join(INDEX_DB);
        if !index_src.is_file() {
            return Err("ERR_IMPORT_NO_INDEX|".into());
        }
        if same_dir(&src, &data_dir) {
            return Err("ERR_IMPORT_SAME_DIR|".into());
        }
        let index_staged = data_dir.join(format!("{INDEX_DB}{INCOMING}"));
        std::fs::copy(&index_src, &index_staged).map_err(|e| format!("ERR_IMPORT_COPY|{e}"))?;

        let thumbs_src = src.join(THUMBS_DB);
        if thumbs_src.is_file() {
            let staged = data_dir.join(format!("{THUMBS_DB}{INCOMING}"));
            if let Err(e) = std::fs::copy(&thumbs_src, &staged) {
                // Cache thôi — thiếu thì lượt sau tự cày lại
                tracing::warn!("stage thumbs failed: {e:#}");
                let _ = std::fs::remove_file(&staged);
            }
        }
        Ok(index_staged)
    })
    .await?;
    tracing::info!(staged = %staged.display(), "import staged, restarting");
    app.restart();
}

/// Tráo bản đã dàn sẵn vào chỗ. Gọi lúc KHỞI ĐỘNG, trước khi mở connection nào.
///
/// Best-effort có chủ đích: tráo hỏng không được chặn app mở lên: user còn phải
/// vào được để thử lại hoặc quay về bản cũ. Bản cũ luôn nằm ở `.bak`.
pub fn apply_staged_import(data_dir: &Path) {
    let index_staged = data_dir.join(format!("{INDEX_DB}{INCOMING}"));
    if !index_staged.is_file() {
        return;
    }
    let thumbs_staged = data_dir.join(format!("{THUMBS_DB}{INCOMING}"));
    let with_thumbs = thumbs_staged.is_file();

    if let Err(e) = swap_in(data_dir, INDEX_DB, &index_staged) {
        tracing::error!("import swap index failed: {e:#}");
        let _ = std::fs::remove_file(&index_staged);
        let _ = std::fs::remove_file(&thumbs_staged);
        return;
    }
    if with_thumbs {
        if let Err(e) = swap_in(data_dir, THUMBS_DB, &thumbs_staged) {
            tracing::warn!("import swap thumbs failed: {e:#}");
            let _ = std::fs::remove_file(&thumbs_staged);
        }
    } else {
        // Cache khóa theo file_id của kho CŨ — với index mới thì id trỏ sang
        // file khác hẳn. Giữ lại là hiện nhầm ảnh; xóa thì thumb_warm cày lại.
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(data_dir.join(format!("{THUMBS_DB}{suffix}")));
        }
    }
    tracing::info!(with_thumbs, "imported data applied");
}

fn swap_in(data_dir: &Path, name: &str, staged: &Path) -> std::io::Result<()> {
    let live = data_dir.join(name);
    if live.exists() {
        // Một đời backup: nhập nhầm thì còn đường quay lại
        let bak = data_dir.join(format!("{name}.bak"));
        let _ = std::fs::remove_file(&bak);
        let _ = std::fs::remove_file(data_dir.join(format!("{name}.bak-wal")));
        std::fs::rename(&live, &bak)?;
        // WAL phải đi THEO bản backup. App bị tắt để restart nên WAL chưa chắc
        // đã checkpoint; bỏ nó lại là bản backup thiếu đúng những gì user vừa
        // làm. Tên `<db>-wal` nên đổi thành `index.db.bak-wal` là SQLite tự
        // nhận ra khi mở lại bản backup.
        let _ = std::fs::rename(
            data_dir.join(format!("{name}-wal")),
            data_dir.join(format!("{name}.bak-wal")),
        );
    }
    // Sót lại (rename trên hụt, hoặc không có bản cũ) thì phải dọn: WAL/SHM của
    // DB cũ nằm cạnh file mới là SQLite replay nhầm vào nó.
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(data_dir.join(format!("{name}{suffix}")));
    }
    std::fs::rename(staged, &live)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(p: &Path, body: &str) {
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn staged_import_swaps_and_keeps_one_backup() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        touch(&d.join(INDEX_DB), "old");
        touch(&d.join("index.db-wal"), "stale wal");
        touch(&d.join(THUMBS_DB), "old thumbs");
        touch(&d.join(format!("{INDEX_DB}{INCOMING}")), "new");

        apply_staged_import(d);

        assert_eq!(std::fs::read_to_string(d.join(INDEX_DB)).unwrap(), "new");
        assert_eq!(
            std::fs::read_to_string(d.join("index.db.bak")).unwrap(),
            "old",
            "ban cu phai con duong quay lai"
        );
        assert!(
            !d.join("index.db-wal").exists(),
            "WAL cua ban cu de lai la SQLite replay nham vao file moi"
        );
        assert_eq!(
            std::fs::read_to_string(d.join("index.db.bak-wal")).unwrap(),
            "stale wal",
            "WAL phai di THEO ban backup, khong thi backup thieu phan chua checkpoint"
        );
        assert!(
            !d.join(THUMBS_DB).exists(),
            "nhap index ma khong kem thumbs -> phai xoa cache, id khong an khop"
        );
        assert!(!d.join(format!("{INDEX_DB}{INCOMING}")).exists());
    }

    #[test]
    fn staged_import_with_thumbs_keeps_cache() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        touch(&d.join(INDEX_DB), "old");
        touch(&d.join(THUMBS_DB), "old thumbs");
        touch(&d.join(format!("{INDEX_DB}{INCOMING}")), "new");
        touch(&d.join(format!("{THUMBS_DB}{INCOMING}")), "new thumbs");

        apply_staged_import(d);

        assert_eq!(
            std::fs::read_to_string(d.join(THUMBS_DB)).unwrap(),
            "new thumbs"
        );
        assert_eq!(
            std::fs::read_to_string(d.join("thumbs.db.bak")).unwrap(),
            "old thumbs"
        );
    }

    #[test]
    fn nothing_staged_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        touch(&d.join(INDEX_DB), "live");
        touch(&d.join(THUMBS_DB), "cache");
        apply_staged_import(d);
        assert_eq!(std::fs::read_to_string(d.join(INDEX_DB)).unwrap(), "live");
        assert_eq!(
            std::fs::read_to_string(d.join(THUMBS_DB)).unwrap(),
            "cache",
            "khong co gi de nhap thi khong duoc dung toi cache"
        );
    }
}
