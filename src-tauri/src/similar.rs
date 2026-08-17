//! M7 — trùng GẦN GIỐNG (perceptual): cùng một tấm ảnh nhưng khác byte vì bị
//! nén lại / thu nhỏ / xuất qua app khác. Dedup tuyệt đối không bao giờ thấy
//! những cặp này (lệch 1 byte EXIF là BLAKE3 đã khác).
//!
//! Tách hẳn khỏi [`crate::dedup`] vì BẤT BIẾN XÓA KHÁC NHAU VỀ BẢN CHẤT:
//! ở dedup tuyệt đối, mọi bản bị xóa đều được chứng minh trùng BLAKE3 với bản
//! giữ lại. Ở đây điều đó KHÔNG THỂ chứng minh — các bản thật sự khác nhau về
//! độ phân giải/độ nén. Nên đường xóa của nó có luật riêng, hàm riêng, test
//! riêng, và không bao giờ dùng chung `plan_delete` với nhánh kia:
//!
//! 1. Không bao giờ xóa 100% một nhóm (giống nhánh exact).
//! 2. Bản giữ lại phải còn present và fs khớp snapshot NGAY LÚC xóa.
//! 3. Mọi ứng viên xóa re-check size+mtime sát lệnh trash (TOCTOU).
//! 4. KHÔNG kéo theo MOV của Live Photo: cặp chỉ được đi theo khi đã verify
//!    trùng BLAKE3, mà ở đây không có gì để verify.
//! 5. Recycle Bin + journal như mọi thao tác phá hủy khác.

use std::sync::atomic::Ordering;

use core_db::{DeleteContextRow, PendingPhash, PhashUpsert};
use core_jobs::{JobEvent, JobProgress, Throttle};
use rayon::prelude::*;
use tauri::State;

use crate::commands::{blocking, err, CmdResult, GateGuard};
use crate::dedup::{fs_matches, DeleteResult, SkippedFile};
use crate::state::AppState;

/// Ngưỡng Hamming sống cạnh hàm hash sinh ra nó — đổi lưới là phải đo lại.
use core_media::phash::MAX_DIST;
const PHASH_BATCH: i64 = 256;

// ---------- job tính dhash ----------

#[tauri::command]
pub async fn start_phash_scan(state: State<'_, AppState>) -> CmdResult<Option<i64>> {
    if state.recovery_active.load(Ordering::Acquire) {
        return Err("ERR_RECOVERY_BUSY|".into());
    }
    let gate = state.phash_start_gate.clone();
    if gate
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Ok(state.jobs.active_job_of_kind("phash"));
    }
    let guard = GateGuard(gate);
    if let Some(id) = state.jobs.active_job_of_kind("phash") {
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
            .with(core_db::ops::count_pending_phash)
            .map_err(err)? as u64;
        if total == 0 {
            // Không còn gì để hash nhưng nhóm có thể chưa từng dựng (lần đầu
            // bật tính năng) — dựng lại rồi thôi, không tạo job rác.
            let (groups, waste) = db
                .writer
                .exec(|c| core_db::ops::rebuild_similar_groups(c, MAX_DIST))
                .map_err(err)?;
            let _ = jobs.sender().send(JobEvent::DupGroupsChanged {
                kind: 1,
                groups,
                waste,
            });
            return Ok(None);
        }
        let job_id = db
            .writer
            .exec(|c| core_db::ops::insert_job(c, "phash", None))
            .map_err(err)?;
        let (cancel, pause) = jobs.register_pausable(job_id, "phash", None);
        let events = jobs.sender();
        let writer_cleanup = db.writer.clone();
        std::thread::Builder::new()
            .name(format!("phash-{job_id}"))
            .spawn(move || {
                let events_run = events.clone();
                let cancel_run = cancel.clone();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_phash_job(
                        &db,
                        &thumbs,
                        ffmpeg.as_deref(),
                        &thumb_pool,
                        &cancel_run,
                        &pause,
                        job_id,
                        total,
                        &events_run,
                    )
                }));
                let final_event = match result {
                    Err(_) => JobEvent::Failed {
                        job_id,
                        kind: "phash".into(),
                        error: "ERR_INTERNAL|phash thread panicked".into(),
                    },
                    Ok(Ok(_)) if cancel.load(Ordering::Relaxed) => JobEvent::Cancelled {
                        job_id,
                        kind: "phash".into(),
                    },
                    Ok(Ok(msg)) => JobEvent::Done {
                        job_id,
                        kind: "phash".into(),
                        message: Some(msg),
                    },
                    Ok(Err(e)) => JobEvent::Failed {
                        job_id,
                        kind: "phash".into(),
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
fn run_phash_job(
    db: &core_db::Db,
    thumbs: &core_media::ThumbStore,
    ffmpeg: Option<&std::path::Path>,
    thumb_pool: &crate::state::LifoPool,
    cancel: &core_jobs::CancelFlag,
    pause: &core_jobs::PauseFlag,
    job_id: i64,
    total: u64,
    events: &crossbeam_channel::Sender<JobEvent>,
) -> anyhow::Result<String> {
    // Khớp THUMB_GRID phía frontend: thumb 256 đã có sẵn trong thumbs.db cho
    // gần hết kho (job thumb_warm cày nền), nên hash lấy từ cache là chính —
    // đọc SSD vài ms thay vì decode lại ảnh gốc từ HDD.
    const GRID_S: u32 = 256;
    let mut done: u64 = 0;
    let mut hashed: u64 = 0;
    let mut cursor = 0i64;
    let mut throttle = Throttle::new(200);
    let mut dups = crate::dedup::DupRefresher::new_similar(crate::dedup::DUP_REFRESH_MS, MAX_DIST);
    let _ = events.send(JobEvent::Progress(JobProgress {
        job_id,
        kind: "phash".into(),
        done: 0,
        total: Some(total),
        message: None,
    }));
    // Gom ngay từ đầu: lượt quét trước bị hủy/tắt app giữa chừng vẫn để lại
    // hash trong DB, nhưng nhóm chỉ được dựng lúc job kết thúc — không có lượt
    // này thì user nhìn màn hình trống suốt dù dữ liệu đã có sẵn.
    dups.refresh(db, events)?;
    loop {
        if !crate::dedup::hold_if_paused(
            pause,
            cancel,
            job_id,
            "phash",
            done,
            Some(total.max(done)),
            None,
            events,
        ) {
            break;
        }
        let batch = db
            .pool
            .with(|c| core_db::ops::select_pending_phash(c, cursor, PHASH_BATCH))?;
        let Some(last) = batch.last() else { break };
        cursor = last.file_id;
        let mut ups: Vec<PhashUpsert> = Vec::with_capacity(batch.len());
        for chunk in batch.chunks(32) {
            crate::dedup::yield_to_thumbs(thumb_pool, cancel);
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            ups.par_extend(
                chunk
                    .par_iter()
                    .filter_map(|p| phash_one(p, thumbs, ffmpeg, GRID_S)),
            );
        }
        done += batch.len() as u64;
        hashed += ups.len() as u64;
        dups.mark(ups.len());
        if !ups.is_empty() {
            db.writer
                .exec(move |c| core_db::ops::upsert_phash_batch(c, &ups))?;
        }
        // Nhóm gần giống vừa lộ ra hiện luôn, không đợi hết job
        dups.refresh_if_due(db, events)?;
        if throttle.ready() {
            let _ = events.send(JobEvent::Progress(JobProgress {
                job_id,
                kind: "phash".into(),
                done,
                total: Some(total.max(done)),
                message: None,
            }));
        }
    }
    // Gom nhóm kể cả khi bị hủy giữa chừng: hash đã tính được thì phải dùng
    let (groups, waste) = db
        .writer
        .exec(|c| core_db::ops::rebuild_similar_groups(c, MAX_DIST))?;
    let _ = events.send(JobEvent::DupGroupsChanged {
        kind: 1,
        groups,
        waste,
    });
    if cancel.load(Ordering::Relaxed) {
        return Ok(String::new());
    }
    Ok(format!("{hashed} hashed, {groups} similar groups"))
}

/// dhash 1 file. Ưu tiên thumb đã cache; chưa có thì tạo thumb rồi CẤT LẠI vào
/// cache — công decode ảnh gốc không bị vứt đi, lưới ảnh sau đó hiện luôn.
fn phash_one(
    p: &PendingPhash,
    thumbs: &core_media::ThumbStore,
    ffmpeg: Option<&std::path::Path>,
    size: u32,
) -> Option<PhashUpsert> {
    let cached = thumbs.get(p.file_id, size, p.mtime);
    let bytes = match cached {
        Some(b) => b,
        None => {
            // fs phải còn khớp snapshot DB, và tuyệt đối không hydrate cloud
            let md = std::fs::metadata(&p.path).ok()?;
            if core_media::is_cloud_placeholder(&md)
                || md.len() as i64 != p.size
                || crate::dedup::unix_ms(md.modified().ok()) != p.mtime
            {
                return None;
            }
            let ext = p.ext.as_deref().unwrap_or("");
            // Decoder panic với file hỏng không được giết job
            let made = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                core_media::make_thumb(std::path::Path::new(&p.path), ext, size, ffmpeg)
            }));
            match made {
                Ok(Ok(data)) => {
                    let _ = thumbs.put(p.file_id, size, p.mtime, &data);
                    data
                }
                // Không decode nổi → ghi bia để lượt sau khỏi thử lại
                _ => {
                    return Some(PhashUpsert {
                        file_id: p.file_id,
                        hash: None,
                        src_mtime: p.mtime,
                        src_size: p.size,
                    })
                }
            }
        }
    };
    let hash = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        core_media::dhash_bytes(&bytes).ok().flatten()
    }))
    .ok()
    .flatten();
    Some(PhashUpsert {
        file_id: p.file_id,
        // None = ảnh phẳng (frame đen, nền trơn): ghi bia, không gom nhóm
        hash: hash.map(|h| h.map(|w| w as i64)),
        src_mtime: p.mtime,
        src_size: p.size,
    })
}

// ---------- xóa trong nhóm gần giống ----------

/// Kế hoạch xóa cho MỘT nhóm gần giống.
struct SimilarPlan {
    survivor: DeleteContextRow,
    victims: Vec<DeleteContextRow>,
}

/// Verify THUẦN, test được. Khác `dedup::plan_delete` ở đúng một điểm sống còn:
/// KHÔNG so full_hash (không thể — các bản khác nội dung byte thật sự). Bù lại
/// mọi guard còn lại giữ nguyên và bản giữ lại vẫn phải present + khớp đĩa.
fn plan_similar_delete(
    ctx: &[DeleteContextRow],
    del_set: &std::collections::HashSet<i64>,
    fs_ok: &dyn Fn(&DeleteContextRow) -> bool,
) -> (Vec<SimilarPlan>, Vec<SkippedFile>) {
    use std::collections::{BTreeMap, HashSet};
    let mut by_group: BTreeMap<i64, Vec<&DeleteContextRow>> = BTreeMap::new();
    for row in ctx {
        by_group.entry(row.group_id).or_default().push(row);
    }
    let mut plans: Vec<SimilarPlan> = Vec::new();
    let mut skipped: Vec<SkippedFile> = Vec::new();
    let mut seen: HashSet<i64> = HashSet::new();
    let skip = |list: &mut Vec<SkippedFile>, row: &DeleteContextRow, reason: &str| {
        list.push(SkippedFile {
            file_id: row.file_id,
            name: crate::dedup::file_name(&row.path),
            reason: reason.into(),
        });
    };

    for members in by_group.values() {
        let (marked, keepers): (Vec<&DeleteContextRow>, Vec<&DeleteContextRow>) = members
            .iter()
            .copied()
            .partition(|m| del_set.contains(&m.file_id));
        // Guard cứng: không bao giờ xóa sạch một nhóm
        if keepers.is_empty() {
            for m in &marked {
                if seen.insert(m.file_id) {
                    skip(&mut skipped, m, "KEEP_NONE");
                }
            }
            continue;
        }
        // Bản giữ lại phải còn nguyên trên đĩa NGAY LÚC NÀY
        let survivor = keepers.iter().find(|k| k.status == 0 && fs_ok(k));
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
            } else if !fs_ok(m) {
                skip(&mut skipped, m, "CHANGED_ON_DISK");
            } else {
                victims.push(m.clone());
            }
        }
        if !victims.is_empty() {
            plans.push(SimilarPlan {
                survivor: (*survivor).clone(),
                victims,
            });
        }
    }
    (plans, skipped)
}

/// Xóa bản gần giống đã đánh dấu → Recycle Bin. Backend verify lại toàn bộ
/// trên context nhóm, không tin UI.
#[tauri::command]
pub async fn delete_similar_files(
    state: State<'_, AppState>,
    file_ids: Vec<i64>,
) -> CmdResult<DeleteResult> {
    if state.recovery_active.load(Ordering::Acquire) {
        return Err("ERR_RECOVERY_BUSY|".into());
    }
    // Dùng chung gate với hash/delete: không cho quét hash chen ngang giữa
    // lúc đang trash (nhóm sẽ bị dựng lại dưới chân mình).
    let gate = state.hash_start_gate.clone();
    if gate
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err("ERR_INDEX_BUSY|hash/delete operation is active".into());
    }
    let guard = GateGuard(gate);
    // `running_*` bỏ qua job ĐANG TẠM DỪNG: luồng của nó nằm ngủ, không đọc
    // file, không ghi DB, và cũng không gom lại nhóm (refresher nằm trong vòng
    // lặp) — nên xóa lúc đó an toàn y như lúc không có job nào. UI tự pause hộ
    // trước khi gọi, user không phải hủy cả lượt quét để dọn vài file.
    if state.jobs.running_job_of_kind("hash").is_some()
        || state.jobs.running_job_of_kind("org_hash").is_some()
        || state.jobs.running_job_of_kind("phash").is_some()
    {
        return Err("ERR_INDEX_BUSY|hash job is active".into());
    }
    crate::organize::invalidate_org_preview(&state);
    let db = state.db.clone();
    let lock = state.delete_lock.clone();
    let jobs = state.jobs.clone();
    blocking(move || {
        use std::collections::HashSet;
        let _hash_gate = guard;
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
            .with(|c| core_db::ops::get_similar_delete_context(c, &ids))
            .map_err(err)?;
        let fs_ok = |r: &DeleteContextRow| fs_matches(&r.path, r.size, r.mtime);
        let (plans, mut skipped) = plan_similar_delete(&ctx, &del_set, &fs_ok);
        for &id in &del_set {
            if !ctx.iter().any(|r| r.file_id == id) {
                skipped.push(SkippedFile {
                    file_id: id,
                    name: id.to_string(),
                    reason: "NOT_IN_GROUP".into(),
                });
            }
        }
        if plans.is_empty() {
            return Ok(DeleteResult {
                deleted: 0,
                freed_bytes: 0,
                skipped,
            });
        }

        // Journal write-ahead trước khi đụng file, y như nhánh exact
        let intent: Vec<String> = plans
            .iter()
            .flat_map(|g| g.victims.iter().map(|v| v.path.clone()))
            .collect();
        let journal = serde_json::to_string(&intent).unwrap_or_default();
        let jid = db
            .writer
            .exec(move |c| core_db::ops::insert_job(c, "similar_delete", Some(&journal)))
            .map_err(err)?;
        let cancel = jobs.register(jid, "similar_delete", None);
        let events = jobs.sender();
        let total_work = (intent.len() as u64).max(1);
        let mut progress = Throttle::new(200);
        let mut trashed = 0u64;
        let _ = events.send(JobEvent::Progress(JobProgress {
            job_id: jid,
            kind: "similar_delete".into(),
            done: 0,
            total: Some(total_work),
            message: Some("trash".into()),
        }));

        let mut deleted_ids: Vec<i64> = Vec::new();
        let mut freed: i64 = 0;
        let mut recycle_cache = std::collections::HashMap::new();
        let mut cancelled = false;
        for g in &plans {
            // Bản giữ lại re-verify sát lệnh trash của chính nhóm đó
            if !fs_matches(&g.survivor.path, g.survivor.size, g.survivor.mtime) {
                for v in &g.victims {
                    skipped.push(SkippedFile {
                        file_id: v.file_id,
                        name: crate::dedup::file_name(&v.path),
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
                        name: crate::dedup::file_name(&v.path),
                        reason: "CANCELLED".into(),
                    });
                    continue;
                }
                match crate::dedup::trash_one(&v.path, v.size, v.mtime, &mut recycle_cache) {
                    Ok(()) => {
                        deleted_ids.push(v.file_id);
                        freed += v.size;
                    }
                    Err(reason) => skipped.push(SkippedFile {
                        file_id: v.file_id,
                        name: crate::dedup::file_name(&v.path),
                        reason: reason.into(),
                    }),
                }
                trashed += 1;
                if progress.ready() {
                    let _ = events.send(JobEvent::Progress(JobProgress {
                        job_id: jid,
                        kind: "similar_delete".into(),
                        done: trashed,
                        total: Some(total_work.max(trashed)),
                        message: Some("trash".into()),
                    }));
                }
            }
        }

        let n = deleted_ids.len();
        let ids2 = deleted_ids.clone();
        if let Err(e) = db.writer.exec(move |c| {
            if !ids2.is_empty() {
                core_db::ops::remove_deleted_files(c, &ids2)?;
            }
            Ok(())
        }) {
            let msg = err(e);
            let _ = events.send(JobEvent::Failed {
                job_id: jid,
                kind: "similar_delete".into(),
                error: msg.clone(),
            });
            return Err(msg);
        }
        let _ = events.send(if cancelled {
            JobEvent::Cancelled {
                job_id: jid,
                kind: "similar_delete".into(),
            }
        } else {
            JobEvent::Done {
                job_id: jid,
                kind: "similar_delete".into(),
                message: Some(format!("trashed {n} files")),
            }
        });
        Ok(DeleteResult {
            deleted: n,
            freed_bytes: freed,
            skipped,
        })
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn row(gid: i64, id: i64, size: i64) -> DeleteContextRow {
        DeleteContextRow {
            group_id: gid,
            file_id: id,
            path: format!("D:\\t\\f{id}"),
            kind: 0,
            size,
            mtime: 1,
            status: 0,
            live_pair_id: Some(900 + id), // có pair để chứng minh KHÔNG bị kéo theo
            full_hash: None,              // nhóm gần giống KHÔNG có hash chung
            hashed_size: None,
            hashed_mtime: None,
        }
    }
    fn ok_all(_: &DeleteContextRow) -> bool {
        true
    }
    fn victim_ids(plans: &[SimilarPlan]) -> Vec<i64> {
        plans
            .iter()
            .flat_map(|g| g.victims.iter().map(|v| v.file_id))
            .collect()
    }

    #[test]
    fn deletes_without_hash_but_never_the_whole_group() {
        let ctx = vec![row(1, 10, 2_000_000), row(1, 11, 800_000)];
        let del: HashSet<i64> = [11].into();
        let (plans, skipped) = plan_similar_delete(&ctx, &del, &ok_all);
        assert_eq!(victim_ids(&plans), vec![11], "thieu hash van xoa duoc");
        assert!(skipped.is_empty());

        // Đánh dấu cả nhóm → chặn sạch
        let del_all: HashSet<i64> = [10, 11].into();
        let (plans, skipped) = plan_similar_delete(&ctx, &del_all, &ok_all);
        assert!(plans.is_empty());
        assert!(skipped.iter().all(|s| s.reason == "KEEP_NONE"));
    }

    #[test]
    fn survivor_must_still_match_disk() {
        let ctx = vec![row(1, 10, 2_000_000), row(1, 11, 800_000)];
        let del: HashSet<i64> = [11].into();
        // Bản giữ lại (10) đã đổi trên đĩa → không xóa gì cả
        let fs = |r: &DeleteContextRow| r.file_id != 10;
        let (plans, skipped) = plan_similar_delete(&ctx, &del, &fs);
        assert!(plans.is_empty());
        assert_eq!(skipped[0].reason, "NO_VERIFIED_SURVIVOR");
    }

    #[test]
    fn victim_changed_on_disk_is_skipped() {
        let ctx = vec![row(1, 10, 2_000_000), row(1, 11, 800_000)];
        let del: HashSet<i64> = [11].into();
        let fs = |r: &DeleteContextRow| r.file_id != 11;
        let (plans, skipped) = plan_similar_delete(&ctx, &del, &fs);
        assert!(plans.is_empty());
        assert_eq!(skipped[0].reason, "CHANGED_ON_DISK");
    }

    #[test]
    fn missing_file_is_never_the_survivor() {
        let mut gone = row(1, 10, 2_000_000);
        gone.status = 1;
        let ctx = vec![gone, row(1, 11, 800_000)];
        let del: HashSet<i64> = [11].into();
        let (plans, skipped) = plan_similar_delete(&ctx, &del, &ok_all);
        assert!(
            plans.is_empty(),
            "ban giu lai da mat thi khong duoc xoa ban con"
        );
        assert_eq!(skipped[0].reason, "NO_VERIFIED_SURVIVOR");
    }
}
