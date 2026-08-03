use std::path::Path;
use std::sync::atomic::Ordering;

use core_db::{FileFilter, FileRow, JobRow, RootInfo};
use core_jobs::{JobEvent, JobProgress, Throttle};
use serde::Serialize;
use tauri::State;

use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

#[tauri::command]
pub async fn add_root(state: State<'_, AppState>, path: String) -> CmdResult<i64> {
    if !Path::new(&path).is_dir() {
        return Err(format!("Không phải thư mục: {path}"));
    }
    state
        .db
        .writer
        .exec(move |c| core_db::ops::upsert_root(c, &path))
        .map_err(err)
}

#[tauri::command]
pub async fn list_roots(state: State<'_, AppState>) -> CmdResult<Vec<RootInfo>> {
    state.db.pool.with(core_db::ops::list_roots).map_err(err)
}

#[tauri::command]
pub async fn remove_root(state: State<'_, AppState>, root_id: i64) -> CmdResult<()> {
    state
        .db
        .writer
        .exec(move |c| core_db::ops::remove_root(c, root_id))
        .map_err(err)
}

#[tauri::command]
pub async fn start_scan(state: State<'_, AppState>, root_id: i64) -> CmdResult<i64> {
    let (path, volume_id) = state
        .db
        .pool
        .with(|c| core_db::ops::get_root(c, root_id))
        .map_err(err)?;

    let params = format!("{{\"rootId\":{root_id}}}");
    let job_id = state
        .db
        .writer
        .exec(move |c| core_db::ops::insert_job(c, "scan", Some(&params)))
        .map_err(err)?;

    let cancel = state.jobs.register(job_id);
    let events = state.jobs.sender();
    let writer = state.db.writer.clone();

    std::thread::Builder::new()
        .name(format!("scan-{job_id}"))
        .spawn(move || {
            let mut throttle = Throttle::new(100);
            let result = core_index::scan_root(
                Path::new(&path),
                volume_id,
                job_id,
                &writer,
                &cancel,
                |done| {
                    if throttle.ready() {
                        let _ = events.send(JobEvent::Progress(JobProgress {
                            job_id,
                            kind: "scan".into(),
                            done,
                            total: None,
                            message: None,
                        }));
                    }
                },
            );
            let final_event = match result {
                Ok(_) if cancel.load(Ordering::Relaxed) => JobEvent::Cancelled {
                    job_id,
                    kind: "scan".into(),
                },
                Ok(s) => JobEvent::Done {
                    job_id,
                    kind: "scan".into(),
                    message: Some(format!(
                        "indexed {} files, {} missing",
                        s.indexed, s.marked_missing
                    )),
                },
                Err(e) => JobEvent::Failed {
                    job_id,
                    kind: "scan".into(),
                    error: format!("{e:#}"),
                },
            };
            let _ = events.send(final_event);
        })
        .map_err(|e| e.to_string())?;

    Ok(job_id)
}

#[tauri::command]
pub async fn cancel_job(state: State<'_, AppState>, job_id: i64) -> CmdResult<bool> {
    Ok(state.jobs.cancel(job_id))
}

#[tauri::command]
pub async fn list_jobs(state: State<'_, AppState>) -> CmdResult<Vec<JobRow>> {
    state
        .db
        .pool
        .with(|c| core_db::ops::list_jobs(c, 30))
        .map_err(err)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryResult {
    pub query_id: u64,
    pub total: usize,
}

#[tauri::command]
pub async fn query_files(state: State<'_, AppState>, filter: FileFilter) -> CmdResult<QueryResult> {
    let ids = state
        .db
        .pool
        .with(|c| core_db::query::query_ids(c, &filter))
        .map_err(err)?;
    let total = ids.len();
    let query_id = state.queries.lock().unwrap().insert(ids);
    Ok(QueryResult { query_id, total })
}

#[tauri::command]
pub async fn fetch_rows(
    state: State<'_, AppState>,
    query_id: u64,
    start: usize,
    count: usize,
) -> CmdResult<Vec<FileRow>> {
    let ids = state
        .queries
        .lock()
        .unwrap()
        .get(query_id)
        .ok_or("query expired")?;
    let count = count.min(500);
    let end = start.saturating_add(count).min(ids.len());
    if start >= end {
        return Ok(Vec::new());
    }
    let window = ids[start..end].to_vec();
    state
        .db
        .pool
        .with(|c| core_db::query::fetch_rows(c, &window))
        .map_err(err)
}
