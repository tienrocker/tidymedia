use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use core_db::Db;
use core_jobs::{JobEvent, JobManager};
use tauri::{AppHandle, Emitter, Manager};

/// Cache kết quả query kiểu Everything: giữ nguyên Vec<id> phía Rust,
/// UI chỉ fetch cửa sổ. Giữ 2 generation gần nhất.
pub struct QueryCache {
    next_id: u64,
    entries: VecDeque<(u64, Arc<Vec<i64>>)>,
}

impl QueryCache {
    fn new() -> Self {
        Self {
            next_id: 1,
            entries: VecDeque::new(),
        }
    }

    pub fn insert(&mut self, ids: Vec<i64>) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.entries.push_back((id, Arc::new(ids)));
        while self.entries.len() > 2 {
            self.entries.pop_front();
        }
        id
    }

    pub fn get(&self, id: u64) -> Option<Arc<Vec<i64>>> {
        self.entries
            .iter()
            .find(|(qid, _)| *qid == id)
            .map(|(_, ids)| ids.clone())
    }
}

pub struct AppState {
    pub db: Arc<Db>,
    pub jobs: Arc<JobManager>,
    pub queries: Arc<Mutex<QueryCache>>,
}

/// Data dir: portable.marker cạnh exe → .\data cạnh exe (chạy từ USB);
/// không thì app_data_dir (+\dev cho debug build — dev không được đụng data thật).
fn resolve_data_dir(app: &AppHandle) -> Result<std::path::PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            if exe_dir.join("portable.marker").exists() {
                return Ok(exe_dir.join("data"));
            }
        }
    }
    let mut dir = app.path().app_data_dir()?;
    if cfg!(debug_assertions) {
        dir = dir.join("dev");
    }
    Ok(dir)
}

pub fn init(app: &AppHandle) -> Result<()> {
    let data_dir = resolve_data_dir(app)?;
    tracing::info!(dir = %data_dir.display(), "opening index db");
    let db = Arc::new(Db::open(&data_dir)?);

    let jobs = Arc::new(JobManager::new());
    let events_rx = jobs.receiver();
    let writer = db.writer.clone();

    app.manage(AppState {
        db,
        jobs,
        queries: Arc::new(Mutex::new(QueryCache::new())),
    });

    // Event pump: JobEvent → UI (tauri events) + jobs table + index://changed.
    // index://changed khi đang scan giãn 2.5s — UI re-query cả list, không được spam.
    let handle = app.clone();
    std::thread::Builder::new()
        .name("job-event-pump".into())
        .spawn(move || {
            let mut last_changed: Option<Instant> = None;
            let emit_changed = |handle: &AppHandle, last: &mut Option<Instant>| {
                let due = !last.is_some_and(|t| t.elapsed() < Duration::from_millis(2500));
                if due {
                    *last = Some(Instant::now());
                    let _ = handle.emit("index://changed", ());
                }
            };
            while let Ok(ev) = events_rx.recv() {
                match ev {
                    JobEvent::Progress(p) => {
                        writer.exec_async({
                            let p = p.clone();
                            move |c| {
                                core_db::ops::update_job_progress(
                                    c,
                                    p.job_id,
                                    p.done as i64,
                                    p.total.map(|t| t as i64),
                                    p.message.as_deref(),
                                )
                            }
                        });
                        let is_scan = p.kind == "scan";
                        let _ = handle.emit("job://progress", &p);
                        if is_scan {
                            emit_changed(&handle, &mut last_changed);
                        }
                    }
                    JobEvent::Done {
                        job_id,
                        kind,
                        message,
                    } => {
                        writer.exec_async({
                            let message = message.clone();
                            move |c| {
                                if let Some(m) = &message {
                                    core_db::ops::update_job_progress(c, job_id, -1, None, Some(m))
                                        .ok();
                                }
                                core_db::ops::finish_job(c, job_id, "done", None)
                            }
                        });
                        handle.state::<AppState>().jobs.unregister(job_id);
                        let _ = handle.emit(
                            "job://done",
                            &serde_json::json!({ "jobId": job_id, "kind": kind, "message": message }),
                        );
                        last_changed = None;
                        let _ = handle.emit("index://changed", ());
                    }
                    JobEvent::Failed { job_id, kind, error } => {
                        writer.exec_async({
                            let error = error.clone();
                            move |c| core_db::ops::finish_job(c, job_id, "failed", Some(&error))
                        });
                        handle.state::<AppState>().jobs.unregister(job_id);
                        let _ = handle.emit(
                            "job://failed",
                            &serde_json::json!({ "jobId": job_id, "kind": kind, "error": error }),
                        );
                    }
                    JobEvent::Cancelled { job_id, kind } => {
                        writer.exec_async(move |c| {
                            core_db::ops::finish_job(c, job_id, "cancelled", None)
                        });
                        handle.state::<AppState>().jobs.unregister(job_id);
                        let _ = handle.emit(
                            "job://done",
                            &serde_json::json!({ "jobId": job_id, "kind": kind, "message": "cancelled" }),
                        );
                        let _ = handle.emit("index://changed", ());
                    }
                }
            }
        })
        .expect("spawn job-event-pump");

    Ok(())
}
