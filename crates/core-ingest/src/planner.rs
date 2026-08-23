//! Planner thuần cho organize: quyết định từng file đi đâu, KHÔNG đụng fs/DB.
//! Executor inject trạng thái đích qua callback `target_state` (pattern giống
//! plan_delete của M4) nên toàn bộ invariant test được headless.
//!
//! Live Photo: caller KHÔNG đưa MOV có live_pair vào items - thông tin MOV
//! gắn trên entry của ảnh (`pair`), cặp luôn cùng stem, cùng số phận.

use crate::template::{RenderCtx, Template};

pub trait ClaimStore {
    type Error;

    fn get_claim(&mut self, path_key: &str) -> Result<Option<Option<String>>, Self::Error>;
    fn insert_claim(&mut self, path_key: String, hash: Option<String>) -> Result<(), Self::Error>;
}

impl ClaimStore for std::collections::HashMap<String, Option<String>> {
    type Error = std::convert::Infallible;

    fn get_claim(&mut self, path_key: &str) -> Result<Option<Option<String>>, Self::Error> {
        Ok(self.get(path_key).cloned())
    }

    fn insert_claim(&mut self, path_key: String, hash: Option<String>) -> Result<(), Self::Error> {
        self.insert(path_key, hash);
        Ok(())
    }
}

/// Trạng thái đường dẫn đích (executor trả lời từ fs)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetState {
    Free,
    /// đã có file y hệt nội dung (full hash khớp) tại đúng path này
    SameContent,
    /// có file khác chiếm chỗ
    Occupied,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PlanAction {
    /// cùng volume: fs::rename atomic
    Rename,
    /// khác volume: copy -> flush -> verify BLAKE3 -> trash nguồn
    CopyVerify,
    /// thiếu full hash (cần cho tên hoặc verify) - explicit hash job rồi preview lại
    NeedsHash,
    SkipCloud,
    SkipMissing,
    SkipUncertain,
    /// đích đã có file y hệt nội dung -> để dedup xử, không move
    SkipDuplicate,
    /// đã nằm đúng chỗ đúng tên
    SkipOrganized,
    /// cặp Live Photo không di chuyển được (MOV missing/cloud) -> giữ cả cặp
    SkipPairBlocked,
    /// hết đường tránh đụng độ (thực tế không xảy ra)
    SkipCollision,
    /// mọi ứng viên tên đều cho đường dẫn đích vượt MAX_PATH — cần template
    /// ngắn hơn hoặc library root nông hơn
    SkipPathTooLong,
}

/// MOV đi kèm ảnh Live Photo
#[derive(Debug, Clone)]
pub struct PairInfo {
    pub file_id: i64,
    pub path: String,
    /// ext lowercase không dot (thường "mov")
    pub ext: String,
    pub status: i64,
    /// Snapshot index dùng để từ chối MOV bị thay ngoài app trước/sau preview.
    pub size: i64,
    pub mtime: i64,
    pub hash_hex: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PlanItem {
    pub file_id: i64,
    /// đường dẫn đầy đủ hiện tại
    pub path: String,
    /// ext lowercase không dot
    pub ext: String,
    /// 0 = ảnh, 1 = video (khớp `files.kind`) — token {kind}.
    ///
    /// MOV của cặp Live Photo KHÔNG có PlanItem riêng: nó đi theo `pair` và
    /// nhận đường dẫn dựng từ ctx của tấm ảnh, nên `{kind}` của nó là "Photos".
    /// Đó là hành vi ĐÚNG — tách đôi cặp Live Photo là hỏng cặp.
    pub kind: i64,
    pub status: i64,
    pub taken_ms: i64,
    /// date::SRC_* - SRC_MTIME_UNCERTAIN bị loại trừ khi include_uncertain
    pub taken_source: i64,
    /// hex BLAKE3 full (64 ký tự) nếu đã hash + còn hợp lệ
    pub hash_hex: Option<String>,
    pub camera: Option<String>,
    /// thư mục nguồn tương đối root (token {relpath}) — None = không xác định
    pub rel_dir: Option<String>,
    /// tên thư mục cha trực tiếp (token {folder})
    pub folder: Option<String>,
    /// stem tên file gốc (token {name})
    pub orig_stem: Option<String>,
    /// Toạ độ nơi chụp cho token {place}/{province}/{district}/{ward}/{country}.
    /// None = ảnh không có GPS → các token đó render rỗng.
    pub gps: Option<(f64, f64)>,
    pub pair: Option<PairInfo>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PlanEntry {
    pub file_id: i64,
    pub action: PlanAction,
    pub old_path: String,
    /// Some với Rename/CopyVerify; NeedsHash không hứa path khi chưa biết fingerprint
    pub new_path: Option<String>,
    /// MOV của cặp Live Photo: (file_id, old_path, new_path)
    pub pair_move: Option<(i64, String, String)>,
}

fn drive_of(path: &str) -> Option<char> {
    let mut c = path.chars();
    let d = c.next()?;
    if c.next() == Some(':') {
        Some(d.to_ascii_uppercase())
    } else {
        None
    }
}

fn join_target(lib_root: &str, segs: &[String], file: &str, ext: &str) -> String {
    let root = lib_root.trim_end_matches(['\\', '/']);
    let mut out = String::from(root);
    for s in segs {
        out.push('\\');
        out.push_str(s);
    }
    out.push('\\');
    out.push_str(file);
    if !ext.is_empty() {
        out.push('.');
        out.push_str(ext);
    }
    out
}

/// So sánh path không phân biệt hoa thường — CÙNG phép fold Unicode với claim
/// key (to_uppercase), không phải ASCII-only: tên thư mục có dấu ("Bác Tuấn"
/// vs "BÁC TUẤN") phải nhận ra nhau ở check SkipOrganized, lệch là file bị
/// re-plan sang tên escalate dù đang nằm đúng chỗ.
fn eq_ci(a: &str, b: &str) -> bool {
    if a.eq_ignore_ascii_case(b) {
        return true;
    }
    a.to_uppercase() == b.to_uppercase()
}

/// Lập kế hoạch organize cho 1 nhóm file cùng đích `lib_root`.
/// `target_state` trả trạng thái 1 path đích trên fs; claim nội bộ batch do
/// planner tự quản (2 file cùng batch không bao giờ được cấp cùng đích).
pub fn plan_organize(
    items: &[PlanItem],
    lib_root: &str,
    dir_tpl: &Template,
    file_tpl: &Template,
    include_uncertain: bool,
    target_state: &dyn Fn(&str, &PlanItem) -> TargetState,
) -> Vec<PlanEntry> {
    let mut claimed = std::collections::HashMap::new();
    match plan_organize_incremental(
        items,
        lib_root,
        dir_tpl,
        file_tpl,
        include_uncertain,
        target_state,
        &mut claimed,
    ) {
        Ok(plan) => plan,
        Err(never) => match never {},
    }
}

/// Giống `plan_organize`, nhưng giữ claim set do caller cấp để có thể stream
/// nhiều page mà không cấp trùng destination ở ranh giới page.
pub fn plan_organize_incremental<C: ClaimStore + ?Sized>(
    items: &[PlanItem],
    lib_root: &str,
    dir_tpl: &Template,
    file_tpl: &Template,
    include_uncertain: bool,
    target_state: &dyn Fn(&str, &PlanItem) -> TargetState,
    claimed: &mut C,
) -> Result<Vec<PlanEntry>, C::Error> {
    // path đích (UPPERCASE) -> hash_hex của file đã claim (để nhận SkipDuplicate nội bộ batch)
    let mut out = Vec::with_capacity(items.len());
    let lib_drive = drive_of(lib_root);

    for it in items {
        let entry = |action: PlanAction, new_path: Option<String>| PlanEntry {
            file_id: it.file_id,
            action,
            old_path: it.path.clone(),
            new_path,
            pair_move: None,
        };
        if it.status == 2 {
            out.push(entry(PlanAction::SkipCloud, None));
            continue;
        }
        if it.status != 0 {
            out.push(entry(PlanAction::SkipMissing, None));
            continue;
        }
        if it.taken_source == crate::date::SRC_MTIME_UNCERTAIN && !include_uncertain {
            out.push(entry(PlanAction::SkipUncertain, None));
            continue;
        }
        if let Some(p) = &it.pair {
            if p.status != 0 {
                out.push(entry(PlanAction::SkipPairBlocked, None));
                continue;
            }
        }
        let same_vol = drive_of(&it.path) == lib_drive && lib_drive.is_some();
        // hash cần cho: token {hashN} trong tên, và verify khi copy xuyên volume
        let pair_needs_hash = it
            .pair
            .as_ref()
            .is_some_and(|pair| drive_of(&pair.path) != lib_drive && pair.hash_hex.is_none());
        let needs_hash =
            (it.hash_hex.is_none() && (file_tpl.has_hash || !same_vol)) || pair_needs_hash;
        let hash_hex = it.hash_hex.clone().unwrap_or_default();
        let ctx = RenderCtx::from_taken(it.taken_ms, &hash_hex, it.camera.as_deref())
            .with_source(
                it.rel_dir.as_deref(),
                it.folder.as_deref(),
                it.orig_stem.as_deref(),
            )
            // Tra MỘT lần cho mỗi file: bên dưới render lại tới 100 lượt để né
            // đụng độ tên, mà mỗi lượt tra là quét một dải trong 170k điểm.
            .with_place(
                it.gps
                    .map(|(lat, lon)| core_geo::lookup(lat, lon))
                    .unwrap_or_default(),
            )
            .with_kind(it.kind);
        let segs = dir_tpl.render_dir(&ctx);
        if needs_hash {
            // đường dự kiến chỉ để hiển thị dry-run
            out.push(entry(PlanAction::NeedsHash, None));
            continue;
        }

        // Ứng viên tên: hash mặc định -> hash8 -> hash16 -> suffix đếm
        let mut cands: Vec<String> = Vec::new();
        cands.push(file_tpl.render_file(&ctx, None));
        if file_tpl.has_hash {
            for n in [8usize, 16] {
                let c = file_tpl.render_file(&ctx, Some(n));
                if !cands.contains(&c) {
                    cands.push(c);
                }
            }
        }
        let last = cands.last().unwrap().clone();
        for i in 2..=99u32 {
            cands.push(format!("{last}_{i}"));
        }

        let mut planned: Option<PlanEntry> = None;
        let mut saw_too_long = false;
        for name in &cands {
            let target = join_target(lib_root, &segs, name, &it.ext);
            let pair_target = it
                .pair
                .as_ref()
                .map(|p| join_target(lib_root, &segs, name, &p.ext));
            // đã nằm đúng chỗ (kể cả tên hash8/16 từ lần escalate trước)?
            if eq_ci(&it.path, &target) {
                // Claim cả file đã nằm đúng kho. Điều này làm các candidate đến sau
                // nhận ra cùng content mà không phải đọc lại file trong Preview.
                claimed.insert_claim(target.to_uppercase(), it.hash_hex.clone())?;
                let mut e = entry(PlanAction::SkipOrganized, None);
                // Ảnh đã đúng chỗ nhưng MOV của cặp CHƯA theo (đợt trước fail
                // giữa 2 nửa cặp) → move nốt MOV, không thì cặp xé vĩnh viễn:
                // ảnh mãi SkipOrganized còn MOV bị ẩn khỏi candidates.
                if let (Some(p), Some(pt)) = (&it.pair, &pair_target) {
                    if !eq_ci(&p.path, pt) {
                        claimed.insert_claim(pt.to_uppercase(), None)?;
                        e.pair_move = Some((p.file_id, p.path.clone(), pt.clone()));
                    }
                }
                planned = Some(e);
                break;
            }
            // MAX_PATH: đích quá dài thì thử ứng viên tên khác (KHÔNG break —
            // độ dài không đơn điệu: {hash16} mặc định DÀI HƠN bản escalate
            // hash8). Hết thang → SkipPathTooLong, lý do thật hiện ở preview
            // thay vì MKDIR_FAILED/MOVE_FAILED mờ mịt lúc execute.
            const MAX_TARGET_UTF16: usize = 259; // MAX_PATH 260 gồm NUL
            let too_long = target.encode_utf16().count() > MAX_TARGET_UTF16
                || pair_target
                    .as_deref()
                    .is_some_and(|pt| pt.encode_utf16().count() > MAX_TARGET_UTF16);
            if too_long {
                saw_too_long = true;
                continue;
            }
            let key = target.to_uppercase();
            let claimed_here = claimed.get_claim(&key)?;
            let state = match claimed_here {
                Some(h) => {
                    // Hash rỗng (chưa hash, template không cần) KHÔNG bao giờ
                    // được coi là cùng nội dung — 2 file chưa biết ruột mà trùng
                    // tên phải escalate chứ không phải SkipDuplicate giả.
                    if !hash_hex.is_empty() && h.as_deref() == Some(hash_hex.as_str()) {
                        TargetState::SameContent
                    } else {
                        TargetState::Occupied
                    }
                }
                None => target_state(&target, it),
            };
            match state {
                TargetState::SameContent => {
                    planned = Some(entry(PlanAction::SkipDuplicate, Some(target)));
                    break;
                }
                TargetState::Occupied => continue,
                TargetState::Free => {}
            }
            // cặp Live Photo: đích của MOV cũng phải trống (cùng stem, escalate cùng nhau)
            if let (Some(p), Some(pt)) = (&it.pair, &pair_target) {
                if !eq_ci(&p.path, pt) {
                    let pkey = pt.to_uppercase();
                    let pstate = if claimed.get_claim(&pkey)?.is_some() {
                        TargetState::Occupied
                    } else {
                        target_state(pt, it)
                    };
                    if pstate != TargetState::Free {
                        continue; // escalate cả cặp
                    }
                }
            }
            let action = if same_vol {
                PlanAction::Rename
            } else {
                PlanAction::CopyVerify
            };
            claimed.insert_claim(key, Some(hash_hex.clone()))?;
            let mut e = entry(action, Some(target));
            if let (Some(p), Some(pt)) = (&it.pair, pair_target) {
                claimed.insert_claim(pt.to_uppercase(), None)?;
                e.pair_move = Some((p.file_id, p.path.clone(), pt));
            }
            planned = Some(e);
            break;
        }
        out.push(planned.unwrap_or_else(|| {
            entry(
                if saw_too_long {
                    PlanAction::SkipPathTooLong
                } else {
                    PlanAction::SkipCollision
                },
                None,
            )
        }));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::date::{days_from_civil, MS_PER_DAY, SRC_EXIF, SRC_MTIME_UNCERTAIN};
    use crate::template::{
        parse_template, TemplateKind, DEFAULT_DIR_TEMPLATE, DEFAULT_FILE_TEMPLATE,
    };

    const H1: &str = "aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111";
    /// trùng 4 ký tự đầu với H1, khác từ ký tự 5
    const H2: &str = "aaaa2222aaaa2222aaaa2222aaaa2222aaaa2222aaaa2222aaaa2222aaaa2222";

    fn taken() -> i64 {
        days_from_civil(2019, 6, 14) * MS_PER_DAY + (15 * 3600 + 30 * 60 + 22) * 1000
    }

    fn item(id: i64, path: &str, hash: Option<&str>) -> PlanItem {
        PlanItem {
            file_id: id,
            path: path.into(),
            ext: "jpg".into(),
            kind: 0,
            status: 0,
            taken_ms: taken(),
            taken_source: SRC_EXIF,
            hash_hex: hash.map(|s| s.to_string()),
            camera: None,
            rel_dir: None,
            folder: None,
            orig_stem: None,
            gps: None,
            pair: None,
        }
    }

    fn tpls() -> (Template, Template) {
        (
            parse_template(DEFAULT_DIR_TEMPLATE, TemplateKind::Dir).unwrap(),
            parse_template(DEFAULT_FILE_TEMPLATE, TemplateKind::File).unwrap(),
        )
    }

    fn all_free(_: &str, _: &PlanItem) -> TargetState {
        TargetState::Free
    }

    #[test]
    fn basic_rename_same_volume() {
        let (d, f) = tpls();
        let items = [item(1, r"D:\mess\IMG_001.jpg", Some(H1))];
        let plan = plan_organize(&items, r"D:\MyLib", &d, &f, false, &all_free);
        assert_eq!(plan[0].action, PlanAction::Rename);
        assert_eq!(
            plan[0].new_path.as_deref(),
            Some(r"D:\MyLib\2019\2019-06\20190614_153022_aaaa.jpg")
        );
    }

    #[test]
    fn cross_volume_is_copy_verify() {
        let (d, f) = tpls();
        let items = [item(1, r"E:\mess\a.jpg", Some(H1))];
        let plan = plan_organize(&items, r"D:\MyLib", &d, &f, false, &all_free);
        assert_eq!(plan[0].action, PlanAction::CopyVerify);
    }

    #[test]
    fn skips_cloud_missing_uncertain() {
        let (d, f) = tpls();
        let mut cloud = item(1, r"D:\a.jpg", Some(H1));
        cloud.status = 2;
        let mut missing = item(2, r"D:\b.jpg", Some(H1));
        missing.status = 1;
        let mut unc = item(3, r"D:\c.jpg", Some(H1));
        unc.taken_source = SRC_MTIME_UNCERTAIN;
        let plan = plan_organize(
            &[cloud, missing, unc.clone()],
            r"D:\L",
            &d,
            &f,
            false,
            &all_free,
        );
        assert_eq!(plan[0].action, PlanAction::SkipCloud);
        assert_eq!(plan[1].action, PlanAction::SkipMissing);
        assert_eq!(plan[2].action, PlanAction::SkipUncertain);
        // opt-in uncertain -> được organize
        let plan = plan_organize(&[unc], r"D:\L", &d, &f, true, &all_free);
        assert_eq!(plan[0].action, PlanAction::Rename);
    }

    #[test]
    fn needs_hash_when_template_has_hash_token() {
        let (d, f) = tpls();
        let items = [item(1, r"D:\a.jpg", None)];
        let plan = plan_organize(&items, r"D:\L", &d, &f, false, &all_free);
        assert_eq!(plan[0].action, PlanAction::NeedsHash);
        assert!(plan[0].new_path.is_none());
        // template không hash + cùng volume -> không cần hash
        let f2 = parse_template("{YYYYMMDD}_{hhmmss}", TemplateKind::File).unwrap();
        let plan = plan_organize(&items, r"D:\L", &d, &f2, false, &all_free);
        assert_eq!(plan[0].action, PlanAction::Rename);
        // template không hash nhưng XUYÊN volume -> vẫn cần hash để verify
        let items = [item(1, r"E:\a.jpg", None)];
        let plan = plan_organize(&items, r"D:\L", &d, &f2, false, &all_free);
        assert_eq!(plan[0].action, PlanAction::NeedsHash);
    }

    #[test]
    fn same_content_at_target_is_duplicate() {
        let (d, f) = tpls();
        let items = [item(1, r"D:\mess\a.jpg", Some(H1))];
        let state = |p: &str, _: &PlanItem| {
            if p.ends_with("_aaaa.jpg") {
                TargetState::SameContent
            } else {
                TargetState::Free
            }
        };
        let plan = plan_organize(&items, r"D:\L", &d, &f, false, &state);
        assert_eq!(plan[0].action, PlanAction::SkipDuplicate);
    }

    #[test]
    fn occupied_target_escalates_to_hash8() {
        let (d, f) = tpls();
        let items = [item(1, r"D:\mess\a.jpg", Some(H1))];
        let state = |p: &str, _: &PlanItem| {
            if p.ends_with("_aaaa.jpg") {
                TargetState::Occupied
            } else {
                TargetState::Free
            }
        };
        let plan = plan_organize(&items, r"D:\L", &d, &f, false, &state);
        assert_eq!(plan[0].action, PlanAction::Rename);
        assert!(plan[0]
            .new_path
            .as_deref()
            .unwrap()
            .ends_with("_aaaa1111.jpg"));
    }

    #[test]
    fn two_files_same_second_different_content_get_distinct_names() {
        let (d, f) = tpls();
        let items = [
            item(1, r"D:\x\a.jpg", Some(H1)),
            item(2, r"D:\y\b.jpg", Some(H2)),
        ];
        let plan = plan_organize(&items, r"D:\L", &d, &f, false, &all_free);
        assert_eq!(plan[0].action, PlanAction::Rename);
        assert_eq!(plan[1].action, PlanAction::Rename);
        // cùng giây, hash4 đều "aaaa" -> file 2 phải escalate hash8
        assert!(plan[0].new_path.as_deref().unwrap().ends_with("_aaaa.jpg"));
        assert!(plan[1]
            .new_path
            .as_deref()
            .unwrap()
            .ends_with("_aaaa2222.jpg"));
    }

    #[test]
    fn two_identical_files_second_is_duplicate() {
        let (d, f) = tpls();
        let items = [
            item(1, r"D:\x\a.jpg", Some(H1)),
            item(2, r"D:\y\b.jpg", Some(H1)),
        ];
        let plan = plan_organize(&items, r"D:\L", &d, &f, false, &all_free);
        assert_eq!(plan[0].action, PlanAction::Rename);
        assert_eq!(plan[1].action, PlanAction::SkipDuplicate);
    }

    #[test]
    fn already_organized_is_skip_even_with_escalated_name() {
        let (d, f) = tpls();
        // file đang nằm đúng đích với tên hash4
        let items = [item(
            1,
            r"D:\L\2019\2019-06\20190614_153022_aaaa.jpg",
            Some(H1),
        )];
        let plan = plan_organize(&items, r"D:\L", &d, &f, false, &all_free);
        assert_eq!(plan[0].action, PlanAction::SkipOrganized);
        // đang nằm với tên hash8 (escalate từ trước), slot hash4 bị chiếm
        let items = [item(
            1,
            r"D:\L\2019\2019-06\20190614_153022_aaaa1111.jpg",
            Some(H1),
        )];
        let state = |p: &str, _: &PlanItem| {
            if p.ends_with("_aaaa.jpg") {
                TargetState::Occupied
            } else {
                TargetState::Free
            }
        };
        let plan = plan_organize(&items, r"D:\L", &d, &f, false, &state);
        assert_eq!(plan[0].action, PlanAction::SkipOrganized);
    }

    #[test]
    fn live_pair_moves_together_with_same_stem() {
        let (d, f) = tpls();
        let mut it = item(1, r"D:\mess\IMG_1.heic", Some(H1));
        it.ext = "heic".into();
        it.pair = Some(PairInfo {
            file_id: 2,
            path: r"D:\mess\IMG_1.mov".into(),
            ext: "mov".into(),
            status: 0,
            size: 10,
            mtime: 1,
            hash_hex: None,
        });
        let plan = plan_organize(&[it], r"D:\L", &d, &f, false, &all_free);
        assert_eq!(plan[0].action, PlanAction::Rename);
        assert_eq!(
            plan[0].new_path.as_deref(),
            Some(r"D:\L\2019\2019-06\20190614_153022_aaaa.heic")
        );
        let (pid, _, pnew) = plan[0].pair_move.clone().unwrap();
        assert_eq!(pid, 2);
        assert_eq!(pnew, r"D:\L\2019\2019-06\20190614_153022_aaaa.mov");
    }

    /// Chia thư mục theo {kind} thì MOV của Live Photo phải đi theo ẢNH vào
    /// `Photos\`, KHÔNG được tách sang `Videos\`. Tách đôi là hỏng cặp: cả
    /// Windows lẫn logic ghép cặp của chính app đều dựa vào "cùng thư mục,
    /// cùng stem".
    ///
    /// Hiện tại điều này đúng NHỜ `pair_target` dựng từ chính `segs` của tấm
    /// ảnh — một hệ quả gián tiếp, không phải luật viết tường minh. Test này
    /// khoá nó lại để ai đó đổi cách dựng pair_target thì vỡ ngay.
    #[test]
    fn live_pair_mov_follows_the_photo_into_photos_not_videos() {
        let d = parse_template(r"{kind}\{YYYY}", TemplateKind::Dir).unwrap();
        let f = parse_template("{YYYYMMDD}_{hhmmss}_{hash4}", TemplateKind::File).unwrap();
        let mut it = item(1, r"D:\mess\IMG_1.heic", Some(H1));
        it.ext = "heic".into();
        it.kind = 0;
        it.pair = Some(PairInfo {
            file_id: 2,
            path: r"D:\mess\IMG_1.mov".into(),
            ext: "mov".into(),
            status: 0,
            size: 10,
            mtime: 1,
            hash_hex: None,
        });
        let plan = plan_organize(&[it], r"D:\L", &d, &f, false, &all_free);
        assert_eq!(
            plan[0].new_path.as_deref(),
            Some(r"D:\L\Photos\2019\20190614_153022_aaaa.heic")
        );
        let (_, _, pnew) = plan[0].pair_move.clone().unwrap();
        assert_eq!(
            pnew, r"D:\L\Photos\2019\20190614_153022_aaaa.mov",
            "MOV cua Live Photo phai nam canh anh, khong duoc sang Videos"
        );

        // Video ĐỘC LẬP (không có cặp) thì mới vào Videos
        let mut v = item(3, r"D:\mess\clip.mp4", Some(H1));
        v.ext = "mp4".into();
        v.kind = 1;
        let plan = plan_organize(&[v], r"D:\L", &d, &f, false, &all_free);
        assert_eq!(
            plan[0].new_path.as_deref(),
            Some(r"D:\L\Videos\2019\20190614_153022_aaaa.mp4")
        );
    }

    #[test]
    fn live_pair_blocked_when_mov_not_present() {
        let (d, f) = tpls();
        let mut it = item(1, r"D:\mess\IMG_1.heic", Some(H1));
        it.pair = Some(PairInfo {
            file_id: 2,
            path: r"D:\mess\IMG_1.mov".into(),
            ext: "mov".into(),
            status: 2,
            size: 10,
            mtime: 1,
            hash_hex: None,
        });
        let plan = plan_organize(&[it], r"D:\L", &d, &f, false, &all_free);
        assert_eq!(plan[0].action, PlanAction::SkipPairBlocked);
    }

    #[test]
    fn live_pair_mov_target_occupied_escalates_both() {
        let (d, f) = tpls();
        let mut it = item(1, r"D:\mess\IMG_1.heic", Some(H1));
        it.ext = "heic".into();
        it.pair = Some(PairInfo {
            file_id: 2,
            path: r"D:\mess\IMG_1.mov".into(),
            ext: "mov".into(),
            status: 0,
            size: 10,
            mtime: 1,
            hash_hex: None,
        });
        // stem hash4 trống cho ảnh nhưng .mov bị chiếm -> cả cặp lên hash8
        let state = |p: &str, _: &PlanItem| {
            if p.ends_with("_aaaa.mov") {
                TargetState::Occupied
            } else {
                TargetState::Free
            }
        };
        let plan = plan_organize(&[it], r"D:\L", &d, &f, false, &state);
        assert!(plan[0]
            .new_path
            .as_deref()
            .unwrap()
            .ends_with("_aaaa1111.heic"));
        let (_, _, pnew) = plan[0].pair_move.clone().unwrap();
        assert!(pnew.ends_with("_aaaa1111.mov"));
    }

    #[test]
    fn split_pair_gets_fixup_move_when_image_already_organized() {
        // P1 review: đợt trước ảnh move xong mà MOV fail giữa chừng → ảnh
        // SkipOrganized nhưng planner phải phát lệnh move nốt MOV
        let (d, f) = tpls();
        let mut it = item(1, r"D:\L\2019\2019-06\20190614_153022_aaaa.heic", Some(H1));
        it.ext = "heic".into();
        it.pair = Some(PairInfo {
            file_id: 2,
            path: r"D:\mess\IMG_1.mov".into(),
            ext: "mov".into(),
            status: 0,
            size: 10,
            mtime: 1,
            hash_hex: None,
        });
        let plan = plan_organize(&[it], r"D:\L", &d, &f, false, &all_free);
        assert_eq!(plan[0].action, PlanAction::SkipOrganized);
        let (pid, _, pnew) = plan[0].pair_move.clone().expect("phai co lenh fixup MOV");
        assert_eq!(pid, 2);
        assert_eq!(pnew, r"D:\L\2019\2019-06\20190614_153022_aaaa.mov");
        // Cặp đã nằm cạnh nhau đủ đôi → không fixup gì nữa
        let mut ok = item(1, r"D:\L\2019\2019-06\20190614_153022_aaaa.heic", Some(H1));
        ok.ext = "heic".into();
        ok.pair = Some(PairInfo {
            file_id: 2,
            path: r"D:\L\2019\2019-06\20190614_153022_aaaa.mov".into(),
            ext: "mov".into(),
            status: 0,
            size: 10,
            mtime: 1,
            hash_hex: None,
        });
        let plan = plan_organize(&[ok], r"D:\L", &d, &f, false, &all_free);
        assert_eq!(plan[0].action, PlanAction::SkipOrganized);
        assert!(plan[0].pair_move.is_none());
    }

    #[test]
    fn unhashed_files_never_fake_skip_duplicate() {
        // P1 review: template không hash, 2 file CHƯA hash trùng tên render —
        // không được phán "trùng nội dung", phải escalate suffix đếm
        let d = parse_template(DEFAULT_DIR_TEMPLATE, TemplateKind::Dir).unwrap();
        let f = parse_template("{YYYYMMDD}_{hhmmss}", TemplateKind::File).unwrap();
        let items = [item(1, r"D:\x\a.jpg", None), item(2, r"D:\y\b.jpg", None)];
        let plan = plan_organize(&items, r"D:\L", &d, &f, false, &all_free);
        assert_eq!(plan[0].action, PlanAction::Rename);
        assert_eq!(
            plan[1].action,
            PlanAction::Rename,
            "khong duoc SkipDuplicate gia"
        );
        assert!(plan[1].new_path.as_deref().unwrap().ends_with("_2.jpg"));
    }

    #[test]
    fn no_hash_template_uses_counter_suffix() {
        let d = parse_template(DEFAULT_DIR_TEMPLATE, TemplateKind::Dir).unwrap();
        let f = parse_template("{YYYYMMDD}_{hhmmss}", TemplateKind::File).unwrap();
        let items = [
            item(1, r"D:\x\a.jpg", Some(H1)),
            item(2, r"D:\y\b.jpg", Some(H2)),
        ];
        let plan = plan_organize(&items, r"D:\L", &d, &f, false, &all_free);
        assert!(plan[0]
            .new_path
            .as_deref()
            .unwrap()
            .ends_with("20190614_153022.jpg"));
        assert!(plan[1]
            .new_path
            .as_deref()
            .unwrap()
            .ends_with("20190614_153022_2.jpg"));
    }

    fn src_item(
        id: i64,
        path: &str,
        hash: Option<&str>,
        rel: Option<&str>,
        stem: Option<&str>,
    ) -> PlanItem {
        let mut it = item(id, path, hash);
        it.rel_dir = rel.map(str::to_string);
        it.folder = rel.and_then(|r| r.rsplit('\\').next()).map(str::to_string);
        it.orig_stem = stem.map(str::to_string);
        it
    }

    #[test]
    fn relpath_segs_are_stable_across_name_escalation() {
        let d = parse_template("{relpath}", TemplateKind::Dir).unwrap();
        let f = parse_template(DEFAULT_FILE_TEMPLATE, TemplateKind::File).unwrap();
        // cùng giây, hash4 đều "aaaa" -> file 2 escalate hash8 nhưng THƯ MỤC giữ nguyên
        let items = [
            src_item(
                1,
                r"D:\mess\a.jpg",
                Some(H1),
                Some(r"Bac Tuan\Tet 2008"),
                None,
            ),
            src_item(
                2,
                r"D:\mess\b.jpg",
                Some(H2),
                Some(r"Bac Tuan\Tet 2008"),
                None,
            ),
        ];
        let plan = plan_organize(&items, r"D:\L", &d, &f, false, &all_free);
        assert_eq!(
            plan[0].new_path.as_deref(),
            Some(r"D:\L\Bac Tuan\Tet 2008\20190614_153022_aaaa.jpg")
        );
        assert_eq!(
            plan[1].new_path.as_deref(),
            Some(r"D:\L\Bac Tuan\Tet 2008\20190614_153022_aaaa2222.jpg")
        );
    }

    #[test]
    fn name_token_collision_uses_counter_never_fake_duplicate() {
        // {name} không hash + cùng volume → không bắt hash; 2 file KHÁC nội dung
        // trùng stem phải ra _2 (hành vi chấp nhận: trùng NỘI DUNG thật thì đã
        // có dedup — planner không được phán SkipDuplicate khi không có hash khớp)
        let d = parse_template(DEFAULT_DIR_TEMPLATE, TemplateKind::Dir).unwrap();
        let f = parse_template("{name}", TemplateKind::File).unwrap();
        let items = [
            src_item(
                1,
                r"D:\x\Picture 039.jpg",
                Some(H1),
                None,
                Some("Picture 039"),
            ),
            src_item(
                2,
                r"D:\y\Picture 039.jpg",
                Some(H2),
                None,
                Some("Picture 039"),
            ),
        ];
        let plan = plan_organize(&items, r"D:\L", &d, &f, false, &all_free);
        assert_eq!(plan[0].action, PlanAction::Rename);
        assert_eq!(plan[1].action, PlanAction::Rename);
        assert!(plan[0]
            .new_path
            .as_deref()
            .unwrap()
            .ends_with(r"\Picture 039.jpg"));
        assert!(plan[1]
            .new_path
            .as_deref()
            .unwrap()
            .ends_with(r"\Picture 039_2.jpg"));
    }

    #[test]
    fn relpath_file_already_in_library_is_skip_organized() {
        // Bất biến chống lồng: file đã organize (rel tính từ LIB ROOT) render
        // target == chính nó → SkipOrganized, tuyệt đối không move lần 2
        let d = parse_template("{relpath}", TemplateKind::Dir).unwrap();
        let f = parse_template("{name}", TemplateKind::File).unwrap();
        let items = [src_item(
            1,
            r"D:\L\Bac Tuan\Tet 2008\Picture 039.jpg",
            Some(H1),
            Some(r"Bac Tuan\Tet 2008"),
            Some("Picture 039"),
        )];
        let plan = plan_organize(&items, r"D:\L", &d, &f, false, &all_free);
        assert_eq!(plan[0].action, PlanAction::SkipOrganized);
    }

    #[test]
    fn skip_organized_matches_unicode_case_insensitively() {
        // Path trên đĩa khác case Ở KÝ TỰ CÓ DẤU so với render — eq_ci ASCII-only
        // cũ sẽ trượt SkipOrganized rồi re-plan file đang nằm đúng chỗ
        let d = parse_template("{relpath}", TemplateKind::Dir).unwrap();
        let f = parse_template("{name}", TemplateKind::File).unwrap();
        let items = [src_item(
            1,
            r"D:\L\BÁC TUẤN\Ảnh.jpg",
            Some(H1),
            Some("Bác Tuấn"),
            Some("Ảnh"),
        )];
        let plan = plan_organize(&items, r"D:\L", &d, &f, false, &all_free);
        assert_eq!(plan[0].action, PlanAction::SkipOrganized);
    }

    #[test]
    fn overlong_target_reports_skip_path_too_long() {
        let d = parse_template(DEFAULT_DIR_TEMPLATE, TemplateKind::Dir).unwrap();
        let f = parse_template("{YYYYMMDD}_{hhmmss}", TemplateKind::File).unwrap();
        let lib = format!(r"D:\{}", "L".repeat(300));
        let items = [item(1, r"D:\x\a.jpg", Some(H1))];
        let plan = plan_organize(&items, &lib, &d, &f, false, &all_free);
        assert_eq!(plan[0].action, PlanAction::SkipPathTooLong);
        assert!(plan[0].new_path.is_none());
    }

    #[test]
    fn overlong_default_name_still_fits_via_shorter_hash_escalation() {
        // Độ dài ứng viên KHÔNG đơn điệu: {hash16} mặc định (16 ký tự) tràn
        // nhưng bản escalate hash8 (8 ký tự) vừa — phải Rename chứ không skip
        let d = parse_template(DEFAULT_DIR_TEMPLATE, TemplateKind::Dir).unwrap();
        let f = parse_template("{hash16}", TemplateKind::File).unwrap();
        // target = lib + "\2019\2019-06\" + name + ".jpg" = (3+N) + 14 + len(name) + 4
        // N=225: hash16 → 262 (>259), hash8 → 254 (vừa)
        let lib = format!(r"D:\{}", "L".repeat(225));
        let items = [item(1, r"D:\x\a.jpg", Some(H1))];
        let plan = plan_organize(&items, &lib, &d, &f, false, &all_free);
        assert_eq!(plan[0].action, PlanAction::Rename);
        let name = plan[0]
            .new_path
            .as_deref()
            .unwrap()
            .rsplit('\\')
            .next()
            .unwrap()
            .to_string();
        assert_eq!(name, format!("{}.jpg", &H1[..8]));
    }
}
