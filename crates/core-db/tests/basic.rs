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
