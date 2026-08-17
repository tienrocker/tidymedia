//! M4 exact dedup: hash job (quick → full → rebuild groups) + delete có verify.
//!
//! 5 invariant an toàn dữ liệu áp ở đây:
//! 1. Không bao giờ xóa khi survivor chưa được verify bằng BLAKE3 FULL còn
//!    hợp lệ (hashed_size/mtime khớp cả DB lẫn filesystem NGAY LÚC XÓA).
//! 2. TOCTOU: mọi ứng viên xóa re-check size+mtime fs ngay trước khi trash.
//! 3. (rename/copy-verify là chuyện M5 — M4 không move gì.)
//! 4. Mọi delete → Recycle Bin (crate trash), KHÔNG hard-delete.
//! 5. Journal: đợt xóa ghi vào jobs (kind dedup_delete, params = danh sách path).

use std::path::Path;
use std::sync::atomic::Ordering;

use core_db::{DeleteContextRow, DupGroupRow, DupMemberRow, HashUpsert, PendingHash};
use core_jobs::{JobEvent, JobProgress, Throttle};
use rayon::prelude::*;
use serde::Serialize;
use tauri::State;

use crate::commands::{blocking, err, CmdResult, GateGuard};
use crate::state::{AppState, LifoPool};

/// Batch full hash nhỏ: mỗi file là 1 lượt đọc TOÀN BỘ nội dung (ảnh 5-50MB).
const QUICK_BATCH: i64 = 256;
const FULL_BATCH: i64 = 16;

// ---------- hash job ----------

/// Quét trùng lặp: quick hash → full hash → rebuild nhóm. Idempotent như meta.
#[tauri::command]
pub async fn start_hash_scan(state: State<'_, AppState>) -> CmdResult<Option<i64>> {
    if state.recovery_active.load(Ordering::Acquire) {
        return Err("ERR_RECOVERY_BUSY|".into());
    }
    let gate = state.hash_start_gate.clone();
    if gate
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        if let Some(id) = state.jobs.active_job_of_kind("hash") {
            return Ok(Some(id));
        }
        return Err("ERR_INDEX_BUSY|organize hash preparation is active".into());
    }
    let guard = GateGuard(gate);
    if let Some(id) = state.jobs.active_job_of_kind("hash") {
        return Ok(Some(id));
    }
    if state.jobs.active_job_of_kind("org_hash").is_some() {
        return Err("ERR_INDEX_BUSY|organize hash preparation is active".into());
    }
    let db = state.db.clone();
    let jobs = state.jobs.clone();
    let thumb_pool = state.thumb_pool.clone();
    let op_gate = state.index_op_gate.clone();
    let preview = state.org_preview.clone();
    let preview_cancel = state.org_preview_cancel.clone();
    blocking(move || {
        let _gate = guard;
        let _op = op_gate.lock().unwrap_or_else(|p| p.into_inner());
        if jobs.active_job_of_kind("scan").is_some() {
            return Err("ERR_INDEX_BUSY|scan is active".into());
        }
        if jobs.active_job_of_kind("organize").is_some()
            || jobs.active_job_of_kind("org_undo").is_some()
        {
            return Err("ERR_INDEX_BUSY|organize/undo is active".into());
        }
        let job_id = db
            .writer
            .exec(|c| core_db::ops::insert_job(c, "hash", None))
            .map_err(err)?;
        let (cancel, pause) = jobs.register_pausable(job_id, "hash", None);
        if let Some(active_preview) = preview_cancel
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            active_preview.store(true, Ordering::Relaxed);
        }
        *preview.lock().unwrap_or_else(|p| p.into_inner()) = None;
        let events = jobs.sender();
        let writer_cleanup = db.writer.clone();

        std::thread::Builder::new()
            .name(format!("hash-{job_id}"))
            .spawn(move || {
                let events_run = events.clone();
                let cancel_run = cancel.clone();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_hash_job(&db, &thumb_pool, &cancel_run, &pause, job_id, &events_run)
                }));
                let final_event = match result {
                    Err(_) => JobEvent::Failed {
                        job_id,
                        kind: "hash".into(),
                        error: "ERR_INTERNAL|hash thread panicked".into(),
                    },
                    Ok(Ok(_)) if cancel.load(Ordering::Relaxed) => JobEvent::Cancelled {
                        job_id,
                        kind: "hash".into(),
                    },
                    Ok(Ok(msg)) => JobEvent::Done {
                        job_id,
                        kind: "hash".into(),
                        message: Some(msg),
                    },
                    Ok(Err(e)) => JobEvent::Failed {
                        job_id,
                        kind: "hash".into(),
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

/// Nhường I/O cho thumb interactive: user ĐANG cuộn/nhìn grid (có request thumb
/// trong 2s gần nhất) thì job nền không giành ổ đĩa (HDD bão hòa là thumb đen
/// xì hàng phút). Backlog thumb cũ không tính — không được treo job vô hạn.
pub(crate) fn yield_to_thumbs(thumb_pool: &LifoPool, cancel: &core_jobs::CancelFlag) {
    while thumb_pool.active_within(std::time::Duration::from_secs(2))
        && !cancel.load(Ordering::Relaxed)
    {
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
}

/// Nhịp dựng lại nhóm trùng giữa chừng job hash. 8s là đủ "sống" cho mắt người
/// mà không biến writer thành cối xay.
pub(crate) const DUP_REFRESH_MS: u64 = 8_000;

/// Gom nhóm trùng GIỮA CHỪNG job hash thay vì chỉ một lần lúc xong: quét 34k
/// file trên HDD mất hàng giờ, không ai muốn nhìn màn hình câm suốt.
///
/// `rebuild_dup_groups` là full recompute nhưng (a) giữ nguyên group id nên UI
/// không nhảy lung tung và selection không bị prune oan, (b) DB nằm ở app-data
/// chứ không phải ổ media đang bị cày → vài chục ms mỗi lượt.
pub(crate) struct DupRefresher {
    throttle: Throttle,
    dirty: bool,
}

impl DupRefresher {
    pub(crate) fn new(interval_ms: u64) -> Self {
        Self {
            throttle: Throttle::new(interval_ms),
            dirty: false,
        }
    }

    /// Vừa ghi thêm hash → lượt refresh tới mới có gì để gom.
    pub(crate) fn mark(&mut self, wrote: usize) {
        self.dirty |= wrote > 0;
    }

    /// Thuần (test được): chỉ chạy khi CÓ hash mới và đã hết cửa sổ throttle.
    /// Short-circuit có chủ đích — chưa dirty thì không đụng vào đồng hồ, batch
    /// đầu tiên có hash mới được rebuild ngay lập tức.
    fn due(&mut self) -> bool {
        self.dirty && self.throttle.ready()
    }

    pub(crate) fn refresh_if_due(
        &mut self,
        db: &core_db::Db,
        events: &crossbeam_channel::Sender<JobEvent>,
    ) -> anyhow::Result<()> {
        if !self.due() {
            return Ok(());
        }
        self.refresh(db, events)
    }

    pub(crate) fn refresh(
        &mut self,
        db: &core_db::Db,
        events: &crossbeam_channel::Sender<JobEvent>,
    ) -> anyhow::Result<()> {
        self.dirty = false;
        let (groups, waste) = db.writer.exec(core_db::ops::rebuild_dup_groups)?;
        let _ = events.send(JobEvent::DupGroupsChanged { groups, waste });
        Ok(())
    }
}

/// Ngủ khi user bấm ⏸, báo trạng thái đúng lúc ĐỔI (không spam). Trả false =
/// job phải dừng hẳn (đã bị hủy trong lúc đang dừng).
#[allow(clippy::too_many_arguments)] // context của 1 progress event, không tách được cho gọn hơn
pub(crate) fn hold_if_paused(
    pause: &core_jobs::PauseFlag,
    cancel: &core_jobs::CancelFlag,
    job_id: i64,
    kind: &str,
    done: u64,
    total: Option<u64>,
    running_msg: Option<&str>,
    events: &crossbeam_channel::Sender<JobEvent>,
) -> bool {
    if !pause.load(Ordering::Relaxed) {
        return !cancel.load(Ordering::Relaxed);
    }
    let progress = |message: Option<&str>| {
        let _ = events.send(JobEvent::Progress(JobProgress {
            job_id,
            kind: kind.into(),
            done,
            total,
            message: message.map(str::to_string),
        }));
    };
    progress(Some("user_paused"));
    let alive = core_jobs::wait_while_paused(pause, cancel);
    if alive {
        progress(running_msg);
    }
    alive
}

fn run_hash_job(
    db: &core_db::Db,
    thumb_pool: &LifoPool,
    cancel: &core_jobs::CancelFlag,
    pause: &core_jobs::PauseFlag,
    job_id: i64,
    events: &crossbeam_channel::Sender<JobEvent>,
) -> anyhow::Result<String> {
    let mut throttle = Throttle::new(200);
    let mut done: u64 = 0;
    let mut dups = DupRefresher::new(DUP_REFRESH_MS);

    // Lượt quét trước bị hủy giữa phase 2 để lại full hash CHƯA từng được gom
    // nhóm — dựng lại ngay lúc bấm Scan thay vì bắt user đợi hết job mới thấy.
    dups.refresh(db, events)?;

    // Phase 1: quick hash (xxh3 8KB) cho mọi file trong nhóm cùng size.
    // Tổng phải gồm CẢ phase 2 ngay từ đầu: quick thường đã xong sạch từ lượt
    // trước (=0), lấy mỗi nó rồi max(1) là UI hiện "0 / 1" treo hàng chục phút.
    let quick_total = db.pool.with(core_db::ops::count_pending_quick)? as u64;
    let full_estimate = db.pool.with(core_db::ops::count_pending_full)? as u64;
    // UI phải thấy job NGAY khi bấm — không đợi batch đầu (có thể đang yield)
    let _ = events.send(JobEvent::Progress(JobProgress {
        job_id,
        kind: "hash".into(),
        done: 0,
        total: Some(quick_total + full_estimate),
        message: Some(if quick_total > 0 { "quick" } else { "verify" }.into()),
    }));
    let mut cursor = 0i64;
    loop {
        if !hold_if_paused(
            pause,
            cancel,
            job_id,
            "hash",
            done,
            Some((quick_total + full_estimate).max(done)),
            Some("quick"),
            events,
        ) {
            return Ok(String::new());
        }
        let batch = db
            .pool
            .with(|c| core_db::ops::select_pending_quick(c, cursor, QUICK_BATCH))?;
        let Some(last) = batch.last() else { break };
        cursor = last.file_id;
        let mut ups: Vec<HashUpsert> = Vec::with_capacity(batch.len());
        for chunk in batch.chunks(64) {
            yield_to_thumbs(thumb_pool, cancel);
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            ups.par_extend(chunk.par_iter().filter_map(hash_one_quick));
        }
        done += batch.len() as u64;
        if !ups.is_empty() {
            db.writer
                .exec(move |c| core_db::ops::upsert_hash_batch(c, &ups))?;
        }
        if throttle.ready() {
            let _ = events.send(JobEvent::Progress(JobProgress {
                job_id,
                kind: "hash".into(),
                done,
                total: Some((quick_total + full_estimate).max(done)),
                message: Some("quick".into()),
            }));
        }
    }

    // Phase 2: BLAKE3 full — chỉ trong nhóm (size, quick64) nghi trùng.
    // Đếm lại sau phase 1: quick vừa chạy có thể lộ ra ứng viên mới.
    let full_total = db.pool.with(core_db::ops::count_pending_full)? as u64;
    let grand_total = quick_total.max(done) + full_total;
    // Bắn ngay, không đợi throttle: batch full đầu tiên trên HDD có thể mất cả
    // phút (16 file đọc trọn nội dung), UI không được đứng số cũ suốt ngần ấy.
    let _ = events.send(JobEvent::Progress(JobProgress {
        job_id,
        kind: "hash".into(),
        done,
        total: Some(grand_total.max(done)),
        message: Some("verify".into()),
    }));
    cursor = 0;
    loop {
        if !hold_if_paused(
            pause,
            cancel,
            job_id,
            "hash",
            done,
            Some(grand_total.max(done)),
            Some("verify"),
            events,
        ) {
            return Ok(String::new());
        }
        // Mỗi file phase 2 là 1 lượt đọc TOÀN BỘ nội dung — nhường thumb trước
        yield_to_thumbs(thumb_pool, cancel);
        if cancel.load(Ordering::Relaxed) {
            return Ok(String::new());
        }
        let batch = db
            .pool
            .with(|c| core_db::ops::select_pending_full(c, cursor, FULL_BATCH))?;
        let Some(last) = batch.last() else { break };
        cursor = last.file_id;
        let ups: Vec<HashUpsert> = batch
            .par_iter()
            .filter_map(|p| hash_one_full(p, cancel))
            .collect();
        done += batch.len() as u64;
        dups.mark(ups.len());
        if !ups.is_empty() {
            db.writer
                .exec(move |c| core_db::ops::upsert_hash_batch(c, &ups))?;
        }
        // Nhóm trùng vừa lộ ra hiện luôn trên UI, không đợi hết job
        dups.refresh_if_due(db, events)?;
        if throttle.ready() {
            let _ = events.send(JobEvent::Progress(JobProgress {
                job_id,
                kind: "hash".into(),
                done,
                total: Some(grand_total.max(done)),
                message: Some("verify".into()),
            }));
        }
    }

    // Phase 3: rebuild nhóm trùng từ full_hash (chốt trạng thái cuối)
    let (groups, waste) = db.writer.exec(core_db::ops::rebuild_dup_groups)?;
    let _ = events.send(JobEvent::DupGroupsChanged { groups, waste });
    Ok(format!("{groups} groups, waste {waste}"))
}

/// Đọc được + không phải placeholder + fs còn khớp snapshot DB thì mới hash —
/// lệch là skip (scan sau cập nhật DB rồi job sau hash lại). None không ghi gì.
fn check_fs_matches(p: &PendingHash) -> Option<()> {
    let md = std::fs::metadata(&p.path).ok()?;
    if core_media::is_cloud_placeholder(&md) {
        return None;
    }
    if md.len() as i64 != p.size || unix_ms(md.modified().ok()) != p.mtime {
        return None;
    }
    Some(())
}

fn hash_one_quick(p: &PendingHash) -> Option<HashUpsert> {
    check_fs_matches(p)?;
    let q = core_hash::quick64(Path::new(&p.path)).ok()?;
    Some(HashUpsert {
        file_id: p.file_id,
        quick64: Some(q),
        full: None,
        src_mtime: p.mtime,
        src_size: p.size,
    })
}

/// Đọc theo chunk 1 MiB + poll cờ hủy: một file video 2 GB không được làm nút
/// Stop chết cứng hàng phút. Hủy giữa chừng → None, file vẫn nằm trong hàng
/// chờ nên lượt sau hash lại từ đầu, không mất gì.
fn hash_one_full(p: &PendingHash, cancel: &core_jobs::CancelFlag) -> Option<HashUpsert> {
    check_fs_matches(p)?;
    let h = core_hash::full_blake3_cancellable(Path::new(&p.path), cancel).ok()??;
    Some(HashUpsert {
        file_id: p.file_id,
        quick64: None, // COALESCE giữ quick cũ
        full: Some(h.to_vec()),
        src_mtime: p.mtime,
        src_size: p.size,
    })
}

pub(crate) fn unix_ms(t: Option<std::time::SystemTime>) -> i64 {
    t.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------- queries cho UI ----------

/// `kind`: 0 = trùng tuyệt đối, 1 = gần giống (perceptual).
#[tauri::command]
pub async fn list_dup_groups(
    state: State<'_, AppState>,
    kind: Option<i64>,
) -> CmdResult<Vec<DupGroupRow>> {
    let db = state.db.clone();
    let kind = kind.unwrap_or(0);
    blocking(move || {
        db.pool
            .with(|c| core_db::ops::list_dup_groups(c, kind))
            .map_err(err)
    })
    .await
}

#[tauri::command]
pub async fn get_dup_group(
    state: State<'_, AppState>,
    group_id: i64,
) -> CmdResult<Vec<DupMemberRow>> {
    let db = state.db.clone();
    blocking(move || {
        db.pool
            .with(|c| core_db::ops::get_dup_group(c, group_id))
            .map_err(err)
    })
    .await
}

/// Member rút gọn của MỌI nhóm — UI cần để tick "chọn tất cả" theo rule mà
/// không phải mở lần lượt vài nghìn nhóm.
#[tauri::command]
pub async fn list_dup_members_brief(
    state: State<'_, AppState>,
    kind: Option<i64>,
) -> CmdResult<Vec<core_db::DupMemberBrief>> {
    let db = state.db.clone();
    let kind = kind.unwrap_or(0);
    blocking(move || {
        db.pool
            .with(|c| core_db::ops::list_dup_members_brief(c, kind))
            .map_err(err)
    })
    .await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DedupStats {
    pub groups: i64,
    pub waste: i64,
}

#[tauri::command]
pub async fn dedup_stats(state: State<'_, AppState>, kind: Option<i64>) -> CmdResult<DedupStats> {
    let db = state.db.clone();
    let kind = kind.unwrap_or(0);
    blocking(move || {
        let (groups, waste) = db
            .pool
            .with(|c| core_db::ops::dedup_stats(c, kind))
            .map_err(err)?;
        Ok(DedupStats { groups, waste })
    })
    .await
}

// ---------- delete ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteResult {
    pub deleted: usize,
    pub freed_bytes: i64,
    pub skipped: Vec<SkippedFile>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedFile {
    pub file_id: i64,
    pub name: String,
    pub reason: String, // code ổn định, frontend dịch
}

/// Kế hoạch xóa của 1 nhóm sau verify: survivor + danh sách bản sẽ trash.
struct GroupPlan {
    survivor: DeleteContextRow,
    victims: Vec<DeleteContextRow>,
}

/// Verify THUẦN (không fs/db trực tiếp — `fs_ok` inject được để unit-test đủ
/// invariant). BTreeMap để thứ tự nhóm ổn định, kết quả deterministic.
fn plan_delete(
    ctx: &[DeleteContextRow],
    del_set: &std::collections::HashSet<i64>,
    fs_ok: &dyn Fn(&DeleteContextRow) -> bool,
) -> (Vec<GroupPlan>, Vec<SkippedFile>) {
    use std::collections::{BTreeMap, HashSet};
    let mut by_group: BTreeMap<i64, Vec<&DeleteContextRow>> = BTreeMap::new();
    for row in ctx {
        by_group.entry(row.group_id).or_default().push(row);
    }

    let mut plans: Vec<GroupPlan> = Vec::new();
    let mut skipped: Vec<SkippedFile> = Vec::new();
    let mut seen: HashSet<i64> = HashSet::new();
    let skip = |list: &mut Vec<SkippedFile>, row: &DeleteContextRow, reason: &str| {
        list.push(SkippedFile {
            file_id: row.file_id,
            name: file_name(&row.path),
            reason: reason.into(),
        });
    };

    for members in by_group.values() {
        let (marked, keepers): (Vec<&DeleteContextRow>, Vec<&DeleteContextRow>) = members
            .iter()
            .copied()
            .partition(|m| del_set.contains(&m.file_id));

        // Guard cứng: không bao giờ xóa 100% member của nhóm
        if keepers.is_empty() {
            for m in &marked {
                if seen.insert(m.file_id) {
                    skip(&mut skipped, m, "KEEP_NONE");
                }
            }
            continue;
        }
        // Invariant #1: phải có ÍT NHẤT 1 survivor có BLAKE3 hợp lệ và file
        // thật trên đĩa còn đúng y snapshot đã hash
        let survivor = keepers.iter().find(|k| {
            k.status == 0
                && k.full_hash.is_some()
                && k.hashed_size == Some(k.size)
                && k.hashed_mtime == Some(k.mtime)
                && fs_ok(k)
        });
        let Some(survivor) = survivor else {
            for m in &marked {
                if seen.insert(m.file_id) {
                    skip(&mut skipped, m, "NO_VERIFIED_SURVIVOR");
                }
            }
            continue;
        };

        let mut victims: Vec<DeleteContextRow> = Vec::new();
        for m in marked {
            if !seen.insert(m.file_id) {
                continue;
            }
            if m.status != 0 {
                skip(&mut skipped, m, "NOT_PRESENT");
            } else if m.full_hash.is_none()
                || m.full_hash != survivor.full_hash
                || m.hashed_size != Some(m.size)
                || m.hashed_mtime != Some(m.mtime)
            {
                // quick hash không bao giờ là căn cứ — full phải khớp survivor
                skip(&mut skipped, m, "HASH_MISMATCH");
            } else if !fs_ok(m) {
                // TOCTOU: file đã bị sửa/di chuyển sau khi hash → không đụng
                skip(&mut skipped, m, "CHANGED_ON_DISK");
            } else {
                victims.push(m.clone());
            }
        }
        if !victims.is_empty() {
            plans.push(GroupPlan {
                survivor: (*survivor).clone(),
                victims,
            });
        }
    }
    (plans, skipped)
}

#[derive(Clone)]
struct PairPlan {
    victim: DeleteContextRow,
    owner_image_id: i64,
    /// MOV còn sống đã được BLAKE3-verify là cùng nội dung với `victim`.
    survivor: DeleteContextRow,
}

/// Live-pair expansion THUẦN. Luật cứng:
/// - CHỈ đi từ ảnh (kind=0) sang MOV — live_pair_id 2 chiều, đi từ MOV sẽ
///   trỏ ngược về ẢNH GỐC và xóa nhầm nó (P0 của review).
/// - Pair là keeper/member sống của bất kỳ nhóm nào trong context → KHÔNG đụng.
/// - Cả victim MOV lẫn survivor MOV phải present, hash snapshot còn hợp lệ,
///   BLAKE3 bằng nhau và filesystem còn khớp ngay lúc lập plan.
fn plan_pairs(
    plans: &[GroupPlan],
    keeper_ids: &std::collections::HashSet<i64>,
    lookup: &dyn Fn(i64) -> Option<DeleteContextRow>,
    fs_ok: &dyn Fn(&DeleteContextRow) -> bool,
) -> Vec<PairPlan> {
    use std::collections::HashSet;
    let planned: HashSet<i64> = plans
        .iter()
        .flat_map(|g| g.victims.iter().map(|v| v.file_id))
        .collect();
    let mut out: Vec<PairPlan> = Vec::new();
    for g in plans {
        // Không có survivor ảnh kèm MOV thì MOV victim là nội dung độc nhất về
        // mặt compound item: giữ lại, remove_deleted_files sẽ tự unpair nó.
        let Some(survivor_pair_id) = g.survivor.live_pair_id else {
            continue;
        };
        if planned.contains(&survivor_pair_id) {
            continue;
        }
        let Some(survivor_pair) = lookup(survivor_pair_id) else {
            continue;
        };
        for v in &g.victims {
            if v.kind != 0 {
                continue;
            }
            let Some(pid) = v.live_pair_id else { continue };
            if keeper_ids.contains(&pid)
                || planned.contains(&pid)
                || out.iter().any(|p| p.victim.file_id == pid)
            {
                continue;
            }
            let Some(victim_pair) = lookup(pid) else {
                continue;
            };
            let hashes_match =
                victim_pair.full_hash.is_some() && victim_pair.full_hash == survivor_pair.full_hash;
            let snapshot_valid = |p: &DeleteContextRow| {
                p.status == 0
                    && p.kind == 1
                    && p.hashed_size == Some(p.size)
                    && p.hashed_mtime == Some(p.mtime)
                    && fs_ok(p)
            };
            if pid != survivor_pair_id
                && hashes_match
                && snapshot_valid(&victim_pair)
                && snapshot_valid(&survivor_pair)
            {
                out.push(PairPlan {
                    victim: victim_pair,
                    owner_image_id: v.file_id,
                    survivor: survivor_pair.clone(),
                });
            }
        }
    }
    out
}

/// Lọc pair NGAY TRƯỚC khi trash (thuần, test được). Pair chỉ được trash khi:
/// - Ảnh chủ THỰC SỰ đã bị xóa — plan_pairs tính từ kế hoạch, nhưng victim có
///   thể bị skip lúc thực thi (SURVIVOR_CHANGED / CHANGED_ON_DISK / TRASH_FAILED);
///   trash MOV của ảnh còn nằm trên đĩa = xé cặp Live Photo còn sống.
/// - Không còn ảnh sống nào KHÁC trỏ vào MOV này — HEIC + JPG export cùng stem
///   cùng pair về 1 MOV: xóa JPG không được kéo MOV của HEIC đi theo.
fn filter_pairs_for_trash(
    pairs: &[PairPlan],
    deleted: &std::collections::HashSet<i64>,
    image_refs: &dyn Fn(i64) -> Option<Vec<i64>>, // None = lookup lỗi → fail closed
) -> Vec<PairPlan> {
    pairs
        .iter()
        .filter(|p| {
            deleted.contains(&p.owner_image_id)
                && image_refs(p.victim.file_id)
                    .is_some_and(|refs| refs.iter().all(|img| deleted.contains(img)))
        })
        .cloned()
        .collect()
}

/// Xóa các bản trùng đã đánh dấu → Recycle Bin. Backend verify lại TOÀN BỘ
/// invariant trên context nhóm — không tin UI. Toàn bộ chạy dưới delete_lock:
/// 2 đợt xóa song song có thể verify chéo rồi xóa sạch cả nhóm.
#[tauri::command]
pub async fn delete_dup_files(
    state: State<'_, AppState>,
    file_ids: Vec<i64>,
) -> CmdResult<DeleteResult> {
    if state
        .recovery_active
        .load(std::sync::atomic::Ordering::Acquire)
    {
        return Err("ERR_RECOVERY_BUSY|".into());
    }
    // Share the hash-start gate with both hash commands. Holding it through the
    // destructive operation closes the check/register race where a hash job could
    // otherwise start immediately after the active-job check below.
    let gate = state.hash_start_gate.clone();
    if gate
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err("ERR_INDEX_BUSY|hash/delete operation is active".into());
    }
    let guard = GateGuard(gate);
    if state.jobs.active_job_of_kind("hash").is_some()
        || state.jobs.active_job_of_kind("org_hash").is_some()
    {
        return Err("ERR_INDEX_BUSY|hash job is active".into());
    }
    crate::organize::invalidate_org_preview(&state);
    let db = state.db.clone();
    let lock = state.delete_lock.clone();
    let jobs = state.jobs.clone();
    blocking(move || {
        use std::collections::{HashMap, HashSet};
        let _hash_gate = guard;
        // Poison từ panic đợt trước không được khóa chết mọi delete sau này
        let _serialize = lock.lock().unwrap_or_else(|p| p.into_inner());

        let del_set: HashSet<i64> = file_ids.iter().copied().collect();
        if del_set.is_empty() {
            return Ok(DeleteResult {
                deleted: 0,
                freed_bytes: 0,
                skipped: vec![],
            });
        }
        let ids: Vec<i64> = del_set.iter().copied().collect();
        let ctx = db
            .pool
            .with(|c| core_db::ops::get_delete_context(c, &ids))
            .map_err(err)?;

        let fs_ok = |r: &DeleteContextRow| fs_matches(&r.path, r.size, r.mtime);
        let (plans, mut skipped) = plan_delete(&ctx, &del_set, &fs_ok);

        // Id gửi lên nhưng không thuộc nhóm nào → không xóa (UI lỗi?)
        for &id in &del_set {
            if !ctx.iter().any(|r| r.file_id == id) {
                skipped.push(SkippedFile {
                    file_id: id,
                    name: id.to_string(),
                    reason: "NOT_IN_GROUP".into(),
                });
            }
        }

        // Keeper = mọi file trong context KHÔNG bị xóa (kể cả marked-but-skipped
        // — chúng vẫn nằm trên đĩa và có thể là bản sống duy nhất ở nhóm khác)
        let victim_ids: HashSet<i64> = plans
            .iter()
            .flat_map(|g| g.victims.iter().map(|v| v.file_id))
            .collect();
        let keeper_ids: HashSet<i64> = ctx
            .iter()
            .map(|r| r.file_id)
            .filter(|id| !victim_ids.contains(id))
            .collect();
        // `plan_pairs` is intentionally pure, so carry a lookup failure out-of-band and
        // abort before journaling/trashing anything. A database failure must never be
        // interpreted as "this Live Photo has no paired video".
        let pair_lookup_error = std::cell::RefCell::new(None::<String>);
        let pairs = plan_pairs(
            &plans,
            &keeper_ids,
            &|pid| {
                if pair_lookup_error.borrow().is_some() {
                    return None;
                }
                match db
                    .pool
                    .with(|c| core_db::ops::get_delete_file_context(c, pid))
                {
                    Ok(row) => row,
                    Err(e) => {
                        *pair_lookup_error.borrow_mut() = Some(err(e));
                        None
                    }
                }
            },
            &fs_ok,
        );
        if let Some(e) = pair_lookup_error.into_inner() {
            return Err(e);
        }

        // Đọc toàn bộ references TRƯỚC lệnh trash đầu tiên. Destructive path
        // phải fail-closed: lỗi DB không bao giờ được diễn giải thành "0 ảnh
        // đang tham chiếu" rồi cho phép xóa MOV.
        let mut pair_refs: HashMap<i64, Vec<i64>> = HashMap::new();
        for pair in &pairs {
            let pid = pair.victim.file_id;
            if let std::collections::hash_map::Entry::Vacant(slot) = pair_refs.entry(pid) {
                let refs = db
                    .pool
                    .with(|c| core_db::ops::image_refs_of_pair(c, pid))
                    .map_err(err)?;
                slot.insert(refs);
            }
        }

        if plans.is_empty() && pairs.is_empty() {
            return Ok(DeleteResult {
                deleted: 0,
                freed_bytes: 0,
                skipped,
            });
        }

        // Journal WRITE-AHEAD (invariant #5): ghi intent TRƯỚC khi trash —
        // panic/crash giữa chừng vẫn còn vết những path đã định xóa.
        let intent: Vec<String> = plans
            .iter()
            .flat_map(|g| g.victims.iter().map(|v| v.path.clone()))
            .chain(pairs.iter().map(|p| p.victim.path.clone()))
            .collect();
        let journal = serde_json::to_string(&intent).unwrap_or_default();
        let jid = db
            .writer
            .exec(move |c| core_db::ops::insert_job(c, "dedup_delete", Some(&journal)))
            .map_err(err)?;

        // Dọn cả kho là hàng nghìn lệnh IFileOperation (~30ms/file) → phải hiện
        // tiến độ và hủy được, không thì UI như treo cả chục phút. Hủy = "không
        // trash thêm nữa"; những gì đã trash vẫn được dọn khỏi DB như thường.
        let cancel = jobs.register(jid, "dedup_delete", None);
        let events = jobs.sender();
        let total_work = (intent.len() as u64).max(1);
        let mut progress = Throttle::new(200);
        let mut trashed: u64 = 0;
        let _ = events.send(JobEvent::Progress(JobProgress {
            job_id: jid,
            kind: "dedup_delete".into(),
            done: 0,
            total: Some(total_work),
            message: Some("trash".into()),
        }));

        // Trash theo TỪNG NHÓM: survivor re-verify ngay trước victims của nhóm
        // đó, candidate re-check fs ngay trước từng lệnh trash — cửa sổ TOCTOU
        // tính bằng mili-giây thay vì phút.
        let mut deleted_ids: Vec<i64> = Vec::new();
        let mut freed: i64 = 0;
        let mut recycle_cache: HashMap<char, bool> = HashMap::new();
        let mut cancelled = false;
        for g in &plans {
            if !fs_matches(&g.survivor.path, g.survivor.size, g.survivor.mtime) {
                for v in &g.victims {
                    skipped.push(SkippedFile {
                        file_id: v.file_id,
                        name: file_name(&v.path),
                        reason: "SURVIVOR_CHANGED".into(),
                    });
                }
                continue;
            }
            for v in &g.victims {
                if cancelled || cancel.load(Ordering::Relaxed) {
                    cancelled = true;
                    skipped.push(SkippedFile {
                        file_id: v.file_id,
                        name: file_name(&v.path),
                        reason: "CANCELLED".into(),
                    });
                    continue;
                }
                match trash_one(&v.path, v.size, v.mtime, &mut recycle_cache) {
                    Ok(()) => {
                        deleted_ids.push(v.file_id);
                        freed += v.size;
                    }
                    Err(reason) => skipped.push(SkippedFile {
                        file_id: v.file_id,
                        name: file_name(&v.path),
                        reason: reason.into(),
                    }),
                }
                trashed += 1;
                if progress.ready() {
                    let _ = events.send(JobEvent::Progress(JobProgress {
                        job_id: jid,
                        kind: "dedup_delete".into(),
                        done: trashed,
                        total: Some(total_work.max(trashed)),
                        message: Some("trash".into()),
                    }));
                }
            }
        }
        // Pair chỉ trash khi ảnh chủ THỰC SỰ đã xóa và không còn ảnh sống nào
        // khác trỏ vào MOV (HEIC + JPG cùng stem share 1 MOV).
        let deleted_set: HashSet<i64> = deleted_ids.iter().copied().collect();
        let pairs_final =
            filter_pairs_for_trash(&pairs, &deleted_set, &|pid| pair_refs.get(&pid).cloned());
        for p in &pairs_final {
            if cancelled || cancel.load(Ordering::Relaxed) {
                cancelled = true;
                skipped.push(SkippedFile {
                    file_id: p.victim.file_id,
                    name: file_name(&p.victim.path),
                    reason: "CANCELLED".into(),
                });
                continue;
            }
            // Survivor MOV re-check sát lệnh trash giống survivor ảnh. Hash đã
            // được đối chiếu trong plan; size+mtime đảm bảo đúng snapshot hash.
            if !fs_matches(&p.survivor.path, p.survivor.size, p.survivor.mtime) {
                skipped.push(SkippedFile {
                    file_id: p.victim.file_id,
                    name: file_name(&p.victim.path),
                    reason: "SURVIVOR_CHANGED".into(),
                });
                continue;
            }
            match trash_one(
                &p.victim.path,
                p.victim.size,
                p.victim.mtime,
                &mut recycle_cache,
            ) {
                Ok(()) => {
                    deleted_ids.push(p.victim.file_id);
                    freed += p.victim.size;
                }
                Err(reason) => skipped.push(SkippedFile {
                    file_id: p.victim.file_id,
                    name: file_name(&p.victim.path),
                    reason: reason.into(),
                }),
            }
            trashed += 1;
            if progress.ready() {
                let _ = events.send(JobEvent::Progress(JobProgress {
                    job_id: jid,
                    kind: "dedup_delete".into(),
                    done: trashed,
                    total: Some(total_work.max(trashed)),
                    message: Some("trash".into()),
                }));
            }
        }

        // Dọn DB cho những file THỰC SỰ đã vào Thùng rác (kể cả khi bị hủy giữa
        // chừng — bỏ qua bước này là index còn trỏ vào file không còn ở đó).
        let n = deleted_ids.len();
        let ids2 = deleted_ids.clone();
        if let Err(e) = db.writer.exec(move |c| {
            if !ids2.is_empty() {
                core_db::ops::remove_deleted_files(c, &ids2)?;
            }
            Ok(())
        }) {
            // Job đã register: lỗi cũng phải có event kết thúc, không thì
            // Jobs panel treo một job ma không bao giờ tắt.
            let msg = err(e);
            let _ = events.send(JobEvent::Failed {
                job_id: jid,
                kind: "dedup_delete".into(),
                error: msg.clone(),
            });
            return Err(msg);
        }
        // Journal chốt qua pump (nó lo finish_job + unregister + job://done)
        let _ = events.send(if cancelled {
            JobEvent::Cancelled {
                job_id: jid,
                kind: "dedup_delete".into(),
            }
        } else {
            JobEvent::Done {
                job_id: jid,
                kind: "dedup_delete".into(),
                message: Some(format!("trashed {n} files")),
            }
        });

        Ok(DeleteResult {
            deleted: deleted_ids.len(),
            freed_bytes: freed,
            skipped,
        })
    })
    .await
}

/// Trash 1 file với đủ chốt chặn: volume phải có Recycle Bin (ổ FAT32/exFAT
/// không có → IFileOperation sẽ XÓA VĨNH VIỄN, vi phạm invariant #4 → từ
/// chối), fs re-check ngay trước lệnh xóa.
pub(crate) fn trash_one(
    path: &str,
    size: i64,
    mtime: i64,
    recycle_cache: &mut std::collections::HashMap<char, bool>,
) -> Result<(), &'static str> {
    let drive = path.chars().next().unwrap_or('?').to_ascii_uppercase();
    let has_bin = *recycle_cache
        .entry(drive)
        .or_insert_with(|| volume_supports_recycle(drive));
    if !has_bin {
        return Err("NO_RECYCLE_BIN");
    }
    if !fs_matches(path, size, mtime) {
        return Err("CHANGED_ON_DISK");
    }
    match trash::delete(path) {
        Ok(()) => Ok(()),
        Err(e) => {
            tracing::warn!(path = %path, "trash failed: {e}");
            Err("TRASH_FAILED")
        }
    }
}

/// Volume có hỗ trợ Recycle Bin thật không. Probe thư mục `$Recycle.Bin` sai
/// cả 2 chiều: ổ FAT32 có sẵn folder rác từ máy khác → pass rồi bị
/// IFileOperation XÓA VĨNH VIỄN; ổ NTFS mới tinh chưa từng recycle → chưa có
/// folder → từ chối oan. Đúng cách: hỏi hệ điều hành loại drive + filesystem.
/// Chỉ cho DRIVE_FIXED + NTFS/ReFS — ổ removable từ chối (hướng an toàn:
/// thà bắt user tự xóa còn hơn hard-delete ngầm).
pub(crate) fn volume_supports_recycle(drive: char) -> bool {
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetVolumeInformationW};
    /// winbase.h: DRIVE_FIXED = 3 (giá trị ABI cố định)
    const DRIVE_FIXED: u32 = 3;
    if !drive.is_ascii_alphabetic() {
        return false;
    }
    let root: Vec<u16> = format!("{drive}:\\")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        if GetDriveTypeW(root.as_ptr()) != DRIVE_FIXED {
            return false;
        }
        let mut fs_name = [0u16; 64];
        if GetVolumeInformationW(
            root.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs_name.as_mut_ptr(),
            fs_name.len() as u32,
        ) == 0
        {
            return false;
        }
        let len = fs_name.iter().position(|&c| c == 0).unwrap_or(0);
        let name = String::from_utf16_lossy(&fs_name[..len]);
        matches!(name.as_str(), "NTFS" | "ReFS")
    }
}

/// fs còn đúng y snapshot DB không (size + mtime + không phải placeholder).
pub(crate) fn fs_matches(path: &str, size: i64, mtime: i64) -> bool {
    match std::fs::metadata(path) {
        Ok(md) => {
            !core_media::is_cloud_placeholder(&md)
                && md.len() as i64 == size
                && unix_ms(md.modified().ok()) == mtime
        }
        Err(_) => false,
    }
}

pub(crate) fn file_name(path: &str) -> String {
    path.rsplit('\\').next().unwrap_or(path).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn row(gid: i64, id: i64, kind: i64, pair: Option<i64>, hash: u8) -> DeleteContextRow {
        DeleteContextRow {
            group_id: gid,
            file_id: id,
            path: format!("D:\\t\\f{id}"),
            kind,
            size: 100,
            mtime: 1,
            status: 0,
            live_pair_id: pair,
            full_hash: Some(vec![hash; 32]),
            hashed_size: Some(100),
            hashed_mtime: Some(1),
        }
    }
    fn ids(plans: &[GroupPlan]) -> Vec<i64> {
        plans
            .iter()
            .flat_map(|g| g.victims.iter().map(|v| v.file_id))
            .collect()
    }
    fn ok_all(_: &DeleteContextRow) -> bool {
        true
    }

    #[test]
    fn dup_refresher_needs_new_hashes_then_throttles() {
        let mut r = DupRefresher::new(60_000);
        assert!(!r.due(), "chua hash duoc gi thi khong rebuild");
        r.mark(0);
        assert!(!r.due(), "batch khong ghi duoc hash nao cung khong tinh");
        r.mark(5);
        assert!(r.due(), "co hash moi -> rebuild ngay lan dau, khong doi 8s");
        r.mark(5);
        assert!(!r.due(), "trong cua so throttle thi thoi");
    }

    #[test]
    fn keep_none_guard_blocks_whole_group() {
        let ctx = vec![row(1, 10, 0, None, 1), row(1, 11, 0, None, 1)];
        let del: HashSet<i64> = [10, 11].into();
        let (plans, skipped) = plan_delete(&ctx, &del, &ok_all);
        assert!(plans.is_empty());
        assert!(skipped.iter().all(|s| s.reason == "KEEP_NONE"));
        assert_eq!(skipped.len(), 2);
    }

    #[test]
    fn no_verified_survivor_blocks_group() {
        let ctx = vec![row(1, 10, 0, None, 1), row(1, 11, 0, None, 1)];
        let del: HashSet<i64> = [10].into();
        // keeper (11) fs fail → không có survivor
        let fs = |r: &DeleteContextRow| r.file_id != 11;
        let (plans, skipped) = plan_delete(&ctx, &del, &fs);
        assert!(plans.is_empty());
        assert_eq!(skipped[0].reason, "NO_VERIFIED_SURVIVOR");
    }

    #[test]
    fn hash_mismatch_and_changed_on_disk_skip_victim_only() {
        let mut bad_hash = row(1, 12, 0, None, 9); // hash khác survivor
        bad_hash.full_hash = Some(vec![9; 32]);
        let ctx = vec![row(1, 10, 0, None, 1), row(1, 11, 0, None, 1), bad_hash];
        let del: HashSet<i64> = [11, 12].into();
        let (plans, skipped) = plan_delete(&ctx, &del, &ok_all);
        assert_eq!(ids(&plans), vec![11]);
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].reason, "HASH_MISMATCH");

        let ctx2 = vec![row(2, 20, 0, None, 1), row(2, 21, 0, None, 1)];
        let del2: HashSet<i64> = [21].into();
        let fs = |r: &DeleteContextRow| r.file_id != 21;
        let (plans2, skipped2) = plan_delete(&ctx2, &del2, &fs);
        assert!(plans2.is_empty());
        assert_eq!(skipped2[0].reason, "CHANGED_ON_DISK");
    }

    #[test]
    fn unique_pair_is_preserved_without_survivor_pair() {
        // Ảnh trùng nhưng keeper không có MOV: MOV 100 là nội dung độc nhất,
        // tuyệt đối không được kéo theo ảnh victim.
        let ctx = vec![row(1, 10, 0, Some(100), 1), row(1, 11, 0, None, 1)];
        let del: HashSet<i64> = [10].into();
        let (plans, _) = plan_delete(&ctx, &del, &ok_all);
        let keeper_ids: HashSet<i64> = [11].into();
        let lookup = |_: i64| panic!("keeper khong co pair thi khong duoc lookup MOV victim");
        let pairs = plan_pairs(&plans, &keeper_ids, &lookup, &ok_all);
        assert!(pairs.is_empty());
    }

    #[test]
    fn pair_followed_only_with_verified_matching_survivor_pair() {
        let ctx = vec![row(1, 10, 0, Some(100), 1), row(1, 11, 0, Some(101), 1)];
        let del: HashSet<i64> = [10].into();
        let (plans, _) = plan_delete(&ctx, &del, &ok_all);
        let keeper_ids: HashSet<i64> = [11].into();
        let lookup = |id: i64| Some(row(0, id, 1, None, 7));
        let pairs = plan_pairs(&plans, &keeper_ids, &lookup, &ok_all);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].victim.file_id, 100);
        assert_eq!(pairs[0].survivor.file_id, 101);
    }

    #[test]
    fn pair_with_different_hash_is_preserved() {
        let ctx = vec![row(1, 10, 0, Some(100), 1), row(1, 11, 0, Some(101), 1)];
        let del: HashSet<i64> = [10].into();
        let (plans, _) = plan_delete(&ctx, &del, &ok_all);
        let keeper_ids: HashSet<i64> = [11].into();
        let lookup = |id: i64| Some(row(0, id, 1, None, if id == 100 { 7 } else { 8 }));
        assert!(plan_pairs(&plans, &keeper_ids, &lookup, &ok_all).is_empty());
    }

    #[test]
    fn pair_never_followed_from_mov_victim() {
        // P0 review: nhóm MOV trùng, MOV 20 có live_pair_id trỏ về ẢNH GỐC 200.
        // Xóa MOV 20 KHÔNG ĐƯỢC kéo ảnh 200 theo.
        let ctx = vec![row(1, 20, 1, Some(200), 1), row(1, 21, 1, None, 1)];
        let del: HashSet<i64> = [20].into();
        let (plans, _) = plan_delete(&ctx, &del, &ok_all);
        assert_eq!(ids(&plans), vec![20]);
        let keeper_ids: HashSet<i64> = [21].into();
        let lookup =
            |_: i64| -> Option<DeleteContextRow> { panic!("khong duoc lookup pair tu MOV victim") };
        let pairs = plan_pairs(&plans, &keeper_ids, &lookup, &ok_all);
        assert!(pairs.is_empty());
    }

    #[test]
    fn pair_that_is_keeper_elsewhere_is_protected() {
        // P0 review: MOV 100 vừa là pair của ảnh 10 (bị xóa), vừa là KEEPER
        // của nhóm MOV {100, 101} nơi 101 bị xóa → 100 phải sống.
        let ctx = vec![
            row(1, 10, 0, Some(100), 1),
            row(1, 11, 0, None, 1),
            row(2, 100, 1, Some(10), 2),
            row(2, 101, 1, None, 2),
        ];
        let del: HashSet<i64> = [10, 101].into();
        let (plans, _) = plan_delete(&ctx, &del, &ok_all);
        assert_eq!(ids(&plans), vec![10, 101]);
        let victim_ids: HashSet<i64> = ids(&plans).into_iter().collect();
        let keeper_ids: HashSet<i64> = ctx
            .iter()
            .map(|r| r.file_id)
            .filter(|id| !victim_ids.contains(id))
            .collect();
        assert!(keeper_ids.contains(&100));
        let lookup = |_: i64| -> Option<DeleteContextRow> { panic!("100 la keeper, cam lookup") };
        let pairs = plan_pairs(&plans, &keeper_ids, &lookup, &ok_all);
        assert!(pairs.is_empty(), "keeper cua nhom khac khong duoc trash");
    }

    #[test]
    fn pair_lookup_rejects_non_video() {
        // Phòng thủ kép: kể cả dữ liệu pair bậy (trỏ vào ảnh), lookup kind!=1 → bỏ
        let ctx = vec![row(1, 10, 0, Some(300), 1), row(1, 11, 0, Some(301), 1)];
        let del: HashSet<i64> = [10].into();
        let (plans, _) = plan_delete(&ctx, &del, &ok_all);
        let keeper_ids: HashSet<i64> = [11].into();
        let lookup = |id: i64| Some(row(0, id, 0, None, 1)); // kind 0!
        let pairs = plan_pairs(&plans, &keeper_ids, &lookup, &ok_all);
        assert!(pairs.is_empty());
    }

    #[test]
    fn pair_not_trashed_when_owner_was_skipped() {
        // P1 review: victim ảnh 10 bị skip lúc thực thi (vd SURVIVOR_CHANGED)
        // → MOV 100 của nó KHÔNG được trash (xé cặp Live Photo còn sống).
        let pairs = vec![PairPlan {
            victim: row(0, 100, 1, None, 7),
            owner_image_id: 10,
            survivor: row(0, 101, 1, None, 7),
        }];
        let deleted: HashSet<i64> = HashSet::new(); // 10 không xóa được
        let refs = |_: i64| Some(vec![10]);
        assert!(filter_pairs_for_trash(&pairs, &deleted, &refs).is_empty());
        // owner xóa thật → pair đi cùng
        let deleted: HashSet<i64> = [10].into();
        assert_eq!(filter_pairs_for_trash(&pairs, &deleted, &refs).len(), 1);
    }

    #[test]
    fn shared_mov_protected_while_other_image_alive() {
        // P1 review: HEIC (20) + JPG (10) cùng stem đều pair về MOV 100.
        // Xóa JPG 10 → MOV phải Ở LẠI vì HEIC 20 còn sống vẫn trỏ vào nó.
        let pairs = vec![PairPlan {
            victim: row(0, 100, 1, None, 7),
            owner_image_id: 10,
            survivor: row(0, 101, 1, None, 7),
        }];
        let deleted: HashSet<i64> = [10].into();
        let refs = |_: i64| Some(vec![10, 20]); // 20 còn sống
        assert!(filter_pairs_for_trash(&pairs, &deleted, &refs).is_empty());
        // cả 2 ảnh cùng bị xóa trong đợt này → MOV mới được đi
        let deleted: HashSet<i64> = [10, 20].into();
        assert_eq!(filter_pairs_for_trash(&pairs, &deleted, &refs).len(), 1);
    }

    #[test]
    fn pair_reference_lookup_error_is_fail_closed() {
        let pairs = vec![PairPlan {
            victim: row(0, 100, 1, None, 7),
            owner_image_id: 10,
            survivor: row(0, 101, 1, None, 7),
        }];
        let deleted: HashSet<i64> = [10].into();
        assert!(filter_pairs_for_trash(&pairs, &deleted, &|_| None).is_empty());
    }
}
