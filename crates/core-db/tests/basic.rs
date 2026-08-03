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
    }
}

#[test]
fn scan_upsert_query_reconcile() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path()).unwrap();

    let root_id = db
        .writer
        .exec(|c| ops::upsert_root(c, "D:\\TestPhotos\\"))
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

    // Kind filter: videos
    let f = FileFilter {
        kind: Some(1),
        ..Default::default()
    };
    let ids = db.pool.with(|c| query::query_ids(c, &f)).unwrap();
    assert_eq!(ids.len(), 1);

    // Hydrate rows giữ thứ tự
    let f = FileFilter {
        sort: Some("size_desc".into()),
        ..Default::default()
    };
    let ids = db.pool.with(|c| query::query_ids(c, &f)).unwrap();
    let rows = db.pool.with(|c| query::fetch_rows(c, &ids)).unwrap();
    assert_eq!(rows[0].name, "IMG_1234.mov");
    assert_eq!(rows.last().unwrap().size, 1000);

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

    // Roots có count đúng
    let roots = db.pool.with(ops::list_roots).unwrap();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].file_count, 3);
}
