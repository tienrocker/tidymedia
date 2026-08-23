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

    // Job warm thumbnail phải cày theo ĐÚNG thứ tự user sẽ cuộn, nếu không thì
    // ảnh trên màn hình đầu tiên lại được warm sau cùng.
    //
    // So THỨ TỰ TƯƠNG ĐỐI chứ không so nguyên danh sách: hai bên cố ý khác tập
    // hợp. Browse hiện cả file cloud (status=2) mà warm thì không được đụng
    // (đọc là kéo file về từ mạng), còn warm có cả MOV của Live Photo mà browse
    // ẩn đi. Ràng "bằng nhau" là test sẽ vỡ oan khi hai ca đó xuất hiện.
    let warm = db.pool.with(ops::select_present_ids_by_date).unwrap();
    let browse = db
        .pool
        .with(|c| query::query_ids(c, &FileFilter::default()))
        .unwrap();
    let keep = |src: &[i64], other: &[i64]| -> Vec<i64> {
        src.iter()
            .copied()
            .filter(|id| other.contains(id))
            .collect()
    };
    assert_eq!(
        keep(&warm, &browse),
        keep(&browse, &warm),
        "thu tu warm phai khop thu tu browse mac dinh"
    );
    // Cụ thể: ảnh mới nhất theo NGÀY CHỤP đứng đầu, không phải file index trước
    assert_eq!(warm[0], mid_id);
    assert_eq!(warm.last(), Some(&old_id));
}

/// Lọc theo thiết bị: chuỗi khớp ĐÚNG, và danh sách thiết bị chỉ đếm file đang
/// hiện diện — thiết bị mà mọi ảnh của nó đã mất thì để lại chỉ tổ chọn vào rồi
/// ra 0 kết quả.
#[test]
fn camera_filter_matches_exactly_and_lists_only_live_devices() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path()).unwrap();
    db.writer.exec(|c| ops::upsert_root(c, "D:\\P")).unwrap();
    let entries = vec![
        entry("D:\\P", "a.jpg", "jpg", 0, 100, 1),
        entry("D:\\P", "b.jpg", "jpg", 0, 100, 2),
        entry("D:\\P", "c.jpg", "jpg", 0, 100, 3),
        entry("D:\\P", "gone.jpg", "jpg", 0, 100, 4),
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
    let (a, b, c_id, gone) = (
        id_of("a.jpg"),
        id_of("b.jpg"),
        id_of("c.jpg"),
        id_of("gone.jpg"),
    );
    let meta = |file_id: i64, cam: &str, mtime: i64| core_db::MetaUpsert {
        file_id,
        camera: Some(cam.to_string()),
        meta_state: 1,
        src_mtime: mtime,
        src_size: 100,
        ..Default::default()
    };
    let rows = vec![
        meta(a, "Apple iPhone 12", 1),
        meta(b, "Apple iPhone 12", 2),
        // Tên chỉ khác phần đuôi — lọc phải KHÔNG dính, nếu dùng LIKE thì dính
        meta(c_id, "Apple iPhone 12 Pro", 3),
        meta(gone, "Canon EOS R5", 4),
    ];
    db.writer
        .exec(move |c| ops::upsert_meta_batch(c, &rows))
        .unwrap();

    let by_cam = |cam: &str| {
        let f = FileFilter {
            camera: Some(cam.into()),
            ..Default::default()
        };
        db.pool.with(|c| query::query_ids(c, &f)).unwrap()
    };
    let mut got = by_cam("Apple iPhone 12");
    got.sort();
    assert_eq!(got, vec![a, b], "khong duoc dinh ban 'Pro'");
    assert_eq!(by_cam("Apple iPhone 12 Pro"), vec![c_id]);
    assert!(by_cam("May Khong Ton Tai").is_empty());

    let cams = db.pool.with(query::list_cameras).unwrap();
    assert_eq!(cams[0].camera, "Apple iPhone 12");
    assert_eq!(cams[0].count, 2, "nhieu file nhat phai dung dau");
    assert_eq!(cams.len(), 3);

    // gone.jpg biến mất (ổ tháo ra) → Canon rời khỏi danh sách thiết bị
    db.writer
        .exec(move |c| -> anyhow::Result<()> {
            c.execute("UPDATE files SET status = 1 WHERE id = ?1", [gone])?;
            Ok(())
        })
        .unwrap();
    let cams = db.pool.with(query::list_cameras).unwrap();
    assert!(!cams.iter().any(|c| c.camera == "Canon EOS R5"), "{cams:?}");
}

/// Organize giới hạn theo thư mục nguồn. Ca nguy hiểm là hai thư mục mà tên
/// cái này là TIỀN TỐ của cái kia — kho thật có đúng `anh` và `anh cuoi` cạnh
/// nhau, chọn `anh` mà nuốt luôn `anh cuoi` là chuyển nhầm hàng nghìn file.
#[test]
fn org_scope_limits_to_chosen_folders_without_prefix_bleed() {
    use core_db::org;

    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path()).unwrap();
    db.writer.exec(|c| ops::upsert_root(c, "D:\\P")).unwrap();
    let entries = vec![
        entry("D:\\P\\anh", "a.jpg", "jpg", 0, 100, 1),
        entry("D:\\P\\anh\\2019", "b.jpg", "jpg", 0, 100, 2),
        entry("D:\\P\\anh cuoi", "c.jpg", "jpg", 0, 100, 3),
        entry("D:\\P\\vod", "d.mp4", "mp4", 1, 100, 4),
    ];
    db.writer
        .exec(move |c| {
            let mut cache = HashMap::new();
            ops::upsert_scan_batch(c, 1, 1, &entries, &mut cache)
        })
        .unwrap();

    let names = |scopes: Vec<String>| -> Vec<String> {
        let mut v = db
            .pool
            .with(|c| org::select_org_candidates(c, 1, 0, 100, &scopes))
            .unwrap()
            .into_iter()
            .map(|r| r.path)
            .collect::<Vec<_>>();
        v.sort();
        v
    };
    let count = |scopes: Vec<String>| -> i64 {
        db.pool
            .with(|c| org::count_org_candidates(c, 1, &scopes))
            .unwrap()
    };

    // Rỗng = cả volume, đúng hành vi trước khi có tham số này
    assert_eq!(names(vec![]).len(), 4);
    assert_eq!(count(vec![]), 4);

    // Chọn "anh": lấy cả thư mục con, KHÔNG đụng "anh cuoi"
    let only_anh = names(vec!["D:\\P\\anh".into()]);
    assert_eq!(
        only_anh,
        vec![
            "D:\\P\\anh\\2019\\b.jpg".to_string(),
            "D:\\P\\anh\\a.jpg".to_string()
        ],
        "chon 'anh' khong duoc nuot 'anh cuoi'"
    );
    assert_eq!(count(vec!["D:\\P\\anh".into()]), 2, "dem phai khop select");

    // Nhiều scope cùng lúc
    assert_eq!(
        count(vec!["D:\\P\\anh cuoi".into(), "D:\\P\\vod".into()]),
        2
    );
    // Thư mục không tồn tại → rỗng, không nổ
    assert_eq!(count(vec!["D:\\P\\khong co".into()]), 0);
    // Không phân biệt hoa thường, và dấu \ thừa ở cuối vẫn nhận
    assert_eq!(count(vec!["d:\\p\\ANH\\".into()]), 2);
}

/// Giới hạn thư mục nguồn KHÔNG được loại file đã nằm trong kho đích.
///
/// Loại nó ra là hỏng hai thứ: (a) đổi template rồi gom lại thì file cũ nằm im
/// ở đường dẫn cũ, kho thành nửa nọ nửa kia; (b) cặp Live Photo gom hụt (ảnh
/// xong, MOV fail) không bao giờ hàn lại được, vì đường sửa nằm ở nhánh
/// SkipOrganized của planner mà ảnh thì không còn được chọn nữa.
#[test]
fn org_scope_still_includes_files_already_in_the_library() {
    use core_db::org;

    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path()).unwrap();
    db.writer.exec(|c| ops::upsert_root(c, "D:\\P")).unwrap();
    let entries = vec![
        entry("D:\\P\\icloud", "new.jpg", "jpg", 0, 100, 1),
        // Đã gom vào kho ở lượt trước — kho nằm NGOÀI mọi thư mục nguồn
        entry("D:\\P\\media\\Photos\\2019", "done.jpg", "jpg", 0, 100, 2),
        // Ngoài cả nguồn lẫn kho → vẫn phải bị loại
        entry("D:\\P\\vod", "phim.mp4", "mp4", 1, 100, 3),
        // Từng được gom nhưng đã UNDO → đang nằm chỗ user để nó, không phải chỗ
        // organize đặt. Ngoài phạm vi thì phải bị loại như mọi file khác.
        entry("D:\\P\\reverted", "back.jpg", "jpg", 0, 100, 4),
        // Op còn active nhưng user đã TỰ TAY dời file khỏi chỗ op đặt → lịch
        // sử op không chứng minh hiện tại, phải bị loại như file thường.
        entry("D:\\P\\strayed", "moved.jpg", "jpg", 0, 100, 5),
    ];
    db.writer
        .exec(move |c| {
            let mut cache = HashMap::new();
            ops::upsert_scan_batch(c, 1, 1, &entries, &mut cache)
        })
        .unwrap();
    db.writer
        .exec(|c| org::set_library_root(c, "D:\\P\\media"))
        .unwrap();

    let id_of = |dir: &str, name: &str| -> i64 {
        let (dir, name) = (dir.to_string(), name.to_string());
        db.pool.with(move |c| {
            c.query_row(
                "SELECT f.id FROM files f JOIN dirs d ON d.id = f.dir_id
                 WHERE d.path = ?1 AND f.name = ?2",
                rusqlite::params![dir, name],
                |r| r.get(0),
            )
            .unwrap()
        })
    };
    let done_id = id_of("D:\\P\\media\\Photos\\2019", "done.jpg");
    let back_id = id_of("D:\\P\\reverted", "back.jpg");
    let moved_id = id_of("D:\\P\\strayed", "moved.jpg");
    db.writer
        .exec(move |c| {
            // Op đặt done.jpg vào ĐÚNG chỗ nó đang nằm — marker đối chiếu
            // new_path với vị trí hiện tại, đích bịa thì không còn là fixture
            // của "đang được organize giữ".
            let done = org::insert_org_op(
                c,
                1,
                done_id,
                "D:\\P\\icloud\\a.jpg",
                "D:\\P\\media\\Photos\\2019\\done.jpg",
                "D:\\P\\media",
            )?;
            org::mark_org_op_done(c, done)?;
            // back.jpg: gom xong rồi undo. Kèm chính op undo (done, CHƯA kịp
            // tự đánh dấu undone — residue của crash cuối lượt): đích của op
            // undo trùng vị trí hiện tại, nhưng nó KHÔNG được làm file thành
            // "đang được organize giữ" — nó là dấu user rút file RA.
            let undone = org::insert_org_op(
                c,
                1,
                back_id,
                "D:\\P\\reverted\\back.jpg",
                "D:\\P\\media\\Photos\\2019\\back.jpg",
                "D:\\P\\media",
            )?;
            org::mark_org_op_done(c, undone)?;
            org::mark_org_op_undone(c, undone)?;
            let undo_intent = org::insert_undo_op(
                c,
                2,
                back_id,
                "D:\\P\\media\\Photos\\2019\\back.jpg",
                "D:\\P\\reverted\\back.jpg",
                undone,
            )?;
            org::mark_org_op_done(c, undo_intent)?;
            // moved.jpg: op active trỏ vào kho nhưng file thật đã bị user dời
            // về D:\P\strayed từ lâu (rescan cập nhật vị trí).
            let strayed = org::insert_org_op(
                c,
                1,
                moved_id,
                "D:\\P\\icloud\\moved.jpg",
                "D:\\P\\media\\Photos\\2019\\moved.jpg",
                "D:\\P\\media",
            )?;
            org::mark_org_op_done(c, strayed)?;
            Ok(())
        })
        .unwrap();

    let scopes = vec!["D:\\P\\icloud".to_string()];
    let paths = |scopes: &[String]| {
        let s = scopes.to_vec();
        let mut p = db
            .pool
            .with(move |c| org::select_org_candidates(c, 1, 0, 100, &s))
            .unwrap()
            .into_iter()
            .map(|r| r.path)
            .collect::<Vec<_>>();
        p.sort();
        p
    };
    let expected = vec![
        "D:\\P\\icloud\\new.jpg".to_string(),
        "D:\\P\\media\\Photos\\2019\\done.jpg".to_string(),
    ];
    assert_eq!(
        paths(&scopes),
        expected,
        "file organize dang giu phai con la ung vien; file da undo hoac da bi \
         user doi di cho khac thi khong"
    );
    // Provenance đi kèm: ứng viên organize đang giữ phải mang gốc kho TẠI THỜI
    // ĐIỂM đặt (nguồn tính {relpath} sau khi user đổi thư mục kho); file thường
    // thì không có.
    let s2 = scopes.clone();
    let roots_of: Vec<(String, Option<String>)> = db
        .pool
        .with(move |c| org::select_org_candidates(c, 1, 0, 100, &s2))
        .unwrap()
        .into_iter()
        .map(|r| (r.path, r.managed_lib_root))
        .collect();
    for (path, lib) in roots_of {
        let want = if path.ends_with("done.jpg") {
            Some("D:\\P\\media".to_string())
        } else {
            None
        };
        assert_eq!(lib, want, "managed_lib_root cua {path}");
    }
    let for_count = scopes.clone();
    assert_eq!(
        db.pool
            .with(move |c| org::count_org_candidates(c, 1, &for_count))
            .unwrap(),
        2,
        "dem phai khop select"
    );

    // Kho đích là THƯ MỤC CHA của thư mục nguồn. Bản sửa trước nới theo cây kho
    // nên cấu hình này làm phạm vi mất sạch tác dụng: `vod` lọt vào đúng cái mà
    // phạm vi sinh ra để chặn, im lặng, không một cảnh báo nào.
    db.writer
        .exec(|c| org::set_library_root(c, "D:\\P"))
        .unwrap();
    assert_eq!(
        paths(&scopes),
        expected,
        "kho dich trum len nguon KHONG duoc keo ca o vao pham vi"
    );

    // Đổi thư mục kho: `set_library_root` chỉ ghi đè path chứ không dời file,
    // nên bám theo cây kho thì file ở kho CŨ không thuộc nguồn lẫn kho mới —
    // biến mất khỏi ứng viên, không bao giờ được xếp sang kho mới nữa.
    db.writer
        .exec(|c| org::set_library_root(c, "D:\\P\\media2"))
        .unwrap();
    assert_eq!(
        paths(&scopes),
        expected,
        "doi thu muc kho khong duoc lam mat dau file dang o kho cu"
    );
}

/// Nhãn + album là dữ liệu user tự tạo, KHÔNG dựng lại được bằng quét — nên
/// test ở đây soi kỹ mấy đường mất mát âm thầm.
#[test]
fn tags_and_albums_survive_the_ways_they_could_silently_break() {
    use core_db::collections as col;

    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path()).unwrap();
    db.writer.exec(|c| ops::upsert_root(c, "D:\\P")).unwrap();
    let entries = (1..=4)
        .map(|i| entry("D:\\P", &format!("{i}.jpg"), "jpg", 0, 100, i))
        .collect::<Vec<_>>();
    db.writer
        .exec(move |c| {
            let mut cache = HashMap::new();
            ops::upsert_scan_batch(c, 1, 1, &entries, &mut cache)
        })
        .unwrap();
    let ids: Vec<i64> = db
        .pool
        .with(|c| -> anyhow::Result<Vec<i64>> {
            let mut st = c.prepare("SELECT id FROM files ORDER BY name")?;
            let v = st
                .query_map([], |r| r.get(0))?
                .collect::<Result<Vec<i64>, _>>()?;
            Ok(v)
        })
        .unwrap();

    // Gõ lại tên khác hoa thường KHÔNG được đẻ nhãn thứ hai trông y hệt
    let want = ids[..2].to_vec();
    let t1 = db
        .writer
        .exec(move |c| col::tag_files(c, "Gia dinh", &want))
        .unwrap();
    let want = ids[2..3].to_vec();
    let t2 = db
        .writer
        .exec(move |c| col::tag_files(c, "  gia   DINH  ", &want))
        .unwrap();
    assert_eq!(t1, t2, "cung mot nhan, khong duoc tao nhan thu hai");
    let tags = db.pool.with(col::list_tags).unwrap();
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].name, "Gia dinh", "giu ten lan dau, khong ghi de");
    assert_eq!(tags[0].count, 3);

    // Tên rỗng/toàn khoảng trắng bị từ chối — nhãn vô hình thì user không bấm trúng
    assert!(db
        .writer
        .exec(move |c| col::tag_files(c, "   ", &[]))
        .is_err());

    // Lọc theo nhãn
    let f = FileFilter {
        tag_id: Some(t1),
        ..Default::default()
    };
    assert_eq!(db.pool.with(|c| query::query_ids(c, &f)).unwrap().len(), 3);

    // Album giữ ĐÚNG thứ tự user thêm vào, kể cả khi thêm làm nhiều đợt và thứ
    // tự đó ngược hẳn với thứ tự ngày
    // Thứ tự thêm CỐ Ý khác hẳn thứ tự ngày (ngày giảm dần cho {1,2,4} là
    // [4,2,1]) — trùng nhau thì phép so bên dưới không chứng minh được gì.
    let album = db
        .writer
        .exec(|c| col::create_album(c, "Tết 2019"))
        .unwrap();
    let first = vec![ids[0], ids[3]];
    let n = db
        .writer
        .exec(move |c| col::add_to_album(c, album, &first))
        .unwrap();
    assert_eq!(n, 2);
    // Thêm lại file đã có + một file mới → chỉ file mới được tính
    let again = vec![ids[3], ids[1]];
    let n = db
        .writer
        .exec(move |c| col::add_to_album(c, album, &again))
        .unwrap();
    assert_eq!(n, 1, "file da co san trong album khong duoc dem lai");

    let f = FileFilter {
        album_id: Some(album),
        sort: Some("album".into()),
        ..Default::default()
    };
    assert_eq!(
        db.pool.with(|c| query::query_ids(c, &f)).unwrap(),
        vec![ids[0], ids[3], ids[1]],
        "phai giu thu tu da them, khong sap lai theo ngay"
    );
    // Cùng album nhưng sort mặc định → quay về thứ tự ngày
    let f = FileFilter {
        album_id: Some(album),
        ..Default::default()
    };
    assert_eq!(
        db.pool.with(|c| query::query_ids(c, &f)).unwrap(),
        vec![ids[3], ids[1], ids[0]],
        "sort mac dinh van la ngay giam dan"
    );

    // sort="album" mà KHÔNG xem album nào thì phải rơi về ngày, không nổ
    let f = FileFilter {
        sort: Some("album".into()),
        ..Default::default()
    };
    assert_eq!(db.pool.with(|c| query::query_ids(c, &f)).unwrap().len(), 4);

    // Xoá file → nó rời khỏi nhãn lẫn album, không để lại dòng mồ côi làm sai
    // số đếm (đây là thứ bản schema cũ thiếu khoá ngoại đã làm sai)
    let gone = ids[1];
    db.writer
        .exec(move |c| -> anyhow::Result<()> {
            c.execute("DELETE FROM files WHERE id = ?1", [gone])?;
            Ok(())
        })
        .unwrap();
    let orphans: i64 = db
        .pool
        .with(|c| -> anyhow::Result<i64> {
            Ok(c.query_row(
                "SELECT (SELECT COUNT(*) FROM file_tags WHERE file_id = ?1)
                      + (SELECT COUNT(*) FROM album_files WHERE file_id = ?1)",
                [gone],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(orphans, 0, "xoa file phai keo theo dong nhan/album");

    // Xoá album KHÔNG được đụng tới file — album là cách xếp, không phải nơi chứa
    db.writer
        .exec(move |c| col::delete_album(c, album))
        .unwrap();
    assert!(db.pool.with(col::list_albums).unwrap().is_empty());
    let left: i64 = db
        .pool
        .with(|c| -> anyhow::Result<i64> {
            Ok(c.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?)
        })
        .unwrap();
    assert_eq!(left, 3, "xoa album khong duoc xoa file");
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

/// Export = `VACUUM INTO`: bản xuất phải là MỘT file tự đủ (gộp WAL, không cần
/// kèm -wal/-shm) và không bao giờ đè lên file có sẵn.
#[test]
fn vacuum_into_writes_a_self_contained_copy_and_never_overwrites() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path()).unwrap();
    db.writer
        .exec(|c| ops::upsert_root(c, "D:\\Photos"))
        .unwrap();
    let entries = vec![entry("D:\\Photos", "a.jpg", "jpg", 0, 10, 1)];
    db.writer
        .exec(move |c| {
            let mut cache = HashMap::new();
            ops::upsert_scan_batch(c, 1, 1, &entries, &mut cache)
        })
        .unwrap();

    // Ghi vừa xong còn nằm trong WAL — bản xuất phải có nó
    let out = tmp.path().join("export").join("index.db");
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    db.vacuum_into(&out).unwrap();
    assert!(out.is_file());
    assert!(
        !out.with_extension("db-wal").exists(),
        "ban xuat phai tu du, khong keo theo WAL"
    );

    let copy = Db::open(out.parent().unwrap()).unwrap();
    let roots = copy.pool.with(ops::list_roots).unwrap();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].path, "D:\\Photos");
    assert_eq!(
        roots[0].file_count, 0,
        "file_count chi cap nhat khi scan xong"
    );
    let n: i64 = copy
        .pool
        .with(|c| -> anyhow::Result<i64> {
            Ok(c.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?)
        })
        .unwrap();
    assert_eq!(n, 1, "du lieu trong WAL phai theo sang ban xuat");

    // Đích đã tồn tại: SQLite tự từ chối, không đè file của ai cả
    assert!(
        db.vacuum_into(&out).is_err(),
        "khong duoc de len file da co"
    );
}

/// Id nhóm phải sống sót qua rebuild kể cả khi thành viên NHỎ NHẤT rời nhóm.
/// Khóa cũ là `MIN(file_id)` nên mất đúng member đó là nhóm nhận id mới, UI
/// tưởng nhóm biến mất và vứt sạch tick user vừa đánh dấu.
#[test]
fn similar_group_id_survives_losing_its_smallest_member() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(tmp.path()).unwrap();
    db.writer.exec(|c| ops::upsert_root(c, "D:\\S")).unwrap();
    let entries = vec![
        entry("D:\\S", "a.jpg", "jpg", 0, 3_000_000, 10),
        entry("D:\\S", "b.jpg", "jpg", 0, 2_000_000, 20),
        entry("D:\\S", "c.jpg", "jpg", 0, 1_000_000, 30),
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
    let rows: Vec<core_db::PhashUpsert> = pending
        .iter()
        .map(|p| core_db::PhashUpsert {
            file_id: p.file_id,
            hash: Some([0b1010_1010, 0, 0, 0]),
            src_mtime: p.mtime,
            src_size: p.size,
        })
        .collect();
    db.writer
        .exec(move |c| ops::upsert_phash_batch(c, &rows))
        .unwrap();
    db.writer
        .exec(|c| ops::rebuild_similar_groups(c, 6))
        .unwrap();
    let before = db.pool.with(|c| ops::list_dup_groups(c, 1)).unwrap();
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].count, 3);
    let gid = before[0].id;

    // File nhỏ nhất biến mất khỏi kho (xóa ngoài app / ổ tháo ra)
    let smallest = pending.iter().map(|p| p.file_id).min().unwrap();
    db.writer
        .exec(move |c| {
            c.execute("UPDATE files SET status = 1 WHERE id = ?1", [smallest])?;
            Ok(())
        })
        .unwrap();
    db.writer
        .exec(|c| ops::rebuild_similar_groups(c, 6))
        .unwrap();

    let after = db.pool.with(|c| ops::list_dup_groups(c, 1)).unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].count, 2);
    assert_eq!(
        after[0].id, gid,
        "mat member nho nhat khong duoc lam doi id nhom"
    );
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
        .with(|c| org::select_org_candidates(c, 1, 0, 100, &[]))
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
            org::insert_org_op(
                c,
                jid,
                img_id,
                "D:\\Mess\\IMG_1.heic",
                "D:\\MyLib2\\x.heic",
                "D:\\MyLib2",
            )
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
        .with(|c| org::select_org_candidates(c, 1, 0, 100, &[]))
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
    // Hoàn tác MỌI cột thêm sau v5, không riêng cột của bước đang xét: một DB
    // v5 thật thì chưa có cột nào cả, mà ensure_schema sẽ chạy hết v5 → v8.
    conn.execute_batch(
        "ALTER TABLE org_ops DROP COLUMN recovery_error;
         ALTER TABLE org_ops DROP COLUMN recovery_attempted_at;
         ALTER TABLE org_ops DROP COLUMN reverses_op_id;
         ALTER TABLE org_ops DROP COLUMN lib_root;
         DROP INDEX org_ops_file;
         ALTER TABLE media_meta DROP COLUMN gps_lat;
         ALTER TABLE media_meta DROP COLUMN gps_lon;
         ALTER TABLE media_meta DROP COLUMN meta_ver;
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

/// v9 → v11: index tra org_ops theo file + cột phân loại undo/provenance.
/// Gọt DB hiện hành về đúng hình dạng v9 rồi migrate — dựng thẳng schema mới
/// xong hạ user_version thì index/cột đã có sẵn, test không chứng minh gì.
#[test]
fn schema_v9_migrates_org_ops_index_and_journal_columns() {
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    ops::ensure_schema(&mut conn).unwrap();
    conn.execute_batch(
        "DROP INDEX org_ops_file;
         ALTER TABLE org_ops DROP COLUMN reverses_op_id;
         ALTER TABLE org_ops DROP COLUMN lib_root;
         PRAGMA user_version = 9;",
    )
    .unwrap();

    ops::ensure_schema(&mut conn).unwrap();

    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, ops::SCHEMA_VERSION);
    let indexes: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'index' AND name = 'org_ops_file'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(indexes, 1, "migration phai tu tao index org_ops_file");
    let names: Vec<String> = conn
        .prepare("PRAGMA table_info(org_ops)")
        .unwrap()
        .query_map([], |r| r.get(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        names.iter().any(|name| name == "reverses_op_id"),
        "{names:?}"
    );
    assert!(names.iter().any(|name| name == "lib_root"), "{names:?}");
}

/// v7 → v8: thêm toạ độ + dấu phiên bản bộ trích. Meta ĐANG CÓ phải còn nguyên
/// — nâng cấp app không được làm kho ảnh mất ngày chụp rồi bắt quét lại từ đầu.
#[test]
fn schema_v8_adds_gps_without_losing_existing_meta() {
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    ops::ensure_schema(&mut conn).unwrap();
    conn.execute_batch(
        "ALTER TABLE media_meta DROP COLUMN gps_lat;
         ALTER TABLE media_meta DROP COLUMN gps_lon;
         ALTER TABLE media_meta DROP COLUMN meta_ver;
         ALTER TABLE org_ops DROP COLUMN reverses_op_id;
         ALTER TABLE org_ops DROP COLUMN lib_root;
         DROP INDEX org_ops_file;
         PRAGMA user_version = 7;",
    )
    .unwrap();
    let file_id = {
        ops::upsert_root(&mut conn, "D:\\P").unwrap();
        let mut cache = HashMap::new();
        ops::upsert_scan_batch(
            &mut conn,
            1,
            1,
            &[entry("D:\\P", "IMG_1.JPG", "jpg", 0, 100, 1)],
            &mut cache,
        )
        .unwrap();
        conn.query_row("SELECT id FROM files", [], |r| r.get::<_, i64>(0))
            .unwrap()
    };
    conn.execute(
        "INSERT INTO media_meta(file_id, width, height, taken_at, camera, meta_state)
         VALUES(?1, 4032, 3024, 1560526222000, 'Apple iPhone 12', 1)",
        rusqlite::params![file_id],
    )
    .unwrap();

    ops::ensure_schema(&mut conn).unwrap();

    let (w, taken, cam, ver): (i64, i64, String, i64) = conn
        .query_row(
            "SELECT width, taken_at, camera, meta_ver FROM media_meta WHERE file_id = ?1",
            rusqlite::params![file_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        (w, taken, cam.as_str()),
        (4032, 1_560_526_222_000, "Apple iPhone 12")
    );
    // meta_ver = 0 < META_VERSION → job meta tự chọn lại để trích bù toạ độ
    assert_eq!(ver, 0);
    assert!(ops::count_pending_meta(&conn, true).unwrap() >= 1);
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
