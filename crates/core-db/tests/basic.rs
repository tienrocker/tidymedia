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

/// Kho bị copy qua lại nhiều lần thì mtime là NGÀY COPY: ảnh 2018 có thể mang
/// mtime 2024. Sắp xếp/lọc mặc định phải theo ngày chụp, thiếu EXIF mới lùi về
/// mtime — nhưng file chưa có meta KHÔNG được biến mất khỏi danh sách.
#[test]
fn date_sort_and_filter_prefer_capture_time() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path()).unwrap();
    db.writer.exec(|c| ops::upsert_root(c, "D:\\P")).unwrap();
    let entries = vec![
        // (tên, mtime) — old.jpg là ca thật: chụp 2018 nhưng file copy năm 2024
        entry("D:\\P", "old.jpg", "jpg", 0, 100, 1_704_000_000_000),
        entry("D:\\P", "nometa.jpg", "jpg", 0, 100, 1_600_000_000_000),
        entry("D:\\P", "mid.jpg", "jpg", 0, 100, 1_500_000_000_000),
    ];
    db.writer
        .exec(move |c| {
            let mut cache = HashMap::new();
            ops::upsert_scan_batch(c, 1, 1, &entries, &mut cache)
        })
        .unwrap();
    let id_of = |name: &str| -> i64 {
        db.pool
            .with(|c| -> anyhow::Result<i64> {
                Ok(
                    c.query_row("SELECT id FROM files WHERE name = ?1", [name], |r| {
                        r.get::<_, i64>(0)
                    })?,
                )
            })
            .unwrap()
    };
    let (old_id, mid_id, nometa_id) = (id_of("old.jpg"), id_of("mid.jpg"), id_of("nometa.jpg"));
    let metas = vec![
        core_db::MetaUpsert {
            file_id: old_id,
            taken_at: Some(1_533_000_000_000), // 2018
            meta_state: 1,
            src_mtime: 1_704_000_000_000,
            src_size: 100,
            ..Default::default()
        },
        core_db::MetaUpsert {
            file_id: mid_id,
            taken_at: Some(1_650_000_000_000), // 2022
            meta_state: 1,
            src_mtime: 1_500_000_000_000,
            src_size: 100,
            ..Default::default()
        },
    ];
    db.writer
        .exec(move |c| ops::upsert_meta_batch(c, &metas))
        .unwrap();

    // Mặc định = ngày chụp: mid(2022) > nometa(mtime 2020) > old(2018)
    let ids = db
        .pool
        .with(|c| query::query_ids(c, &FileFilter::default()))
        .unwrap();
    assert_eq!(
        ids,
        vec![mid_id, nometa_id, old_id],
        "mac dinh phai sap theo ngay chup, file khong co meta lui ve mtime"
    );

    // Ngày file: old(2024) > nometa(2020) > mid(2017)
    let f = FileFilter {
        date_field: Some("mtime".into()),
        ..Default::default()
    };
    let ids = db.pool.with(|c| query::query_ids(c, &f)).unwrap();
    assert_eq!(ids, vec![old_id, nometa_id, mid_id]);

    // Lọc "chụp từ 2020 trở đi" phải loại ảnh 2018 dù file của nó mới nhất
    let f = FileFilter {
        date_from: Some(1_577_836_800_000), // 2020-01-01
        ..Default::default()
    };
    let ids = db.pool.with(|c| query::query_ids(c, &f)).unwrap();
    assert_eq!(ids, vec![mid_id, nometa_id]);

    // Cùng khoảng đó nhưng theo ngày file thì old.jpg lại lọt (mtime 2024)
    let f = FileFilter {
        date_from: Some(1_577_836_800_000),
        date_field: Some("mtime".into()),
        ..Default::default()
    };
    let ids = db.pool.with(|c| query::query_ids(c, &f)).unwrap();
    assert_eq!(ids, vec![old_id, nometa_id]);

    // Lọc theo độ phân giải (INNER JOIN meta) không được đá nhau với join ngày
    let f = FileFilter {
        min_px: Some(1),
        ..Default::default()
    };
    assert!(db
        .pool
        .with(|c| query::query_ids(c, &f))
        .unwrap()
        .is_empty());
}

/// `low` = 64 bit thấp nhất của hash 256-bit; 3 word còn lại để 0 nên khoảng
/// cách giữa hai item bằng đúng khoảng cách giữa hai `low` — test đọc dễ.
fn item(
    file_id: i64,
    low: i64,
    wh: Option<(i64, i64)>,
    taken_at: Option<i64>,
) -> core_db::ClusterItem {
    core_db::ClusterItem {
        file_id,
        hash: [low, 0, 0, 0],
        width: wh.map(|(w, _)| w),
        height: wh.map(|(_, h)| h),
        taken_at,
    }
}

/// Nhóm "gần giống": cùng ảnh nhưng khác byte (nén lại / thu nhỏ). Dedup tuyệt
/// đối không bao giờ thấy chúng, nên đây là đường duy nhất dọn được.
#[test]
fn similar_groups_cluster_by_perceptual_distance() {
    // Thuần: 2 ảnh sát nhau + 1 ảnh xa → đúng 1 nhóm 2 thành viên
    let items = vec![
        item(1, 0b1011_0110, Some((3024, 3024)), None),
        item(2, 0b1011_0111, Some((1772, 1772)), None), // lech 1 bit, cung ti le
        item(3, !0b1011_0110, Some((3024, 3024)), None), // khac han
    ];
    let clusters = ops::cluster_similar(&items, 6);
    assert_eq!(clusters, vec![vec![1, 2]]);

    // Tỉ lệ khung hình khác hẳn thì KHÔNG gom dù hash sát nhau (dhash 8x8 bỏ
    // hết thông tin khung hình nên ảnh dọc/ngang có thể ra hash gần nhau)
    let items = vec![
        item(1, 0b1011_0110, Some((4000, 3000)), None),
        item(2, 0b1011_0111, Some((1080, 1920)), None),
    ];
    assert!(ops::cluster_similar(&items, 6).is_empty());

    // Thiếu kích thước (meta chưa chạy tới) thì vẫn gom theo hash
    let items = vec![item(1, 5, None, None), item(2, 5, Some((100, 100)), None)];
    assert_eq!(ops::cluster_similar(&items, 6), vec![vec![1, 2]]);

    // Nối chuỗi: a~b, b~c nhưng a xa c → union-find vẫn gom cả 3 vào 1 nhóm
    let items = vec![
        item(1, 0b0000_0000, Some((10, 10)), None),
        item(2, 0b0000_1111, Some((10, 10)), None),
        item(3, 0b1111_1111, Some((10, 10)), None),
    ];
    assert_eq!(ops::cluster_similar(&items, 4), vec![vec![1, 2, 3]]);
}

/// Chốt chặn giờ bấm máy. Số liệu lấy từ kho thật: loạt 13 tấm chụp liên tiếp
/// cùng cảnh `IMG_8194..IMG_8206` (hash gần như y hệt vì người chỉ chiếm 1-2 ô
/// trên lưới 9x8) so với 4 bản của cùng một tấm `IMG_1463`.
#[test]
fn similar_groups_never_merge_different_shutter_presses() {
    // 13 mốc EXIF thật, trải 14 giây trong cùng một phút (2016-11-19 12:08).
    // Hash cho y hệt nhau — chốt chặn duy nhất tách được chúng là taken_at.
    let burst_ms = [
        3_000, 3_870, 5_000, 5_720, 12_000, 12_670, 13_360, 14_110, 14_820, 15_410, 16_090, 16_720,
        17_190,
    ];
    let burst: Vec<_> = burst_ms
        .iter()
        .enumerate()
        .map(|(i, ms)| {
            item(
                8194 + i as i64,
                0b1011_0110,
                Some((5472, 3648)),
                Some(1_479_557_283_000 + ms),
            )
        })
        .collect();
    assert!(
        ops::cluster_similar(&burst, 6).is_empty(),
        "13 lan bam may khac nhau khong duoc gom - do la 13 anh that"
    );

    // 4 bản của CÙNG một lần bấm máy: 3 độ phân giải, 4 dung lượng, hash lệch
    // 0-3 bit, nhưng chung đúng một mốc 2018-08-02 18:02:24.802
    let ts = Some(1_533_232_944_802);
    let real_dup = vec![
        item(1, 0b1011_0110, Some((3024, 3024)), ts), // icloud
        item(2, 0b1011_0111, Some((3024, 3024)), ts), // iphone
        item(3, 0b1011_0100, Some((3024, 3024)), ts), // TienIphone
        item(4, 0b1011_1110, Some((1772, 1772)), ts), // anh\2019, bi resize
    ];
    assert_eq!(
        ops::cluster_similar(&real_dup, 6),
        vec![vec![1, 2, 3, 4]],
        "cung mot lan bam may -> van phai gom du 4 ban"
    );

    // Ảnh qua app nhắn tin bị xóa sạch EXIF: thiếu mốc thì không chặn, vẫn gom
    // được với bản gốc (đây là ca thường gặp NHẤT của M7)
    let stripped = vec![
        item(1, 0b1011_0110, Some((3024, 3024)), ts),
        item(2, 0b1011_0111, Some((1024, 1024)), None),
    ];
    assert_eq!(ops::cluster_similar(&stripped, 6), vec![vec![1, 2]]);

    // Bắc cầu qua member thiếu mốc: A(mốc 1) ~ B(không mốc) ~ C(mốc 2). Chẻ
    // theo mốc, B bị loại vì không có cơ sở gán về bên nào.
    let bridged = vec![
        item(1, 0b1011_0110, Some((10, 10)), Some(1_000)),
        item(2, 0b1011_0110, Some((10, 10)), Some(1_000)),
        item(3, 0b1011_0110, Some((10, 10)), None),
        item(4, 0b1011_0110, Some((10, 10)), Some(2_000)),
        item(5, 0b1011_0110, Some((10, 10)), Some(2_000)),
    ];
    assert_eq!(
        ops::cluster_similar(&bridged, 6),
        vec![vec![1, 2], vec![4, 5]],
        "chia dung theo moc, member khong moc bi loai chu khong doan bua"
    );
}

#[test]
fn similar_groups_are_stable_and_measure_reclaimable_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path()).unwrap();
    db.writer.exec(|c| ops::upsert_root(c, "D:\\S")).unwrap();
    let entries = vec![
        entry("D:\\S", "big.jpg", "jpg", 0, 2_000_000, 10),
        entry("D:\\S", "small.jpg", "jpg", 0, 800_000, 20),
        entry("D:\\S", "other.jpg", "jpg", 0, 500_000, 30),
    ];
    db.writer
        .exec(move |c| {
            let mut cache = HashMap::new();
            ops::upsert_scan_batch(c, 1, 1, &entries, &mut cache)
        })
        .unwrap();
    let pending = db
        .pool
        .with(|c| ops::select_pending_phash(c, 0, 100))
        .unwrap();
    assert_eq!(pending.len(), 3, "anh chua co phash deu vao hang cho");

    let rows: Vec<core_db::PhashUpsert> = pending
        .iter()
        .map(|p| core_db::PhashUpsert {
            file_id: p.file_id,
            // big/small gần nhau, other khác hẳn; "small" phẳng -> None để
            // kiểm luôn nhánh ghi bia
            hash: if p.path.ends_with("other.jpg") {
                Some([!0b1010_1010, 0, 0, 0])
            } else {
                Some([0b1010_1010, 0, 0, 0])
            },
            src_mtime: p.mtime,
            src_size: p.size,
        })
        .collect();
    db.writer
        .exec(move |c| ops::upsert_phash_batch(c, &rows))
        .unwrap();
    assert_eq!(
        db.pool.with(ops::count_pending_phash).unwrap(),
        0,
        "hash roi thi khong duoc chon lai (job phai hoi tu)"
    );

    let (groups, waste) = db
        .writer
        .exec(|c| ops::rebuild_similar_groups(c, 6))
        .unwrap();
    assert_eq!(groups, 1);
    assert_eq!(
        waste, 800_000,
        "giu ban NANG nhat -> don duoc dung phan con lai, khong phai (n-1)*max"
    );

    let list = db.pool.with(|c| ops::list_dup_groups(c, 1)).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].count, 2);
    let gid = list[0].id;
    db.writer
        .exec(|c| ops::rebuild_similar_groups(c, 6))
        .unwrap();
    let again = db.pool.with(|c| ops::list_dup_groups(c, 1)).unwrap();
    assert_eq!(
        again[0].id, gid,
        "rebuild giu group id, UI khong bi dong nhom"
    );

    // Nhóm exact (kind 0) không bị đụng tới
    assert!(db
        .pool
        .with(|c| ops::list_dup_groups(c, 0))
        .unwrap()
        .is_empty());

    // Context xóa phủ cả nhóm dù chỉ đưa 1 id
    let members = db.pool.with(|c| ops::get_dup_group(c, gid)).unwrap();
    let ctx = db
        .pool
        .with(|c| ops::get_similar_delete_context(c, &[members[0].file_id]))
        .unwrap();
    assert_eq!(ctx.len(), 2);
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

    let list = db.pool.with(|c| ops::list_dup_groups(c, 0)).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].count, 3);
    assert!(!list[0].samples.is_empty());
    let stable_group_id = list[0].id;

    db.writer.exec(ops::rebuild_dup_groups).unwrap();
    let rebuilt = db.pool.with(|c| ops::list_dup_groups(c, 0)).unwrap();
    assert_eq!(
        rebuilt[0].id, stable_group_id,
        "unchanged hash keeps group id"
    );

    let members = db
        .pool
        .with(|c| ops::get_dup_group(c, stable_group_id))
        .unwrap();
    assert_eq!(members.len(), 3);

    // Brief = nguyên liệu để UI tick "chọn tất cả" mà không mở từng nhóm:
    // đúng nhóm đó, đúng 3 bản, kèm field cần cho rule.
    let brief = db.pool.with(|c| ops::list_dup_members_brief(c, 0)).unwrap();
    assert_eq!(brief.len(), 3);
    assert!(brief.iter().all(|b| b.group_id == stable_group_id));
    assert!(brief.iter().all(|b| b.status == 0 && b.size == 1000));
    let mut brief_ids: Vec<i64> = brief.iter().map(|b| b.file_id).collect();
    brief_ids.sort_unstable();
    let mut member_ids: Vec<i64> = members.iter().map(|m| m.file_id).collect();
    member_ids.sort_unstable();
    assert_eq!(brief_ids, member_ids);

    // File mất (status != 0) không được đề nghị xóa lẫn chọn làm bản giữ
    db.writer
        .exec({
            let gone = members[2].file_id;
            move |c| {
                c.execute("UPDATE files SET status = 1 WHERE id = ?1", [gone])?;
                Ok(())
            }
        })
        .unwrap();
    let brief_present = db.pool.with(|c| ops::list_dup_members_brief(c, 0)).unwrap();
    assert_eq!(brief_present.len(), 2, "ban da mat bi loai khoi brief");
    db.writer
        .exec({
            let gone = members[2].file_id;
            move |c| {
                c.execute("UPDATE files SET status = 0 WHERE id = ?1", [gone])?;
                Ok(())
            }
        })
        .unwrap();

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
    let (g2, w2) = db.pool.with(|c| ops::dedup_stats(c, 0)).unwrap();
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
    assert_eq!(version, ops::SCHEMA_VERSION);
    let mut columns = conn.prepare("PRAGMA table_info(org_ops)").unwrap();
    let names: Vec<String> = columns
        .query_map([], |r| r.get(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(names.iter().any(|name| name == "recovery_error"));
    assert!(names.iter().any(|name| name == "recovery_attempted_at"));
}

#[test]
fn schema_newer_version_bails_instead_of_wiping() {
    // App cũ mở data mới (downgrade) không được wipe — org_ops journal (undo +
    // recovery intents) không rebuild lại được bằng rescan.
    let newer = ops::SCHEMA_VERSION + 1;
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    ops::ensure_schema(&mut conn).unwrap();
    conn.execute_batch(&format!(
        "INSERT INTO kv(key, value) VALUES('canary', 'still-here');
         PRAGMA user_version = {newer};"
    ))
    .unwrap();

    let err = ops::ensure_schema(&mut conn).unwrap_err().to_string();
    assert!(err.contains("ERR_SCHEMA_TOO_NEW"), "{err}");

    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, newer);
    let canary: String = conn
        .query_row("SELECT value FROM kv WHERE key = 'canary'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(canary, "still-here");
}
