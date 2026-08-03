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
    pub db: Db,
    pub jobs: JobManager,
    pub queries: Mutex<QueryCache>,
}

pub fn init(app: &AppHandle) -> Result<()> {
    let mut data_dir = app.path().app_data_dir()?;
    if cfg!(debug_assertions) {
        // Dev build tuyệt đối không đụng data của bản cài thật.
        data_dir = data_dir.join("dev");
    }
    tracing::info!(dir = %data_dir.display(), "opening index db");
    let db = Db::open(&data_dir)?;

    let jobs = JobManager::new();
    let events_rx = jobs.receiver();
    let writer = db.writer.clone();

    app.manage(AppState {
        db,
        jobs,
        queries: Mutex::new(QueryCache::new()),
    });

    // Event pump: JobEvent → UI (tauri events) + jobs table + index://changed debounce.
    let handle = app.clone();
    std::thread::Builder::new()
        .name("job-event-pump".into())
        .spawn(move || {
            let mut last_changed = Instant::now() - Duration::from_secs(60);
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
                        if is_scan && last_changed.elapsed() >= Duration::from_millis(500) {
                            last_changed = Instant::now();
                            let _ = handle.emit("index://changed", ());
                        }
                    }
                    JobEvent::Done {
                        job_id,
                        kind,
                        message,
                    } => {
                        writer.exec_async(move |c| core_db::ops::finish_job(c, job_id, "done", None));
                        handle.state::<AppState>().jobs.unregister(job_id);
                        let _ = handle.emit(
                            "job://done",
                            &serde_json::json!({ "jobId": job_id, "kind": kind, "message": message }),
                        );
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
