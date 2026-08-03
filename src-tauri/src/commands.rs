use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use core_db::{FileFilter, FileRow, JobRow, RootInfo};
use core_jobs::{JobEvent, JobProgress, Throttle};
use serde::Serialize;
use tauri::State;

use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

/// Mọi command đều blocking DB/fs → chạy trên blocking pool, không giữ tokio worker.
async fn blocking<R, F>(f: F) -> CmdResult<R>
where
    R: Send + 'static,
    F: FnOnce() -> CmdResult<R> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| format!("ERR_INTERNAL|join: {e}"))?
}

/// Canonical hóa root TRƯỚC khi vào DB: giải alias (case, subst, mapped drive,
/// symlink) bằng fs::canonicalize; UNC bị từ chối (M1 chỉ hỗ trợ ổ local).
fn canonicalize_root(path: &str) -> CmdResult<String> {
    let p = Path::new(path);
    if !p.is_dir() {
        return Err(format!("ERR_NOT_DIR|{path}"));
    }
    let canon = std::fs::canonicalize(p).map_err(|e| format!("ERR_NOT_DIR|{path}: {e}"))?;
    let s = canon.to_string_lossy().to_string();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        if rest.starts_with("UNC\\") {
            return Err(format!("ERR_UNC_UNSUPPORTED|{path}"));
        }
        Ok(rest.to_string())
    } else if s.starts_with(r"\\") {
        Err(format!("ERR_UNC_UNSUPPORTED|{path}"))
    } else {
        Ok(s)
    }
}

#[tauri::command]
pub async fn add_root(state: State<'_, AppState>, path: String) -> CmdResult<i64> {
    let writer = state.db.writer.clone();
    blocking(move || {
        let canonical = canonicalize_root(&path)?;
        writer
            .exec(move |c| core_db::ops::upsert_root(c, &canonical))
            .map_err(err)
    })
    .await
}

#[tauri::command]
pub async fn list_roots(state: State<'_, AppState>) -> CmdResult<Vec<RootInfo>> {
    let db = state.db.clone();
    blocking(move || db.pool.with(core_db::ops::list_roots).map_err(err)).await
}

#[tauri::command]
pub async fn remove_root(state: State<'_, AppState>, root_id: i64) -> CmdResult<()> {
    let db = state.db.clone();
    let jobs = state.jobs.clone();
    blocking(move || {
        // Scan đang chạy trên root này phải chết hẳn trước khi xóa index —
        // nếu không dir_cache của nó sẽ ghi file vào dir id đã bị xóa/tái dùng.
        let cancelled = jobs.cancel_scans_for_root(root_id);
        let deadline = Instant::now() + Duration::from_secs(30);
        while jobs.any_active(&cancelled) {
            if Instant::now() > deadline {
                return Err("ERR_SCAN_STOP_TIMEOUT|scan không dừng sau 30s".into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        db.writer
            .exec(move |c| core_db::ops::remove_root_chunked(c, root_id))
            .map_err(err)
    })
    .await
}

#[tauri::command]
pub async fn start_scan(state: State<'_, AppState>, root_id: i64) -> CmdResult<i64> {
    if let Some(job_id) = state.jobs.active_scan_for_root(root_id) {
        return Err(format!("ERR_SCAN_ACTIVE|{job_id}"));
    }
    let db = state.db.clone();
    let jobs = state.jobs.clone();
    blocking(move || {
        let (path, volume_id) = db
            .pool
            .with(|c| core_db::ops::get_root(c, root_id))
            .map_err(err)?;
        let excluded = db
            .pool
            .with(core_db::ops::get_excluded_paths)
            .map_err(err)?;
        let gen = db
            .writer
            .exec(|c| core_db::ops::next_scan_gen(c))
            .map_err(err)?;
        let params = format!("{{\"rootId\":{root_id}}}");
        let job_id = db
            .writer
            .exec(move |c| core_db::ops::insert_job(c, "scan", Some(&params)))
            .map_err(err)?;

        let cancel = jobs.register(job_id, "scan", Some(root_id));
        let events = jobs.sender();
        let writer = db.writer.clone();

        std::thread::Builder::new()
            .name(format!("scan-{job_id}"))
            .spawn(move || {
                let events_progress = events.clone();
                let cancel_progress = cancel.clone();
                // Panic ở bất kỳ đâu trong scan cũng phải ra terminal event —
                // không bao giờ để job kẹt 'running' vĩnh viễn.
                let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    let mut throttle = Throttle::new(100);
                    core_index::scan_root(
                        Path::new(&path),
                        volume_id,
                        gen,
                        &writer,
                        &cancel_progress,
                        &excluded,
                        |done| {
                            if throttle.ready() {
                                let _ = events_progress.send(JobEvent::Progress(JobProgress {
                                    job_id,
                                    kind: "scan".into(),
                                    done,
                                    total: None,
                                    message: None,
                                }));
                            }
                        },
                    )
                }));
                let final_event = match result {
                    Err(_) => JobEvent::Failed {
                        job_id,
                        kind: "scan".into(),
                        error: "ERR_INTERNAL|scan thread panicked".into(),
                    },
                    Ok(Ok(_)) if cancel.load(Ordering::Relaxed) => JobEvent::Cancelled {
                        job_id,
                        kind: "scan".into(),
                    },
                    Ok(Ok(s)) => {
                        let mut msg =
                            format!("indexed {}, missing {}", s.indexed, s.marked_missing);
                        if s.walk_errors > 0 || s.skipped_lossy_names > 0 {
                            msg.push_str(&format!(
                                " (warn: {} read errors, {} skipped names)",
                                s.walk_errors, s.skipped_lossy_names
                            ));
                        }
                        JobEvent::Done {
                            job_id,
                            kind: "scan".into(),
                            message: Some(msg),
                        }
                    }
                    Ok(Err(e)) => JobEvent::Failed {
                        job_id,
                        kind: "scan".into(),
                        error: format!("{e:#}"),
                    },
                };
                let _ = events.send(final_event);
            })
            .map_err(|e| format!("ERR_INTERNAL|spawn: {e}"))?;

        Ok(job_id)
    })
    .await
}

#[tauri::command]
pub async fn cancel_job(state: State<'_, AppState>, job_id: i64) -> CmdResult<bool> {
    Ok(state.jobs.cancel(job_id))
}

#[tauri::command]
pub async fn list_jobs(state: State<'_, AppState>) -> CmdResult<Vec<JobRow>> {
    let db = state.db.clone();
    blocking(move || {
        db.pool
            .with(|c| core_db::ops::list_jobs(c, 30))
            .map_err(err)
    })
    .await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryResult {
    pub query_id: u64,
    pub total: usize,
}

#[tauri::command]
pub async fn query_files(state: State<'_, AppState>, filter: FileFilter) -> CmdResult<QueryResult> {
    let db = state.db.clone();
    let queries = state.queries.clone();
    blocking(move || {
        let ids = db
            .pool
            .with(|c| core_db::query::query_ids(c, &filter))
            .map_err(err)?;
        let total = ids.len();
        let query_id = queries.lock().unwrap().insert(ids);
        Ok(QueryResult { query_id, total })
    })
    .await
}

#[tauri::command]
pub async fn fetch_rows(
    state: State<'_, AppState>,
    query_id: u64,
    start: usize,
    count: usize,
) -> CmdResult<Vec<Option<FileRow>>> {
    let db = state.db.clone();
    let queries = state.queries.clone();
    blocking(move || {
        let ids = queries
            .lock()
            .unwrap()
            .get(query_id)
            .ok_or("ERR_QUERY_EXPIRED|")?;
        let count = count.min(500);
        let end = start.saturating_add(count).min(ids.len());
        if start >= end {
            return Ok(Vec::new());
        }
        let window = ids[start..end].to_vec();
        db.pool
            .with(|c| core_db::query::fetch_rows(c, &window))
            .map_err(err)
    })
    .await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub setup_done: bool,
    pub tz_offset_minutes: Option<i32>,
}

#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> CmdResult<Settings> {
    let db = state.db.clone();
    blocking(move || {
        db.pool
            .with(|c| {
                let setup_done = core_db::ops::kv_get(c, "setup_done")?
                    .map(|v| v == "1")
                    .unwrap_or(false);
                let tz_offset_minutes =
                    core_db::ops::kv_get(c, "tz_offset_minutes")?.and_then(|v| v.parse().ok());
                Ok(Settings {
                    setup_done,
                    tz_offset_minutes,
                })
            })
            .map_err(err)
    })
    .await
}

#[tauri::command]
pub async fn get_excluded_paths(state: State<'_, AppState>) -> CmdResult<Vec<String>> {
    let db = state.db.clone();
    blocking(move || db.pool.with(core_db::ops::get_excluded_paths).map_err(err)).await
}

#[tauri::command]
pub async fn set_excluded_paths(state: State<'_, AppState>, paths: Vec<String>) -> CmdResult<()> {
    let writer = state.db.writer.clone();
    blocking(move || {
        writer
            .exec(move |c| core_db::ops::set_excluded_paths(c, &paths))
            .map_err(err)
    })
    .await
}

#[tauri::command]
pub async fn set_settings(
    state: State<'_, AppState>,
    tz_offset_minutes: i32,
    setup_done: bool,
) -> CmdResult<()> {
    let writer = state.db.writer.clone();
    blocking(move || {
        writer
            .exec(move |c| {
                core_db::ops::kv_set(c, "tz_offset_minutes", &tz_offset_minutes.to_string())?;
                core_db::ops::kv_set(c, "setup_done", if setup_done { "1" } else { "0" })?;
                Ok(())
            })
            .map_err(err)
    })
    .await
}
