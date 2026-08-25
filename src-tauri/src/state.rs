use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
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

/// Phân loại job trong pool — `clear()` xả THUMB khi rời grid mà không đụng
/// MEDIA (video đang phát request Range liên tục, xả là khựng).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PoolTag {
    Thumb,
    Media,
}

type LifoJob = (PoolTag, Box<dyn FnOnce() + Send + 'static>);

/// Pool LIFO cho thumb/media: job MỚI NHẤT chạy trước. Cuộn nhanh qua nghìn
/// cell là nghìn request vào hàng; WebView hủy request của cell đã cuộn qua
/// nhưng task Rust vẫn chạy hết — FIFO bắt cell ĐANG hiển thị xếp sau toàn bộ
/// backlog (mỗi MOV là một lần spawn ffmpeg), kể cả cache hit cũng phải chờ.
/// LIFO đảo lại: màn hình hiện tại luôn ưu tiên, backlog cũ chạy lúc rảnh và
/// vẫn làm ấm cache.
pub struct LifoPool {
    shared: Arc<(Mutex<Vec<LifoJob>>, Condvar)>,
    running: Arc<std::sync::atomic::AtomicUsize>,
    /// epoch ms của lần spawn gần nhất — phân biệt nhu cầu TƯƠI (user đang
    /// cuộn/nhìn) với backlog cũ đang xả nốt.
    last_spawn_ms: Arc<std::sync::atomic::AtomicU64>,
}

fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl LifoPool {
    pub fn new(threads: usize, name: &str) -> Self {
        let shared: Arc<(Mutex<Vec<LifoJob>>, Condvar)> =
            Arc::new((Mutex::new(Vec::new()), Condvar::new()));
        let running = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let last_spawn_ms = Arc::new(std::sync::atomic::AtomicU64::new(0));
        for i in 0..threads.max(1) {
            let shared = shared.clone();
            let running = running.clone();
            std::thread::Builder::new()
                .name(format!("{name}-{i}"))
                .spawn(move || loop {
                    let job = {
                        let (queue, ready) = &*shared;
                        let mut q = queue.lock().unwrap_or_else(|p| p.into_inner());
                        loop {
                            if let Some((_, job)) = q.pop() {
                                // Tăng NGAY trong lock: không có khoảnh khắc
                                // queue rỗng + running=0 khi job sắp chạy.
                                running.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                break job;
                            }
                            q = ready.wait(q).unwrap_or_else(|p| p.into_inner());
                        }
                    };
                    // Handler đã catch_unwind; lưới thứ 2 này giữ worker sống.
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).is_err() {
                        tracing::error!("thumb pool task panicked (caught)");
                    }
                    running.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                })
                .expect("spawn thumb pool worker");
        }
        Self {
            shared,
            running,
            last_spawn_ms,
        }
    }

    pub fn spawn(&self, tag: PoolTag, job: impl FnOnce() + Send + 'static) {
        self.last_spawn_ms
            .store(epoch_ms(), std::sync::atomic::Ordering::Relaxed);
        let (queue, ready) = &*self.shared;
        queue
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push((tag, Box::new(job)));
        ready.notify_one();
    }

    /// Xả các job CHƯA chạy thuộc `tag` — gọi khi rời chế độ grid/đổi filter:
    /// request tương ứng đã bị webview hủy từ lâu, giữ lại chỉ tổ giành I/O
    /// với job nền. Trả số job đã bỏ.
    pub fn clear(&self, tag: PoolTag) -> usize {
        let (queue, _) = &*self.shared;
        let mut q = queue.lock().unwrap_or_else(|p| p.into_inner());
        let before = q.len();
        q.retain(|(t, _)| *t != tag);
        before - q.len()
    }

    /// Số job đang xếp hàng + đang chạy.
    pub fn pending(&self) -> usize {
        let (queue, _) = &*self.shared;
        queue.lock().unwrap_or_else(|p| p.into_inner()).len()
            + self.running.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Nhu cầu thumb TƯƠI: còn việc VÀ có request mới trong `window` gần đây.
    /// Job nền (meta/hash) chỉ nhường đĩa khi user đang thực sự cuộn/nhìn —
    /// backlog cũ (cell đã cuộn qua từ lâu) xả nốt ở nền, KHÔNG được phép
    /// treo vô hạn một job user chủ động bấm.
    pub fn active_within(&self, window: std::time::Duration) -> bool {
        if self.pending() == 0 {
            return false;
        }
        let last = self
            .last_spawn_ms
            .load(std::sync::atomic::Ordering::Relaxed);
        epoch_ms().saturating_sub(last) < window.as_millis() as u64
    }
}

pub struct AppState {
    /// Thư mục chứa index.db + thumbs.db. Export/Import cần để (a) không cho
    /// xuất đè lên chính nó, (b) dàn file nhập vào đúng chỗ.
    pub data_dir: std::path::PathBuf,
    pub db: Arc<Db>,
    pub jobs: Arc<JobManager>,
    pub queries: Arc<Mutex<QueryCache>>,
    /// Cache thumbnail WebP (thumbs.db, LRU 2GB) — mất là re-generate, không quý.
    pub thumbs: Arc<core_media::ThumbStore>,
    /// Pool riêng cho decode thumb + đọc media (LIFO, xem [`LifoPool`]) —
    /// không bao giờ chiếm thread webview hay tokio worker.
    pub thumb_pool: Arc<LifoPool>,
    /// ffmpeg cho HEIC/AVIF/JXL + keyframe video. None = hiện icon thay thumb.
    pub ffmpeg: Option<std::path::PathBuf>,
    /// ffprobe cho metadata video. None = video không có meta/duration (chờ tool).
    pub ffprobe: Option<std::path::PathBuf>,
    /// Gate serialize đoạn khởi động meta job (check-active → insert → register
    /// không atomic; StrictMode mount 2 lần / 2 scan xong sát nhau sẽ đua).
    pub meta_start_gate: Arc<std::sync::atomic::AtomicBool>,
    /// Gate tương tự cho hash job (M4 dedup).
    pub hash_start_gate: Arc<std::sync::atomic::AtomicBool>,
    /// Gate cho job warm thumbnail nền (ưu tiên thấp nhất).
    pub thumb_warm_gate: Arc<std::sync::atomic::AtomicBool>,
    /// Chống 2 job phash khởi động song song (check-rồi-register có khe đua).
    pub phash_start_gate: Arc<std::sync::atomic::AtomicBool>,
    /// Gate tương tự cho organize/undo job (M5).
    pub org_start_gate: Arc<std::sync::atomic::AtomicBool>,
    /// Serialize các bước chuyển trạng thái giữa scan, watched-root mutation
    /// và organize/undo. Chỉ giữ lúc check/register hoặc lúc xóa root, không
    /// giữ suốt scan/organize job.
    pub index_op_gate: Arc<Mutex<()>>,
    /// Dry-run organize gần nhất, gồm chính plan sẽ được execute. Không query/
    /// re-plan sau khi user confirm nên phạm vi consent không thể tự nở ra.
    pub(crate) org_preview: Arc<Mutex<Option<crate::organize::OrgPreviewTicket>>>,
    /// Preview có thể full-hash thư viện lớn; cho UI hủy giữa từng file.
    pub(crate) org_preview_cancel: Arc<Mutex<Option<core_jobs::CancelFlag>>>,
    pub org_preview_seq: Arc<std::sync::atomic::AtomicU64>,
    /// Số đời của CẤU HÌNH organize (template, phạm vi nguồn, thư mục kho,
    /// timezone, và cả meta lazy-extract — mọi thứ preview đọc để tính đích).
    ///
    /// Mỗi lệnh đổi cấu hình tăng nó lên NGAY khi vào lệnh — trước mọi `.await`
    /// — và ticket preview nhớ số đời lúc nó được tính. Vứt preview sau khi lưu
    /// xong là chưa đủ: giữa lúc user bấm đổi phạm vi và lúc lệnh đó ghi xong
    /// kv có một khe, và trong khe đó ticket cũ vẫn hợp lệ nên `start_organize`
    /// lấy được nguyên cái plan của phạm vi CŨ rồi chạy — vứt preview sau đó
    /// không gọi lại được plan đã chạy. So số đời thì khe biến mất: ticket tính
    /// theo cấu hình cũ không bao giờ khớp số đời hiện tại nữa.
    pub org_config_gen: Arc<std::sync::atomic::AtomicU64>,
    /// Số lệnh đổi cấu hình ĐANG MỞ (đã vào lệnh, chưa ghi xong). Số đời một
    /// mình không đóng được khe này: setter bump ở entry rồi ghi kv mất tới
    /// hàng chục giây (`set_settings` chờ meta gate); một preview BẮT ĐẦU sau
    /// bump đó chụp được số đời MỚI nhưng đọc cấu hình CŨ/ghi dở, và ticket
    /// của nó hợp lệ cho tới lần bump đóng — user bấm Gom trong cửa sổ đó là
    /// chạy plan sai. Ticket không được cài lẫn lấy khi biến này khác 0.
    /// Tăng/giảm dưới cùng mutex `org_preview` với số đời (xem
    /// `config_write_begin`/`config_write_end`).
    pub org_config_writers: Arc<std::sync::atomic::AtomicU64>,
    /// True while startup crash recovery runs on its background thread.
    pub recovery_active: Arc<std::sync::atomic::AtomicBool>,
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
    // TRƯỚC khi mở bất kỳ connection nào: bản nhập đã dàn sẵn từ lượt chạy
    // trước được tráo vào đây. Đổi file SQLite dưới chân connection đang mở là
    // hỏng dữ liệu, nên đây là chỗ duy nhất làm được việc này.
    std::fs::create_dir_all(&data_dir)?;
    crate::transfer::apply_staged_import(&data_dir);
    tracing::info!(dir = %data_dir.display(), "opening index db");
    let db = Arc::new(Db::open(&data_dir)?);

    // Hard process kills bypass tempfile Drop, so sweep only old files with our exact names.
    crate::organize::cleanup_stale_preview_files(&data_dir);

    let jobs = Arc::new(JobManager::new());
    let events_rx = jobs.receiver();
    let writer = db.writer.clone();

    // Tạo trước để closure bootstrap bên dưới còn hủy được preview — AppState
    // lúc đó chưa tồn tại.
    let org_preview: Arc<Mutex<Option<crate::organize::OrgPreviewTicket>>> =
        Arc::new(Mutex::new(None));
    let org_preview_cancel: Arc<Mutex<Option<core_jobs::CancelFlag>>> = Arc::new(Mutex::new(None));

    // Bootstrap 1 lần sau khi lên schema v4: ghép Live Photo cho index CÓ SẴN
    // (bình thường pairing chỉ chạy sau scan — index migrate lên không rescan
    // thì MOV cứ hiện mãi). Async, không chặn khởi động.
    writer.exec_async({
        let (preview, cancel_slot) = (org_preview.clone(), org_preview_cancel.clone());
        move |c| {
            if core_db::ops::kv_get(c, "live_pair_bootstrap")?.is_none() {
                for r in core_db::ops::list_roots(c)? {
                    core_db::ops::pair_live_photos(c, &r.path)?;
                }
                core_db::ops::kv_set(c, "live_pair_bootstrap", "1")?;
                tracing::info!("live photo pairing bootstrapped for existing index");
                // `live_pair_id` là đầu vào của organize preview (MOV đã ghép
                // bị ẩn khỏi ứng viên). Chạy async nên preview đầu tiên trên
                // index lớn có thể tính TRƯỚC khi ghép xong → plan coi MOV là
                // file độc lập, xé cặp. Hủy để lượt sau đọc index đã ghép.
                crate::organize::invalidate_preview_slots(&preview, &cancel_slot);
            }
            Ok(())
        }
    });

    let thumbs = Arc::new(core_media::ThumbStore::open(
        &data_dir.join("thumbs.db"),
        THUMB_CACHE_CAP_BYTES,
    )?);
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let thumb_pool = Arc::new(LifoPool::new((cores / 2).clamp(2, 6), "thumb"));
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

    let index_op_gate = Arc::new(Mutex::new(()));
    let delete_lock = Arc::new(std::sync::Mutex::new(()));
    let recovery_active = Arc::new(std::sync::atomic::AtomicBool::new(true));
    app.manage(AppState {
        data_dir: data_dir.clone(),
        db: db.clone(),
        jobs: jobs.clone(),
        queries: Arc::new(Mutex::new(QueryCache::new())),
        thumbs,
        thumb_pool,
        ffmpeg,
        ffprobe,
        meta_start_gate: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        hash_start_gate: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        thumb_warm_gate: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        phash_start_gate: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        org_start_gate: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        index_op_gate: index_op_gate.clone(),
        org_preview,
        org_preview_cancel,
        org_preview_seq: Arc::new(std::sync::atomic::AtomicU64::new(1)),
        org_config_gen: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        org_config_writers: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        recovery_active: recovery_active.clone(),
        delete_lock: delete_lock.clone(),
    });

    // Recovery is best-effort and never prevents the window from opening. Mutating commands
    // reject while the flag is set, so the background task cannot race scan/delete/organize.
    let recovery_db = db.clone();
    let recovery_jobs = jobs.clone();
    std::thread::Builder::new()
        .name("organize-recovery".into())
        .spawn(move || {
            struct RecoveryFlag(std::sync::Arc<std::sync::atomic::AtomicBool>);
            impl Drop for RecoveryFlag {
                fn drop(&mut self) {
                    self.0.store(false, std::sync::atomic::Ordering::Release);
                }
            }
            // Declared first so it drops last. `recovery_active` cannot become false
            // while either lock is still held (NEW-4 lock-order window).
            let _done = RecoveryFlag(recovery_active);
            let pending_count = match recovery_db.pool.with(core_db::org::pending_org_ops) {
                Ok(pending) => pending.len() as u64,
                Err(e) => {
                    tracing::error!(
                        error = %format!("{e:#}"),
                        "could not inspect pending organize recovery; app will continue"
                    );
                    return;
                }
            };
            if pending_count == 0 {
                return;
            }
            let job_id = match recovery_db
                .writer
                .exec(|c| core_db::ops::insert_job(c, "recovery", None))
            {
                Ok(id) => id,
                Err(e) => {
                    tracing::error!(
                        error = %format!("{e:#}"),
                        "could not create recovery job; app will continue"
                    );
                    return;
                }
            };
            let cancel = recovery_jobs.register(job_id, "recovery", None);
            let events = recovery_jobs.sender();
            let result = {
                // Canonical nested order everywhere that needs both locks:
                // index_op_gate -> delete_lock.
                let _index = index_op_gate.lock().unwrap_or_else(|p| p.into_inner());
                let _filesystem = delete_lock.lock().unwrap_or_else(|p| p.into_inner());
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut report = |done, total| {
                        let _ =
                            events.send(core_jobs::JobEvent::Progress(core_jobs::JobProgress {
                                job_id,
                                kind: "recovery".into(),
                                done,
                                total: Some(total),
                                message: Some("recover".into()),
                            }));
                    };
                    crate::organize::recover_pending_ops(
                        &recovery_db,
                        Some(&cancel),
                        Some(&mut report),
                    )
                }))
            };
            let final_event = match result {
                Err(_) => JobEvent::Failed {
                    job_id,
                    kind: "recovery".into(),
                    error: "ERR_INTERNAL|organize recovery panicked".into(),
                },
                Ok(Ok(())) if cancel.load(std::sync::atomic::Ordering::Relaxed) => {
                    JobEvent::Cancelled {
                        job_id,
                        kind: "recovery".into(),
                    }
                }
                Ok(Ok(())) => JobEvent::Done {
                    job_id,
                    kind: "recovery".into(),
                    message: Some(format!("recovered {pending_count} operations")),
                },
                Ok(Err(e)) => JobEvent::Failed {
                    job_id,
                    kind: "recovery".into(),
                    error: format!("{e:#}"),
                },
            };
            let _ = events.send(final_event);
        })?;

    // Event pump: JobEvent → UI (tauri events) + jobs table + index://changed.
    // index://changed khi đang scan giãn 2.5s — UI re-query cả list, không được spam.
    let handle = app.clone();
    std::thread::Builder::new()
        .name("job-event-pump".into())
        .spawn(move || {
            let mut last_changed: Option<Instant> = None;
            // Bản sao thứ ba của đoạn này từng nằm ngay đây. Ba bản chép tay
            // là ba cơ hội để một chỗ quên cập nhật — và đã xảy ra thật.
            let invalidate_preview = |handle: &AppHandle| {
                crate::organize::invalidate_org_preview(&handle.state::<AppState>());
            };
            let emit_changed = |handle: &AppHandle, last: &mut Option<Instant>| {
                invalidate_preview(handle);
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
                        invalidate_preview(&handle);
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
                    // Nhóm trùng dựng lại giữa chừng: KHÔNG đụng bảng jobs, KHÔNG
                    // invalidate org preview (không có file nào thay đổi) — chỉ báo
                    // UI nạp lại danh sách nhóm.
                    JobEvent::DupGroupsChanged {
                        kind,
                        groups,
                        waste,
                    } => {
                        let _ = handle.emit(
                            "dup://changed",
                            &serde_json::json!({ "kind": kind, "groups": groups, "waste": waste }),
                        );
                    }
                    JobEvent::Failed { job_id, kind, error } => {
                        invalidate_preview(&handle);
                        writer.exec_async({
                            let error = error.clone();
                            move |c| core_db::ops::finish_job(c, job_id, "failed", Some(&error))
                        });
                        handle.state::<AppState>().jobs.unregister(job_id);
                        let _ = handle.emit(
                            "job://failed",
                            &serde_json::json!({ "jobId": job_id, "kind": kind, "error": error }),
                        );
                        let _ = handle.emit("index://changed", ());
                    }
                    JobEvent::Cancelled { job_id, kind } => {
                        invalidate_preview(&handle);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifo_pool_runs_newest_job_first() {
        let pool = LifoPool::new(1, "test-thumb");
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let order: Arc<Mutex<Vec<i32>>> = Arc::new(Mutex::new(Vec::new()));

        // Job chặn giữ worker duy nhất bận trong lúc xếp 3 job sau vào hàng
        pool.spawn(PoolTag::Thumb, move || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        started_rx.recv().unwrap();
        for i in 1..=3 {
            let order = order.clone();
            pool.spawn(PoolTag::Thumb, move || order.lock().unwrap().push(i));
        }
        release_tx.send(()).unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        while order.lock().unwrap().len() < 3 {
            assert!(Instant::now() < deadline, "pool khong chay het job");
            std::thread::sleep(Duration::from_millis(5));
        }
        // Job vao SAU chay TRUOC — cell dang hien thi luon thang backlog
        assert_eq!(*order.lock().unwrap(), vec![3, 2, 1]);
    }

    #[test]
    fn lifo_pool_clear_drops_only_tagged_queued_jobs() {
        let pool = LifoPool::new(1, "test-clear");
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let order: Arc<Mutex<Vec<i32>>> = Arc::new(Mutex::new(Vec::new()));

        pool.spawn(PoolTag::Thumb, move || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        started_rx.recv().unwrap();
        for i in 1..=2 {
            let order = order.clone();
            pool.spawn(PoolTag::Thumb, move || order.lock().unwrap().push(i));
        }
        let media_order = order.clone();
        pool.spawn(PoolTag::Media, move || media_order.lock().unwrap().push(99));

        // Xả thumb: 2 job thumb đang xếp hàng bị bỏ, media giữ nguyên
        assert_eq!(pool.clear(PoolTag::Thumb), 2);
        release_tx.send(()).unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        while order.lock().unwrap().is_empty() {
            assert!(Instant::now() < deadline, "media job phai chay");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(*order.lock().unwrap(), vec![99]);
        assert_eq!(pool.clear(PoolTag::Thumb), 0);
    }
}
