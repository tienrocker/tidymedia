//! Planner thuần cho organize: quyết định từng file đi đâu, KHÔNG đụng fs/DB.
//! Executor inject trạng thái đích qua callback `target_state` (pattern giống
//! plan_delete của M4) nên toàn bộ invariant test được headless.
//!
//! Live Photo: caller KHÔNG đưa MOV có live_pair vào items - thông tin MOV
//! gắn trên entry của ảnh (`pair`), cặp luôn cùng stem, cùng số phận.

use crate::template::{RenderCtx, Template};

/// Trạng thái đường dẫn đích (executor trả lời từ fs)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetState {
    Free,
    /// đã có file y hệt nội dung (full hash khớp) tại đúng path này
    SameContent,
    /// có file khác chiếm chỗ
    Occupied,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanAction {
    /// cùng volume: fs::rename atomic
    Rename,
    /// khác volume: copy -> flush -> verify BLAKE3 -> trash nguồn
    CopyVerify,
    /// thiếu full hash (cần cho tên hoặc cho verify) - executor hash rồi re-plan
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
}

/// MOV đi kèm ảnh Live Photo
#[derive(Debug, Clone)]
pub struct PairInfo {
    pub file_id: i64,
    pub path: String,
    /// ext lowercase không dot (thường "mov")
    pub ext: String,
    pub status: i64,
}

#[derive(Debug, Clone)]
pub struct PlanItem {
    pub file_id: i64,
    /// đường dẫn đầy đủ hiện tại
    pub path: String,
    /// ext lowercase không dot
    pub ext: String,
    pub status: i64,
    pub taken_ms: i64,
    /// date::SRC_* - SRC_MTIME_UNCERTAIN bị loại trừ khi include_uncertain
    pub taken_source: i64,
    /// hex BLAKE3 full (64 ký tự) nếu đã hash + còn hợp lệ
    pub hash_hex: Option<String>,
    pub camera: Option<String>,
    pub pair: Option<PairInfo>,
}

#[derive(Debug, Clone)]
pub struct PlanEntry {
    pub file_id: i64,
    pub action: PlanAction,
    pub old_path: String,
    /// Some với Rename/CopyVerify (NeedsHash: đường dự kiến với hash = "????")
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

fn eq_ci(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
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
    use std::collections::HashMap;
    // path đích (UPPERCASE) -> hash_hex của file đã claim (để nhận SkipDuplicate nội bộ batch)
    let mut claimed: HashMap<String, Option<String>> = HashMap::new();
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
        let needs_hash = it.hash_hex.is_none() && (file_tpl.has_hash || !same_vol);
        let hash_hex = it.hash_hex.clone().unwrap_or_default();
        let ctx = RenderCtx::from_taken(it.taken_ms, &hash_hex, it.camera.as_deref());
        let segs = dir_tpl.render_dir(&ctx);
        if needs_hash {
            // đường dự kiến chỉ để hiển thị dry-run
            let tentative = {
                let ctx =
                    RenderCtx::from_taken(it.taken_ms, "????????????????", it.camera.as_deref());
                let name = file_tpl.render_file(&ctx, None);
                join_target(lib_root, &dir_tpl.render_dir(&ctx), &name, &it.ext)
            };
            out.push(entry(PlanAction::NeedsHash, Some(tentative)));
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
        for name in &cands {
            let target = join_target(lib_root, &segs, name, &it.ext);
            let pair_target = it
                .pair
                .as_ref()
                .map(|p| join_target(lib_root, &segs, name, &p.ext));
            // đã nằm đúng chỗ (kể cả tên hash8/16 từ lần escalate trước)?
            if eq_ci(&it.path, &target) {
                let mut e = entry(PlanAction::SkipOrganized, None);
                // Ảnh đã đúng chỗ nhưng MOV của cặp CHƯA theo (đợt trước fail
                // giữa 2 nửa cặp) → move nốt MOV, không thì cặp xé vĩnh viễn:
                // ảnh mãi SkipOrganized còn MOV bị ẩn khỏi candidates.
                if let (Some(p), Some(pt)) = (&it.pair, &pair_target) {
                    if !eq_ci(&p.path, pt) {
                        claimed.insert(pt.to_uppercase(), None);
                        e.pair_move = Some((p.file_id, p.path.clone(), pt.clone()));
                    }
                }
                planned = Some(e);
                break;
            }
            let key = target.to_uppercase();
            let claimed_here = claimed.get(&key);
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
                    let pstate = if claimed.contains_key(&pkey) {
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
            claimed.insert(key, Some(hash_hex.clone()));
            let mut e = entry(action, Some(target));
            if let (Some(p), Some(pt)) = (&it.pair, pair_target) {
                claimed.insert(pt.to_uppercase(), None);
                e.pair_move = Some((p.file_id, p.path.clone(), pt));
            }
            planned = Some(e);
            break;
        }
        out.push(planned.unwrap_or_else(|| entry(PlanAction::SkipCollision, None)));
    }
    out
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
            status: 0,
            taken_ms: taken(),
            taken_source: SRC_EXIF,
            hash_hex: hash.map(|s| s.to_string()),
            camera: None,
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
        assert!(plan[0].new_path.as_deref().unwrap().contains("????"));
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

    #[test]
    fn live_pair_blocked_when_mov_not_present() {
        let (d, f) = tpls();
        let mut it = item(1, r"D:\mess\IMG_1.heic", Some(H1));
        it.pair = Some(PairInfo {
            file_id: 2,
            path: r"D:\mess\IMG_1.mov".into(),
            ext: "mov".into(),
            status: 2,
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
}
