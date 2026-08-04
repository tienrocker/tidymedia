use std::collections::HashMap;

use core_db::{ops, query, Db, FileFilter, ScanEntry};

fn entry(dir: &str, name: &str, ext: &str, kind: i64, size: i64, mtime: i64) -> ScanEntry {
    ScanEntry {
        dir_path: dir.into(),
        name: name.into(),
        ext: ext.into(),
        kind,
        size,
        mtime,
        attrs: 0,
        status: 0,
    }
}

#[test]
fn scan_upsert_query_reconcile() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path()).unwrap();

    let root_id = db
        .writer
        .exec(|c| ops::upsert_root(c, "D:\\TestPhotos"))
        .unwrap();
    assert_eq!(root_id, 1);

    let entries = vec![
        entry("D:\\TestPhotos", "IMG_1234.jpg", "jpg", 0, 1000, 500),
        entry("D:\\TestPhotos", "IMG_1234.mov", "mov", 1, 9000, 500),
        entry(
            "D:\\TestPhotos\\2019",
            "beach_sunset.jpg",
            "jpg",
            0,
            2000,
            900,
        ),
        entry("D:\\TestPhotos\\2019", "ảnh tết.png", "png", 0, 3000, 100),
    ];
    db.writer
        .exec(move |c| {
            let mut cache = HashMap::new();
            ops::upsert_scan_batch(c, 1, 10, &entries, &mut cache)
        })
        .unwrap();

    // All files
    let ids = db
        .pool
        .with(|c| query::query_ids(c, &FileFilter::default()))
        .unwrap();
    assert_eq!(ids.len(), 4);

    // FTS trigram: substring giữa tên
    let f = FileFilter {
        text: Some("each".into()),
        ..Default::default()
    };
    let ids = db.pool.with(|c| query::query_ids(c, &f)).unwrap();
    assert_eq!(ids.len(), 1);

    // Accent-insensitive: "anh tet" phải ra "ảnh tết.png"
    let f = FileFilter {
        text: Some("anh tet".into()),
        ..Default::default()
    };
    let ids = db.pool.with(|c| query::query_ids(c, &f)).unwrap();
    assert_eq!(ids.len(), 1, "search khong dau phai match ten co dau");

    // Query ngắn <3 ký tự: substring (không phải prefix-only)
    let f = FileFilter {
        text: Some("ea".into()),
        ..Default::default()
    };
    let ids = db.pool.with(|c| query::query_ids(c, &f)).unwrap();
    assert_eq!(ids.len(), 1, "'ea' phai match beach_sunset (substring)");

    // Hydrate rows giữ thứ tự + đúng 1 slot mỗi id
    let f = FileFilter {
        sort: Some("size_desc".into()),
        ..Default::default()
    };
    let ids = db.pool.with(|c| query::query_ids(c, &f)).unwrap();
    let rows = db.pool.with(|c| query::fetch_rows(c, &ids)).unwrap();
    assert_eq!(rows.len(), ids.len());
    assert_eq!(rows[0].as_ref().unwrap().name, "IMG_1234.mov");

    // Slot None cho id không tồn tại — không lệch vị trí
    let mut with_ghost = ids.clone();
    with_ghost.insert(1, 999_999);
    let rows = db.pool.with(|c| query::fetch_rows(c, &with_ghost)).unwrap();
    assert!(rows[1].is_none());
    assert_eq!(rows[0].as_ref().unwrap().name, "IMG_1234.mov");

    // Rescan gen=11 thiếu 1 file -> reconcile đánh dấu missing
    let rescan = vec![
        entry("D:\\TestPhotos", "IMG_1234.jpg", "jpg", 0, 1000, 500),
        entry("D:\\TestPhotos", "IMG_1234.mov", "mov", 1, 9000, 500),
        entry(
            "D:\\TestPhotos\\2019",
            "beach_sunset.jpg",
            "jpg",
            0,
            2000,
            900,
        ),
    ];
    db.writer
        .exec(move |c| {
            let mut cache = HashMap::new();
            ops::upsert_scan_batch(c, 1, 11, &rescan, &mut cache)
        })
        .unwrap();
    let missing = db
        .writer
        .exec(|c| ops::reconcile_scan(c, "D:\\TestPhotos", 11))
        .unwrap();
    assert_eq!(missing, 1);
    let ids = db
        .pool
        .with(|c| query::query_ids(c, &FileFilter::default()))
        .unwrap();
    assert_eq!(ids.len(), 3);

    // Roots có count cache đúng (reconcile đã refresh)
    let roots = db.pool.with(ops::list_roots).unwrap();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].file_count, 3);
}

#[test]
fn reconcile_preserves_unreadable_subtree() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path()).unwrap();
    db.writer
        .exec(|c| ops::upsert_root(c, "D:\\Photos"))
        .unwrap();
    let initial = vec![
        entry("D:\\Photos\\ok", "keep.jpg", "jpg", 0, 10, 1),
        entry("D:\\Photos\\locked", "still-there.jpg", "jpg", 0, 20, 1),
        entry("D:\\Photos\\gone", "deleted.jpg", "jpg", 0, 30, 1),
    ];
    db.writer
        .exec(move |c| {
            let mut cache = HashMap::new();
            ops::upsert_scan_batch(c, 1, 1, &initial, &mut cache)
        })
        .unwrap();
    let rescan = vec![entry("D:\\Photos\\ok", "keep.jpg", "jpg", 0, 10, 1)];
    db.writer
        .exec(move |c| {
            let mut cache = HashMap::new();
            ops::upsert_scan_batch(c, 1, 2, &rescan, &mut cache)
        })
        .unwrap();

    let marked = db
        .writer
        .exec(|c| ops::reconcile_scan_excluding(c, "D:\\Photos", 2, &["D:\\Photos\\locked".into()]))
        .unwrap();
    assert_eq!(marked, 1, "chi file trong subtree gone bi missing");

    let locked = db
        .pool
        .with(|c| {
            query::query_ids(
                c,
                &FileFilter {
                    text: Some("still-there".into()),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    assert_eq!(locked.len(), 1, "subtree doc loi phai giu index cu");
}

#[test]
fn root_overlap_rules() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path()).unwrap();

    let id1 = db
        .writer
        .exec(|c| ops::upsert_root(c, "D:\\Photos"))
        .unwrap();

    // Trùng y hệt (khác case) → idempotent, trả id cũ
    let id_same = db
        .writer
        .exec(|c| ops::upsert_root(c, "d:\\photos\\"))
        .unwrap();
    assert_eq!(id1, id_same);

    // Root con nằm trong root cũ → bị chặn
    let e = db
        .writer
        .exec(|c| ops::upsert_root(c, "D:\\Photos\\2019"))
        .unwrap_err();
    assert!(e.to_string().contains("ERR_ROOT_COVERED"));

    // Sibling có tên chung prefix KHÔNG bị coi là overlap
    let id2 = db
        .writer
        .exec(|c| ops::upsert_root(c, "D:\\PhotosOld"))
        .unwrap();
    assert_ne!(id1, id2);

    // Root to hơn nuốt các root con
    let id_drive = db.writer.exec(|c| ops::upsert_root(c, "D:\\")).unwrap();
    let roots = db.pool.with(ops::list_roots).unwrap();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].id, id_drive);
    assert_eq!(roots[0].path, "D:\\");

    // Drive-relative bị từ chối
    let e = db.writer.exec(|c| ops::upsert_root(c, "D:")).unwrap_err();
    assert!(e.to_string().contains("ERR_ROOT_DRIVE_RELATIVE"));
}

#[test]
fn live_photo_pairing_hides_mov_and_flags_image() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path()).unwrap();
    db.writer.exec(|c| ops::upsert_root(c, "D:\\LP")).unwrap();

    let entries = vec![
        entry("D:\\LP", "IMG_0001.HEIC", "heic", 0, 4000, 1),
        entry("D:\\LP", "IMG_0001.MOV", "mov", 1, 2000, 1),
        entry("D:\\LP", "solo_clip.mov", "mov", 1, 5000, 1), // video thường
        entry("D:\\LP", "note.jpg", "jpg", 0, 100, 1),       // ảnh không cặp
        entry("D:\\LP\\Sub", "IMG_0001.MOV", "mov", 1, 999, 1), // khác dir — KHÔNG ghép
    ];
    db.writer
        .exec(move |c| {
            let mut cache = HashMap::new();
            ops::upsert_scan_batch(c, 1, 1, &entries, &mut cache)
        })
        .unwrap();

    let paired = db
        .writer
        .exec(|c| ops::pair_live_photos(c, "D:\\LP"))
        .unwrap();
    assert_eq!(paired, 1, "chi IMG_0001.HEIC duoc ghep");

    // MOV đã ghép bị ẩn; MOV khác dir + solo clip vẫn hiện
    let ids = db
        .pool
        .with(|c| query::query_ids(c, &FileFilter::default()))
        .unwrap();
    let rows = db.pool.with(|c| query::fetch_rows(c, &ids)).unwrap();
    let names: Vec<_> = rows
        .iter()
        .flatten()
        .map(|r| (r.name.clone(), r.dir.clone()))
        .collect();
    assert_eq!(ids.len(), 4, "MOV cua Live Photo phai bi an: {names:?}");
    assert!(!names
        .iter()
        .any(|(n, d)| n == "IMG_0001.MOV" && d == "D:\\LP"));

    // Ảnh HEIC gắn cờ is_live; ảnh/video thường thì không
    let by_name: std::collections::HashMap<String, bool> = rows
        .iter()
        .flatten()
        .map(|r| (r.name.clone(), r.is_live))
        .collect();
    assert!(by_name["IMG_0001.HEIC"]);
    assert!(!by_name["note.jpg"]);
    assert!(!by_name["solo_clip.mov"]);

    // Idempotent: chạy lại không đổi kết quả
    let paired2 = db
        .writer
        .exec(|c| ops::pair_live_photos(c, "D:\\LP"))
        .unwrap();
    assert_eq!(paired2, 1);
}

#[test]
fn dedup_hash_pipeline_and_groups() {
    use core_db::HashUpsert;

    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path()).unwrap();
    db.writer.exec(|c| ops::upsert_root(c, "D:\\Dup")).unwrap();

    // a1/a2/a3 cùng size 1000 (nghi trùng), b khác size, c cùng size 1000
    // nhưng nội dung khác (quick sẽ khác)
    let entries = vec![
        entry("D:\\Dup", "a1.jpg", "jpg", 0, 1000, 10),
        entry("D:\\Dup", "a2.jpg", "jpg", 0, 1000, 20),
        entry("D:\\Dup\\Sub", "a3.jpg", "jpg", 0, 1000, 30),
        entry("D:\\Dup", "b.jpg", "jpg", 0, 555, 10),
        entry("D:\\Dup", "c.jpg", "jpg", 0, 1000, 40),
    ];
    db.writer
        .exec(move |c| {
            let mut cache = HashMap::new();
            ops::upsert_scan_batch(c, 1, 1, &entries, &mut cache)
        })
        .unwrap();

    // Tầng 1: chỉ file size 1000 (4 file) cần quick hash, b.jpg (size độc nhất) không
    let pending = db
        .pool
        .with(|c| ops::select_pending_quick(c, 0, 100))
        .unwrap();
    assert_eq!(pending.len(), 4, "b.jpg size doc nhat khong can hash");

    // Quick: a* giống nhau, c khác
    let quick: Vec<HashUpsert> = pending
        .iter()
        .map(|p| HashUpsert {
            file_id: p.file_id,
            quick64: Some(if p.path.contains("\\c.jpg") { 999 } else { 111 }),
            full: None,
            src_mtime: p.mtime,
            src_size: p.size,
        })
        .collect();
    db.writer
        .exec(move |c| ops::upsert_hash_batch(c, &quick))
        .unwrap();

    // Tầng 3: chỉ nhóm (1000, 111) = 3 file cần full hash; c (quick độc) không
    let pending_full = db
        .pool
        .with(|c| ops::select_pending_full(c, 0, 100))
        .unwrap();
    assert_eq!(pending_full.len(), 3, "c.jpg quick doc nhat khong can full");

    let full: Vec<HashUpsert> = pending_full
        .iter()
        .map(|p| HashUpsert {
            file_id: p.file_id,
            quick64: None, // giữ quick cũ qua COALESCE
            full: Some(vec![0xAB; 32]),
            src_mtime: p.mtime,
            src_size: p.size,
        })
        .collect();
    db.writer
        .exec(move |c| ops::upsert_hash_batch(c, &full))
        .unwrap();

    let (groups, waste) = db.writer.exec(ops::rebuild_dup_groups).unwrap();
    assert_eq!(groups, 1);
    assert_eq!(waste, 2000, "3 ban x 1000 bytes -> giai phong duoc 2000");

    let list = db.pool.with(ops::list_dup_groups).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].count, 3);
    assert!(!list[0].samples.is_empty());
    let stable_group_id = list[0].id;

    db.writer.exec(ops::rebuild_dup_groups).unwrap();
    let rebuilt = db.pool.with(ops::list_dup_groups).unwrap();
    assert_eq!(
        rebuilt[0].id, stable_group_id,
        "unchanged hash keeps group id"
    );

    let members = db
        .pool
        .with(|c| ops::get_dup_group(c, stable_group_id))
        .unwrap();
    assert_eq!(members.len(), 3);

    // Delete context phủ CẢ nhóm khi chỉ đưa 1 id
    let ctx = db
        .pool
        .with(|c| ops::get_delete_context(c, &[members[0].file_id]))
        .unwrap();
    assert_eq!(ctx.len(), 3);
    assert!(ctx.iter().all(|r| r.full_hash.is_some()));

    // Xóa 2 bản -> stats về 0 nhóm (nhóm còn 1 member bị loại khỏi list/stats)
    db.writer
        .exec({
            let ids = vec![members[0].file_id, members[1].file_id];
            move |c| ops::remove_deleted_files(c, &ids)
        })
        .unwrap();
    let (g2, w2) = db.pool.with(ops::dedup_stats).unwrap();
    assert_eq!((g2, w2), (0, 0));
    let ids = db
        .pool
        .with(|c| query::query_ids(c, &FileFilter::default()))
        .unwrap();
    assert_eq!(ids.len(), 3, "con a3 + b + c trong index");

    // Rescan cùng gen mtime mới cho a3 -> trigger xóa hash -> pending quick lại
    let rescan = vec![entry("D:\\Dup\\Sub", "a3.jpg", "jpg", 0, 1000, 99)];
    db.writer
        .exec(move |c| {
            let mut cache = HashMap::new();
            ops::upsert_scan_batch(c, 1, 2, &rescan, &mut cache)
        })
        .unwrap();
    let pending2 = db
        .pool
        .with(|c| ops::select_pending_quick(c, 0, 100))
        .unwrap();
    // a3 (hash bị trigger dọn) + c (chưa từng đủ nhóm full nhưng quick còn) —
    // chỉ a3 thiếu quick vì c vẫn giữ hash hợp lệ
    assert_eq!(pending2.len(), 1);
    assert!(pending2[0].path.ends_with("a3.jpg"));
}

#[test]
fn root_scope_is_case_insensitive_and_exact() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path()).unwrap();
    db.writer
        .exec(|c| ops::upsert_root(c, "D:\\Photos"))
        .unwrap();

    let entries = vec![
        entry("d:\\photos", "a.jpg", "jpg", 0, 1, 1),
        entry("D:\\Photos\\Trip", "b.jpg", "jpg", 0, 1, 1),
        entry("D:\\PhotosOld", "c.jpg", "jpg", 0, 1, 1), // sibling — ngoài scope
    ];
    db.writer
        .exec(move |c| {
            let mut cache = HashMap::new();
            ops::upsert_scan_batch(c, 1, 5, &entries, &mut cache)
        })
        .unwrap();

    let f = FileFilter {
        root_path: Some("D:\\Photos".into()),
        ..Default::default()
    };
    let ids = db.pool.with(|c| query::query_ids(c, &f)).unwrap();
    assert_eq!(
        ids.len(),
        2,
        "scope phai gom root (khac case) + subtree, khong gom sibling"
    );
}

#[test]
fn org_library_roots_journal_and_relocate() {
    use core_db::org;

    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path()).unwrap();
    db.writer.exec(|c| ops::upsert_root(c, "D:\\Mess")).unwrap();

    // 1) library root: dat, doi path (cung volume = update), primary tu dong
    db.writer
        .exec(|c| org::set_library_root(c, "d:\\MyLib"))
        .unwrap();
    db.writer
        .exec(|c| org::set_library_root(c, "E:\\Media"))
        .unwrap();
    let roots = db.pool.with(org::list_library_roots).unwrap();
    assert_eq!(roots.len(), 2);
    let d_root = roots.iter().find(|r| r.path.starts_with('D')).unwrap();
    assert_eq!(d_root.path, "D:\\MyLib");
    assert!(d_root.is_primary, "root dau tien la primary");
    db.writer
        .exec(|c| org::set_library_root(c, "D:\\MyLib2"))
        .unwrap();
    let roots = db.pool.with(org::list_library_roots).unwrap();
    assert_eq!(roots.len(), 2, "moi volume toi da 1 root - goi lai la DOI");
    assert!(roots.iter().any(|r| r.path == "D:\\MyLib2"));

    // 2) candidates: anh + MOV pair gap thanh 1
    let entries = vec![
        entry("D:\\Mess", "IMG_1.heic", "heic", 0, 100, 1),
        entry("D:\\Mess", "IMG_1.mov", "mov", 1, 900, 1),
        entry("D:\\Mess", "solo.mp4", "mp4", 1, 500, 2),
    ];
    db.writer
        .exec(move |c| {
            let mut cache = HashMap::new();
            ops::upsert_scan_batch(c, 1, 5, &entries, &mut cache)
        })
        .unwrap();
    db.writer
        .exec(|c| ops::pair_live_photos(c, "D:\\Mess"))
        .unwrap();
    let cands = db
        .pool
        .with(|c| org::select_org_candidates(c, 1, 0, 100))
        .unwrap();
    assert_eq!(cands.len(), 2, "MOV co pair phai an, di theo anh");
    let img = cands.iter().find(|x| x.ext == "heic").unwrap();
    let pair = img.pair.as_ref().expect("anh Live phai keo theo MOV");
    assert_eq!(pair.ext, "mov");
    assert_eq!(pair.path, "D:\\Mess\\IMG_1.mov");

    // 3) journal write-ahead + relocate + undo bookkeeping
    let img_id = img.file_id;
    let jid = db
        .writer
        .exec(|c| ops::insert_job(c, "organize", None))
        .unwrap();
    let op = db
        .writer
        .exec(move |c| {
            org::insert_org_op(c, jid, img_id, "D:\\Mess\\IMG_1.heic", "D:\\MyLib2\\x.heic")
        })
        .unwrap();
    assert_eq!(db.pool.with(org::pending_org_ops).unwrap().len(), 1);
    db.writer
        .exec(move |c| org::update_file_location(c, img_id, "D:\\MyLib2\\2019\\x.heic"))
        .unwrap();
    db.writer
        .exec(move |c| org::mark_org_op_done(c, op))
        .unwrap();
    assert!(db.pool.with(org::pending_org_ops).unwrap().is_empty());

    // path moi phan anh trong candidates lan sau + original_name giu ten cu
    let cands = db
        .pool
        .with(|c| org::select_org_candidates(c, 1, 0, 100))
        .unwrap();
    let moved = cands.iter().find(|x| x.file_id == img_id).unwrap();
    assert_eq!(moved.path, "D:\\MyLib2\\2019\\x.heic");
    let orig: Option<String> = db
        .pool
        .with(|c| -> anyhow::Result<Option<String>> {
            Ok(c.query_row(
                "SELECT original_name FROM files WHERE id = ?1",
                [img_id],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(orig.as_deref(), Some("IMG_1.heic"));

    let batches = db.pool.with(org::list_org_batches).unwrap();
    assert_eq!(batches.len(), 1);
    assert_eq!((batches[0].moved, batches[0].undone), (1, 0));
    let undo_ops = db
        .pool
        .with(|c| org::ops_of_batch_for_undo(c, jid))
        .unwrap();
    assert_eq!(undo_ops.len(), 1);
    db.writer
        .exec(move |c| org::mark_org_op_undone(c, op))
        .unwrap();
    let batches = db.pool.with(org::list_org_batches).unwrap();
    assert_eq!((batches[0].moved, batches[0].undone), (0, 1));
}

#[test]
fn finish_root_scan_updates_bookkeeping_without_reconcile() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path()).unwrap();
    db.writer
        .exec(|c| ops::upsert_root(c, r"D:\Photos"))
        .unwrap();
    db.writer
        .exec(|c| {
            let mut cache = HashMap::new();
            ops::upsert_scan_batch(
                c,
                1,
                7,
                &[entry(r"D:\Photos", "kept.jpg", "jpg", 0, 10, 1)],
                &mut cache,
            )
        })
        .unwrap();

    // This is the path used when reconciliation is conservatively skipped after an
    // unscoped walker error: root completion/counting must still happen.
    db.writer
        .exec(|c| ops::finish_root_scan(c, r"D:\Photos"))
        .unwrap();
    let root = db.pool.with(ops::list_roots).unwrap().remove(0);
    assert!(root.last_scan_at.is_some());
    assert_eq!(root.file_count, 1);
}

#[test]
fn schema_v5_migrates_recovery_terminal_state_columns() {
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    ops::ensure_schema(&mut conn).unwrap();
    conn.execute_batch(
        "ALTER TABLE org_ops DROP COLUMN recovery_error;
         ALTER TABLE org_ops DROP COLUMN recovery_attempted_at;
         PRAGMA user_version = 5;",
    )
    .unwrap();

    ops::ensure_schema(&mut conn).unwrap();

    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 6);
    let mut columns = conn.prepare("PRAGMA table_info(org_ops)").unwrap();
    let names: Vec<String> = columns
        .query_map([], |r| r.get(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(names.iter().any(|name| name == "recovery_error"));
    assert!(names.iter().any(|name| name == "recovery_attempted_at"));
}
