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

use crate::state::AppState;

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

/// Meta job: trích dimensions + EXIF cho mọi ảnh chưa có meta. Idempotent —
/// gọi lúc nào cũng được (sau scan, lúc mở app): đang chạy → trả job id đang
/// chạy; không còn gì để làm → None (không tạo job row rác).
#[tauri::command]
pub async fn start_meta_scan(state: State<'_, AppState>) -> CmdResult<Option<i64>> {
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
        let cancel = jobs.register(job_id, "meta", None);
        let events = jobs.sender();
        let writer_cleanup = db.writer.clone();

        std::thread::Builder::new()
            .name(format!("meta-{job_id}"))
            .spawn(move || {
                let events_progress = events.clone();
                let cancel_run = cancel.clone();
                let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    run_meta_job(&db, &ctx, &cancel_run, job_id, total, &events_progress)
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
                    Ok(Ok(done)) => JobEvent::Done {
                        job_id,
                        kind: "meta".into(),
                        message: Some(format!("meta {done}")),
                    },
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

fn run_meta_job(
    db: &core_db::Db,
    ctx: &MetaCtx,
    cancel: &core_jobs::CancelFlag,
    job_id: i64,
    total: i64,
    events: &crossbeam_channel::Sender<JobEvent>,
) -> anyhow::Result<u64> {
    let include_video = ctx.ffprobe.is_some();
    // tz đọc 1 lần mỗi job — video creation_time là UTC, cần đổi ra wall-clock
    let tz_offset_min: i64 = db
        .pool
        .with(|c| core_db::ops::kv_get(c, "tz_offset_minutes"))?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut done: u64 = 0;
    let mut cursor: i64 = 0;
    let mut throttle = Throttle::new(200);
    loop {
        if cancel.load(Ordering::Relaxed) {
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
        let metas: Vec<MetaUpsert> = batch
            .par_iter()
            .filter_map(|p| extract_one_meta(p, ctx, tz_offset_min))
            .collect();
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
fn extract_one_meta(p: &PendingMeta, ctx: &MetaCtx, tz_offset_min: i64) -> Option<MetaUpsert> {
    let path = Path::new(&p.path);
    match std::fs::metadata(path) {
        Ok(md) if !core_media::is_cloud_placeholder(&md) => {}
        _ => return None,
    }
    if p.kind == 1 {
        // select_pending_meta chỉ trả video khi có ffprobe
        let ff = ctx.ffprobe.as_deref()?;
        let m = std::panic::catch_unwind(AssertUnwindSafe(|| {
            core_media::probe_video(ff, path, tz_offset_min)
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
        meta_state: if m.ok { 1 } else { 2 },
        src_mtime: p.mtime,
        src_size: p.size,
        ..Default::default()
    })
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
                d.meta_state = Some(if m.ok { 1 } else { 2 });
                let row = MetaUpsert {
                    file_id: d.id,
                    width: d.width,
                    height: d.height,
                    taken_at: d.taken_at,
                    date_source: m.date_source,
                    camera: d.camera.clone(),
                    orientation: d.orientation,
                    meta_state: if m.ok { 1 } else { 2 },
                    src_mtime: d.mtime,
                    src_size: d.size,
                    ..Default::default()
                };
                db.writer
                    .exec_async(move |c| core_db::ops::upsert_meta_batch(c, &[row]));
            }
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
