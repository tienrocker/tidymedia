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
    /// Cache thumbnail WebP (thumbs.db, LRU 2GB) — mất là re-generate, không quý.
    pub thumbs: Arc<core_media::ThumbStore>,
    /// Pool riêng cho decode thumb + đọc media — không bao giờ chiếm thread
    /// webview hay tokio worker.
    pub thumb_pool: Arc<rayon::ThreadPool>,
    /// ffmpeg cho HEIC/AVIF/JXL + keyframe video. None = hiện icon thay thumb.
    pub ffmpeg: Option<std::path::PathBuf>,
    /// ffprobe cho metadata video. None = video không có meta/duration (chờ tool).
    pub ffprobe: Option<std::path::PathBuf>,
    /// Gate serialize đoạn khởi động meta job (check-active → insert → register
    /// không atomic; StrictMode mount 2 lần / 2 scan xong sát nhau sẽ đua).
    pub meta_start_gate: Arc<std::sync::atomic::AtomicBool>,
    /// Gate tương tự cho hash job (M4 dedup).
    pub hash_start_gate: Arc<std::sync::atomic::AtomicBool>,
    /// Serialize TOÀN BỘ delete_dup_files: 2 đợt xóa chạy song song có thể
    /// verify chéo rồi xóa sạch cả nhóm (mỗi bên tưởng bên kia giữ bản sống).
    pub delete_lock: Arc<std::sync::Mutex<()>>,
}

const THUMB_CACHE_CAP_BYTES: i64 = 2 * 1024 * 1024 * 1024;

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

    // Bootstrap 1 lần sau khi lên schema v4: ghép Live Photo cho index CÓ SẴN
    // (bình thường pairing chỉ chạy sau scan — index migrate lên không rescan
    // thì MOV cứ hiện mãi). Async, không chặn khởi động.
    writer.exec_async(|c| {
        if core_db::ops::kv_get(c, "live_pair_bootstrap")?.is_none() {
            for r in core_db::ops::list_roots(c)? {
                core_db::ops::pair_live_photos(c, &r.path)?;
            }
            core_db::ops::kv_set(c, "live_pair_bootstrap", "1")?;
            tracing::info!("live photo pairing bootstrapped for existing index");
        }
        Ok(())
    });

    let thumbs = Arc::new(core_media::ThumbStore::open(
        &data_dir.join("thumbs.db"),
        THUMB_CACHE_CAP_BYTES,
    )?);
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let thumb_pool = Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads((cores / 2).clamp(2, 6))
            .thread_name(|i| format!("thumb-{i}"))
            // Lưới an toàn thứ 2 (handler đã catch_unwind): KHÔNG có handler thì
            // rayon abort cả process khi task panic.
            .panic_handler(|_| tracing::error!("thumb pool task panicked (caught)"))
            .build()?,
    );
    let ffmpeg = core_media::find_ffmpeg();
    let ffprobe = core_media::find_ffprobe();
    match &ffmpeg {
        Some(p) => tracing::info!(path = %p.display(), "ffmpeg found — HEIC/AVIF/video thumbs on"),
        None => tracing::info!("ffmpeg not found — HEIC/AVIF/video hiện icon"),
    }
    match &ffprobe {
        Some(p) => tracing::info!(path = %p.display(), "ffprobe found — video metadata on"),
        None => tracing::info!("ffprobe not found — video meta để chờ"),
    }

    app.manage(AppState {
        db,
        jobs,
        queries: Arc::new(Mutex::new(QueryCache::new())),
        thumbs,
        thumb_pool,
        ffmpeg,
        ffprobe,
        meta_start_gate: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        hash_start_gate: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        delete_lock: Arc::new(std::sync::Mutex::new(())),
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
