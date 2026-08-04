//! M5 organize: gom media về library root do user chỉ định, đặt tên theo
//! template configurable, journal write-ahead + undo theo batch.
//!
//! Invariant (kế thừa M4):
//! - Cùng volume = fs::rename ATOMIC, không bao giờ ghi đè (target tồn tại là lỗi).
//! - Xuyên volume = copy → sync → verify BLAKE3 → trash nguồn (nguồn vào Recycle
//!   Bin, không hard-delete; volume nguồn không có Recycle Bin → từ chối).
//! - Mọi op ghi org_ops TRƯỚC khi đụng fs; crash giữa chừng recover được.
//! - Toàn bộ execute/undo chạy dưới delete_lock (không đua với dedup delete)
//!   + org_start_gate chống double-start.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use core_db::{org, OrgCandidateRow};
use core_ingest::date::resolve_taken;
use core_ingest::planner::{plan_organize, PairInfo, PlanAction, PlanEntry, PlanItem, TargetState};
use core_ingest::template::{
    parse_template, RenderCtx, Template, TemplateKind, DEFAULT_DIR_TEMPLATE, DEFAULT_FILE_TEMPLATE,
};
use core_jobs::{JobEvent, JobProgress, Throttle};
use serde::Serialize;
use tauri::State;

use crate::commands::{blocking, canonicalize_root, err, CmdResult, GateGuard};
use crate::dedup::{fs_matches, unix_ms, volume_supports_recycle};
use crate::state::AppState;

const CAND_BATCH: i64 = 256;
const PREVIEW_SAMPLE_CAP: usize = 500;

// ---------- settings / library roots ----------

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct OrgSettings {
    pub dir_template: String,
    pub file_template: String,
    /// render thử với thời điểm + hash mẫu để UI preview ngay
    pub sample: String,
}

fn load_templates(db: &core_db::Db) -> anyhow::Result<(Template, Template, String, String)> {
    let (dir_s, file_s) = db.pool.with(|c| -> anyhow::Result<(String, String)> {
        Ok((
            core_db::ops::kv_get(c, "org_dir_template")?
                .unwrap_or_else(|| DEFAULT_DIR_TEMPLATE.to_string()),
            core_db::ops::kv_get(c, "org_file_template")?
                .unwrap_or_else(|| DEFAULT_FILE_TEMPLATE.to_string()),
        ))
    })?;
    let dir = parse_template(&dir_s, TemplateKind::Dir).map_err(|e| anyhow::anyhow!("{e}"))?;
    let file = parse_template(&file_s, TemplateKind::File).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok((dir, file, dir_s, file_s))
}

fn sample_render(dir: &Template, file: &Template) -> String {
    // 2019-06-14 15:30:22, hash mẫu — chỉ để user thấy hình dạng kết quả
    let ms = core_ingest::date::days_from_civil(2019, 6, 14) * core_ingest::date::MS_PER_DAY
        + (15 * 3600 + 30 * 60 + 22) * 1000;
    let ctx = RenderCtx::from_taken(ms, "a3f81c92d4e5b6a7a3f81c92d4e5b6a7", Some("Canon EOS R5"));
    let mut segs = dir.render_dir(&ctx);
    segs.push(format!("{}.jpg", file.render_file(&ctx, None)));
    segs.join("\\")
}

#[tauri::command]
pub async fn get_org_settings(state: State<'_, AppState>) -> CmdResult<OrgSettings> {
    let db = state.db.clone();
    blocking(move || {
        let (dir, file, dir_s, file_s) = load_templates(&db).map_err(err)?;
        Ok(OrgSettings {
            sample: sample_render(&dir, &file),
            dir_template: dir_s,
            file_template: file_s,
        })
    })
    .await
}

#[tauri::command]
pub async fn set_org_settings(
    state: State<'_, AppState>,
    dir_template: String,
    file_template: String,
) -> CmdResult<OrgSettings> {
    let db = state.db.clone();
    blocking(move || {
        // Validate TRƯỚC khi lưu — lỗi trả ERR_TPL_* cho UI dịch
        let dir = parse_template(&dir_template, TemplateKind::Dir).map_err(|e| e.to_string())?;
        let file = parse_template(&file_template, TemplateKind::File).map_err(|e| e.to_string())?;
        let (d2, f2) = (dir_template.clone(), file_template.clone());
        db.writer
            .exec(move |c| {
                core_db::ops::kv_set(c, "org_dir_template", &d2)?;
                core_db::ops::kv_set(c, "org_file_template", &f2)?;
                Ok(())
            })
            .map_err(err)?;
        Ok(OrgSettings {
            sample: sample_render(&dir, &file),
            dir_template,
            file_template,
        })
    })
    .await
}

#[tauri::command]
pub async fn list_library_roots(
    state: State<'_, AppState>,
) -> CmdResult<Vec<core_db::LibraryRootRow>> {
    let db = state.db.clone();
    blocking(move || db.pool.with(org::list_library_roots).map_err(err)).await
}

#[tauri::command]
pub async fn set_library_root(state: State<'_, AppState>, path: String) -> CmdResult<i64> {
    let db = state.db.clone();
    blocking(move || {
        let canonical = canonicalize_root(&path)?;
        db.writer
            .exec(move |c| org::set_library_root(c, &canonical))
            .map_err(err)
    })
    .await
}

#[tauri::command]
pub async fn remove_library_root(state: State<'_, AppState>, id: i64) -> CmdResult<()> {
    let db = state.db.clone();
    blocking(move || {
        db.writer
            .exec(move |c| org::remove_library_root(c, id))
            .map_err(err)
    })
    .await
}

// ---------- plan chung (preview + execute dùng cùng đường) ----------

struct ItemMeta {
    size: i64,
    mtime: i64,
    hash: Option<[u8; 32]>,
}

fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// hash trong DB còn hiệu lực? (đúng size + mtime snapshot)
fn valid_hash(c: &OrgCandidateRow) -> Option<Vec<u8>> {
    match (&c.full_hash, c.hashed_size, c.hashed_mtime) {
        (Some(h), Some(s), Some(m)) if s == c.size && m == c.mtime && h.len() == 32 => {
            Some(h.clone())
        }
        _ => None,
    }
}

fn build_items(
    cands: &[OrgCandidateRow],
    tz_offset_min: i32,
    now_ms: i64,
) -> (Vec<PlanItem>, HashMap<i64, ItemMeta>) {
    let mut metas = HashMap::new();
    let items = cands
        .iter()
        .map(|c| {
            let name = c.path.rsplit('\\').next().unwrap_or(&c.path);
            let r = resolve_taken(
                c.taken_at,
                c.date_source,
                name,
                c.mtime,
                tz_offset_min,
                now_ms,
            );
            let hash = valid_hash(c);
            metas.insert(
                c.file_id,
                ItemMeta {
                    size: c.size,
                    mtime: c.mtime,
                    hash: hash.as_deref().and_then(|h| h.try_into().ok()),
                },
            );
            PlanItem {
                file_id: c.file_id,
                path: c.path.clone(),
                ext: c.ext.to_lowercase(),
                status: c.status,
                taken_ms: r.taken_ms,
                taken_source: r.source,
                hash_hex: hash.as_deref().map(to_hex),
                camera: c.camera.clone(),
                pair: c.pair.as_ref().map(|p| PairInfo {
                    file_id: p.file_id,
                    path: p.path.clone(),
                    ext: p.ext.to_lowercase(),
                    status: p.status,
                }),
            }
        })
        .collect();
    (items, metas)
}

/// Trạng thái đích trên fs. `deep` = so BLAKE3 thật khi size trùng (execute);
/// preview chỉ so size (không đọc nội dung file nào khi dry-run).
fn probe_target(path: &str, item: &PlanItem, deep: bool) -> TargetState {
    let Ok(md) = std::fs::metadata(path) else {
        return TargetState::Free;
    };
    if md.len() as i64 == metadata_size_of(item) {
        if !deep {
            return TargetState::SameContent; // preview: size trùng coi như dup tiềm năng
        }
        if let Some(hex) = &item.hash_hex {
            if !core_media::is_cloud_placeholder(&md) {
                if let Ok(h) = core_hash::full_blake3(Path::new(path)) {
                    if to_hex(&h) == *hex {
                        return TargetState::SameContent;
                    }
                }
            }
        }
    }
    TargetState::Occupied
}

// planner không mang size trong PlanItem — lấy qua path? Không: size nằm trong
// metas; nhưng probe cần trực tiếp. Nhét size vào hash side-channel là bẩn —
// thay vào đó parse từ fs của item (item.path đã fs_matches trước execute).
fn metadata_size_of(item: &PlanItem) -> i64 {
    std::fs::metadata(&item.path)
        .map(|m| m.len() as i64)
        .unwrap_or(-1)
}

// ---------- preview ----------

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct OrgPreviewRow {
    pub file_id: i64,
    pub old_path: String,
    pub new_path: Option<String>,
    pub action: String,
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct OrgPreview {
    pub total: i64,
    pub moves: i64,
    pub copies: i64,
    pub needs_hash: i64,
    pub skip_organized: i64,
    pub skip_duplicate: i64,
    pub skip_cloud: i64,
    pub skip_uncertain: i64,
    pub skip_pair_blocked: i64,
    pub skip_other: i64,
    /// chữ cái các ổ có media nhưng CHƯA đặt library root
    pub volumes_missing_root: Vec<String>,
    pub sample: Vec<OrgPreviewRow>,
}

fn action_name(a: &PlanAction) -> &'static str {
    match a {
        PlanAction::Rename => "MOVE",
        PlanAction::CopyVerify => "COPY",
        PlanAction::NeedsHash => "NEEDS_HASH",
        PlanAction::SkipCloud => "SKIP_CLOUD",
        PlanAction::SkipMissing => "SKIP_MISSING",
        PlanAction::SkipUncertain => "SKIP_UNCERTAIN",
        PlanAction::SkipDuplicate => "SKIP_DUPLICATE",
        PlanAction::SkipOrganized => "SKIP_ORGANIZED",
        PlanAction::SkipPairBlocked => "SKIP_PAIR_BLOCKED",
        PlanAction::SkipCollision => "SKIP_COLLISION",
    }
}

#[tauri::command]
pub async fn org_preview(
    state: State<'_, AppState>,
    include_uncertain: bool,
) -> CmdResult<OrgPreview> {
    let db = state.db.clone();
    blocking(move || {
        let (dir_tpl, file_tpl, ..) = load_templates(&db).map_err(err)?;
        let tz = get_tz(&db).map_err(err)?;
        let now = now_ms();
        let roots = db.pool.with(org::list_library_roots).map_err(err)?;
        let mut out = OrgPreview::default();

        // Ổ nào có root index nhưng chưa có library root → báo UI
        let watch_roots = db.pool.with(core_db::ops::list_roots).map_err(err)?;
        for w in &watch_roots {
            let has_lib = roots.iter().any(|r| r.volume_id == w.volume_id);
            if !has_lib {
                if let Some(l) = w.path.chars().next() {
                    let s = l.to_ascii_uppercase().to_string();
                    if !out.volumes_missing_root.contains(&s) {
                        out.volumes_missing_root.push(s);
                    }
                }
            }
        }

        for root in &roots {
            let mut cursor = 0i64;
            loop {
                let cands = db
                    .pool
                    .with(|c| org::select_org_candidates(c, root.volume_id, cursor, CAND_BATCH))
                    .map_err(err)?;
                let Some(last) = cands.last() else { break };
                cursor = last.file_id;
                let (items, _metas) = build_items(&cands, tz, now);
                let plan = plan_organize(
                    &items,
                    &root.path,
                    &dir_tpl,
                    &file_tpl,
                    include_uncertain,
                    &|p, it| probe_target(p, it, false),
                );
                for e in &plan {
                    out.total += 1;
                    match e.action {
                        PlanAction::Rename => out.moves += 1,
                        PlanAction::CopyVerify => out.copies += 1,
                        PlanAction::NeedsHash => out.needs_hash += 1,
                        PlanAction::SkipOrganized => out.skip_organized += 1,
                        PlanAction::SkipDuplicate => out.skip_duplicate += 1,
                        PlanAction::SkipCloud => out.skip_cloud += 1,
                        PlanAction::SkipUncertain => out.skip_uncertain += 1,
                        PlanAction::SkipPairBlocked => out.skip_pair_blocked += 1,
                        _ => out.skip_other += 1,
                    }
                    // Pair MOV đi cùng ảnh: đếm vào tổng LUÔN LUÔN (không phụ
                    // thuộc sample còn chỗ hay không — cap chỉ giới hạn hiển thị)
                    if e.pair_move.is_some() {
                        out.total += 1;
                        out.moves += i64::from(e.action == PlanAction::Rename);
                        out.copies += i64::from(e.action == PlanAction::CopyVerify);
                    }
                    // Sample ưu tiên hàng có hành động (move/copy/needs-hash)
                    let interesting = matches!(
                        e.action,
                        PlanAction::Rename | PlanAction::CopyVerify | PlanAction::NeedsHash
                    );
                    if interesting && out.sample.len() < PREVIEW_SAMPLE_CAP {
                        out.sample.push(OrgPreviewRow {
                            file_id: e.file_id,
                            old_path: e.old_path.clone(),
                            new_path: e.new_path.clone(),
                            action: action_name(&e.action).into(),
                        });
                        if let Some((pid, pold, pnew)) = &e.pair_move {
                            if out.sample.len() < PREVIEW_SAMPLE_CAP {
                                out.sample.push(OrgPreviewRow {
                                    file_id: *pid,
                                    old_path: pold.clone(),
                                    new_path: Some(pnew.clone()),
                                    action: action_name(&e.action).into(),
                                });
                            }
                        }
                    }
                }
            }
        }
        Ok(out)
    })
    .await
}

// ---------- execute ----------

fn get_tz(db: &core_db::Db) -> anyhow::Result<i32> {
    Ok(db
        .pool
        .with(|c| core_db::ops::kv_get(c, "tz_offset_minutes"))?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[tauri::command]
pub async fn start_organize(
    state: State<'_, AppState>,
    include_uncertain: bool,
) -> CmdResult<Option<i64>> {
    let gate = state.org_start_gate.clone();
    if gate
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Ok(state.jobs.active_job_of_kind("organize"));
    }
    let guard = GateGuard(gate);
    if let Some(id) = state.jobs.active_job_of_kind("organize") {
        return Ok(Some(id));
    }
    // Undo đang chạy thì không cho organize chen (và ngược lại) — delete_lock
    // vẫn serialize được nhưng bắt user chờ ngầm hàng giờ là tệ, chặn sớm.
    if state.jobs.active_job_of_kind("org_undo").is_some() {
        return Err("ERR_ORG_BUSY|".into());
    }
    let db = state.db.clone();
    let jobs = state.jobs.clone();
    let lock = state.delete_lock.clone();
    blocking(move || {
        let _gate = guard;
        let job_id = db
            .writer
            .exec(|c| core_db::ops::insert_job(c, "organize", None))
            .map_err(err)?;
        let cancel = jobs.register(job_id, "organize", None);
        let events = jobs.sender();
        let writer_cleanup = db.writer.clone();
        std::thread::Builder::new()
            .name(format!("organize-{job_id}"))
            .spawn(move || {
                let events_run = events.clone();
                let cancel_run = cancel.clone();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_organize_job(
                        &db,
                        &lock,
                        &cancel_run,
                        job_id,
                        &events_run,
                        include_uncertain,
                    )
                }));
                let final_event = match result {
                    Err(_) => JobEvent::Failed {
                        job_id,
                        kind: "organize".into(),
                        error: "ERR_INTERNAL|organize thread panicked".into(),
                    },
                    Ok(Ok(_)) if cancel.load(Ordering::Relaxed) => JobEvent::Cancelled {
                        job_id,
                        kind: "organize".into(),
                    },
                    Ok(Ok(msg)) => JobEvent::Done {
                        job_id,
                        kind: "organize".into(),
                        message: Some(msg),
                    },
                    Ok(Err(e)) => JobEvent::Failed {
                        job_id,
                        kind: "organize".into(),
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

#[derive(Default)]
struct OrgTally {
    moved: u64,
    hashed: u64,
    skipped: HashMap<&'static str, u64>,
}

impl OrgTally {
    fn skip(&mut self, reason: &'static str) {
        *self.skipped.entry(reason).or_insert(0) += 1;
    }
    fn message(&self) -> String {
        let mut m = format!("moved {}", self.moved);
        if self.hashed > 0 {
            m.push_str(&format!(", hashed {}", self.hashed));
        }
        let mut reasons: Vec<_> = self.skipped.iter().collect();
        reasons.sort_by(|a, b| b.1.cmp(a.1));
        for (r, n) in reasons {
            m.push_str(&format!(", {n} {r}"));
        }
        m
    }
}

fn run_organize_job(
    db: &core_db::Db,
    fs_lock: &Arc<Mutex<()>>,
    cancel: &core_jobs::CancelFlag,
    job_id: i64,
    events: &crossbeam_channel::Sender<JobEvent>,
    include_uncertain: bool,
) -> anyhow::Result<String> {
    // Poison từ panic đợt trước không được khóa chết mọi thao tác fs sau này
    let _serialize = fs_lock.lock().unwrap_or_else(|p| p.into_inner());
    recover_pending_ops(db)?;

    let (dir_tpl, file_tpl, ..) = load_templates(db)?;
    let tz = get_tz(db)?;
    let now = now_ms();
    let roots = db.pool.with(org::list_library_roots)?;
    if roots.is_empty() {
        anyhow::bail!("ERR_ORG_NO_LIBRARY_ROOT|");
    }
    let mut total: u64 = 0;
    for r in &roots {
        total += db
            .pool
            .with(|c| org::count_org_candidates(c, r.volume_id))? as u64;
    }

    let mut tally = OrgTally::default();
    let mut throttle = Throttle::new(200);
    let mut done: u64 = 0;

    for root in &roots {
        let mut cursor = 0i64;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Ok(tally.message());
            }
            let cands = db
                .pool
                .with(|c| org::select_org_candidates(c, root.volume_id, cursor, CAND_BATCH))?;
            let Some(last) = cands.last() else { break };
            cursor = last.file_id;
            let (mut items, mut metas) = build_items(&cands, tz, now);

            // Hash-on-demand: file thiếu hash mà tên cần {hashN} (hoặc sẽ copy
            // xuyên volume) → BLAKE3 ngay, ghi vào bảng hashes (guard id+mtime+size)
            let lib_drive = root.path.chars().next().map(|c| c.to_ascii_uppercase());
            for it in items.iter_mut() {
                if cancel.load(Ordering::Relaxed) {
                    return Ok(tally.message());
                }
                let same_vol = it.path.chars().next().map(|c| c.to_ascii_uppercase()) == lib_drive;
                // Thứ tự skip PHẢI khớp planner: file mà planner sẽ SkipUncertain/
                // SkipPairBlocked thì đừng BLAKE3 cả ruột nó ra vô ích (mặc định
                // include_uncertain=false, thư viện bừa thì đây là phần rất lớn).
                let uncertain_skip =
                    it.taken_source == core_ingest::date::SRC_MTIME_UNCERTAIN && !include_uncertain;
                let pair_blocked = it.pair.as_ref().is_some_and(|p| p.status != 0);
                if it.hash_hex.is_some()
                    || (!file_tpl.has_hash && same_vol)
                    || it.status != 0
                    || uncertain_skip
                    || pair_blocked
                {
                    continue;
                }
                let meta = &metas[&it.file_id];
                if !fs_matches(&it.path, meta.size, meta.mtime) {
                    continue; // planner sẽ NeedsHash → skip HASH_FAILED
                }
                let Ok(h) = core_hash::full_blake3(Path::new(&it.path)) else {
                    continue;
                };
                tally.hashed += 1;
                let up = core_db::HashUpsert {
                    file_id: it.file_id,
                    quick64: None,
                    full: Some(h.to_vec()),
                    src_mtime: meta.mtime,
                    src_size: meta.size,
                };
                db.writer
                    .exec(move |c| core_db::ops::upsert_hash_batch(c, &[up]))?;
                it.hash_hex = Some(to_hex(&h));
                metas.get_mut(&it.file_id).unwrap().hash = Some(h);
            }

            let plan = plan_organize(
                &items,
                &root.path,
                &dir_tpl,
                &file_tpl,
                include_uncertain,
                &|p, it| probe_target(p, it, true),
            );
            for e in &plan {
                if cancel.load(Ordering::Relaxed) {
                    return Ok(tally.message());
                }
                done += 1;
                match e.action {
                    PlanAction::Rename | PlanAction::CopyVerify => {
                        execute_move(db, job_id, e, &metas, &mut tally);
                    }
                    PlanAction::NeedsHash => tally.skip("HASH_FAILED"),
                    // Ảnh đã đúng chỗ nhưng MOV của cặp còn kẹt lại (đợt trước
                    // fail giữa 2 nửa) → move nốt MOV để hàn cặp
                    PlanAction::SkipOrganized if e.pair_move.is_some() => {
                        execute_pair_fixup(db, job_id, e, &mut tally);
                    }
                    ref a => tally.skip(action_name(a)),
                }
                if throttle.ready() {
                    let _ = events.send(JobEvent::Progress(JobProgress {
                        job_id,
                        kind: "organize".into(),
                        done,
                        total: Some(total.max(done)),
                        message: None,
                    }));
                }
            }
        }
    }
    Ok(tally.message())
}

/// Crash recovery: op còn intent (done_at NULL). Nguồn còn trên đĩa → move chưa
/// xảy ra / dở dang → bỏ intent (nguồn nguyên vẹn = không mất gì; target rác
/// nếu có sẽ bị coi là Occupied → escalate tên, vô hại). Nguồn mất + target có
/// → move đã xong trước crash → hoàn tất phần DB.
fn recover_pending_ops(db: &core_db::Db) -> anyhow::Result<()> {
    let pending = db.pool.with(org::pending_org_ops)?;
    for op in pending {
        let src_exists = Path::new(&op.old_path).exists();
        let dst_exists = Path::new(&op.new_path).exists();
        let (op_id, file_id, new_path) = (op.id, op.file_id, op.new_path.clone());
        if !src_exists && dst_exists {
            db.writer.exec(move |c| {
                org::update_file_location(c, file_id, &new_path)?;
                org::mark_org_op_done(c, op_id)?;
                Ok(())
            })?;
        } else {
            db.writer.exec(move |c| org::delete_org_op(c, op_id))?;
        }
    }
    Ok(())
}

fn parent_of(path: &str) -> Option<&Path> {
    Path::new(path).parent()
}

/// Rename KHÔNG BAO GIỜ ghi đè. `std::fs::rename` trên Windows bật
/// MOVEFILE_REPLACE_EXISTING (P0 của review: target tồn tại là bị THAY THẾ
/// không cứu được) — phải gọi thẳng MoveFileExW với flags = 0: target tồn
/// tại → lỗi, cùng volume vẫn atomic. Không MOVEFILE_COPY_ALLOWED — xuyên
/// volume tự lo bằng copy-verify.
fn rename_no_replace(from: &str, to: &str) -> Result<(), &'static str> {
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
    let f: Vec<u16> = from.encode_utf16().chain(std::iter::once(0)).collect();
    let t: Vec<u16> = to.encode_utf16().chain(std::iter::once(0)).collect();
    let ok = unsafe { MoveFileExW(f.as_ptr(), t.as_ptr(), 0) };
    if ok == 0 {
        Err("MOVE_FAILED")
    } else {
        Ok(())
    }
}

/// Move 1 file (đã có journal row op_id). Trả Ok = fs move XONG (DB do caller).
fn move_file_fs(
    old_path: &str,
    new_path: &str,
    size: i64,
    mtime: i64,
    expected_hash: Option<&[u8; 32]>,
) -> Result<(), &'static str> {
    if !fs_matches(old_path, size, mtime) {
        return Err("CHANGED_ON_DISK");
    }
    if let Some(p) = parent_of(new_path) {
        std::fs::create_dir_all(p).map_err(|_| "MKDIR_FAILED")?;
    }
    let same_vol = old_path.chars().next().map(|c| c.to_ascii_uppercase())
        == new_path.chars().next().map(|c| c.to_ascii_uppercase());
    if same_vol {
        return rename_no_replace(old_path, new_path);
    }
    // Xuyên volume: nguồn phải trash được (không hard-delete nguồn)
    let src_drive = old_path.chars().next().unwrap_or('?').to_ascii_uppercase();
    if !volume_supports_recycle(src_drive) {
        return Err("NO_RECYCLE_BIN");
    }
    let Some(expected) = expected_hash else {
        return Err("HASH_FAILED"); // copy-verify bắt buộc có hash đối chiếu
    };
    let tmp = format!("{new_path}.tidymedia-tmp");
    let copy = (|| -> std::io::Result<()> {
        std::fs::copy(old_path, &tmp)?;
        std::fs::OpenOptions::new()
            .write(true)
            .open(&tmp)?
            .sync_all()?;
        // copy không giữ mtime → set lại cho khớp snapshot DB (mọi guard sau
        // này so size+mtime đều dựa vào giá trị này)
        filetime::set_file_mtime(
            &tmp,
            filetime::FileTime::from_unix_time(mtime / 1000, ((mtime % 1000) * 1_000_000) as u32),
        )?;
        Ok(())
    })();
    if copy.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return Err("COPY_FAILED");
    }
    match core_hash::full_blake3(Path::new(&tmp)) {
        Ok(h) if &h == expected => {}
        _ => {
            let _ = std::fs::remove_file(&tmp);
            return Err("VERIFY_FAILED");
        }
    }
    if rename_no_replace(&tmp, new_path).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return Err("MOVE_FAILED"); // target vừa bị chiếm — bỏ, không ghi đè
    }
    if trash::delete(old_path).is_err() {
        // Bản copy đã verify nằm trong library; nguồn trash fail thì để lại —
        // dedup sẽ bắt cặp trùng này sau. KHÔNG coi là lỗi move.
        tracing::warn!(path = %old_path, "organize: trash nguồn thất bại, để lại bản gốc");
    }
    Ok(())
}

fn execute_move(
    db: &core_db::Db,
    batch_id: i64,
    e: &PlanEntry,
    metas: &HashMap<i64, ItemMeta>,
    tally: &mut OrgTally,
) {
    let Some(new_path) = e.new_path.clone() else {
        return;
    };
    let meta = &metas[&e.file_id];

    // Journal write-ahead cho ảnh + pair (nếu có) TRƯỚC mọi thao tác fs
    let (old_a, new_a, fid) = (e.old_path.clone(), new_path.clone(), e.file_id);
    let pair = e.pair_move.clone();
    let ops_ids = db.writer.exec({
        let pair = pair.clone();
        move |c| {
            let a = org::insert_org_op(c, batch_id, fid, &old_a, &new_a)?;
            let b = match &pair {
                Some((pid, pold, pnew)) => Some(org::insert_org_op(c, batch_id, *pid, pold, pnew)?),
                None => None,
            };
            Ok((a, b))
        }
    });
    let Ok((op_a, op_b)) = ops_ids else {
        tally.skip("DB_ERROR");
        return;
    };

    match move_file_fs(
        &e.old_path,
        &new_path,
        meta.size,
        meta.mtime,
        meta.hash.as_ref(),
    ) {
        Ok(()) => {
            let np = new_path.clone();
            let res = db.writer.exec(move |c| {
                org::update_file_location(c, fid, &np)?;
                org::mark_org_op_done(c, op_a)?;
                Ok(())
            });
            if res.is_ok() {
                tally.moved += 1;
            } else {
                tally.skip("DB_ERROR");
            }
        }
        Err(reason) => {
            tally.skip(reason);
            let _ = db.writer.exec(move |c| org::delete_org_op(c, op_a));
            if let Some(b) = op_b {
                let _ = db.writer.exec(move |c| org::delete_org_op(c, b));
            }
            return;
        }
    }

    // Pair đi CÙNG stem — chỉ move khi ảnh đã move xong
    if let (Some((pid, pold, pnew)), Some(op_b)) = (pair, op_b) {
        move_pair(db, op_b, pid, &pold, &pnew, tally);
    }
}

/// Move MOV của cặp Live Photo (journal op đã insert sẵn từ caller).
fn move_pair(db: &core_db::Db, op_b: i64, pid: i64, pold: &str, pnew: &str, tally: &mut OrgTally) {
    let (psize, pmtime) = match std::fs::metadata(pold) {
        Ok(m) => (m.len() as i64, unix_ms(m.modified().ok())),
        Err(_) => (-1, -1),
    };
    // Cross-volume pair: cần hash MOV — không có sẵn thì hash tại chỗ
    let same_vol = pold.chars().next().map(|c| c.to_ascii_uppercase())
        == pnew.chars().next().map(|c| c.to_ascii_uppercase());
    let phash = if same_vol {
        None
    } else {
        core_hash::full_blake3(Path::new(pold)).ok()
    };
    match move_file_fs(pold, pnew, psize, pmtime, phash.as_ref()) {
        Ok(()) => {
            let pnew2 = pnew.to_string();
            let res = db.writer.exec(move |c| {
                org::update_file_location(c, pid, &pnew2)?;
                org::mark_org_op_done(c, op_b)?;
                Ok(())
            });
            if res.is_ok() {
                tally.moved += 1;
            } else {
                tally.skip("DB_ERROR");
            }
        }
        Err(reason) => {
            tally.skip(reason);
            let _ = db.writer.exec(move |c| org::delete_org_op(c, op_b));
        }
    }
}

/// Ảnh đã SkipOrganized nhưng planner phát hiện MOV chưa nằm cạnh (cặp bị xé
/// từ đợt trước) → chỉ move MOV. Journal write-ahead như mọi op khác.
fn execute_pair_fixup(db: &core_db::Db, batch_id: i64, e: &PlanEntry, tally: &mut OrgTally) {
    let Some((pid, pold, pnew)) = e.pair_move.clone() else {
        return;
    };
    let ins = db.writer.exec({
        let (po, pn) = (pold.clone(), pnew.clone());
        move |c| org::insert_org_op(c, batch_id, pid, &po, &pn)
    });
    let Ok(op_b) = ins else {
        tally.skip("DB_ERROR");
        return;
    };
    move_pair(db, op_b, pid, &pold, &pnew, tally);
}

// ---------- undo ----------

#[tauri::command]
pub async fn list_org_batches(state: State<'_, AppState>) -> CmdResult<Vec<core_db::OrgBatchRow>> {
    let db = state.db.clone();
    blocking(move || db.pool.with(org::list_org_batches).map_err(err)).await
}

#[tauri::command]
pub async fn undo_org_batch(state: State<'_, AppState>, batch_id: i64) -> CmdResult<Option<i64>> {
    let gate = state.org_start_gate.clone();
    if gate
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Ok(state.jobs.active_job_of_kind("org_undo"));
    }
    let guard = GateGuard(gate);
    if let Some(id) = state.jobs.active_job_of_kind("org_undo") {
        return Ok(Some(id));
    }
    if state.jobs.active_job_of_kind("organize").is_some() {
        return Err("ERR_ORG_BUSY|".into());
    }
    let db = state.db.clone();
    let jobs = state.jobs.clone();
    let lock = state.delete_lock.clone();
    blocking(move || {
        let _gate = guard;
        let params = format!("{{\"batchId\":{batch_id}}}");
        let job_id = db
            .writer
            .exec(move |c| core_db::ops::insert_job(c, "org_undo", Some(&params)))
            .map_err(err)?;
        let cancel = jobs.register(job_id, "org_undo", None);
        let events = jobs.sender();
        let writer_cleanup = db.writer.clone();
        std::thread::Builder::new()
            .name(format!("org-undo-{job_id}"))
            .spawn(move || {
                let events_run = events.clone();
                let cancel_run = cancel.clone();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_undo_job(&db, &lock, &cancel_run, job_id, batch_id)
                }));
                let final_event = match result {
                    Err(_) => JobEvent::Failed {
                        job_id,
                        kind: "org_undo".into(),
                        error: "ERR_INTERNAL|undo thread panicked".into(),
                    },
                    Ok(Ok(_)) if cancel.load(Ordering::Relaxed) => JobEvent::Cancelled {
                        job_id,
                        kind: "org_undo".into(),
                    },
                    Ok(Ok(msg)) => JobEvent::Done {
                        job_id,
                        kind: "org_undo".into(),
                        message: Some(msg),
                    },
                    Ok(Err(e)) => JobEvent::Failed {
                        job_id,
                        kind: "org_undo".into(),
                        error: format!("{e:#}"),
                    },
                };
                let _ = events_run.send(final_event);
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

fn run_undo_job(
    db: &core_db::Db,
    fs_lock: &Arc<Mutex<()>>,
    cancel: &core_jobs::CancelFlag,
    undo_job_id: i64,
    batch_id: i64,
) -> anyhow::Result<String> {
    // Poison từ panic đợt trước không được khóa chết mọi thao tác fs sau này
    let _serialize = fs_lock.lock().unwrap_or_else(|p| p.into_inner());
    recover_pending_ops(db)?;
    let ops = db.pool.with(|c| org::ops_of_batch_for_undo(c, batch_id))?;
    let mut tally = OrgTally::default();
    for op in ops {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        // Trạng thái hiện tại của file (size/mtime giữ nguyên qua move)
        let fid = op.file_id;
        let row = db.pool.with(|c| org::file_size_mtime(c, fid))?;
        let Some((size, mtime)) = row else {
            tally.skip("NOT_PRESENT"); // file đã bị xóa khỏi index sau organize
            continue;
        };
        if Path::new(&op.old_path).exists() {
            tally.skip("TARGET_TAKEN"); // chỗ cũ đã có file khác
            continue;
        }
        // Write-ahead intent cho CHÍNH bước undo (P1 review: crash giữa rename
        // và ghi DB làm op gốc mãi "done", file thì đã về chỗ cũ — không đường
        // nào sửa; có intent thì recovery hoàn tất nốt phần DB).
        let (np, opath) = (op.new_path.clone(), op.old_path.clone());
        let undo_op = db
            .writer
            .exec(move |c| org::insert_org_op(c, undo_job_id, fid, &np, &opath))?;
        // Cross-volume undo cần hash đối chiếu — hash bản hiện tại trước khi copy
        let same_vol = op.new_path.chars().next().map(|c| c.to_ascii_uppercase())
            == op.old_path.chars().next().map(|c| c.to_ascii_uppercase());
        let hash = if same_vol {
            None
        } else {
            core_hash::full_blake3(Path::new(&op.new_path)).ok()
        };
        match move_file_fs(&op.new_path, &op.old_path, size, mtime, hash.as_ref()) {
            Ok(()) => {
                let (op_id, old_path) = (op.id, op.old_path.clone());
                db.writer.exec(move |c| {
                    org::update_file_location(c, fid, &old_path)?;
                    org::mark_org_op_done(c, undo_op)?;
                    org::mark_org_op_undone(c, op_id)?;
                    // Op undo tự đánh dấu undone luôn — batch undo không hiện
                    // như 1 đợt "undo được" trong history (tránh undo-của-undo
                    // rối loạn; muốn redo thì chạy organize lại là ra)
                    org::mark_org_op_undone(c, undo_op)?;
                    Ok(())
                })?;
                tally.moved += 1;
            }
            Err(reason) => {
                tally.skip(reason);
                let _ = db.writer.exec(move |c| org::delete_org_op(c, undo_op));
            }
        }
    }
    Ok(tally.message())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_file(path: &Path, content: &[u8]) -> (i64, i64) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
        let md = fs::metadata(path).unwrap();
        (md.len() as i64, unix_ms(md.modified().ok()))
    }

    /// End-to-end trên fs THẬT (cùng volume, tempdir): organize move đúng chỗ
    /// theo template, journal chốt done, undo trả về nguyên trạng.
    #[test]
    fn organize_and_undo_roundtrip_same_volume() {
        let tmp = tempfile::tempdir().unwrap();
        let db = core_db::Db::open(&tmp.path().join("db")).unwrap();
        let mess = tmp.path().join("mess");
        let lib = tmp.path().join("MyLib");
        fs::create_dir_all(&lib).unwrap();

        let f1 = mess.join("IMG_20190614_153022.jpg");
        let (s1, m1) = write_file(&f1, b"anh mot");

        // Seed index: root + file row (đường dẫn THẬT, size/mtime THẬT)
        let mess_s = mess.to_str().unwrap().to_string();
        let lib_s = lib.to_str().unwrap().to_string();
        db.writer
            .exec({
                let mess_s = mess_s.clone();
                move |c| core_db::ops::upsert_root(c, &mess_s)
            })
            .unwrap();
        let f1_dir = core_db::ops::normalize_path(&mess_s);
        db.writer
            .exec(move |c| {
                let mut cache = std::collections::HashMap::new();
                core_db::ops::upsert_scan_batch(
                    c,
                    1,
                    1,
                    &[core_db::ScanEntry {
                        dir_path: f1_dir,
                        name: "IMG_20190614_153022.jpg".into(),
                        ext: "jpg".into(),
                        kind: 0,
                        size: s1,
                        mtime: m1,
                        attrs: 0,
                        status: 0,
                    }],
                    &mut cache,
                )
            })
            .unwrap();
        db.writer
            .exec({
                let lib_s = lib_s.clone();
                move |c| org::set_library_root(c, &lib_s)
            })
            .unwrap();

        let lock = Arc::new(Mutex::new(()));
        let cancel = core_jobs::CancelFlag::default();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let jid = db
            .writer
            .exec(|c| core_db::ops::insert_job(c, "organize", None))
            .unwrap();
        let msg = run_organize_job(&db, &lock, &cancel, jid, &tx, false).unwrap();
        assert!(msg.starts_with("moved 1"), "ket qua: {msg}");

        // File nằm đúng template {YYYY}\{YYYY}-{MM}\{YYYYMMDD}_{hhmmss}_{hash4}
        assert!(!f1.exists(), "nguon phai bien mat");
        let month_dir = lib.join("2019").join("2019-06");
        let moved: Vec<String> = fs::read_dir(&month_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(moved.len(), 1, "{moved:?}");
        assert!(
            moved[0].starts_with("20190614_153022_") && moved[0].ends_with(".jpg"),
            "{moved:?}"
        );

        // Idempotent: chạy lại = 0 moved (SkipOrganized)
        let msg2 = run_organize_job(&db, &lock, &cancel, jid, &tx, false).unwrap();
        assert!(msg2.starts_with("moved 0"), "lan 2: {msg2}");

        // Undo trả về nguyên trạng
        let undo_jid = db
            .writer
            .exec(|c| core_db::ops::insert_job(c, "org_undo", None))
            .unwrap();
        let msg3 = run_undo_job(&db, &lock, &cancel, undo_jid, jid).unwrap();
        assert!(msg3.starts_with("moved 1"), "undo: {msg3}");
        assert!(f1.exists(), "nguon phai quay lai sau undo");
        assert_eq!(fs::read_dir(&month_dir).unwrap().count(), 0);
    }

    /// P0 review: std::fs::rename trên Windows GHI ĐÈ target — wrapper phải từ chối.
    #[test]
    fn rename_no_replace_refuses_existing_target() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.jpg");
        let b = tmp.path().join("b.jpg");
        write_file(&a, b"noi dung a");
        write_file(&b, b"noi dung b PHAI SONG");
        let (a_s, b_s) = (a.to_str().unwrap(), b.to_str().unwrap());
        assert_eq!(rename_no_replace(a_s, b_s), Err("MOVE_FAILED"));
        assert_eq!(fs::read(&b).unwrap(), b"noi dung b PHAI SONG");
        assert!(a.exists(), "nguon phai con nguyen sau khi bi tu choi");
        // target trống thì rename bình thường (atomic cùng volume)
        let c = tmp.path().join("c.jpg");
        assert!(rename_no_replace(a_s, c.to_str().unwrap()).is_ok());
        assert!(!a.exists());
        assert_eq!(fs::read(&c).unwrap(), b"noi dung a");
    }

    /// Crash recovery: op intent mà nguồn còn nguyên → bỏ intent, không đụng fs.
    #[test]
    fn recovery_drops_stale_intent_when_source_intact() {
        let tmp = tempfile::tempdir().unwrap();
        let db = core_db::Db::open(&tmp.path().join("db")).unwrap();
        let src = tmp.path().join("a.jpg");
        write_file(&src, b"x");
        let jid = db
            .writer
            .exec(|c| core_db::ops::insert_job(c, "organize", None))
            .unwrap();
        let (src_s, dst_s) = (
            src.to_str().unwrap().to_string(),
            tmp.path()
                .join("lib")
                .join("b.jpg")
                .to_str()
                .unwrap()
                .to_string(),
        );
        db.writer
            .exec(move |c| org::insert_org_op(c, jid, 1, &src_s, &dst_s))
            .unwrap();
        recover_pending_ops(&db).unwrap();
        assert!(db.pool.with(org::pending_org_ops).unwrap().is_empty());
        assert!(src.exists());
    }
}
