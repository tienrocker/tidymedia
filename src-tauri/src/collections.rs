//! Command cho nhãn + album.
//!
//! Đây là dữ liệu user tự tạo, KHÔNG dựng lại được bằng quét (khác index, khác
//! thumb cache). Nên mọi thứ ở đây do user bấm mới chạy, và phần xoá do UI hỏi
//! lại trước khi gọi xuống.

use core_db::NamedCount;
use tauri::State;

use crate::commands::{blocking, err, CmdResult};
use crate::state::AppState;

#[tauri::command]
pub async fn list_tags(state: State<'_, AppState>) -> CmdResult<Vec<NamedCount>> {
    let db = state.db.clone();
    blocking(move || db.pool.with(core_db::collections::list_tags).map_err(err)).await
}

#[tauri::command]
pub async fn list_albums(state: State<'_, AppState>) -> CmdResult<Vec<NamedCount>> {
    let db = state.db.clone();
    blocking(move || db.pool.with(core_db::collections::list_albums).map_err(err)).await
}

#[tauri::command]
pub async fn tags_of_file(state: State<'_, AppState>, file_id: i64) -> CmdResult<Vec<NamedCount>> {
    let db = state.db.clone();
    blocking(move || {
        db.pool
            .with(|c| core_db::collections::tags_of_file(c, file_id))
            .map_err(err)
    })
    .await
}

#[tauri::command]
pub async fn tag_files(
    state: State<'_, AppState>,
    name: String,
    file_ids: Vec<i64>,
) -> CmdResult<i64> {
    let db = state.db.clone();
    blocking(move || {
        db.writer
            .exec(move |c| core_db::collections::tag_files(c, &name, &file_ids))
            .map_err(err)
    })
    .await
}

#[tauri::command]
pub async fn untag_files(
    state: State<'_, AppState>,
    tag_id: i64,
    file_ids: Vec<i64>,
) -> CmdResult<()> {
    let db = state.db.clone();
    blocking(move || {
        db.writer
            .exec(move |c| core_db::collections::untag_files(c, tag_id, &file_ids))
            .map_err(err)
    })
    .await
}

#[tauri::command]
pub async fn delete_tag(state: State<'_, AppState>, tag_id: i64) -> CmdResult<()> {
    let db = state.db.clone();
    blocking(move || {
        db.writer
            .exec(move |c| core_db::collections::delete_tag(c, tag_id))
            .map_err(err)
    })
    .await
}

#[tauri::command]
pub async fn rename_tag(state: State<'_, AppState>, tag_id: i64, name: String) -> CmdResult<()> {
    let db = state.db.clone();
    blocking(move || {
        db.writer
            .exec(move |c| core_db::collections::rename_tag(c, tag_id, &name))
            .map_err(err)
    })
    .await
}

#[tauri::command]
pub async fn create_album(state: State<'_, AppState>, name: String) -> CmdResult<i64> {
    let db = state.db.clone();
    blocking(move || {
        db.writer
            .exec(move |c| core_db::collections::create_album(c, &name))
            .map_err(err)
    })
    .await
}

/// Trả số file THỰC SỰ được thêm — file đã có sẵn trong album không tính, để UI
/// nói đúng "thêm 3" thay vì "thêm 10" khi 7 cái đã nằm sẵn trong đó.
#[tauri::command]
pub async fn add_to_album(
    state: State<'_, AppState>,
    album_id: i64,
    file_ids: Vec<i64>,
) -> CmdResult<usize> {
    let db = state.db.clone();
    blocking(move || {
        db.writer
            .exec(move |c| core_db::collections::add_to_album(c, album_id, &file_ids))
            .map_err(err)
    })
    .await
}

#[tauri::command]
pub async fn remove_from_album(
    state: State<'_, AppState>,
    album_id: i64,
    file_ids: Vec<i64>,
) -> CmdResult<()> {
    let db = state.db.clone();
    blocking(move || {
        db.writer
            .exec(move |c| core_db::collections::remove_from_album(c, album_id, &file_ids))
            .map_err(err)
    })
    .await
}

#[tauri::command]
pub async fn delete_album(state: State<'_, AppState>, album_id: i64) -> CmdResult<()> {
    let db = state.db.clone();
    blocking(move || {
        db.writer
            .exec(move |c| core_db::collections::delete_album(c, album_id))
            .map_err(err)
    })
    .await
}

#[tauri::command]
pub async fn rename_album(
    state: State<'_, AppState>,
    album_id: i64,
    name: String,
) -> CmdResult<()> {
    let db = state.db.clone();
    blocking(move || {
        db.writer
            .exec(move |c| core_db::collections::rename_album(c, album_id, &name))
            .map_err(err)
    })
    .await
}

/// Id của một khoảng trong kết quả query đang mở — để chọn theo dải (shift-click)
/// và "chọn tất cả" mà KHÔNG phải nạp row của từng file.
///
/// Query cũ đã bị thay (user đổi filter) thì trả rỗng chứ không lỗi: lúc đó
/// selection cũng không còn nghĩa gì.
#[tauri::command]
pub async fn query_id_range(
    state: State<'_, AppState>,
    query_id: u64,
    start: usize,
    count: usize,
) -> CmdResult<Vec<i64>> {
    let queries = state.queries.clone();
    blocking(move || {
        let guard = queries.lock().unwrap();
        let Some(ids) = guard.get(query_id) else {
            return Ok(Vec::new());
        };
        let end = start.saturating_add(count).min(ids.len());
        Ok(ids.get(start..end).unwrap_or_default().to_vec())
    })
    .await
}
