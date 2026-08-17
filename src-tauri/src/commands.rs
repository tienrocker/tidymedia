use std::os::windows::process::CommandExt;
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use core_db::{FileDetail, FileFilter, FileRow, JobRow, MetaUpsert, PendingMeta, RootInfo};
use core_jobs::{JobEvent, JobProgress, Throttle};
use rayon::prelude::*;
use serde::Serialize;
use tauri::State;

use crate::state::{AppState, LifoPool};

pub(crate) type CmdResult<T> = Result<T, String>;

pub(crate) fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

/// Mọi command đều blocking DB/fs → chạy trên blocking pool, không giữ tokio worker.
pub(crate) async fn blocking<R, F>(f: F) -> CmdResult<R>
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
pub(crate) fn canonicalize_root(path: &str) -> CmdResult<String> {
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
    if state.recovery_active.load(Ordering::Acquire) {
        return Err("ERR_RECOVERY_BUSY|".into());
    }
    crate::organize::invalidate_org_preview(&state);
    let writer = state.db.writer.clone();
    let jobs = state.jobs.clone();
    let op_gate = state.index_op_gate.clone();
    blocking(move || {
        let _op = op_gate.lock().unwrap_or_else(|p| p.into_inner());
        if jobs.active_job_of_kind("scan").is_some()
            || jobs.active_job_of_kind("hash").is_some()
            || jobs.active_job_of_kind("org_hash").is_some()
            || jobs.active_job_of_kind("organize").is_some()
            || jobs.active_job_of_kind("org_undo").is_some()
        {
            return Err("ERR_INDEX_BUSY|scan/organize is active".into());
        }
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
    if state.recovery_active.load(Ordering::Acquire) {
        return Err("ERR_RECOVERY_BUSY|".into());
    }
    crate::organize::invalidate_org_preview(&state);
    let db = state.db.clone();
    let jobs = state.jobs.clone();
    let op_gate = state.index_op_gate.clone();
    let fs_lock = state.delete_lock.clone();
    blocking(move || {
        // Canonical nested lock order: index_op_gate -> delete_lock. The filesystem
        // acquisition remains fail-fast, so the operation gate is never held while waiting.
        let _op = op_gate.lock().unwrap_or_else(|p| p.into_inner());
        let _fs = match fs_lock.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                return Err("ERR_INDEX_BUSY|filesystem operation is active".into())
            }
        };
        if jobs.active_job_of_kind("hash").is_some()
            || jobs.active_job_of_kind("org_hash").is_some()
        {
            return Err("ERR_INDEX_BUSY|hash job is active".into());
        }
        if jobs.active_job_of_kind("organize").is_some()
            || jobs.active_job_of_kind("org_undo").is_some()
        {
            return Err("ERR_ORG_BUSY|".into());
        }
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
    if state.recovery_active.load(Ordering::Acquire) {
        return Err("ERR_RECOVERY_BUSY|".into());
    }
    crate::organize::invalidate_org_preview(&state);
    if let Some(job_id) = state.jobs.active_scan_for_root(root_id) {
        return Err(format!("ERR_SCAN_ACTIVE|{job_id}"));
    }
    let db = state.db.clone();
    let jobs = state.jobs.clone();
    let op_gate = state.index_op_gate.clone();
    blocking(move || {
        let _op = op_gate.lock().unwrap_or_else(|p| p.into_inner());
        if jobs.active_job_of_kind("hash").is_some()
            || jobs.active_job_of_kind("org_hash").is_some()
        {
            return Err("ERR_INDEX_BUSY|hash job is active".into());
        }
        if jobs.active_job_of_kind("organize").is_some()
            || jobs.active_job_of_kind("org_undo").is_some()
        {
            return Err("ERR_ORG_BUSY|".into());
        }
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

        // Register ATOMIC — thua race với scan khác vừa start cùng root thì
        // chốt job row failed và trả lỗi thay vì chạy scan gen chồng nhau.
        let Some(cancel) = jobs.try_register_scan(job_id, root_id) else {
            let _ = db.writer.exec(move |c| {
                core_db::ops::finish_job(c, job_id, "failed", Some("ERR_SCAN_ACTIVE"))
            });
            return Err(format!("ERR_SCAN_ACTIVE|{root_id}"));
        };
        let events = jobs.sender();
        let writer = db.writer.clone();
        let writer_cleanup = db.writer.clone();

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
                        // Ghép cặp Live Photo (HEIC+MOV cùng stem) trong root vừa
                        // scan — lỗi chỉ log, không fail scan đã thành công.
                        let root = path.clone();
                        if let Err(e) =
                            writer.exec(move |c| core_db::ops::pair_live_photos(c, &root))
                        {
                            tracing::warn!("pair_live_photos failed: {e:#}");
                        }
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
            .map_err(|e| {
                // Như meta: spawn fail phải dọn registry + job row, không để
                // job ma chặn ERR_SCAN_ACTIVE vĩnh viễn.
                jobs.unregister(job_id);
                writer_cleanup.exec_async(move |c| {
                    core_db::ops::finish_job(c, job_id, "failed", Some("ERR_INTERNAL|spawn failed"))
                });
                format!("ERR_INTERNAL|spawn: {e}")
            })?;

        Ok(job_id)
    })
    .await
}

/// Hạ gate start khi ra khỏi scope — kể cả early-return/lỗi, không bao giờ
/// khóa chết đường khởi động job (dùng chung cho meta + hash).
pub(crate) struct GateGuard(pub(crate) std::sync::Arc<std::sync::atomic::AtomicBool>);
impl Drop for GateGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Xả hàng đợi thumb CHƯA chạy — UI gọi khi rời chế độ grid/đổi filter: mọi
/// request cũ đã bị webview hủy, giữ lại chỉ tổ giành I/O với job nền.
#[tauri::command]
pub fn clear_thumb_queue(state: State<'_, AppState>) -> usize {
    let dropped = state.thumb_pool.clear(crate::state::PoolTag::Thumb);
    if dropped > 0 {
        tracing::debug!(dropped, "cleared stale thumb queue");
    }
    dropped
}

/// Job warm thumbnail nền — ƯU TIÊN THẤP NHẤT toàn app: tạo sẵn thumb lưới
/// cho cả kho, nhưng ngủ khi (a) còn bất kỳ thumb interactive nào đang chờ
/// (kể cả backlog) hoặc (b) BẤT KỲ job nào khác đang chạy. Chỉ ăn I/O lúc
/// app rảnh hoàn toàn; hủy được như mọi job.
#[tauri::command]
pub async fn start_thumb_warm(state: State<'_, AppState>) -> CmdResult<Option<i64>> {
    if state.recovery_active.load(Ordering::Acquire) {
        return Err("ERR_RECOVERY_BUSY|".into());
    }
    let gate = state.thumb_warm_gate.clone();
    if gate
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Ok(state.jobs.active_job_of_kind("thumb_warm"));
    }
    let guard = GateGuard(gate);
    if let Some(id) = state.jobs.active_job_of_kind("thumb_warm") {
        return Ok(Some(id));
    }
    let db = state.db.clone();
    let jobs = state.jobs.clone();
    let thumbs = state.thumbs.clone();
    let ffmpeg = state.ffmpeg.clone();
    let thumb_pool = state.thumb_pool.clone();
    blocking(move || {
        let _gate = guard;
        let total = db
            .pool
            .with(core_db::ops::count_present_files)
            .map_err(err)?;
        if total == 0 {
            return Ok(None);
        }
        let job_id = db
            .writer
            .exec(|c| core_db::ops::insert_job(c, "thumb_warm", None))
            .map_err(err)?;
        let (cancel, pause) = jobs.register_pausable(job_id, "thumb_warm", None);
        let events = jobs.sender();
        let writer_cleanup = db.writer.clone();
        let jobs_run = jobs.clone();
        std::thread::Builder::new()
            .name(format!("thumb-warm-{job_id}"))
            .spawn(move || {
                let events_run = events.clone();
                let cancel_run = cancel.clone();
                let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    run_thumb_warm_job(
                        &db,
                        &thumbs,
                        ffmpeg.as_deref(),
                        &thumb_pool,
                        &jobs_run,
                        &cancel_run,
                        &pause,
                        job_id,
                        total as u64,
                        &events_run,
                    )
                }));
                let final_event = match result {
                    Err(_) => JobEvent::Failed {
                        job_id,
                        kind: "thumb_warm".into(),
                        error: "ERR_INTERNAL|thumb warm thread panicked".into(),
                    },
                    Ok(Ok(_)) if cancel.load(Ordering::Relaxed) => JobEvent::Cancelled {
                        job_id,
                        kind: "thumb_warm".into(),
                    },
                    Ok(Ok(made)) => JobEvent::Done {
                        job_id,
                        kind: "thumb_warm".into(),
                        message: Some(format!("warmed +{made} thumbs")),
                    },
                    Ok(Err(e)) => JobEvent::Failed {
                        job_id,
                        kind: "thumb_warm".into(),
                        error: format!("{e:#}"),
                    },
                };
                let _ = events.send(final_event);
            })
            .map_err(|e| {
                jobs.unregister(job_id);
                writer_cleanup.exec_async(move |c| {
                    core_db::ops::finish_job(c, job_id, "failed", Some("ERR_INTERNAL|spawn failed"))
                });
                format!("ERR_INTERNAL|spawn: {e}")
            })?;
        Ok(Some(job_id))
    })
    .await
}

#[allow(clippy::too_many_arguments)] // private, 1 call site — context thật của job
fn run_thumb_warm_job(
    db: &core_db::Db,
    thumbs: &core_media::ThumbStore,
    ffmpeg: Option<&std::path::Path>,
    thumb_pool: &LifoPool,
    jobs: &core_jobs::JobManager,
    cancel: &core_jobs::CancelFlag,
    pause: &core_jobs::PauseFlag,
    job_id: i64,
    total: u64,
    events: &crossbeam_channel::Sender<JobEvent>,
) -> anyhow::Result<u64> {
    // Cỡ thumb lưới — khớp THUMB_GRID phía frontend (?s=256).
    const GRID_S: u32 = 256;
    const OTHER_JOBS: &[&str] = &[
        "scan",
        "meta",
        "hash",
        "org_hash",
        "organize",
        "org_undo",
        "recovery",
        "dedup_delete",
        "similar_delete",
        "phash",
    ];
    let _ = events.send(JobEvent::Progress(JobProgress {
        job_id,
        kind: "thumb_warm".into(),
        done: 0,
        total: Some(total.max(1)),
        message: Some("warm".into()),
    }));
    let mut done = 0u64;
    let mut made = 0u64;
    let mut throttle = Throttle::new(500);
    let mut cursor = 0i64;
    // Đang nhường đường hay không — chỉ bắn event lúc trạng thái ĐỔI.
    let mut paused = false;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(made);
        }
        let ids = db
            .pool
            .with(|c| core_db::ops::select_present_ids(c, cursor, 256))?;
        let Some(&last) = ids.last() else { break };
        cursor = last;
        for id in ids {
            // User bấm ⏸ → ngủ hẳn, không đụng đĩa cho tới khi bấm ▶
            if !crate::dedup::hold_if_paused(
                pause,
                cancel,
                job_id,
                "thumb_warm",
                done,
                Some(total.max(done)),
                Some("warm"),
                events,
            ) {
                return Ok(made);
            }
            // ƯU TIÊN THẤP NHẤT: còn thumb interactive (kể cả backlog) hay
            // job khác đang chạy → ngủ, tuyệt đối không đụng đĩa.
            loop {
                if cancel.load(Ordering::Relaxed) {
                    return Ok(made);
                }
                let other_active = OTHER_JOBS
                    .iter()
                    .any(|k| jobs.active_job_of_kind(k).is_some());
                if thumb_pool.pending() == 0 && !other_active {
                    break;
                }
                // Đứng im hàng giờ mà UI không nói gì thì nhìn y như treo —
                // báo trạng thái ĐÚNG LÚC ĐỔI (không spam mỗi 250ms).
                if !paused {
                    paused = true;
                    let _ = events.send(JobEvent::Progress(JobProgress {
                        job_id,
                        kind: "thumb_warm".into(),
                        done,
                        total: Some(total.max(done)),
                        message: Some("paused".into()),
                    }));
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            if paused {
                paused = false;
                let _ = events.send(JobEvent::Progress(JobProgress {
                    job_id,
                    kind: "thumb_warm".into(),
                    done,
                    total: Some(total.max(done)),
                    message: Some("warm".into()),
                }));
            }
            done += 1;
            let Some(src) = db.pool.with(|c| core_db::ops::get_media_src(c, id))? else {
                continue;
            };
            if src.status != 0 || thumbs.has(id, GRID_S, src.mtime) {
                continue;
            }
            // Guard như protocol: placeholder cấm đọc, snapshot phải khớp đĩa
            let md = match std::fs::metadata(&src.path) {
                Ok(md) if !core_media::is_cloud_placeholder(&md) => md,
                _ => continue,
            };
            if md.len() as i64 != src.size || crate::dedup::unix_ms(md.modified().ok()) != src.mtime
            {
                continue;
            }
            let path = std::path::Path::new(&src.path);
            let ext = src.ext.as_deref().unwrap_or("");
            // Decoder panic với file hỏng không được giết job — bỏ qua file đó
            let generated = std::panic::catch_unwind(AssertUnwindSafe(|| {
                if src.kind == 1 {
                    match ffmpeg {
                        Some(ff) => core_media::make_video_thumb(ff, path, GRID_S, src.duration_ms),
                        None => anyhow::bail!("no ffmpeg"),
                    }
                } else {
                    core_media::make_thumb(path, ext, GRID_S, ffmpeg)
                }
            }));
            if let Ok(Ok(data)) = generated {
                if thumbs.put(id, GRID_S, src.mtime, &data).is_ok() {
                    made += 1;
                }
            }
            if throttle.ready() {
                let _ = events.send(JobEvent::Progress(JobProgress {
                    job_id,
                    kind: "thumb_warm".into(),
                    done,
                    total: Some(total.max(done)),
                    message: Some("warm".into()),
                }));
            }
        }
    }
    Ok(made)
}

/// Meta job: trích dimensions + EXIF cho mọi ảnh chưa có meta. Idempotent —
/// gọi lúc nào cũng được (sau scan, lúc mở app): đang chạy → trả job id đang
/// chạy; không còn gì để làm → None (không tạo job row rác).
#[tauri::command]
pub async fn start_meta_scan(state: State<'_, AppState>) -> CmdResult<Option<i64>> {
    if state.recovery_active.load(Ordering::Acquire) {
        return Err("ERR_RECOVERY_BUSY|".into());
    }
    // Gate atomic: check-active → count → insert → register kéo dài hàng trăm
    // ms; không có gate thì 2 lời gọi sát nhau tạo 2 job cày trùng cả thư viện.
    let gate = state.meta_start_gate.clone();
    if gate
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Ok(state.jobs.active_job_of_kind("meta"));
    }
    let guard = GateGuard(gate);
    if let Some(id) = state.jobs.active_job_of_kind("meta") {
        return Ok(Some(id)); // guard drop → gate hạ
    }
    let db = state.db.clone();
    let jobs = state.jobs.clone();
    let thumb_pool = state.thumb_pool.clone();
    let ctx = MetaCtx {
        ffprobe: state.ffprobe.clone(),
    };
    blocking(move || {
        // Giữ gate tới hết đoạn khởi động (sau register job đã hiện trong
        // active map thì caller khác tự thấy) — drop ở mọi đường ra.
        let _gate = guard;
        let include_video = ctx.ffprobe.is_some();
        let total = db
            .pool
            .with(|c| core_db::ops::count_pending_meta(c, include_video))
            .map_err(err)?;
        if total == 0 {
            return Ok(None);
        }
        let job_id = db
            .writer
            .exec(|c| core_db::ops::insert_job(c, "meta", None))
            .map_err(err)?;
        let (cancel, pause) = jobs.register_pausable(job_id, "meta", None);
        let events = jobs.sender();
        let writer_cleanup = db.writer.clone();

        std::thread::Builder::new()
            .name(format!("meta-{job_id}"))
            .spawn(move || {
                let events_progress = events.clone();
                let cancel_run = cancel.clone();
                let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    run_meta_job(
                        &db,
                        &ctx,
                        &thumb_pool,
                        &cancel_run,
                        &pause,
                        job_id,
                        total,
                        &events_progress,
                    )
                }));
                let final_event = match result {
                    Err(_) => JobEvent::Failed {
                        job_id,
                        kind: "meta".into(),
                        error: "ERR_INTERNAL|meta thread panicked".into(),
                    },
                    Ok(Ok(_)) if cancel.load(Ordering::Relaxed) => JobEvent::Cancelled {
                        job_id,
                        kind: "meta".into(),
                    },
                    Ok(Ok(done)) => {
                        // "+N (M left)": N của RIÊNG lượt này (job incremental,
                        // restart là chia lượt) + M còn thiếu toàn kho — tránh
                        // hiểu nhầm "meta 13238" là tổng đã trích của cả thư viện.
                        let left = db
                            .pool
                            .with(|c| core_db::ops::count_pending_meta(c, ctx.ffprobe.is_some()));
                        JobEvent::Done {
                            job_id,
                            kind: "meta".into(),
                            message: Some(match left {
                                Ok(left) => format!("meta +{done} ({left} left)"),
                                Err(_) => format!("meta +{done}"),
                            }),
                        }
                    }
                    Ok(Err(e)) => JobEvent::Failed {
                        job_id,
                        kind: "meta".into(),
                        error: format!("{e:#}"),
                    },
                };
                let _ = events.send(final_event);
            })
            .map_err(|e| {
                // Spawn fail mà bỏ mặc: job row kẹt 'running' + active map giữ
                // id ma → không bao giờ chạy meta được nữa tới khi restart.
                jobs.unregister(job_id);
                writer_cleanup.exec_async(move |c| {
                    core_db::ops::finish_job(c, job_id, "failed", Some("ERR_INTERNAL|spawn failed"))
                });
                format!("ERR_INTERNAL|spawn: {e}")
            })?;
        Ok(Some(job_id))
    })
    .await
}

/// Context meta job: tz để đổi creation_time UTC → wall-clock; ffprobe cho video.
struct MetaCtx {
    ffprobe: Option<std::path::PathBuf>,
}

#[allow(clippy::too_many_arguments)] // private, 1 call site — context thật của job
fn run_meta_job(
    db: &core_db::Db,
    ctx: &MetaCtx,
    thumb_pool: &LifoPool,
    cancel: &core_jobs::CancelFlag,
    pause: &core_jobs::PauseFlag,
    job_id: i64,
    total: i64,
    events: &crossbeam_channel::Sender<JobEvent>,
) -> anyhow::Result<u64> {
    let include_video = ctx.ffprobe.is_some();
    // tz đọc 1 lần mỗi job — video creation_time là UTC, cần đổi ra wall-clock
    let (timezone, tz_offset_min): (Option<String>, i64) = db.pool.with(|c| {
        let timezone = core_db::ops::kv_get(c, "timezone")?;
        let offset = core_db::ops::kv_get(c, "tz_offset_minutes")?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        Ok::<_, anyhow::Error>((timezone, offset))
    })?;
    let mut done: u64 = 0;
    let mut cursor: i64 = 0;
    let mut throttle = Throttle::new(200);
    // UI thấy job ngay khi khởi động, kể cả khi batch đầu đang yield cho thumb
    let _ = events.send(JobEvent::Progress(JobProgress {
        job_id,
        kind: "meta".into(),
        done: 0,
        total: Some(total.max(1) as u64),
        message: None,
    }));
    loop {
        if !crate::dedup::hold_if_paused(
            pause,
            cancel,
            job_id,
            "meta",
            done,
            Some(total.max(done as i64) as u64),
            None,
            events,
        ) {
            return Ok(done);
        }
        // Video batch nhỏ hơn: mỗi file là 1 process ffprobe (~50-100ms)
        let batch = db
            .pool
            .with(|c| core_db::ops::select_pending_meta(c, cursor, 128, include_video))?;
        let Some(last) = batch.last() else {
            return Ok(done);
        };
        cursor = last.file_id;
        // Ảnh: header-only read; video: ffprobe — song song trên global rayon pool.
        // None = file không đọc được (ổ rút...) → không ghi row, job sau thử lại.
        // Chia chunk nhỏ + NHƯỜNG ĐĨA: kho trên HDD mà meta cày EXIF song song
        // là thumb user đang nhìn chết đói I/O (đen xì hàng phút). Có thumb
        // đang chờ → meta đứng lại tới khi pool rảnh; SSD gần như không bao
        // giờ phải chờ vì thumb xong trong vài ms.
        let mut metas: Vec<MetaUpsert> = Vec::with_capacity(batch.len());
        for chunk in batch.chunks(16) {
            crate::dedup::yield_to_thumbs(thumb_pool, cancel);
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            metas.par_extend(
                chunk
                    .par_iter()
                    .filter_map(|p| extract_one_meta(p, ctx, timezone.as_deref(), tz_offset_min)),
            );
        }
        let n = batch.len() as u64;
        if !metas.is_empty() {
            db.writer
                .exec(move |c| core_db::ops::upsert_meta_batch(c, &metas))?;
        }
        done += n;
        if throttle.ready() {
            let _ = events.send(JobEvent::Progress(JobProgress {
                job_id,
                kind: "meta".into(),
                done,
                // Scan chạy song song có thể thêm file → done vượt total ban đầu
                total: Some(total.max(done as i64) as u64),
                message: None,
            }));
        }
    }
}

/// None = file không truy cập được (ổ offline, bị lock) HOẶC là cloud
/// placeholder (status DB stale — đọc là hydrate, cấm tuyệt đối) — KHÔNG ghi
/// row để job sau retry khi file quay lại. Truy cập được thì luôn ra row
/// (decode fail → meta_state=2, không bao giờ chọn lại file hỏng thật).
fn extract_one_meta(
    p: &PendingMeta,
    ctx: &MetaCtx,
    timezone: Option<&str>,
    tz_offset_min: i64,
) -> Option<MetaUpsert> {
    let path = Path::new(&p.path);
    match std::fs::metadata(path) {
        Ok(md) if !core_media::is_cloud_placeholder(&md) => {}
        _ => return None,
    }
    if p.kind == 1 {
        // select_pending_meta chỉ trả video khi có ffprobe
        let ff = ctx.ffprobe.as_deref()?;
        let m = std::panic::catch_unwind(AssertUnwindSafe(|| {
            core_media::probe_video_in_timezone(ff, path, timezone, tz_offset_min)
        }))
        .unwrap_or_default();
        return Some(MetaUpsert {
            file_id: p.file_id,
            width: m.width.map(i64::from),
            height: m.height.map(i64::from),
            taken_at: m.taken_at,
            date_source: m.date_source,
            duration_ms: m.duration_ms,
            vcodec: m.vcodec,
            acodec: m.acodec,
            bitrate: m.bitrate,
            fps: m.fps,
            gps_lat: m.gps_lat,
            gps_lon: m.gps_lon,
            meta_state: if m.ok { 1 } else { 2 },
            src_mtime: p.mtime,
            src_size: p.size,
            ..Default::default()
        });
    }
    let m = std::panic::catch_unwind(|| core_media::extract_image_meta(path)).unwrap_or_default();
    Some(MetaUpsert {
        file_id: p.file_id,
        width: m.width.map(i64::from),
        height: m.height.map(i64::from),
        taken_at: m.taken_at,
        date_source: m.date_source,
        camera: m.camera,
        orientation: m.orientation.map(i64::from),
        gps_lat: m.gps_lat,
        gps_lon: m.gps_lon,
        meta_state: if m.ok { 1 } else { 2 },
        src_mtime: p.mtime,
        src_size: p.size,
        ..Default::default()
    })
}

/// Tên nơi chụp để ĐỌC, ghép từ chi tiết nhất tới rộng nhất và GIỮ NGUYÊN DẤU
/// ("Phường Lý Thái Tổ, Quận Hoàn Kiếm, Hà Nội, Vietnam"). Khác hẳn tên dùng
/// đặt thư mục — chỗ đó bỏ dấu, xem `core_geo::fold_ascii`.
///
/// Bỏ tầng trùng tên nhau: Hà Nội là thành phố trực thuộc trung ương nên
/// `city` và `province` bằng nhau, in cả hai thì thành "Hà Nội, Hà Nội".
fn place_label(lat: Option<f64>, lon: Option<f64>) -> Option<String> {
    let (lat, lon) = (lat?, lon?);
    let p = core_geo::lookup(lat, lon);
    let mut parts: Vec<&str> = Vec::new();
    for name in [p.ward, p.district, p.city, p.province, p.country]
        .into_iter()
        .flatten()
    {
        if !parts.contains(&name) {
            parts.push(name);
        }
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// Chi tiết file cho panel info lightbox. Meta chưa có (job chưa chạy tới) mà
/// là ảnh present → trích ngay tại chỗ (header-only, vài ms) + persist async.
#[tauri::command]
pub async fn get_file_meta(
    state: State<'_, AppState>,
    file_id: i64,
) -> CmdResult<Option<FileDetail>> {
    let db = state.db.clone();
    blocking(move || {
        let mut detail = db
            .pool
            .with(|c| core_db::ops::get_file_detail(c, file_id))
            .map_err(err)?;
        if let Some(d) = detail.as_mut() {
            let readable = std::fs::metadata(core_db::ops::join_path(&d.dir, &d.name))
                .map(|md| !core_media::is_cloud_placeholder(&md))
                .unwrap_or(false);
            if d.meta_state.is_none() && d.kind == 0 && d.status == 0 && readable {
                let path = core_db::ops::join_path(&d.dir, &d.name);
                let m = core_media::extract_image_meta(Path::new(&path));
                d.width = m.width.map(i64::from);
                d.height = m.height.map(i64::from);
                d.taken_at = m.taken_at;
                d.camera = m.camera.clone();
                d.orientation = m.orientation.map(i64::from);
                d.gps_lat = m.gps_lat;
                d.gps_lon = m.gps_lon;
                d.meta_state = Some(if m.ok { 1 } else { 2 });
                let row = MetaUpsert {
                    file_id: d.id,
                    width: d.width,
                    height: d.height,
                    taken_at: d.taken_at,
                    date_source: m.date_source,
                    camera: d.camera.clone(),
                    orientation: d.orientation,
                    gps_lat: d.gps_lat,
                    gps_lon: d.gps_lon,
                    meta_state: if m.ok { 1 } else { 2 },
                    src_mtime: d.mtime,
                    src_size: d.size,
                    ..Default::default()
                };
                db.writer
                    .exec_async(move |c| core_db::ops::upsert_meta_batch(c, &[row]));
            }
            d.place = place_label(d.gps_lat, d.gps_lon);
        }
        Ok(detail)
    })
    .await
}

/// Mở file bằng app mặc định của hệ thống (video codec lạ WebView2 không phát).
/// Đi qua explorer.exe (ShellExecute) — KHÔNG qua cmd, khỏi dính shell parse
/// với tên file chứa &, %, khoảng trắng.
#[tauri::command]
pub async fn open_file(state: State<'_, AppState>, file_id: i64) -> CmdResult<()> {
    let db = state.db.clone();
    blocking(move || {
        let path = resolve_present_path(&db, file_id)?;
        // raw_arg tự bọc quote: std chỉ quote khi có space, mà explorer.exe
        // parse dấu phẩy làm separator → path "D:\Family,2019\..." sẽ gãy.
        let mut cmd = std::process::Command::new("explorer.exe");
        cmd.raw_arg(format!("\"{path}\""));
        cmd.spawn().map_err(|e| format!("ERR_INTERNAL|open: {e}"))?;
        Ok(())
    })
    .await
}

/// Mở Explorer trỏ thẳng vào file.
#[tauri::command]
pub async fn reveal_file(state: State<'_, AppState>, file_id: i64) -> CmdResult<()> {
    let db = state.db.clone();
    blocking(move || {
        let path = resolve_present_path(&db, file_id)?;
        // explorer /select, không nhận path qua arg riêng — ghép 1 chuỗi
        let mut cmd = std::process::Command::new("explorer.exe");
        cmd.raw_arg(format!("/select,\"{path}\""));
        cmd.spawn()
            .map_err(|e| format!("ERR_INTERNAL|explorer: {e}"))?;
        Ok(())
    })
    .await
}

fn resolve_present_path(db: &core_db::Db, file_id: i64) -> CmdResult<String> {
    let src = db
        .pool
        .with(|c| core_db::ops::get_media_src(c, file_id))
        .map_err(err)?
        .ok_or("ERR_FILE_GONE|")?;
    if src.status == 1 || src.status == 3 {
        return Err(format!("ERR_FILE_GONE|{}", src.path));
    }
    // Invariant: check attrs TẠI CHỖ — status DB có thể stale, mở file
    // placeholder bằng app ngoài cũng là kéo hydrate.
    match std::fs::metadata(&src.path) {
        Err(_) => return Err(format!("ERR_FILE_GONE|{}", src.path)),
        Ok(md) if core_media::is_cloud_placeholder(&md) => {
            return Err(format!("ERR_FILE_CLOUD|{}", src.path));
        }
        Ok(_) => {}
    }
    Ok(src.path)
}

#[tauri::command]
pub async fn cancel_job(state: State<'_, AppState>, job_id: i64) -> CmdResult<bool> {
    Ok(state.jobs.cancel(job_id))
}

/// Tạm dừng / chạy tiếp job nền. `false` = job không còn hoặc kind không được
/// phép dừng (job đụng file thật ôm khóa, xem `core_jobs::PAUSABLE_KINDS`) —
/// UI chỉ cần bỏ qua, không phải lỗi.
#[tauri::command]
pub async fn pause_job(state: State<'_, AppState>, job_id: i64, paused: bool) -> CmdResult<bool> {
    Ok(state.jobs.set_paused(job_id, paused))
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

#[tauri::command]
pub fn list_active_jobs(state: State<'_, AppState>) -> Vec<JobProgress> {
    state.jobs.active_jobs()
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
    pub timezone: Option<String>,
}

fn read_settings(db: &core_db::Db) -> anyhow::Result<Settings> {
    db.pool.with(|c| {
        let stored_setup_done = core_db::ops::kv_get(c, "setup_done")?
            .map(|v| v == "1")
            .unwrap_or(false);
        let tz_offset_minutes =
            core_db::ops::kv_get(c, "tz_offset_minutes")?.and_then(|v| v.parse().ok());
        let timezone = core_db::ops::kv_get(c, "timezone")?;
        // Fixed offset cũ không xác định duy nhất được IANA zone/DST.
        // Buộc xác nhận wizard một lần thay vì âm thầm đoán sai.
        let setup_done = stored_setup_done && timezone.is_some();
        Ok(Settings {
            setup_done,
            tz_offset_minutes,
            timezone,
        })
    })
}

#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> CmdResult<Settings> {
    let db = state.db.clone();
    blocking(move || read_settings(&db).map_err(err)).await
}

#[tauri::command]
pub async fn get_excluded_paths(state: State<'_, AppState>) -> CmdResult<Vec<String>> {
    let db = state.db.clone();
    blocking(move || db.pool.with(core_db::ops::get_excluded_paths).map_err(err)).await
}

#[tauri::command]
pub async fn set_excluded_paths(state: State<'_, AppState>, paths: Vec<String>) -> CmdResult<()> {
    if state.recovery_active.load(Ordering::Acquire) {
        return Err("ERR_RECOVERY_BUSY|".into());
    }
    crate::organize::invalidate_org_preview(&state);
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
    timezone: String,
    tz_offset_minutes: i32,
    setup_done: bool,
) -> CmdResult<()> {
    if state.recovery_active.load(Ordering::Acquire) {
        return Err("ERR_RECOVERY_BUSY|".into());
    }
    let db = state.db.clone();
    let jobs = state.jobs.clone();
    let gate = state.meta_start_gate.clone();
    let preview = state.org_preview.clone();
    let preview_cancel = state.org_preview_cancel.clone();
    let result =
        blocking(move || apply_settings(&db, &jobs, gate, timezone, tz_offset_minutes, setup_done))
            .await;
    if result.is_ok() {
        if let Some(cancel) = preview_cancel
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            cancel.store(true, Ordering::Relaxed);
        }
        *preview.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
    result
}

fn apply_settings(
    db: &core_db::Db,
    jobs: &core_jobs::JobManager,
    gate: std::sync::Arc<std::sync::atomic::AtomicBool>,
    timezone: String,
    tz_offset_minutes: i32,
    setup_done: bool,
) -> CmdResult<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    if core_ingest::date::timezone_offset_minutes(&timezone, now).is_none() {
        return Err(format!("ERR_TZ_INVALID|{timezone}"));
    }

    // Serialize với start_meta_scan. Job dùng zone cũ phải dừng hoàn toàn
    // TRƯỚC khi xóa rows, nếu không batch đang ffprobe có thể ghi dữ liệu
    // cũ trở lại ngay sau DELETE.
    let gate_deadline = Instant::now() + Duration::from_secs(30);
    while gate
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        if Instant::now() > gate_deadline {
            return Err("ERR_META_STOP_TIMEOUT|meta start gate busy".into());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let _gate = GateGuard(gate);

    let old_timezone = db
        .pool
        .with(|c| core_db::ops::kv_get(c, "timezone"))
        .map_err(err)?;
    let timezone_changed = old_timezone.as_deref() != Some(timezone.as_str());
    if timezone_changed {
        if let Some(id) = jobs.active_job_of_kind("meta") {
            jobs.cancel(id);
            // Acquiring the start gate and stopping a running worker are two distinct
            // waits. Reusing the first deadline could cancel a healthy job and then time
            // out immediately, leaving the requested setting unapplied.
            let stop_deadline = Instant::now() + Duration::from_secs(30);
            while jobs.active_job_of_kind("meta").is_some() {
                if Instant::now() > stop_deadline {
                    return Err("ERR_META_STOP_TIMEOUT|meta job did not stop".into());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }

    db.writer
        .exec(move |c| {
            let new_offset = tz_offset_minutes.to_string();
            core_db::ops::kv_set(c, "tz_offset_minutes", &new_offset)?;
            core_db::ops::kv_set(c, "timezone", &timezone)?;
            core_db::ops::kv_set(c, "setup_done", if setup_done { "1" } else { "0" })?;
            if timezone_changed {
                // creation_time UTC của video đã được đổi thành wall-clock
                // theo zone cũ; xóa để meta job probe lại với DST-aware zone.
                c.execute(
                    "DELETE FROM media_meta WHERE file_id IN
                           (SELECT id FROM files WHERE kind = 1)",
                    [],
                )?;
            }
            Ok(())
        })
        .map_err(err)
}

#[cfg(test)]
mod settings_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[test]
    fn legacy_fixed_offset_requires_iana_confirmation() {
        let tmp = tempfile::tempdir().unwrap();
        let db = core_db::Db::open(tmp.path()).unwrap();
        db.writer
            .exec(|c| {
                core_db::ops::kv_set(c, "setup_done", "1")?;
                core_db::ops::kv_set(c, "tz_offset_minutes", "420")
            })
            .unwrap();
        let old = read_settings(&db).unwrap();
        assert!(!old.setup_done);
        assert!(old.timezone.is_none());

        db.writer
            .exec(|c| core_db::ops::kv_set(c, "timezone", "Asia/Ho_Chi_Minh"))
            .unwrap();
        assert!(read_settings(&db).unwrap().setup_done);
    }

    #[test]
    fn timezone_change_waits_for_old_meta_job_to_stop() {
        let tmp = tempfile::tempdir().unwrap();
        let db = core_db::Db::open(tmp.path()).unwrap();
        db.writer
            .exec(|c| core_db::ops::kv_set(c, "timezone", "Etc/UTC"))
            .unwrap();
        let jobs = Arc::new(core_jobs::JobManager::new());
        let cancel = jobs.register(7, "meta", None);
        let jobs_worker = jobs.clone();
        let worker = std::thread::spawn(move || {
            while !cancel.load(Ordering::Relaxed) {
                std::thread::yield_now();
            }
            jobs_worker.unregister(7);
        });

        apply_settings(
            &db,
            &jobs,
            Arc::new(AtomicBool::new(false)),
            "America/New_York".into(),
            -240,
            true,
        )
        .unwrap();
        worker.join().unwrap();

        assert!(jobs.active_job_of_kind("meta").is_none());
        assert_eq!(
            db.pool
                .with(|c| core_db::ops::kv_get(c, "timezone"))
                .unwrap()
                .as_deref(),
            Some("America/New_York")
        );
    }
}
