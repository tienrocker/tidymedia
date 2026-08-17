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
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use core_db::{org, OrgCandidateRow, RootInfo};
use core_ingest::date::{resolve_taken_with, timezone_offset_minutes};
use core_ingest::planner::{
    plan_organize_incremental, ClaimStore, PairInfo, PlanAction, PlanEntry, PlanItem, TargetState,
};
use core_ingest::template::{
    parse_template, RenderCtx, Template, TemplateKind, DEFAULT_DIR_TEMPLATE, DEFAULT_FILE_TEMPLATE,
};
use core_jobs::{JobEvent, JobProgress, Throttle};
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::commands::{blocking, canonicalize_root, err, CmdResult, GateGuard};
use crate::dedup::{fs_matches, unix_ms, volume_supports_recycle};
use crate::state::{AppState, LifoPool};

const CAND_BATCH: i64 = 256;
const PREVIEW_SAMPLE_CAP: usize = 500;

fn preview_temp_owner_pid(name: &str) -> Option<u32> {
    ["org-preview-", "org-claims-"]
        .iter()
        .find_map(|prefix| name.strip_prefix(prefix))?
        .split('-')
        .next()?
        .parse()
        .ok()
}

#[cfg(windows)]
fn process_is_running(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ACCESS_DENIED};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            // An elevated/protected live process may deny the query. Conservatively
            // preserve its plan; cleanup is best-effort and must never delete live work.
            GetLastError() == ERROR_ACCESS_DENIED
        } else {
            let _ = CloseHandle(handle);
            true
        }
    }
}

#[cfg(not(windows))]
fn process_is_running(pid: u32) -> bool {
    pid == std::process::id() || Path::new(&format!("/proc/{pid}")).exists()
}

pub(crate) fn cleanup_stale_preview_files(data_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(data_dir) else {
        return;
    };
    let stale_after = std::time::Duration::from_secs(24 * 60 * 60);
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let owned = (name.starts_with("org-preview-") && name.ends_with(".jsonl"))
            || (name.starts_with("org-claims-") && name.ends_with(".sqlite"));
        if !owned {
            continue;
        }
        // A preview can legitimately remain open for more than 24 hours. PID ownership
        // prevents a second app instance from deleting another live instance's frozen plan.
        if preview_temp_owner_pid(&name).is_some_and(process_is_running) {
            continue;
        }
        let is_stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age >= stale_after);
        if is_stale {
            if let Err(e) = std::fs::remove_file(entry.path()) {
                tracing::warn!(path = %entry.path().display(), error = %e,
                    "failed to remove stale organize preview temp file");
            }
        }
    }
}

pub(crate) fn invalidate_org_preview(state: &AppState) {
    if let Some(cancel) = state
        .org_preview_cancel
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
    {
        cancel.store(true, Ordering::Relaxed);
    }
    *state.org_preview.lock().unwrap_or_else(|p| p.into_inner()) = None;
}

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
    // 2019-06-14 15:30:22, hash + nguồn mẫu — chỉ để user thấy hình dạng kết quả
    let ms = core_ingest::date::days_from_civil(2019, 6, 14) * core_ingest::date::MS_PER_DAY
        + (15 * 3600 + 30 * 60 + 22) * 1000;
    let ctx = RenderCtx::from_taken(ms, "a3f81c92d4e5b6a7a3f81c92d4e5b6a7", Some("Canon EOS R5"))
        .with_source(
            Some(r"Bac Tuan\Tet 2008"),
            Some("Tet 2008"),
            Some("Picture 039"),
        )
        // Toạ độ mẫu: Hồ Hoàn Kiếm. Tra thật qua core-geo chứ không cắm chuỗi
        // cứng — preview phải cho user thấy đúng cái tên họ sẽ nhận được, gồm
        // cả việc tên địa điểm bị bỏ dấu.
        .with_place(core_geo::lookup(21.0287, 105.8524));
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

/// Validate + render ví dụ KHÔNG lưu — UI gọi debounce khi user đang gõ
/// template để thấy kết quả/lỗi realtime.
#[tauri::command]
pub async fn preview_org_template(
    dir_template: String,
    file_template: String,
) -> CmdResult<String> {
    let dir = parse_template(&dir_template, TemplateKind::Dir).map_err(|e| e.to_string())?;
    let file = parse_template(&file_template, TemplateKind::File).map_err(|e| e.to_string())?;
    Ok(sample_render(&dir, &file))
}

#[tauri::command]
pub async fn set_org_settings(
    state: State<'_, AppState>,
    dir_template: String,
    file_template: String,
) -> CmdResult<OrgSettings> {
    if state.recovery_active.load(Ordering::Acquire) {
        return Err("ERR_RECOVERY_BUSY|".into());
    }
    let db = state.db.clone();
    let preview = state.org_preview.clone();
    let preview_cancel = state.org_preview_cancel.clone();
    let result = blocking(move || {
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

#[tauri::command]
pub async fn list_library_roots(
    state: State<'_, AppState>,
) -> CmdResult<Vec<core_db::LibraryRootRow>> {
    let db = state.db.clone();
    blocking(move || db.pool.with(org::list_library_roots).map_err(err)).await
}

#[tauri::command]
pub async fn set_library_root(state: State<'_, AppState>, path: String) -> CmdResult<i64> {
    if state.recovery_active.load(Ordering::Acquire) {
        return Err("ERR_RECOVERY_BUSY|".into());
    }
    let db = state.db.clone();
    let preview = state.org_preview.clone();
    let preview_cancel = state.org_preview_cancel.clone();
    let result = blocking(move || {
        let canonical = canonicalize_root(&path)?;
        db.writer
            .exec(move |c| org::set_library_root(c, &canonical))
            .map_err(err)
    })
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

#[tauri::command]
pub async fn remove_library_root(state: State<'_, AppState>, id: i64) -> CmdResult<()> {
    if state.recovery_active.load(Ordering::Acquire) {
        return Err("ERR_RECOVERY_BUSY|".into());
    }
    let db = state.db.clone();
    let preview = state.org_preview.clone();
    let preview_cancel = state.org_preview_cancel.clone();
    let result = blocking(move || {
        db.writer
            .exec(move |c| org::remove_library_root(c, id))
            .map_err(err)
    })
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

// ---------- plan chung (preview + execute dùng cùng đường) ----------

#[derive(Clone, Serialize, Deserialize)]
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

fn valid_pair_hash(p: &core_db::OrgPairRow) -> Option<Vec<u8>> {
    match (&p.full_hash, p.hashed_size, p.hashed_mtime) {
        (Some(h), Some(s), Some(m)) if s == p.size && m == p.mtime && h.len() == 32 => {
            Some(h.clone())
        }
        _ => None,
    }
}

/// `dir` nằm tại/dưới `base`? Trả phần còn lại đã bỏ '\' đầu ("" nếu trùng).
/// So không phân biệt hoa thường (fold to_uppercase như path_key); slice bằng
/// get(..len) để không panic giữa ký tự unicode nhiều byte.
fn rel_under<'a>(dir: &'a str, base: &str) -> Option<&'a str> {
    let base = base.trim_end_matches('\\');
    if base.is_empty() {
        return None;
    }
    let head = dir.get(..base.len())?;
    if head.to_uppercase() != base.to_uppercase() {
        return None;
    }
    let rest = &dir[base.len()..];
    if rest.is_empty() {
        return Some("");
    }
    if !rest.starts_with('\\') {
        return None; // "E:\images2" không phải con của "E:\images"
    }
    Some(rest.trim_start_matches('\\'))
}

fn build_items(
    cands: &[OrgCandidateRow],
    tz: &TimezoneSetting,
    now_ms: i64,
    lib_root_path: &str,
    watch_roots: &[RootInfo],
) -> (Vec<PlanItem>, HashMap<i64, ItemMeta>) {
    let mut metas = HashMap::new();
    let items = cands
        .iter()
        .map(|c| {
            let name = c.path.rsplit('\\').next().unwrap_or(&c.path);
            // Gốc tính {relpath}: LIB ROOT TRƯỚC watch root — file đã organize
            // phải render target == chính nó (SkipOrganized idempotent). Ưu
            // tiên watch root bao ngoài kho sẽ lồng "Library\Library\..." thêm
            // một tầng mỗi lần chạy. Không thuộc root nào (root đã remove sau
            // scan) → None: {relpath} render rỗng, file về thẳng phần template
            // còn lại thay vì đoán bừa.
            let rel = rel_under(&c.dir_path, lib_root_path).or_else(|| {
                watch_roots
                    .iter()
                    .find_map(|w| rel_under(&c.dir_path, &w.path))
            });
            let rel_dir = rel.filter(|r| !r.is_empty()).map(str::to_string);
            let folder = match rel {
                // File nằm ngay tại root: không lấy tên root làm {folder}
                Some(r) => r
                    .rsplit('\\')
                    .next()
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
                None => (c.dir_path.len() > 3)
                    .then(|| {
                        c.dir_path
                            .rsplit('\\')
                            .next()
                            .unwrap_or_default()
                            .to_string()
                    })
                    .filter(|s| !s.is_empty()),
            };
            let src_name = c.original_name.as_deref().unwrap_or(name);
            let orig_stem = Some(
                match src_name.rsplit_once('.') {
                    Some((stem, _)) if !stem.is_empty() => stem, // ".foo" giữ cả tên
                    _ => src_name,
                }
                .to_string(),
            );
            let r = resolve_taken_with(
                c.taken_at,
                c.date_source,
                name,
                c.mtime,
                &|ms| tz.offset_at(ms),
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
            let pair = c.pair.as_ref().map(|p| {
                let pair_hash = valid_pair_hash(p);
                metas.insert(
                    p.file_id,
                    ItemMeta {
                        size: p.size,
                        mtime: p.mtime,
                        hash: pair_hash.as_deref().and_then(|h| h.try_into().ok()),
                    },
                );
                PairInfo {
                    file_id: p.file_id,
                    path: p.path.clone(),
                    ext: p.ext.to_lowercase(),
                    status: p.status,
                    size: p.size,
                    mtime: p.mtime,
                    hash_hex: pair_hash.as_deref().map(to_hex),
                }
            });
            PlanItem {
                file_id: c.file_id,
                path: c.path.clone(),
                ext: c.ext.to_lowercase(),
                status: c.status,
                taken_ms: r.taken_ms,
                taken_source: r.source,
                hash_hex: hash.as_deref().map(to_hex),
                camera: c.camera.clone(),
                rel_dir,
                folder,
                orig_stem,
                gps: c.gps,
                pair,
            }
        })
        .collect();
    (items, metas)
}

/// Preview is metadata-only. For an existing indexed destination, compare only full hashes
/// that were already cached by an explicit hash job. This restores duplicate-import detection
/// without reading target content and does not depend on candidate iteration order.
fn probe_target(db: &core_db::Db, path: &str, item: &PlanItem) -> anyhow::Result<TargetState> {
    match std::fs::metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(TargetState::Free),
        Err(_) => Ok(TargetState::Occupied),
        Ok(md) => {
            let target_is_main_type = Path::new(path)
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case(&item.ext));
            let Some(source_hash) = item.hash_hex.as_deref().filter(|h| !h.is_empty()) else {
                return Ok(TargetState::Occupied);
            };
            if !target_is_main_type {
                return Ok(TargetState::Occupied);
            }
            let cached = db.pool.with(|c| org::valid_full_hash_at_path(c, path))?;
            Ok(
                if cached.is_some_and(|cached| {
                    // The cached hash only proves index and hash agree. If the destination was
                    // edited outside the app since the last scan, the index is stale and calling
                    // this a duplicate would report an import as already-in-library when it is
                    // not. Trust it only while the on-disk snapshot still matches.
                    md.len() as i64 == cached.size
                        && unix_ms(md.modified().ok()) == cached.mtime
                        && to_hex(&cached.full_hash) == source_hash
                }) {
                    TargetState::SameContent
                } else {
                    TargetState::Occupied
                },
            )
        }
    }
}

/// Preview chỉ được cấp action cho đúng snapshot index. Nếu source đã đổi/mất
/// sau scan, đánh blocked ngay trong plan thay vì để execute mới báo skip.
fn validate_source_snapshots(items: &mut [PlanItem], metas: &HashMap<i64, ItemMeta>) {
    for item in items {
        if item.status == 0 {
            let meta = &metas[&item.file_id];
            if !fs_matches(&item.path, meta.size, meta.mtime) {
                item.status = 1;
            }
        }
    }
}

/// BLAKE3 full nhưng kiểm cờ hủy giữa các chunk 1 MiB: một video vài GB không
/// được làm nút Stop chết cứng hàng phút. `None` = lỗi đọc HOẶC vừa bị hủy —
/// cả hai đều xử lý như nhau: bỏ file đó, lượt sau hash lại.
fn hash_full_interruptible(path: &str, cancel: Option<&core_jobs::CancelFlag>) -> Option<[u8; 32]> {
    match cancel {
        Some(c) => core_hash::full_blake3_cancellable(Path::new(path), c)
            .ok()
            .flatten(),
        None => core_hash::full_blake3(Path::new(path)).ok(),
    }
}

/// Full-hash items required by `{hashN}` or cross-volume verification. Only the explicit,
/// cancellable preparation job calls this; Preview never reads file contents.
#[allow(clippy::too_many_arguments)] // private, 1 call site — context thật của job
fn ensure_required_hashes(
    db: &core_db::Db,
    items: &mut [PlanItem],
    metas: &mut HashMap<i64, ItemMeta>,
    lib_root: &str,
    file_tpl: &Template,
    include_uncertain: bool,
    cancel: Option<&core_jobs::CancelFlag>,
    thumb_pool: Option<&LifoPool>,
) -> anyhow::Result<u64> {
    // Mỗi hash là 1 lượt đọc TOÀN BỘ file — user đang cuộn/nhìn grid thì
    // nhường ổ đĩa trước (HDD bão hòa là grid đen xì). Backlog cũ không tính.
    let yield_to_thumbs = || {
        if let Some(pool) = thumb_pool {
            while pool.active_within(std::time::Duration::from_secs(2))
                && !cancel.is_some_and(|c| c.load(Ordering::Relaxed))
            {
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
        }
    };
    let lib_drive = lib_root.chars().next().map(|c| c.to_ascii_uppercase());
    let mut hashed = 0u64;
    let mut upserts = Vec::new();
    for it in items.iter_mut() {
        if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Ok(hashed);
        }
        let same_vol = it.path.chars().next().map(|c| c.to_ascii_uppercase()) == lib_drive;
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
            continue;
        }
        yield_to_thumbs();
        let Some(h) = hash_full_interruptible(&it.path, cancel) else {
            continue;
        };
        upserts.push(core_db::HashUpsert {
            file_id: it.file_id,
            quick64: None,
            full: Some(h.to_vec()),
            src_mtime: meta.mtime,
            src_size: meta.size,
        });
        it.hash_hex = Some(to_hex(&h));
        metas.get_mut(&it.file_id).expect("item meta").hash = Some(h);
        hashed += 1;
    }
    for it in items.iter_mut() {
        let Some(pair) = it.pair.as_mut() else {
            continue;
        };
        if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Ok(hashed);
        }
        let same_vol = pair.path.chars().next().map(|c| c.to_ascii_uppercase()) == lib_drive;
        let uncertain_skip =
            it.taken_source == core_ingest::date::SRC_MTIME_UNCERTAIN && !include_uncertain;
        if pair.hash_hex.is_some()
            || same_vol
            || it.status != 0
            || pair.status != 0
            || uncertain_skip
        {
            continue;
        }
        let Some(meta) = metas.get(&pair.file_id) else {
            continue;
        };
        if !fs_matches(&pair.path, meta.size, meta.mtime) {
            continue;
        }
        yield_to_thumbs();
        let Some(h) = hash_full_interruptible(&pair.path, cancel) else {
            continue;
        };
        upserts.push(core_db::HashUpsert {
            file_id: pair.file_id,
            quick64: None,
            full: Some(h.to_vec()),
            src_mtime: meta.mtime,
            src_size: meta.size,
        });
        pair.hash_hex = Some(to_hex(&h));
        metas.get_mut(&pair.file_id).expect("pair meta").hash = Some(h);
        hashed += 1;
    }
    if !upserts.is_empty() {
        db.writer
            .exec(move |c| core_db::ops::upsert_hash_batch(c, &upserts))?;
    }
    Ok(hashed)
}

/// Snapshot cả nửa MOV của Live Photo tại preview. Executor không được lấy
/// metadata/hash mới tại thời điểm chạy vì file cùng path có thể đã bị thay thế
/// sau khi user xem plan.
fn snapshot_pair_metas(
    items: &mut [PlanItem],
    metas: &mut HashMap<i64, ItemMeta>,
    cancel: Option<&core_jobs::CancelFlag>,
) {
    for item in items {
        let Some(pair) = item.pair.as_mut() else {
            continue;
        };
        if pair.status != 0 {
            continue;
        }
        if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            pair.status = 1;
            continue;
        }
        let Ok(md) = std::fs::metadata(&pair.path) else {
            pair.status = 1;
            continue;
        };
        if !md.is_file() || core_media::is_cloud_placeholder(&md) {
            pair.status = 1;
            continue;
        }
        let size = md.len() as i64;
        let mtime = unix_ms(md.modified().ok());
        if size != pair.size || mtime != pair.mtime {
            pair.status = 1;
            continue;
        }
        let hash = metas.get(&pair.file_id).and_then(|meta| meta.hash);
        metas.insert(pair.file_id, ItemMeta { size, mtime, hash });
    }
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
    pub preview_id: u64,
    pub total: i64,
    pub moves: i64,
    pub copies: i64,
    pub needs_hash: i64,
    pub skip_organized: i64,
    pub skip_duplicate: i64,
    pub skip_cloud: i64,
    pub skip_uncertain: i64,
    pub skip_pair_blocked: i64,
    pub skip_path_too_long: i64,
    pub skip_other: i64,
    /// chữ cái các ổ có media nhưng CHƯA đặt library root
    pub volumes_missing_root: Vec<String>,
    pub sample: Vec<OrgPreviewRow>,
}

struct PreparedPlan {
    file: tempfile::NamedTempFile,
    work_units: usize,
    deferred_needs_hash: u64,
}

impl PreparedPlan {
    fn len(&self) -> usize {
        self.work_units
    }
}

#[derive(Serialize, Deserialize)]
struct StoredPlanEntry {
    entry: PlanEntry,
    main_meta: ItemMeta,
    pair_meta: Option<ItemMeta>,
}

struct DiskClaimStore {
    conn: rusqlite::Connection,
    _path: tempfile::TempPath,
}

impl DiskClaimStore {
    fn new(parent: &Path) -> anyhow::Result<Self> {
        let prefix = format!("org-claims-{}-", std::process::id());
        let file = tempfile::Builder::new()
            .prefix(&prefix)
            .suffix(".sqlite")
            .tempfile_in(parent)?;
        let path = file.into_temp_path();
        let conn = rusqlite::Connection::open(&path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=OFF;
             PRAGMA synchronous=OFF;
             PRAGMA locking_mode=EXCLUSIVE;
             CREATE TABLE claims(path_key TEXT PRIMARY KEY, hash TEXT);",
        )?;
        Ok(Self { conn, _path: path })
    }
}

impl ClaimStore for DiskClaimStore {
    type Error = rusqlite::Error;

    fn get_claim(&mut self, path_key: &str) -> Result<Option<Option<String>>, Self::Error> {
        use rusqlite::OptionalExtension;
        self.conn
            .query_row(
                "SELECT hash FROM claims WHERE path_key = ?1",
                [path_key],
                |r| r.get(0),
            )
            .optional()
    }

    fn insert_claim(&mut self, path_key: String, hash: Option<String>) -> Result<(), Self::Error> {
        self.conn.execute(
            "INSERT OR REPLACE INTO claims(path_key, hash) VALUES(?1, ?2)",
            rusqlite::params![path_key, hash],
        )?;
        Ok(())
    }
}

pub(crate) struct OrgPreviewTicket {
    id: u64,
    include_uncertain: bool,
    prepared: PreparedPlan,
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
        PlanAction::SkipPathTooLong => "SKIP_PATH_TOO_LONG",
    }
}

fn record_preview_entry(out: &mut OrgPreview, e: &PlanEntry) {
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
        PlanAction::SkipPathTooLong => out.skip_path_too_long += 1,
        _ => out.skip_other += 1,
    }

    let pair_action = e.pair_move.as_ref().map(|(_, old, new)| {
        if same_volume(old, new) {
            PlanAction::Rename
        } else {
            PlanAction::CopyVerify
        }
    });
    if let Some(action) = &pair_action {
        out.total += 1;
        match action {
            PlanAction::Rename => out.moves += 1,
            PlanAction::CopyVerify => out.copies += 1,
            _ => unreachable!("pair move is always rename/copy"),
        }
    }

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
    }
    if let (Some((pid, pold, pnew)), Some(action)) = (&e.pair_move, &pair_action) {
        if out.sample.len() < PREVIEW_SAMPLE_CAP {
            out.sample.push(OrgPreviewRow {
                file_id: *pid,
                old_path: pold.clone(),
                new_path: Some(pnew.clone()),
                action: action_name(action).into(),
            });
        }
    }
}

fn plan_work_units(e: &PlanEntry) -> usize {
    let main = usize::from(matches!(
        e.action,
        PlanAction::Rename | PlanAction::CopyVerify
    ));
    main + usize::from(e.pair_move.is_some())
}

#[tauri::command]
pub async fn org_preview(
    state: State<'_, AppState>,
    include_uncertain: bool,
) -> CmdResult<OrgPreview> {
    if state.recovery_active.load(Ordering::Acquire) {
        return Err("ERR_RECOVERY_BUSY|".into());
    }
    let op_gate = state.index_op_gate.clone();
    let db = state.db.clone();
    let ticket = state.org_preview.clone();
    let seq = state.org_preview_seq.clone();
    let cancel_slot = state.org_preview_cancel.clone();
    let jobs = state.jobs.clone();
    let id = seq.fetch_add(1, Ordering::Relaxed);
    let cancel: core_jobs::CancelFlag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let preflight_ticket = ticket.clone();
    let preflight_cancel_slot = cancel_slot.clone();
    let preflight_cancel = cancel.clone();
    blocking(move || {
        let _op = op_gate.lock().unwrap_or_else(|p| p.into_inner());
        if jobs.active_job_of_kind("scan").is_some()
            || jobs.active_job_of_kind("meta").is_some()
            || jobs.active_job_of_kind("hash").is_some()
            || jobs.active_job_of_kind("org_hash").is_some()
        {
            return Err("ERR_INDEX_BUSY|metadata/hash job is active".into());
        }
        if jobs.active_job_of_kind("organize").is_some()
            || jobs.active_job_of_kind("org_undo").is_some()
        {
            return Err("ERR_ORG_BUSY|".into());
        }
        let mut slot = preflight_cancel_slot
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(previous) = slot.replace(preflight_cancel) {
            previous.store(true, Ordering::Relaxed);
        }
        *preflight_ticket.lock().unwrap_or_else(|p| p.into_inner()) = None;
        Ok(())
    })
    .await?;
    let cancel_run = cancel.clone();
    let result = blocking(move || {
        compute_org_preview(&db, include_uncertain, Some(&cancel_run)).map_err(err)
    })
    .await;
    let mut slot = cancel_slot.lock().unwrap_or_else(|p| p.into_inner());
    let is_current = slot.as_ref().is_some_and(|c| Arc::ptr_eq(c, &cancel));
    if !is_current || cancel.load(Ordering::Relaxed) {
        return Err("ERR_ORG_PREVIEW_CANCELLED|".into());
    }
    let (mut out, prepared) = match result {
        Ok(value) => value,
        Err(e) => {
            *slot = None;
            return Err(e);
        }
    };
    out.preview_id = id;
    *ticket.lock().unwrap_or_else(|p| p.into_inner()) = Some(OrgPreviewTicket {
        id,
        include_uncertain,
        prepared,
    });
    *slot = None;
    Ok(out)
}

#[tauri::command]
pub fn cancel_org_preview(state: State<'_, AppState>) -> bool {
    let slot = state
        .org_preview_cancel
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    slot.as_ref().is_some_and(|cancel| {
        cancel.store(true, Ordering::Relaxed);
        true
    })
}

/// Explicitly prepare the full hashes needed for hash-based filenames and cross-volume
/// copy verification. Keeping this as a normal cancellable job makes Preview metadata-only.
#[tauri::command]
pub async fn start_org_hash_scan(
    state: State<'_, AppState>,
    include_uncertain: bool,
) -> CmdResult<Option<i64>> {
    if state.recovery_active.load(Ordering::Acquire) {
        return Err("ERR_RECOVERY_BUSY|".into());
    }
    let gate = state.hash_start_gate.clone();
    if gate
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        if let Some(id) = state.jobs.active_job_of_kind("org_hash") {
            return Ok(Some(id));
        }
        return Err("ERR_INDEX_BUSY|duplicate hash scan is active".into());
    }
    let guard = GateGuard(gate);
    if let Some(id) = state.jobs.active_job_of_kind("org_hash") {
        return Ok(Some(id));
    }
    if state.jobs.active_job_of_kind("hash").is_some() {
        return Err("ERR_INDEX_BUSY|duplicate hash scan is active".into());
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
        if jobs.active_job_of_kind("hash").is_some()
            || jobs.active_job_of_kind("org_hash").is_some()
        {
            return Err("ERR_INDEX_BUSY|hash job is active".into());
        }
        if jobs.active_job_of_kind("scan").is_some()
            || jobs.active_job_of_kind("organize").is_some()
            || jobs.active_job_of_kind("org_undo").is_some()
        {
            return Err("ERR_INDEX_BUSY|scan/organize is active".into());
        }
        let job_id = db
            .writer
            .exec(|c| core_db::ops::insert_job(c, "org_hash", None))
            .map_err(err)?;
        let (cancel, pause) = jobs.register_pausable(job_id, "org_hash", None);
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
            .name(format!("org-hash-{job_id}"))
            .spawn(move || {
                let events_run = events.clone();
                let cancel_run = cancel.clone();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_org_hash_job(
                        &db,
                        Some(&thumb_pool),
                        &cancel_run,
                        &pause,
                        job_id,
                        &events_run,
                        include_uncertain,
                    )
                }));
                let final_event = match result {
                    Err(_) => JobEvent::Failed {
                        job_id,
                        kind: "org_hash".into(),
                        error: "ERR_INTERNAL|organize hash thread panicked".into(),
                    },
                    Ok(Ok(_)) if cancel.load(Ordering::Relaxed) => JobEvent::Cancelled {
                        job_id,
                        kind: "org_hash".into(),
                    },
                    Ok(Ok(message)) => JobEvent::Done {
                        job_id,
                        kind: "org_hash".into(),
                        message: Some(message),
                    },
                    Ok(Err(e)) => JobEvent::Failed {
                        job_id,
                        kind: "org_hash".into(),
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
fn run_org_hash_job(
    db: &core_db::Db,
    thumb_pool: Option<&LifoPool>,
    cancel: &core_jobs::CancelFlag,
    pause: &core_jobs::PauseFlag,
    job_id: i64,
    events: &crossbeam_channel::Sender<JobEvent>,
    include_uncertain: bool,
) -> anyhow::Result<String> {
    let (_, file_tpl, ..) = load_templates(db)?;
    let tz = get_tz(db)?;
    let now = now_ms();
    let roots = db.pool.with(org::list_library_roots)?;
    if roots.is_empty() {
        anyhow::bail!("ERR_ORG_NO_LIBRARY_ROOT|");
    }
    let watch_roots = db.pool.with(core_db::ops::list_roots)?;
    let total = roots.iter().try_fold(0u64, |sum, root| {
        db.pool
            .with(|c| org::count_org_candidates(c, root.volume_id))
            .map(|count| sum + count as u64)
    })?;
    // UI thấy job ngay khi khởi động, kể cả khi đang yield cho thumb
    let _ = events.send(JobEvent::Progress(JobProgress {
        job_id,
        kind: "org_hash".into(),
        done: 0,
        total: Some(total.max(1)),
        message: Some("hash".into()),
    }));
    let mut done = 0u64;
    let mut hashed = 0u64;
    let mut throttle = Throttle::new(200);
    let mut cancelled = false;
    // Job này cũng full-hash hàng chục nghìn file — nhóm trùng nó lộ ra hiện
    // dần trên tab Dedup thay vì đợi tới cuối (dùng chung helper với hash job).
    let mut dups = crate::dedup::DupRefresher::new(crate::dedup::DUP_REFRESH_MS);
    'roots: for root in &roots {
        let mut cursor = 0i64;
        loop {
            if !crate::dedup::hold_if_paused(
                pause,
                cancel,
                job_id,
                "org_hash",
                done,
                Some(total.max(done)),
                Some("hash"),
                events,
            ) {
                // Hashes computed so far are already persisted; break out instead of
                // returning so the duplicate groups they revealed still get rebuilt.
                cancelled = true;
                break 'roots;
            }
            let cands = db
                .pool
                .with(|c| org::select_org_candidates(c, root.volume_id, cursor, CAND_BATCH))?;
            let Some(last) = cands.last() else { break };
            cursor = last.file_id;
            done += cands.len() as u64;
            let (mut items, mut metas) = build_items(&cands, &tz, now, &root.path, &watch_roots);
            validate_source_snapshots(&mut items, &metas);
            snapshot_pair_metas(&mut items, &mut metas, Some(cancel));
            let wrote = ensure_required_hashes(
                db,
                &mut items,
                &mut metas,
                &root.path,
                &file_tpl,
                include_uncertain,
                Some(cancel),
                thumb_pool,
            )?;
            hashed += wrote;
            dups.mark(wrote as usize);
            dups.refresh_if_due(db, events)?;
            if throttle.ready() {
                let _ = events.send(JobEvent::Progress(JobProgress {
                    job_id,
                    kind: "org_hash".into(),
                    done,
                    total: Some(total.max(done)),
                    message: Some("hash".into()),
                }));
            }
        }
    }
    if hashed > 0 {
        let (groups, waste) = db.writer.exec(core_db::ops::rebuild_dup_groups)?;
        let _ = events.send(JobEvent::DupGroupsChanged {
            kind: 0,
            groups,
            waste,
        });
    }
    if cancelled {
        return Ok(String::new());
    }
    Ok(format!("prepared {hashed} hashes"))
}

fn compute_org_preview(
    db: &core_db::Db,
    include_uncertain: bool,
    cancel: Option<&core_jobs::CancelFlag>,
) -> anyhow::Result<(OrgPreview, PreparedPlan)> {
    let (dir_tpl, file_tpl, ..) = load_templates(db)?;
    let tz = get_tz(db)?;
    let now = now_ms();
    let roots = db.pool.with(org::list_library_roots)?;
    if roots.is_empty() {
        anyhow::bail!("ERR_ORG_NO_LIBRARY_ROOT|");
    }
    let mut out = OrgPreview::default();
    let plan_dir = db
        .path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("ERR_INTERNAL|index db has no parent"))?;
    let plan_prefix = format!("org-preview-{}-", std::process::id());
    let plan_file = tempfile::Builder::new()
        .prefix(&plan_prefix)
        .suffix(".jsonl")
        .tempfile_in(plan_dir)?;
    let mut plan_out = BufWriter::new(plan_file.reopen()?);
    let mut prepared_work_units = 0usize;

    // Ổ nào có root index nhưng chưa có library root → báo UI
    let watch_roots = db.pool.with(core_db::ops::list_roots)?;
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
        if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            anyhow::bail!("ERR_ORG_PREVIEW_CANCELLED|");
        }
        // Stream từng page, nhưng giữ claim set xuyên toàn volume. Exact plan
        // được ghi JSONL vào app-data thay vì nhân đôi hàng triệu path trong RAM.
        let mut claimed = DiskClaimStore::new(plan_dir)?;
        let mut cursor = 0i64;
        loop {
            if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                anyhow::bail!("ERR_ORG_PREVIEW_CANCELLED|");
            }
            let cands = db
                .pool
                .with(|c| org::select_org_candidates(c, root.volume_id, cursor, CAND_BATCH))?;
            let Some(last) = cands.last() else { break };
            cursor = last.file_id;
            let (mut items, mut metas) = build_items(&cands, &tz, now, &root.path, &watch_roots);
            validate_source_snapshots(&mut items, &metas);
            snapshot_pair_metas(&mut items, &mut metas, cancel);
            let target_probe_error = std::cell::RefCell::new(None::<anyhow::Error>);
            let plan = plan_organize_incremental(
                &items,
                &root.path,
                &dir_tpl,
                &file_tpl,
                include_uncertain,
                &|path, item| {
                    if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                        TargetState::Occupied
                    } else {
                        match probe_target(db, path, item) {
                            Ok(state) => state,
                            Err(e) => {
                                if target_probe_error.borrow().is_none() {
                                    *target_probe_error.borrow_mut() = Some(e);
                                }
                                TargetState::Occupied
                            }
                        }
                    }
                },
                &mut claimed,
            )?;
            if let Some(e) = target_probe_error.into_inner() {
                return Err(e.context("lookup cached hash for organize target"));
            }
            if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                anyhow::bail!("ERR_ORG_PREVIEW_CANCELLED|");
            }
            for e in plan {
                record_preview_entry(&mut out, &e);

                let work_units = plan_work_units(&e);
                if work_units == 0 {
                    continue;
                }

                let main_meta = metas.remove(&e.file_id).ok_or_else(|| {
                    anyhow::anyhow!("ERR_INTERNAL|missing preview meta for {}", e.file_id)
                })?;
                let pair_meta = e
                    .pair_move
                    .as_ref()
                    .and_then(|(pid, _, _)| metas.remove(pid));
                serde_json::to_writer(
                    &mut plan_out,
                    &StoredPlanEntry {
                        entry: e,
                        main_meta,
                        pair_meta,
                    },
                )?;
                plan_out.write_all(b"\n")?;
                prepared_work_units += work_units;
            }
        }
    }
    plan_out.flush()?;
    plan_out.get_ref().sync_all()?;
    drop(plan_out);
    let deferred_needs_hash = out.needs_hash.max(0) as u64;
    Ok((
        out,
        PreparedPlan {
            file: plan_file,
            work_units: prepared_work_units,
            deferred_needs_hash,
        },
    ))
}

// ---------- execute ----------

#[derive(Clone)]
struct TimezoneSetting {
    name: Option<String>,
    fallback_offset_minutes: i32,
}

impl TimezoneSetting {
    fn offset_at(&self, epoch_ms: i64) -> i32 {
        self.name
            .as_deref()
            .and_then(|name| timezone_offset_minutes(name, epoch_ms))
            .unwrap_or(self.fallback_offset_minutes)
    }
}

fn get_tz(db: &core_db::Db) -> anyhow::Result<TimezoneSetting> {
    db.pool.with(|c| {
        let name = core_db::ops::kv_get(c, "timezone")?;
        let fallback_offset_minutes = core_db::ops::kv_get(c, "tz_offset_minutes")?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        Ok(TimezoneSetting {
            name,
            fallback_offset_minutes,
        })
    })
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
    preview_id: u64,
) -> CmdResult<Option<i64>> {
    if state.recovery_active.load(Ordering::Acquire) {
        return Err("ERR_RECOVERY_BUSY|".into());
    }
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
    let op_gate = state.index_op_gate.clone();
    let preview_ticket = state.org_preview.clone();
    blocking(move || {
        let _gate = guard;
        let _op = op_gate.lock().unwrap_or_else(|p| p.into_inner());
        if jobs.active_job_of_kind("hash").is_some()
            || jobs.active_job_of_kind("org_hash").is_some()
        {
            return Err("ERR_INDEX_BUSY|hash job is active".into());
        }
        if jobs.active_job_of_kind("scan").is_some()
            || jobs.active_job_of_kind("organize").is_some()
            || jobs.active_job_of_kind("org_undo").is_some()
        {
            return Err("ERR_ORG_BUSY|".into());
        }
        // Lấy atomically đúng plan đã hiển thị. Không recompute/query candidates
        // sau confirm: file scan thêm chỉ xuất hiện ở preview kế tiếp; target bị
        // chiếm thì executor fail-safe, tuyệt đối không tự đổi destination.
        {
            let slot = preview_ticket.lock().unwrap_or_else(|p| p.into_inner());
            let valid = slot
                .as_ref()
                .is_some_and(|p| p.id == preview_id && p.include_uncertain == include_uncertain);
            if !valid {
                return Err("ERR_ORG_PREVIEW_STALE|missing or mismatched preview".into());
            }
        }
        // Keep the valid preview ticket when the database is temporarily unavailable; the
        // frontend can retry instead of paying for another full preview.
        let job_id = db
            .writer
            .exec(|c| core_db::ops::insert_job(c, "organize", None))
            .map_err(err)?;
        let prepared = {
            let mut slot = preview_ticket.lock().unwrap_or_else(|p| p.into_inner());
            let valid = slot
                .as_ref()
                .is_some_and(|p| p.id == preview_id && p.include_uncertain == include_uncertain);
            if !valid {
                db.writer.exec_async(move |c| {
                    core_db::ops::finish_job(c, job_id, "failed", Some("ERR_ORG_PREVIEW_STALE"))
                });
                return Err("ERR_ORG_PREVIEW_STALE|preview invalidated while starting".into());
            }
            slot.take().expect("validated preview ticket").prepared
        };
        let cancel = jobs.register(job_id, "organize", None);
        let events = jobs.sender();
        let writer_cleanup = db.writer.clone();
        std::thread::Builder::new()
            .name(format!("organize-{job_id}"))
            .spawn(move || {
                let events_run = events.clone();
                let cancel_run = cancel.clone();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_prepared_organize_job(
                        &db,
                        &lock,
                        &cancel_run,
                        job_id,
                        &events_run,
                        prepared,
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
    skipped: HashMap<&'static str, u64>,
}

impl OrgTally {
    fn skip(&mut self, reason: &'static str) {
        *self.skipped.entry(reason).or_insert(0) += 1;
    }
    fn skip_n(&mut self, reason: &'static str, count: u64) {
        if count > 0 {
            *self.skipped.entry(reason).or_insert(0) += count;
        }
    }
    fn message(&self) -> String {
        let mut m = format!("moved {}", self.moved);
        let mut reasons: Vec<_> = self.skipped.iter().collect();
        reasons.sort_by(|a, b| b.1.cmp(a.1));
        for (r, n) in reasons {
            m.push_str(&format!(", {n} {r}"));
        }
        m
    }
}

fn run_prepared_organize_job(
    db: &core_db::Db,
    fs_lock: &Arc<Mutex<()>>,
    cancel: &core_jobs::CancelFlag,
    job_id: i64,
    events: &crossbeam_channel::Sender<JobEvent>,
    prepared: PreparedPlan,
) -> anyhow::Result<String> {
    // Poison từ panic đợt trước không được khóa chết mọi thao tác fs sau này
    let _serialize = fs_lock.lock().unwrap_or_else(|p| p.into_inner());
    recover_pending_ops(db, Some(cancel), None)?;

    let total = prepared.len() as u64;
    let mut tally = OrgTally::default();
    // NeedsHash rows intentionally stay out of JSONL so a million deferred files do
    // not bloat the frozen executable plan. Preserve their aggregate in the result.
    tally.skip_n("NEEDS_HASH", prepared.deferred_needs_hash);
    let mut throttle = Throttle::new(200);
    let plan_reader = BufReader::new(prepared.file.reopen()?);
    let mut done = 0u64;
    for line in plan_reader.lines() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let stored: StoredPlanEntry = serde_json::from_str(&line?)?;
        let e = &stored.entry;
        done += plan_work_units(e) as u64;
        match e.action {
            PlanAction::Rename | PlanAction::CopyVerify => {
                execute_move(
                    db,
                    job_id,
                    e,
                    &stored.main_meta,
                    stored.pair_meta.as_ref(),
                    &mut tally,
                );
            }
            // Ảnh đã đúng chỗ nhưng MOV của cặp còn kẹt lại (đợt trước
            // fail giữa 2 nửa) → move nốt MOV để hàn cặp
            PlanAction::SkipOrganized if e.pair_move.is_some() => {
                execute_pair_fixup(db, job_id, e, stored.pair_meta.as_ref(), &mut tally);
            }
            ref a => tally.skip(action_name(a)),
        }
        if throttle.ready() {
            let _ = events.send(JobEvent::Progress(JobProgress {
                job_id,
                kind: "organize".into(),
                done,
                total: Some(total),
                message: None,
            }));
        }
    }
    db.writer
        .exec(|c| core_db::ops::refresh_all_root_counts(c))?;
    Ok(tally.message())
}

#[cfg(test)]
fn run_organize_job(
    db: &core_db::Db,
    fs_lock: &Arc<Mutex<()>>,
    cancel: &core_jobs::CancelFlag,
    job_id: i64,
    events: &crossbeam_channel::Sender<JobEvent>,
    include_uncertain: bool,
) -> anyhow::Result<String> {
    let (_, prepared) = compute_org_preview(db, include_uncertain, Some(cancel))?;
    run_prepared_organize_job(db, fs_lock, cancel, job_id, events, prepared)
}

/// Crash recovery for write-ahead intents. Source-only means the move never happened and the
/// intent can be dropped. Destination-only is accepted only after snapshot/hash verification.
/// If both copies exist, retain the intent for diagnosis instead of silently accepting a partial
/// cross-volume move or rebinding the index to an ambiguous destination.
pub(crate) fn recover_pending_ops(
    db: &core_db::Db,
    cancel: Option<&core_jobs::CancelFlag>,
    mut progress: Option<&mut dyn FnMut(u64, u64)>,
) -> anyhow::Result<()> {
    let pending = db.pool.with(org::pending_org_ops)?;
    let total = pending.len() as u64;
    if let Some(report) = progress.as_mut() {
        report(0, total);
    }
    let mut index_changed = false;
    for (index, op) in pending.into_iter().enumerate() {
        if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            break;
        }
        let src_exists = Path::new(&op.old_path).exists();
        let dst_exists = Path::new(&op.new_path).exists();
        let (op_id, file_id, new_path) = (op.id, op.file_id, op.new_path.clone());
        if !src_exists && dst_exists {
            let verify = db.pool.with(|c| org::file_verify_context(c, file_id))?;
            let mut cancelled_during_hash = false;
            let matches = verify.is_some_and(|expected| {
                if !fs_matches(&new_path, expected.size, expected.mtime) {
                    return false;
                }
                match expected.full_hash {
                    Some(hash) => match cancel {
                        Some(flag) => {
                            match core_hash::full_blake3_cancellable(Path::new(&new_path), flag) {
                                Ok(Some(actual)) => actual.as_slice() == hash.as_slice(),
                                Ok(None) => {
                                    cancelled_during_hash = true;
                                    false
                                }
                                Err(_) => false,
                            }
                        }
                        None => core_hash::full_blake3(Path::new(&new_path))
                            .map(|actual| actual.as_slice() == hash.as_slice())
                            .unwrap_or(false),
                    },
                    None => true,
                }
            });
            if cancelled_during_hash {
                break;
            }
            if matches {
                db.writer.exec(move |c| {
                    org::update_file_location(c, file_id, &new_path)?;
                    org::mark_org_op_done(c, op_id)?;
                    Ok(())
                })?;
                index_changed = true;
            } else {
                // Không tự bind DB row vào một file chỉ vì path tồn tại. Giữ
                // journal để chẩn đoán/rescan, nhưng không chặn app khởi động.
                tracing::error!(
                    op_id,
                    old = %op.old_path,
                    new = %op.new_path,
                    "organize recovery ambiguous: destination failed verification"
                );
                db.writer.exec(move |c| {
                    org::mark_org_op_recovery_failed(c, op_id, "DESTINATION_VERIFY_FAILED")
                })?;
            }
        } else if src_exists && !dst_exists {
            // Filesystem operation never happened; source is unchanged and there is no target.
            db.writer.exec(move |c| org::delete_org_op(c, op_id))?;
        } else if src_exists && dst_exists {
            tracing::error!(
                op_id,
                old = %op.old_path,
                new = %op.new_path,
                "organize recovery found both source and destination; keeping intent"
            );
            db.writer.exec(move |c| {
                org::mark_org_op_recovery_failed(c, op_id, "BOTH_SOURCE_AND_DESTINATION_EXIST")
            })?;
        } else {
            tracing::error!(
                op_id,
                old = %op.old_path,
                new = %op.new_path,
                "organize recovery ambiguous: both source and destination missing"
            );
            db.writer.exec(move |c| {
                org::mark_org_op_recovery_failed(c, op_id, "BOTH_SOURCE_AND_DESTINATION_MISSING")
            })?;
        }
        if let Some(report) = progress.as_mut() {
            report(index as u64 + 1, total);
        }
    }
    if index_changed {
        db.writer
            .exec(|c| core_db::ops::refresh_all_root_counts(c))?;
    }
    Ok(())
}

fn parent_of(path: &str) -> Option<&Path> {
    Path::new(path).parent()
}

fn same_volume(a: &str, b: &str) -> bool {
    a.chars().next().map(|c| c.to_ascii_uppercase())
        == b.chars().next().map(|c| c.to_ascii_uppercase())
}

static COPY_TEMP_SEQ: AtomicU64 = AtomicU64::new(1);

/// Reserve a temp path with create-new semantics, so cleanup can only remove a file we own.
fn create_copy_temp(new_path: &str) -> std::io::Result<(PathBuf, File)> {
    let target = Path::new(new_path);
    let parent = target
        .parent()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no parent"))?;
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    for _ in 0..128 {
        let seq = COPY_TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(
            ".{name}.tidymedia-{}-{seq}.tmp",
            std::process::id()
        ));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not reserve a unique organize temp file",
    ))
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
    if same_volume(old_path, new_path) {
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
    let (tmp, mut tmp_file) = create_copy_temp(new_path).map_err(|_| "COPY_FAILED")?;
    let copy = (|| -> std::io::Result<()> {
        let mut source = File::open(old_path)?;
        std::io::copy(&mut source, &mut tmp_file)?;
        tmp_file.sync_all()?;
        drop(tmp_file);
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
    match core_hash::full_blake3(&tmp) {
        Ok(h) if &h == expected => {}
        _ => {
            let _ = std::fs::remove_file(&tmp);
            return Err("VERIFY_FAILED");
        }
    }
    let Some(tmp_s) = tmp.to_str() else {
        let _ = std::fs::remove_file(&tmp);
        return Err("COPY_FAILED");
    };
    if rename_no_replace(tmp_s, new_path).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return Err("MOVE_FAILED"); // target vừa bị chiếm — bỏ, không ghi đè
    }
    let trash_result = trash::delete(old_path);
    if !Path::new(old_path).exists() {
        if let Err(e) = trash_result {
            tracing::warn!(old = %old_path, error = %e,
                "organize trash reported an error but source is gone; accepting effective move");
        }
        return Ok(());
    }
    if trash_result.is_ok() {
        tracing::error!(old = %old_path,
            "organize trash reported success but source still exists; rolling destination back");
    }
    let dst_drive = new_path.chars().next().unwrap_or('?').to_ascii_uppercase();
    if volume_supports_recycle(dst_drive) && trash::delete(new_path).is_ok() {
        return Err("TRASH_FAILED_ROLLED_BACK");
    }
    tracing::error!(old = %old_path, new = %new_path,
        "organize source trash failed and destination rollback failed; keeping intent");
    Err("SOURCE_TRASH_FAILED_COPY_KEPT")
}

fn index_matches_meta(db: &core_db::Db, file_id: i64, meta: &ItemMeta) -> anyhow::Result<bool> {
    Ok(db
        .pool
        .with(|c| org::file_size_mtime(c, file_id))?
        .is_some_and(|(size, mtime)| size == meta.size && mtime == meta.mtime))
}

fn execute_move(
    db: &core_db::Db,
    batch_id: i64,
    e: &PlanEntry,
    meta: &ItemMeta,
    pair_meta: Option<&ItemMeta>,
    tally: &mut OrgTally,
) {
    let Some(new_path) = e.new_path.clone() else {
        return;
    };
    match index_matches_meta(db, e.file_id, meta) {
        Ok(true) => {}
        Ok(false) => {
            tally.skip("INDEX_CHANGED");
            return;
        }
        Err(_) => {
            tally.skip("DB_ERROR");
            return;
        }
    }

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
            if reason != "SOURCE_TRASH_FAILED_COPY_KEPT" {
                let _ = db.writer.exec(move |c| org::delete_org_op(c, op_a));
            }
            if let Some(b) = op_b {
                let _ = db.writer.exec(move |c| org::delete_org_op(c, b));
            }
            return;
        }
    }

    // Pair đi CÙNG stem — chỉ move khi ảnh đã move xong
    if let (Some((pid, pold, pnew)), Some(op_b)) = (pair, op_b) {
        let Some(meta) = pair_meta else {
            tally.skip("CHANGED_ON_DISK");
            let _ = db.writer.exec(move |c| org::delete_org_op(c, op_b));
            return;
        };
        move_pair(db, op_b, pid, &pold, &pnew, meta, tally);
    }
}

/// Move MOV của cặp Live Photo (journal op đã insert sẵn từ caller).
fn move_pair(
    db: &core_db::Db,
    op_b: i64,
    pid: i64,
    pold: &str,
    pnew: &str,
    meta: &ItemMeta,
    tally: &mut OrgTally,
) {
    match index_matches_meta(db, pid, meta) {
        Ok(true) => {}
        Ok(false) => {
            tally.skip("INDEX_CHANGED");
            let _ = db.writer.exec(move |c| org::delete_org_op(c, op_b));
            return;
        }
        Err(_) => {
            tally.skip("DB_ERROR");
            let _ = db.writer.exec(move |c| org::delete_org_op(c, op_b));
            return;
        }
    }
    match move_file_fs(pold, pnew, meta.size, meta.mtime, meta.hash.as_ref()) {
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
            if reason != "SOURCE_TRASH_FAILED_COPY_KEPT" {
                let _ = db.writer.exec(move |c| org::delete_org_op(c, op_b));
            }
        }
    }
}

/// Ảnh đã SkipOrganized nhưng planner phát hiện MOV chưa nằm cạnh (cặp bị xé
/// từ đợt trước) → chỉ move MOV. Journal write-ahead như mọi op khác.
fn execute_pair_fixup(
    db: &core_db::Db,
    batch_id: i64,
    e: &PlanEntry,
    pair_meta: Option<&ItemMeta>,
    tally: &mut OrgTally,
) {
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
    let Some(meta) = pair_meta else {
        tally.skip("CHANGED_ON_DISK");
        let _ = db.writer.exec(move |c| org::delete_org_op(c, op_b));
        return;
    };
    move_pair(db, op_b, pid, &pold, &pnew, meta, tally);
}

// ---------- undo ----------

#[tauri::command]
pub async fn list_org_batches(state: State<'_, AppState>) -> CmdResult<Vec<core_db::OrgBatchRow>> {
    let db = state.db.clone();
    blocking(move || db.pool.with(org::list_org_batches).map_err(err)).await
}

#[tauri::command]
pub async fn undo_org_batch(state: State<'_, AppState>, batch_id: i64) -> CmdResult<Option<i64>> {
    if state.recovery_active.load(Ordering::Acquire) {
        return Err("ERR_RECOVERY_BUSY|".into());
    }
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
    let op_gate = state.index_op_gate.clone();
    blocking(move || {
        let _gate = guard;
        let _op = op_gate.lock().unwrap_or_else(|p| p.into_inner());
        if jobs.active_job_of_kind("hash").is_some()
            || jobs.active_job_of_kind("org_hash").is_some()
        {
            return Err("ERR_INDEX_BUSY|hash job is active".into());
        }
        if jobs.active_job_of_kind("scan").is_some()
            || jobs.active_job_of_kind("organize").is_some()
            || jobs.active_job_of_kind("org_undo").is_some()
        {
            return Err("ERR_ORG_BUSY|".into());
        }
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
    recover_pending_ops(db, Some(cancel), None)?;
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
        let hash = if same_volume(&op.new_path, &op.old_path) {
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
                if reason != "SOURCE_TRASH_FAILED_COPY_KEPT" {
                    let _ = db.writer.exec(move |c| org::delete_org_op(c, undo_op));
                }
            }
        }
    }
    db.writer
        .exec(|c| core_db::ops::refresh_all_root_counts(c))?;
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

    #[test]
    fn stale_preview_cleanup_only_removes_owned_old_files() {
        let tmp = tempfile::tempdir().unwrap();
        let stale_preview = tmp.path().join("org-preview-old.jsonl");
        let active_preview = tmp
            .path()
            .join(format!("org-preview-{}-active.jsonl", std::process::id()));
        let dead_preview = tmp.path().join("org-preview-4294967295-dead.jsonl");
        let fresh_claims = tmp.path().join("org-claims-fresh.sqlite");
        let unrelated = tmp.path().join("user-data.jsonl");
        fs::write(&stale_preview, b"old").unwrap();
        fs::write(&active_preview, b"active").unwrap();
        fs::write(&dead_preview, b"dead").unwrap();
        fs::write(&fresh_claims, b"fresh").unwrap();
        fs::write(&unrelated, b"mine").unwrap();
        filetime::set_file_mtime(&stale_preview, filetime::FileTime::from_unix_time(1, 0)).unwrap();
        filetime::set_file_mtime(&active_preview, filetime::FileTime::from_unix_time(1, 0))
            .unwrap();
        filetime::set_file_mtime(&dead_preview, filetime::FileTime::from_unix_time(1, 0)).unwrap();

        cleanup_stale_preview_files(tmp.path());

        assert!(!stale_preview.exists());
        assert!(active_preview.exists());
        assert!(!dead_preview.exists());
        assert!(fresh_claims.exists());
        assert!(unrelated.exists());
    }

    fn seed_root(db: &core_db::Db, root: &Path, library: &Path) {
        fs::create_dir_all(library).unwrap();
        let root_s = root.to_str().unwrap().to_string();
        db.writer
            .exec({
                let root_s = root_s.clone();
                move |c| core_db::ops::upsert_root(c, &root_s)
            })
            .unwrap();
        let library_s = library.to_str().unwrap().to_string();
        db.writer
            .exec(move |c| org::set_library_root(c, &library_s))
            .unwrap();
    }

    fn prepare_hashes(db: &core_db::Db) -> String {
        let cancel = core_jobs::CancelFlag::default();
        let pause = core_jobs::PauseFlag::default();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let job_id = db
            .writer
            .exec(|c| core_db::ops::insert_job(c, "org_hash", None))
            .unwrap();
        run_org_hash_job(db, None, &cancel, &pause, job_id, &tx, false).unwrap()
    }

    fn index_entries(db: &core_db::Db, root: &Path, rows: Vec<(String, i64, i64)>) {
        let dir_path = core_db::ops::normalize_path(root.to_str().unwrap());
        let entries: Vec<core_db::ScanEntry> = rows
            .into_iter()
            .map(|(name, size, mtime)| core_db::ScanEntry {
                dir_path: dir_path.clone(),
                name,
                ext: "jpg".into(),
                kind: 0,
                size,
                mtime,
                attrs: 0,
                status: 0,
            })
            .collect();
        db.writer
            .exec(move |c| {
                let mut cache = HashMap::new();
                core_db::ops::upsert_scan_batch(c, 1, 1, &entries, &mut cache)
            })
            .unwrap();
    }

    #[test]
    fn pair_fixup_is_counted_and_sampled_as_an_action() {
        let mut preview = OrgPreview::default();
        record_preview_entry(
            &mut preview,
            &PlanEntry {
                file_id: 1,
                action: PlanAction::SkipOrganized,
                old_path: r"D:\lib\photo.jpg".into(),
                new_path: None,
                pair_move: Some((2, r"D:\mess\photo.mov".into(), r"D:\lib\photo.mov".into())),
            },
        );
        assert_eq!(preview.total, 2);
        assert_eq!(preview.skip_organized, 1);
        assert_eq!(preview.moves, 1);
        assert_eq!(preview.sample.len(), 1);
        assert_eq!(preview.sample[0].file_id, 2);
        assert_eq!(preview.sample[0].action, "MOVE");
    }

    #[test]
    fn copy_temp_never_reuses_legacy_deterministic_path() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target.jpg");
        let legacy = tmp.path().join("target.jpg.tidymedia-tmp");
        fs::write(&legacy, b"user data").unwrap();

        let (owned, file) = create_copy_temp(target.to_str().unwrap()).unwrap();
        drop(file);
        assert_ne!(owned, legacy);
        assert_eq!(fs::read(&legacy).unwrap(), b"user data");
        fs::remove_file(owned).unwrap();
    }

    #[test]
    fn preview_does_not_read_or_create_missing_content_hashes() {
        let tmp = tempfile::tempdir().unwrap();
        let db = core_db::Db::open(&tmp.path().join("db")).unwrap();
        let source = tmp.path().join("source");
        let library = tmp.path().join("library");
        seed_root(&db, &source, &library);
        let file = source.join("IMG_20190614_153022.jpg");
        let (size, mtime) = write_file(&file, b"content that must not be read by preview");
        index_entries(
            &db,
            &source,
            vec![("IMG_20190614_153022.jpg".into(), size, mtime)],
        );

        let (preview, prepared) = compute_org_preview(&db, false, None).unwrap();
        assert_eq!(preview.needs_hash, 1);
        assert_eq!(preview.moves, 0);
        assert_eq!(prepared.len(), 0);
        assert_eq!(prepared.deferred_needs_hash, 1);
        let candidate = db
            .pool
            .with(|c| org::select_org_candidates(c, 1, 0, 10))
            .unwrap()
            .remove(0);
        assert!(candidate.full_hash.is_none());

        assert!(prepare_hashes(&db).starts_with("prepared 1"));
        let (ready, prepared) = compute_org_preview(&db, false, None).unwrap();
        assert_eq!(ready.moves, 1);
        assert_eq!(ready.needs_hash, 0);
        assert_eq!(prepared.len(), 1);
        assert_eq!(prepared.deferred_needs_hash, 0);
    }

    #[test]
    fn preview_claims_destinations_across_candidate_pages() {
        let tmp = tempfile::tempdir().unwrap();
        let db = core_db::Db::open(&tmp.path().join("db")).unwrap();
        let source = tmp.path().join("source");
        let library = tmp.path().join("library");
        seed_root(&db, &source, &library);

        let mut rows = Vec::new();
        for i in 0..=CAND_BATCH {
            let name = format!("IMG_20190614_153022_{i:03}.jpg");
            let (size, mtime) = write_file(&source.join(&name), b"identical bytes");
            rows.push((name, size, mtime));
        }
        index_entries(&db, &source, rows);
        assert!(prepare_hashes(&db).starts_with("prepared 257"));

        let (preview, prepared) = compute_org_preview(&db, false, None).unwrap();
        assert_eq!(prepared.len(), 1);
        assert_eq!(preview.moves, 1);
        assert_eq!(preview.skip_duplicate, CAND_BATCH);
    }

    #[test]
    fn preview_detects_indexed_duplicate_even_when_import_candidate_is_older() {
        let tmp = tempfile::tempdir().unwrap();
        let db = core_db::Db::open(&tmp.path().join("db")).unwrap();
        let source = tmp.path().join("source");
        let library = tmp.path().join("library");
        seed_root(&db, &source, &library);

        // Index/hash the import first so it has the smaller file id. A fix that only
        // lets SkipOrganized claim destinations cannot handle this ordering.
        let import = source.join("IMG_20190614_153022.jpg");
        let content = b"same bytes already present in the library";
        let (size, mtime) = write_file(&import, content);
        index_entries(
            &db,
            &source,
            vec![("IMG_20190614_153022.jpg".into(), size, mtime)],
        );
        assert!(prepare_hashes(&db).starts_with("prepared 1"));

        let (first, first_plan) = compute_org_preview(&db, false, None).unwrap();
        let target = PathBuf::from(
            first
                .sample
                .first()
                .and_then(|row| row.new_path.as_deref())
                .expect("initial preview target"),
        );
        drop(first_plan);
        let (target_size, target_mtime) = write_file(&target, content);
        index_entries(
            &db,
            target.parent().unwrap(),
            vec![(
                target.file_name().unwrap().to_string_lossy().into_owned(),
                target_size,
                target_mtime,
            )],
        );
        assert!(prepare_hashes(&db).starts_with("prepared 1"));

        let candidates = db
            .pool
            .with(|c| org::select_org_candidates(c, 1, 0, 10))
            .unwrap();
        assert!(candidates[0].path.ends_with("IMG_20190614_153022.jpg"));
        assert_eq!(candidates[1].path, target.to_string_lossy());

        let (preview, prepared) = compute_org_preview(&db, false, None).unwrap();
        assert_eq!(preview.moves + preview.copies, 0);
        assert_eq!(preview.skip_duplicate, 1);
        assert_eq!(preview.skip_organized, 1);
        assert_eq!(prepared.len(), 0);
    }

    #[test]
    fn preview_does_not_call_a_stale_indexed_destination_a_duplicate() {
        let tmp = tempfile::tempdir().unwrap();
        let db = core_db::Db::open(&tmp.path().join("db")).unwrap();
        let source = tmp.path().join("source");
        let library = tmp.path().join("library");
        seed_root(&db, &source, &library);

        let import = source.join("IMG_20190614_153022.jpg");
        let content = b"bytes that used to be the library copy";
        let (size, mtime) = write_file(&import, content);
        index_entries(
            &db,
            &source,
            vec![("IMG_20190614_153022.jpg".into(), size, mtime)],
        );
        assert!(prepare_hashes(&db).starts_with("prepared 1"));

        let (first, first_plan) = compute_org_preview(&db, false, None).unwrap();
        let target = PathBuf::from(
            first
                .sample
                .first()
                .and_then(|row| row.new_path.as_deref())
                .expect("initial preview target"),
        );
        drop(first_plan);
        let (target_size, target_mtime) = write_file(&target, content);
        index_entries(
            &db,
            target.parent().unwrap(),
            vec![(
                target.file_name().unwrap().to_string_lossy().into_owned(),
                target_size,
                target_mtime,
            )],
        );
        assert!(prepare_hashes(&db).starts_with("prepared 1"));

        // The library copy is edited outside the app and never rescanned, so index and
        // hash still describe the OLD content. Reporting the import as "already in the
        // library" here would tell the user to delete a source that is not stored yet.
        std::thread::sleep(std::time::Duration::from_millis(10));
        write_file(&target, b"a different picture after an external edit");

        let (preview, _prepared) = compute_org_preview(&db, false, None).unwrap();
        assert_eq!(
            preview.skip_duplicate, 0,
            "hash cache da stale, khong duoc coi la trung"
        );
        assert_eq!(preview.moves + preview.copies, 1);
    }

    #[test]
    fn execute_uses_frozen_preview_and_ignores_new_candidates() {
        let tmp = tempfile::tempdir().unwrap();
        let db = core_db::Db::open(&tmp.path().join("db")).unwrap();
        let source = tmp.path().join("source");
        let library = tmp.path().join("library");
        seed_root(&db, &source, &library);

        let first = source.join("IMG_20190614_153022.jpg");
        let (size, mtime) = write_file(&first, b"first");
        index_entries(
            &db,
            &source,
            vec![("IMG_20190614_153022.jpg".into(), size, mtime)],
        );
        assert!(prepare_hashes(&db).starts_with("prepared 1"));
        let (_, prepared) = compute_org_preview(&db, false, None).unwrap();

        // Candidate này xuất hiện SAU preview; execute không được tự nới scope.
        let late = source.join("IMG_20200102_030405.jpg");
        let (size, mtime) = write_file(&late, b"late arrival");
        index_entries(
            &db,
            &source,
            vec![("IMG_20200102_030405.jpg".into(), size, mtime)],
        );

        let lock = Arc::new(Mutex::new(()));
        let cancel = core_jobs::CancelFlag::default();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let jid = db
            .writer
            .exec(|c| core_db::ops::insert_job(c, "organize", None))
            .unwrap();
        let msg = run_prepared_organize_job(&db, &lock, &cancel, jid, &tx, prepared).unwrap();
        assert!(msg.starts_with("moved 1"), "{msg}");
        assert!(!first.exists());
        assert!(
            late.exists(),
            "file thêm sau preview phải nằm nguyên tại nguồn"
        );
    }

    #[test]
    fn execute_rejects_preview_after_watched_root_was_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let db = core_db::Db::open(&tmp.path().join("db")).unwrap();
        let source = tmp.path().join("source");
        let library = tmp.path().join("library");
        seed_root(&db, &source, &library);

        let first = source.join("IMG_20190614_153022.jpg");
        let (size, mtime) = write_file(&first, b"first");
        index_entries(
            &db,
            &source,
            vec![("IMG_20190614_153022.jpg".into(), size, mtime)],
        );
        assert!(prepare_hashes(&db).starts_with("prepared 1"));
        let (_, prepared) = compute_org_preview(&db, false, None).unwrap();
        let root_id = db.pool.with(core_db::ops::list_roots).unwrap()[0].id;
        db.writer
            .exec(move |c| core_db::ops::remove_root_chunked(c, root_id))
            .unwrap();

        let lock = Arc::new(Mutex::new(()));
        let cancel = core_jobs::CancelFlag::default();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let jid = db
            .writer
            .exec(|c| core_db::ops::insert_job(c, "organize", None))
            .unwrap();
        let msg = run_prepared_organize_job(&db, &lock, &cancel, jid, &tx, prepared).unwrap();
        assert!(msg.contains("INDEX_CHANGED"), "{msg}");
        assert!(
            first.exists(),
            "stale preview must not move an unindexed file"
        );
    }

    #[test]
    fn execute_rejects_live_pair_replaced_after_preview() {
        let tmp = tempfile::tempdir().unwrap();
        let db = core_db::Db::open(&tmp.path().join("db")).unwrap();
        let source = tmp.path().join("source");
        let library = tmp.path().join("library");
        seed_root(&db, &source, &library);

        let image = source.join("IMG_20190614_153022.heic");
        let movie = source.join("IMG_20190614_153022.mov");
        let (image_size, image_mtime) = write_file(&image, b"image bytes");
        let (movie_size, movie_mtime) = write_file(&movie, b"original movie");
        let dir_path = core_db::ops::normalize_path(source.to_str().unwrap());
        db.writer
            .exec({
                let dir_path = dir_path.clone();
                move |c| {
                    let mut cache = HashMap::new();
                    core_db::ops::upsert_scan_batch(
                        c,
                        1,
                        1,
                        &[
                            core_db::ScanEntry {
                                dir_path: dir_path.clone(),
                                name: "IMG_20190614_153022.heic".into(),
                                ext: "heic".into(),
                                kind: 0,
                                size: image_size,
                                mtime: image_mtime,
                                attrs: 0,
                                status: 0,
                            },
                            core_db::ScanEntry {
                                dir_path,
                                name: "IMG_20190614_153022.mov".into(),
                                ext: "mov".into(),
                                kind: 1,
                                size: movie_size,
                                mtime: movie_mtime,
                                attrs: 0,
                                status: 0,
                            },
                        ],
                        &mut cache,
                    )
                }
            })
            .unwrap();
        let root_s = source.to_str().unwrap().to_string();
        db.writer
            .exec(move |c| core_db::ops::pair_live_photos(c, &root_s))
            .unwrap();
        assert!(prepare_hashes(&db).starts_with("prepared 1"));
        let (_, prepared) = compute_org_preview(&db, false, None).unwrap();

        // Thay MOV sau preview: main image có thể move, MOV mới tuyệt đối không.
        fs::write(&movie, b"replacement movie with different size").unwrap();
        let lock = Arc::new(Mutex::new(()));
        let cancel = core_jobs::CancelFlag::default();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let jid = db
            .writer
            .exec(|c| core_db::ops::insert_job(c, "organize", None))
            .unwrap();
        let msg = run_prepared_organize_job(&db, &lock, &cancel, jid, &tx, prepared).unwrap();
        assert!(msg.contains("CHANGED_ON_DISK"), "{msg}");
        assert!(movie.exists(), "MOV thay sau preview phải được giữ nguyên");
        assert_eq!(
            fs::read(&movie).unwrap(),
            b"replacement movie with different size"
        );
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
            .exec(|c| core_db::ops::refresh_all_root_counts(c))
            .unwrap();
        assert_eq!(
            db.pool.with(core_db::ops::list_roots).unwrap()[0].file_count,
            1
        );
        db.writer
            .exec({
                let lib_s = lib_s.clone();
                move |c| org::set_library_root(c, &lib_s)
            })
            .unwrap();
        assert!(prepare_hashes(&db).starts_with("prepared 1"));

        let lock = Arc::new(Mutex::new(()));
        let cancel = core_jobs::CancelFlag::default();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let jid = db
            .writer
            .exec(|c| core_db::ops::insert_job(c, "organize", None))
            .unwrap();
        let msg = run_organize_job(&db, &lock, &cancel, jid, &tx, false).unwrap();
        assert!(msg.starts_with("moved 1"), "ket qua: {msg}");
        assert_eq!(
            db.pool.with(core_db::ops::list_roots).unwrap()[0].file_count,
            0
        );

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
        assert_eq!(
            db.pool.with(core_db::ops::list_roots).unwrap()[0].file_count,
            1
        );
    }

    #[test]
    fn build_items_source_fields_shapes() {
        fn cand(id: i64, dir: &str, name: &str) -> OrgCandidateRow {
            OrgCandidateRow {
                file_id: id,
                path: format!("{dir}\\{name}"),
                dir_path: dir.into(),
                original_name: None,
                ext: "jpg".into(),
                kind: 0,
                size: 1,
                mtime: 1,
                status: 0,
                taken_at: None,
                date_source: None,
                camera: None,
                gps: None,
                full_hash: None,
                hashed_size: None,
                hashed_mtime: None,
                pair: None,
            }
        }
        let tz = TimezoneSetting {
            name: None,
            fallback_offset_minutes: 0,
        };
        let roots = vec![RootInfo {
            id: 1,
            volume_id: 1,
            path: r"E:\images".into(),
            last_scan_at: None,
            file_count: 0,
        }];
        let (items, _) = build_items(
            &[
                cand(1, r"E:\images", "a.jpg"),
                cand(2, r"E:\images\Bac Tuan\Tet", "b.jpg"),
                cand(3, r"E:\other\x", "c.jpg"),
                cand(4, r"E:\Library\Bac Tuan", "d.jpg"),
                cand(5, r"E:\IMAGES\Bac Tuan", "e.jpg"), // khác case với root
            ],
            &tz,
            0,
            r"E:\Library",
            &roots,
        );
        // Ngay tại watch root: không lấy tên root làm folder
        assert_eq!(items[0].rel_dir, None);
        assert_eq!(items[0].folder, None);
        assert_eq!(items[0].orig_stem.as_deref(), Some("a"));
        // Trong cây watch root
        assert_eq!(items[1].rel_dir.as_deref(), Some(r"Bac Tuan\Tet"));
        assert_eq!(items[1].folder.as_deref(), Some("Tet"));
        // Ngoài mọi root: relpath rỗng, folder = leaf
        assert_eq!(items[2].rel_dir, None);
        assert_eq!(items[2].folder.as_deref(), Some("x"));
        // Trong LIB root: rel tính từ lib root (bất biến chống lồng Library\Library)
        assert_eq!(items[3].rel_dir.as_deref(), Some("Bac Tuan"));
        // Prefix match không phân biệt hoa thường
        assert_eq!(items[4].rel_dir.as_deref(), Some("Bac Tuan"));
    }

    /// {relpath}+{name}: giữ nguyên cây thư mục + tên gốc; chạy lần 2 = 0 move
    /// và path tuyệt đối KHÔNG đổi (tripwire chống lồng); undo về nguyên trạng.
    #[test]
    fn organize_relpath_keeps_structure_idempotent_and_undoable() {
        let tmp = tempfile::tempdir().unwrap();
        let db = core_db::Db::open(&tmp.path().join("db")).unwrap();
        let source = tmp.path().join("source");
        let library = tmp.path().join("library");
        seed_root(&db, &source, &library);
        db.writer
            .exec(|c| {
                core_db::ops::kv_set(c, "org_dir_template", "{relpath}")?;
                core_db::ops::kv_set(c, "org_file_template", "{name}")?;
                Ok(())
            })
            .unwrap();

        let nested = source.join("Bac Tuan").join("Tet 2008");
        let f = nested.join("Picture 039.jpg");
        let (size, mtime) = write_file(&f, b"anh bac Tuan");
        index_entries(&db, &nested, vec![("Picture 039.jpg".into(), size, mtime)]);

        let lock = Arc::new(Mutex::new(()));
        let cancel = core_jobs::CancelFlag::default();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let jid = db
            .writer
            .exec(|c| core_db::ops::insert_job(c, "organize", None))
            .unwrap();
        // "Picture 039" không có ngày trong tên → mtime-uncertain → opt-in
        let msg = run_organize_job(&db, &lock, &cancel, jid, &tx, true).unwrap();
        assert!(msg.starts_with("moved 1"), "{msg}");

        let dest = library
            .join("Bac Tuan")
            .join("Tet 2008")
            .join("Picture 039.jpg");
        assert!(!f.exists(), "nguon phai bien mat");
        assert!(dest.exists(), "phai giu cay thu muc + ten goc: {dest:?}");

        let msg2 = run_organize_job(&db, &lock, &cancel, jid, &tx, true).unwrap();
        assert!(msg2.starts_with("moved 0"), "lan 2: {msg2}");
        assert!(
            dest.exists(),
            "lan 2 file phai nam nguyen cho cu, khong duoc long them tang"
        );

        let undo_jid = db
            .writer
            .exec(|c| core_db::ops::insert_job(c, "org_undo", None))
            .unwrap();
        let msg3 = run_undo_job(&db, &lock, &cancel, undo_jid, jid).unwrap();
        assert!(msg3.starts_with("moved 1"), "undo: {msg3}");
        assert!(f.exists(), "undo phai tra ve cho cu");
        assert!(!dest.exists());
    }

    /// Đổi template SAU khi đã organize: move đúng 1 lần theo template mới
    /// (rel tính từ lib root nên giữ được cấp ngày), {name} khôi phục stem gốc
    /// từ original_name; lần 3 = 0 move.
    #[test]
    fn organize_retemplate_moves_once_and_restores_original_stem() {
        let tmp = tempfile::tempdir().unwrap();
        let db = core_db::Db::open(&tmp.path().join("db")).unwrap();
        let source = tmp.path().join("source");
        let library = tmp.path().join("library");
        seed_root(&db, &source, &library);

        let f = source.join("IMG_20190614_153022.jpg");
        let (size, mtime) = write_file(&f, b"anh co ngay trong ten");
        index_entries(
            &db,
            &source,
            vec![("IMG_20190614_153022.jpg".into(), size, mtime)],
        );
        assert!(prepare_hashes(&db).starts_with("prepared 1"));

        let lock = Arc::new(Mutex::new(()));
        let cancel = core_jobs::CancelFlag::default();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let jid = db
            .writer
            .exec(|c| core_db::ops::insert_job(c, "organize", None))
            .unwrap();
        let msg = run_organize_job(&db, &lock, &cancel, jid, &tx, false).unwrap();
        assert!(msg.starts_with("moved 1"), "{msg}");
        let month_dir = library.join("2019").join("2019-06");
        assert_eq!(fs::read_dir(&month_dir).unwrap().count(), 1);

        // Đổi sang giữ-cấu-trúc + tên gốc rồi chạy lại
        db.writer
            .exec(|c| {
                core_db::ops::kv_set(c, "org_dir_template", "{relpath}")?;
                core_db::ops::kv_set(c, "org_file_template", "{name}")?;
                Ok(())
            })
            .unwrap();
        let msg2 = run_organize_job(&db, &lock, &cancel, jid, &tx, false).unwrap();
        assert!(msg2.starts_with("moved 1"), "retemplate: {msg2}");
        let restored = month_dir.join("IMG_20190614_153022.jpg");
        assert!(
            restored.exists(),
            "rel tu lib root giu cap ngay, {{name}} khoi phuc stem goc"
        );

        let msg3 = run_organize_job(&db, &lock, &cancel, jid, &tx, false).unwrap();
        assert!(msg3.starts_with("moved 0"), "lan 3: {msg3}");

        let orig: Option<String> = db
            .pool
            .with(|c| -> anyhow::Result<Option<String>> {
                Ok(c.query_row("SELECT original_name FROM files LIMIT 1", [], |r| r.get(0))?)
            })
            .unwrap();
        assert_eq!(orig.as_deref(), Some("IMG_20190614_153022.jpg"));
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
        recover_pending_ops(&db, None, None).unwrap();
        assert!(db.pool.with(org::pending_org_ops).unwrap().is_empty());
        assert!(src.exists());
    }

    #[test]
    fn recovery_cancel_keeps_unprocessed_intent_and_reports_total() {
        let tmp = tempfile::tempdir().unwrap();
        let db = core_db::Db::open(&tmp.path().join("db")).unwrap();
        let src = tmp.path().join("a.jpg");
        write_file(&src, b"x");
        let jid = db
            .writer
            .exec(|c| core_db::ops::insert_job(c, "organize", None))
            .unwrap();
        let src_s = src.to_string_lossy().into_owned();
        let dst_s = tmp.path().join("b.jpg").to_string_lossy().into_owned();
        db.writer
            .exec(move |c| org::insert_org_op(c, jid, 1, &src_s, &dst_s))
            .unwrap();

        let cancel = core_jobs::CancelFlag::default();
        cancel.store(true, Ordering::Relaxed);
        let mut updates = Vec::new();
        let mut report = |done, total| updates.push((done, total));
        recover_pending_ops(&db, Some(&cancel), Some(&mut report)).unwrap();

        assert_eq!(updates, [(0, 1)]);
        assert_eq!(db.pool.with(org::pending_org_ops).unwrap().len(), 1);
    }

    #[test]
    fn recovery_keeps_intent_when_cross_volume_partial_has_two_copies() {
        let tmp = tempfile::tempdir().unwrap();
        let db = core_db::Db::open(&tmp.path().join("db")).unwrap();
        let src = tmp.path().join("a.jpg");
        let dst = tmp.path().join("lib").join("b.jpg");
        write_file(&src, b"same bytes");
        write_file(&dst, b"same bytes");
        let jid = db
            .writer
            .exec(|c| core_db::ops::insert_job(c, "organize", None))
            .unwrap();
        let (src_s, dst_s) = (
            src.to_str().unwrap().to_string(),
            dst.to_str().unwrap().to_string(),
        );
        let op_id = db
            .writer
            .exec(move |c| org::insert_org_op(c, jid, 1, &src_s, &dst_s))
            .unwrap();

        recover_pending_ops(&db, None, None).unwrap();

        assert!(db.pool.with(org::pending_org_ops).unwrap().is_empty());
        assert_eq!(
            db.pool
                .with(|c| org::org_op_recovery_error(c, op_id))
                .unwrap()
                .as_deref(),
            Some("BOTH_SOURCE_AND_DESTINATION_EXIST")
        );
        assert!(src.exists());
        assert!(dst.exists());
    }

    /// Không được nhận nhầm một file có sẵn ở target là kết quả của move bị crash.
    #[test]
    fn recovery_preserves_ambiguous_destination_intent() {
        let tmp = tempfile::tempdir().unwrap();
        let db = core_db::Db::open(&tmp.path().join("db")).unwrap();
        let dst = tmp.path().join("lib").join("occupied.jpg");
        write_file(&dst, b"unrelated occupant");
        let jid = db
            .writer
            .exec(|c| core_db::ops::insert_job(c, "organize", None))
            .unwrap();
        let old_path = tmp.path().join("missing.jpg").to_str().unwrap().to_string();
        let new_path = dst.to_str().unwrap().to_string();
        let op_id = db
            .writer
            .exec(move |c| org::insert_org_op(c, jid, 999_999, &old_path, &new_path))
            .unwrap();

        recover_pending_ops(&db, None, None).unwrap();

        assert!(db.pool.with(org::pending_org_ops).unwrap().is_empty());
        assert_eq!(
            db.pool
                .with(|c| org::org_op_recovery_error(c, op_id))
                .unwrap()
                .as_deref(),
            Some("DESTINATION_VERIFY_FAILED")
        );
        assert_eq!(fs::read(&dst).unwrap(), b"unrelated occupant");
    }
}
