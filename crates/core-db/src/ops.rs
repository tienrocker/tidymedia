use std::collections::HashMap;

use anyhow::{anyhow, bail, Result};
use rusqlite::{params, Connection, OptionalExtension};
use unicode_normalization::UnicodeNormalization;

use crate::models::{
    ClusterItem, DeleteContextRow, DupGroupRow, DupMemberBrief, DupMemberRow, FileDetail,
    HashUpsert, JobRow, MediaSrc, MetaUpsert, PendingHash, PendingMeta, PendingPhash, PhashUpsert,
    RootInfo, ScanEntry,
};

/// Bump khi đổi schema. Có migration tăng dần từ v2 trở đi (giữ index của
/// user); version lạ/quá cũ → wipe & recreate (rebuild bằng rescan, ok pre-1.0).
pub const SCHEMA_VERSION: i64 = 8;

/// Phiên bản của BỘ TRÍCH metadata. Bump khi extractor học thêm field: dòng
/// `media_meta` cũ hơn sẽ được [`select_pending_meta`] chọn lại, nên job `meta`
/// sẵn có tự trích bù — không phải viết job mới, và vẫn tạm dừng/tiếp tục được.
///
/// Chỉ chọn lại dòng `meta_state = 1` (đã trích THÀNH CÔNG). Dòng thất bại
/// (file hỏng, format lạ) thì bump version không cứu được, mà đọc lại toàn bộ
/// file hỏng sau mỗi lần bump thì mỗi bản cập nhật lại tốn một lượt quét vô ích.
///
/// | ver | thêm gì |
/// |---|---|
/// | 1 | `gps_lat`/`gps_lon` — EXIF GPS + ISO 6709 của video |
pub const META_VERSION: i64 = 1;

/// Migration tăng dần: MIGRATIONS[i] đưa schema từ version (i+2) lên (i+3).
/// DDL PHẢI giống hệt schema.sql (fresh install đi thẳng schema.sql).
const MIGRATIONS: &[&str] = &[
    // v2 -> v3: trigger invalidate meta/hash khi file đổi nội dung
    "CREATE TRIGGER files_meta_invalidate AFTER UPDATE OF size, mtime ON files
     WHEN old.size != new.size OR old.mtime != new.mtime
     BEGIN
       DELETE FROM media_meta WHERE file_id = old.id;
       DELETE FROM hashes WHERE file_id = old.id;
       DELETE FROM phashes WHERE file_id = old.id;
     END;",
    // v3 -> v4: index reverse lookup Live Photo
    "CREATE INDEX files_live_pair ON files(live_pair_id) WHERE live_pair_id IS NOT NULL;",
    // v4 -> v5: FTS AU trigger thêm WHEN (rescan không rewrite trigram khi tên
    // không đổi) + org_ops.undone_at cho undo M5
    "DROP TRIGGER files_fts_au;
     CREATE TRIGGER files_fts_au AFTER UPDATE OF name_norm ON files
     WHEN old.name_norm IS NOT new.name_norm
     BEGIN
       INSERT INTO files_fts(files_fts, rowid, name_norm) VALUES ('delete', old.id, old.name_norm);
       INSERT INTO files_fts(rowid, name_norm) VALUES (new.id, new.name_norm);
     END;
     ALTER TABLE org_ops ADD COLUMN undone_at INTEGER;",
    // v5 -> v6: ambiguous crash-recovery intents are retained for diagnosis, but marked so
    // startup does not hash/stat the same unresolved files forever.
    "ALTER TABLE org_ops ADD COLUMN recovery_error TEXT;
     ALTER TABLE org_ops ADD COLUMN recovery_attempted_at INTEGER;",
    // v6 -> v7: dhash lên 256 bit (4 dòng seq 0..3). Hash 64-bit cũ vô nghĩa
    // dưới sơ đồ mới — xóa sạch để job tính lại (đọc từ thumb cache nên nhanh),
    // kèm nhóm gần giống vì chúng dựng ra từ hash cũ. KHÔNG có DDL: cột seq đã
    // có sẵn trong schema.sql từ đầu.
    "DELETE FROM phashes;
     DELETE FROM dup_members WHERE group_id IN (SELECT id FROM dup_groups WHERE kind = 1);
     DELETE FROM dup_groups WHERE kind = 1;",
    // v7 -> v8: toạ độ nơi chụp + phiên bản bộ trích. Dòng cũ mang meta_ver = 0
    // nên tự động thấp hơn META_VERSION → job meta trích bù, KHÔNG xoá gì cả:
    // width/height/taken_at/camera đang có vẫn dùng được nguyên trong lúc chờ.
    "ALTER TABLE media_meta ADD COLUMN gps_lat REAL;
     ALTER TABLE media_meta ADD COLUMN gps_lon REAL;
     ALTER TABLE media_meta ADD COLUMN meta_ver INTEGER NOT NULL DEFAULT 0;",
];
const OLDEST_MIGRATABLE: i64 = 2;

pub fn ensure_schema(conn: &mut Connection) -> Result<()> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version == SCHEMA_VERSION {
        return Ok(());
    }
    if (OLDEST_MIGRATABLE..SCHEMA_VERSION).contains(&version) {
        let tx = conn.transaction()?;
        for migration in MIGRATIONS
            .iter()
            .skip((version - OLDEST_MIGRATABLE) as usize)
        {
            tx.execute_batch(migration)?;
        }
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        tx.commit()?;
        tracing::info!("schema migrated v{version} -> v{SCHEMA_VERSION} (index giữ nguyên)");
        return Ok(());
    }
    // App CŨ mở data MỚI (downgrade/rollback) tuyệt đối không được rơi xuống
    // nhánh wipe: mất index còn rebuild được, nhưng org_ops journal (undo +
    // crash-recovery intents đang chờ) thì không. Từ chối mở, giữ nguyên data.
    if version > SCHEMA_VERSION {
        bail!(
            "ERR_SCHEMA_TOO_NEW|index db v{version} > app v{SCHEMA_VERSION} — \
             app cũ hơn dữ liệu; hãy cập nhật app (bảo vệ journal organize/undo)"
        );
    }
    let tx = conn.transaction()?;
    // Wipe schema cũ (v0/v1). DROP TABLE tự kéo trigger + FTS shadow tables theo.
    for t in [
        "files_fts",
        "org_ops",
        "import_seen",
        "imports",
        "library_roots",
        "album_files",
        "albums",
        "file_tags",
        "tags",
        "dup_members",
        "dup_groups",
        "phashes",
        "hashes",
        "media_meta",
        "files",
        "dirs",
        "roots",
        "volumes",
        "jobs",
        "kv",
    ] {
        tx.execute_batch(&format!("DROP TABLE IF EXISTS {t};"))?;
    }
    tx.execute_batch(include_str!("schema.sql"))?;
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()?;
    tracing::info!("schema created/recreated at v{SCHEMA_VERSION}");
    Ok(())
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------- path helpers ----------

/// "D:\Photos\" -> "D:\Photos"; giữ nguyên "D:\"; uppercase drive letter.
pub fn normalize_path(p: &str) -> String {
    let mut s = p.replace('/', "\\");
    while s.len() > 3 && s.ends_with('\\') {
        s.pop();
    }
    let mut chars: Vec<char> = s.chars().collect();
    if chars.len() >= 2 && chars[1] == ':' {
        chars[0] = chars[0].to_ascii_uppercase();
        s = chars.into_iter().collect();
    }
    s
}

/// lowercase + bỏ dấu (NFD strip combining marks, đ→d) — nguồn cho search.
pub fn normalize_for_search(s: &str) -> String {
    s.nfd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .map(|c| match c {
            'đ' => 'd',
            'Đ' => 'D',
            other => other,
        })
        .collect::<String>()
        .to_lowercase()
}

pub(crate) fn drive_letter(path: &str) -> Option<char> {
    let mut ch = path.chars();
    let letter = ch.next()?;
    if letter.is_ascii_alphabetic() && ch.next() == Some(':') {
        Some(letter.to_ascii_uppercase())
    } else {
        None
    }
}

/// Path với separator cuối (để so prefix): "D:\Photos" -> "D:\Photos\", "D:\" giữ nguyên.
fn with_sep(p: &str) -> String {
    if p.ends_with('\\') {
        p.to_string()
    } else {
        format!("{p}\\")
    }
}

/// Range predicate trên path_key (UPPERCASE) cho scope root — dùng được index,
/// không dính LIKE case-folding. Trả (eq, start, end):
///   path_key = eq  OR  (path_key >= start AND path_key < end)
/// end = start với '\' cuối thay bằng ']' (0x5D = 0x5C + 1) — chặn đúng subtree,
/// không dính sibling kiểu "D:\PHOTOSOLD".
pub fn path_range(root: &str) -> (String, String, String) {
    let eq = root.to_uppercase();
    let start = with_sep(&eq);
    let mut end = start.clone();
    end.pop();
    end.push(']');
    (eq, start, end)
}

const ROOT_SCOPE: &str = "(d.path_key = ?1 OR (d.path_key >= ?2 AND d.path_key < ?3))";

// ---------- roots / volumes ----------

/// `canonical` PHẢI là kết quả canonical hóa từ caller (fs::canonicalize, bỏ \\?\,
/// uppercase drive) — hàm này chỉ làm phần DB: chống alias/overlap + absorb.
///
/// Hành vi overlap:
/// - Trùng y hệt → trả root id cũ (idempotent).
/// - Root mới NẰM TRONG root cũ → Err ERR_ROOT_COVERED|<root cũ>.
/// - Root mới BAO TRÙM root cũ → hấp thụ: xóa dòng roots con (GIỮ files/dirs),
///   root mới thay thế.
pub fn upsert_root(conn: &mut Connection, canonical: &str) -> Result<i64> {
    let path = normalize_path(canonical);
    let letter = drive_letter(&path).ok_or_else(|| anyhow!("ERR_ROOT_NO_DRIVE|{path}"))?;
    if path.len() < 3 || !path[2..].starts_with('\\') {
        // "D:" drive-relative — Path::join sẽ tạo "D:2019" sai bét
        bail!("ERR_ROOT_DRIVE_RELATIVE|{path}");
    }

    let up_new = with_sep(&path.to_uppercase());
    let existing: Vec<(i64, String)> = conn
        .prepare("SELECT id, path FROM roots")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let mut absorbed: Vec<i64> = Vec::new();
    for (id, ex_path) in &existing {
        let up_ex = with_sep(&ex_path.to_uppercase());
        if up_ex == up_new {
            return Ok(*id); // idempotent
        }
        if up_new.starts_with(&up_ex) {
            bail!("ERR_ROOT_COVERED|{ex_path}");
        }
        if up_ex.starts_with(&up_new) {
            absorbed.push(*id);
        }
    }

    let tx = conn.transaction()?;
    for id in &absorbed {
        // Chỉ xóa dòng root — files/dirs đã index GIỮ NGUYÊN, root mới phủ lên.
        tx.execute("DELETE FROM roots WHERE id = ?1", params![id])?;
    }
    let guid = format!("{letter}:");
    tx.execute(
        "INSERT INTO volumes(guid, letter, added_at) VALUES(?1, ?1, ?2)
         ON CONFLICT(guid) DO NOTHING",
        params![guid, now_ms()],
    )?;
    let volume_id: i64 = tx.query_row(
        "SELECT id FROM volumes WHERE guid = ?1",
        params![guid],
        |r| r.get(0),
    )?;
    tx.execute(
        "INSERT INTO roots(volume_id, path) VALUES(?1, ?2)",
        params![volume_id, path],
    )?;
    let root_id = tx.last_insert_rowid();
    tx.commit()?;
    if !absorbed.is_empty() {
        tracing::info!(new_root = %path, absorbed = ?absorbed, "root absorbed narrower roots");
    }
    Ok(root_id)
}

pub fn get_root(conn: &Connection, root_id: i64) -> Result<(String, i64)> {
    conn.query_row(
        "SELECT path, volume_id FROM roots WHERE id = ?1",
        params![root_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .map_err(|e| anyhow!("ERR_ROOT_NOT_FOUND|{root_id}: {e}"))
}

pub fn list_roots(conn: &Connection) -> Result<Vec<RootInfo>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, volume_id, path, last_scan_at, file_count FROM roots ORDER BY path",
    )?;
    let roots = stmt
        .query_map([], |r| {
            Ok(RootInfo {
                id: r.get(0)?,
                volume_id: r.get(1)?,
                path: r.get(2)?,
                last_scan_at: r.get(3)?,
                file_count: r.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(roots)
}

/// Đếm thật + cache vào roots.file_count (gọi khi scan xong, không gọi mỗi 500ms).
pub fn refresh_root_count(conn: &Connection, root_path: &str) -> Result<i64> {
    let (eq, start, end) = path_range(root_path);
    let n: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM files f JOIN dirs d ON d.id = f.dir_id
             WHERE f.status IN (0, 2) AND {ROOT_SCOPE}"
        ),
        params![eq, start, end],
        |r| r.get(0),
    )?;
    conn.execute(
        "UPDATE roots SET file_count = ?1 WHERE path = ?2",
        params![n, root_path],
    )?;
    Ok(n)
}

pub fn refresh_all_root_counts(conn: &Connection) -> Result<()> {
    let roots: Vec<String> = {
        let mut stmt = conn.prepare("SELECT path FROM roots")?;
        let rows = stmt
            .query_map([], |r| r.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    for path in roots {
        refresh_root_count(conn, &path)?;
    }
    Ok(())
}

/// Xóa index của root theo chunk 10k/transaction — không giữ writer hàng chục giây,
/// không phình WAL. Caller (command) phải hủy scan đang chạy TRƯỚC khi gọi.
pub fn remove_root_chunked(conn: &mut Connection, root_id: i64) -> Result<()> {
    let (path, _) = get_root(conn, root_id)?;
    let (eq, start, end) = path_range(&path);
    loop {
        let n = conn.execute(
            &format!(
                "DELETE FROM files WHERE id IN (
                   SELECT f.id FROM files f JOIN dirs d ON d.id = f.dir_id
                   WHERE {ROOT_SCOPE} LIMIT 10000)"
            ),
            params![eq, start, end],
        )?;
        if n == 0 {
            break;
        }
    }
    let tx = conn.transaction()?;
    tx.execute(
        &format!(
            "DELETE FROM dirs WHERE id IN (
               SELECT d.id FROM dirs d WHERE {ROOT_SCOPE})"
        ),
        params![eq, start, end],
    )?;
    tx.execute("DELETE FROM roots WHERE id = ?1", params![root_id])?;
    tx.commit()?;
    Ok(())
}

// ---------- scan writes ----------

/// Upsert 1 batch entries trong 1 transaction. `dir_cache` sống theo scan job;
/// caller PHẢI clear cache nếu hàm này Err (tránh dir id chết sau remove_root).
pub fn upsert_scan_batch(
    conn: &mut Connection,
    volume_id: i64,
    gen: i64,
    entries: &[ScanEntry],
    dir_cache: &mut HashMap<String, i64>,
) -> Result<()> {
    let tx = conn.transaction()?;
    {
        let mut dir_ins = tx.prepare_cached(
            "INSERT INTO dirs(volume_id, name, path, path_key) VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(volume_id, path_key) DO NOTHING",
        )?;
        let mut dir_get =
            tx.prepare_cached("SELECT id FROM dirs WHERE volume_id = ?1 AND path_key = ?2")?;
        let mut file_ins = tx.prepare_cached(
            "INSERT INTO files(dir_id, volume_id, name, name_norm, ext, kind, size, mtime, attrs, status, seen_gen)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(dir_id, name) DO UPDATE SET
               name = excluded.name, name_norm = excluded.name_norm,
               ext = excluded.ext, kind = excluded.kind, size = excluded.size,
               mtime = excluded.mtime, attrs = excluded.attrs,
               status = excluded.status, seen_gen = excluded.seen_gen",
        )?;
        for e in entries {
            let path_key = e.dir_path.to_uppercase();
            let dir_id = match dir_cache.get(&path_key) {
                Some(id) => *id,
                None => {
                    let dir_name = e
                        .dir_path
                        .rsplit('\\')
                        .find(|s| !s.is_empty())
                        .unwrap_or(&e.dir_path)
                        .to_string();
                    dir_ins.execute(params![volume_id, dir_name, e.dir_path, path_key])?;
                    let id: i64 = dir_get.query_row(params![volume_id, path_key], |r| r.get(0))?;
                    dir_cache.insert(path_key, id);
                    id
                }
            };
            file_ins.execute(params![
                dir_id,
                volume_id,
                e.name,
                normalize_for_search(&e.name),
                e.ext,
                e.kind,
                e.size,
                e.mtime,
                e.attrs,
                e.status,
                gen
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Đánh dấu missing những file dưới `root_path` không thấy trong generation này.
pub fn reconcile_scan(conn: &mut Connection, root_path: &str, gen: i64) -> Result<usize> {
    let marked = reconcile_scan_excluding(conn, root_path, gen, &[])?;
    finish_root_scan(conn, root_path)?;
    Ok(marked)
}

/// Reconcile một scan hoàn tất nhưng có một số subtree đọc lỗi. File dưới các
/// scope này giữ nguyên status/generation cũ thay vì bị kết luận `missing` oan.
/// Caller phải bỏ reconcile toàn bộ nếu walker có lỗi không xác định được path.
pub fn reconcile_scan_excluding(
    conn: &mut Connection,
    root_path: &str,
    gen: i64,
    unreadable_scopes: &[String],
) -> Result<usize> {
    use rusqlite::params_from_iter;
    use rusqlite::types::Value;

    let (eq, start, end) = path_range(root_path);
    let mut scope_sql = String::from("(d.path_key = ? OR (d.path_key >= ? AND d.path_key < ?))");
    let mut values = vec![
        Value::Integer(gen),
        Value::Text(eq),
        Value::Text(start),
        Value::Text(end),
    ];
    for scope in unreadable_scopes {
        let (x_eq, x_start, x_end) = path_range(scope);
        scope_sql.push_str(" AND NOT (d.path_key = ? OR (d.path_key >= ? AND d.path_key < ?))");
        values.push(Value::Text(x_eq));
        values.push(Value::Text(x_start));
        values.push(Value::Text(x_end));
    }
    let sql = format!(
        "UPDATE files SET status = 1
         WHERE seen_gen < ? AND status IN (0, 2) AND dir_id IN
           (SELECT d.id FROM dirs d WHERE {scope_sql})"
    );
    let n = conn.execute(&sql, params_from_iter(values.iter()))?;
    Ok(n)
}

/// Bookkeeping for every successfully completed scan, including conservative scans where
/// reconcile had to be skipped because an error could not be scoped safely.
pub fn finish_root_scan(conn: &Connection, root_path: &str) -> Result<()> {
    conn.execute(
        "UPDATE roots SET last_scan_at = ?1, scan_state = 'done' WHERE path = ?2",
        params![now_ms(), root_path],
    )?;
    refresh_root_count(conn, root_path)?;
    Ok(())
}

/// Generation đơn điệu tăng, độc lập với jobs.id (jobs có thể bị prune sau này).
pub fn next_scan_gen(conn: &Connection) -> Result<i64> {
    let cur: i64 = kv_get(conn, "scan_gen")?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let next = cur + 1;
    kv_set(conn, "scan_gen", &next.to_string())?;
    Ok(next)
}

// ---------- jobs ----------

pub fn insert_job(conn: &Connection, kind: &str, params_json: Option<&str>) -> Result<i64> {
    conn.execute(
        "INSERT INTO jobs(kind, state, params, created_at, started_at)
         VALUES(?1, 'running', ?2, ?3, ?3)",
        params![kind, params_json, now_ms()],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn update_job_progress(
    conn: &Connection,
    id: i64,
    done: i64,
    total: Option<i64>,
    message: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE jobs SET done = ?2, total = ?3, message = ?4 WHERE id = ?1",
        params![id, done, total, message],
    )?;
    Ok(())
}

pub fn finish_job(conn: &Connection, id: i64, state: &str, error: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE jobs SET state = ?2, error = ?3, finished_at = ?4 WHERE id = ?1",
        params![id, state, error, now_ms()],
    )?;
    Ok(())
}

pub fn list_jobs(conn: &Connection, limit: i64) -> Result<Vec<JobRow>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, kind, state, done, total, message, created_at, finished_at, error
         FROM jobs ORDER BY id DESC LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(params![limit], |r| {
            Ok(JobRow {
                id: r.get(0)?,
                kind: r.get(1)?,
                state: r.get(2)?,
                done: r.get(3)?,
                total: r.get(4)?,
                message: r.get(5)?,
                created_at: r.get(6)?,
                finished_at: r.get(7)?,
                error: r.get(8)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ---------- media meta (M2) ----------

/// Ghép full path từ dirs.path + files.name ("D:\" đã có sep, còn lại thì chưa).
pub fn join_path(dir: &str, name: &str) -> String {
    if dir.ends_with('\\') {
        format!("{dir}{name}")
    } else {
        format!("{dir}\\{name}")
    }
}

/// Điều kiện "file này cần trích meta": chưa có dòng nào, HOẶC đã trích thành
/// công nhưng bằng bộ trích cũ hơn [`META_VERSION`]. Dùng chung cho cả đếm lẫn
/// chọn — hai bên lệch nhau thì job hiện progress sai hoặc chạy mãi không hết.
const NEEDS_META: &str = "(m.file_id IS NULL OR (m.meta_state = 1 AND m.meta_ver < :meta_ver))
     AND f.kind <= :kind_max AND f.status = 0";

/// Số file present chưa có meta — quyết định có mở meta job không.
/// `include_video=false` khi không có ffprobe: video để lại, có tool sẽ làm.
pub fn count_pending_meta(conn: &Connection, include_video: bool) -> Result<i64> {
    let kind_max = if include_video { 1 } else { 0 };
    Ok(conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM files f LEFT JOIN media_meta m ON m.file_id = f.id
             WHERE {NEEDS_META}"
        ),
        rusqlite::named_params! { ":kind_max": kind_max, ":meta_ver": META_VERSION },
        |r| r.get(0),
    )?)
}

/// 1 batch file chờ trích meta, keyset pagination theo f.id (`after_id`) —
/// file KHÔNG ĐỌC ĐƯỢC (ổ rút giữa chừng) được job bỏ qua không ghi row,
/// cursor vẫn tiến nên loop không bao giờ kẹt; job sau tự thử lại.
pub fn select_pending_meta(
    conn: &Connection,
    after_id: i64,
    limit: i64,
    include_video: bool,
) -> Result<Vec<PendingMeta>> {
    let kind_max = if include_video { 1 } else { 0 };
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT f.id, d.path, f.name, f.kind, f.mtime, f.size
         FROM files f
         JOIN dirs d ON d.id = f.dir_id
         LEFT JOIN media_meta m ON m.file_id = f.id
         WHERE {NEEDS_META} AND f.id > :after_id
         ORDER BY f.id LIMIT :limit"
    ))?;
    let rows = stmt
        .query_map(
            rusqlite::named_params! {
                ":after_id": after_id,
                ":limit": limit,
                ":kind_max": kind_max,
                ":meta_ver": META_VERSION,
            },
            |r| {
                let dir: String = r.get(1)?;
                let name: String = r.get(2)?;
                Ok(PendingMeta {
                    file_id: r.get(0)?,
                    path: join_path(&dir, &name),
                    kind: r.get(3)?,
                    mtime: r.get(4)?,
                    size: r.get(5)?,
                })
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn upsert_meta_batch(conn: &mut Connection, rows: &[MetaUpsert]) -> Result<()> {
    let tx = conn.transaction()?;
    {
        // INSERT..SELECT..WHERE EXISTS với guard mtime/size: (a) file bị
        // remove_root xóa giữa chừng → bỏ qua thay vì FK violation rollback cả
        // batch; (b) file ĐỔI NỘI DUNG trong lúc extract (ffprobe 1 batch mất
        // hàng chục giây) → meta vừa trích là của bản cũ, ghi vào là stale
        // vĩnh viễn vì trigger invalidate đã chạy TRƯỚC upsert này — bỏ, job
        // sau trích lại bản mới.
        let mut ins = tx.prepare_cached(
            "INSERT INTO media_meta(file_id, width, height, taken_at, date_source, camera,
                                    orientation, duration_ms, vcodec, acodec, bitrate, fps,
                                    meta_state, gps_lat, gps_lon, meta_ver)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?16, ?17, ?18
             WHERE EXISTS(SELECT 1 FROM files
                          WHERE id = ?1 AND mtime = ?14 AND size = ?15)
             ON CONFLICT(file_id) DO UPDATE SET
               width = excluded.width, height = excluded.height,
               taken_at = excluded.taken_at, date_source = excluded.date_source,
               camera = excluded.camera, orientation = excluded.orientation,
               duration_ms = excluded.duration_ms, vcodec = excluded.vcodec,
               acodec = excluded.acodec, bitrate = excluded.bitrate,
               fps = excluded.fps, meta_state = excluded.meta_state,
               gps_lat = excluded.gps_lat, gps_lon = excluded.gps_lon,
               meta_ver = excluded.meta_ver",
        )?;
        for m in rows {
            ins.execute(params![
                m.file_id,
                m.width,
                m.height,
                m.taken_at,
                m.date_source,
                m.camera,
                m.orientation,
                m.duration_ms,
                m.vcodec,
                m.acodec,
                m.bitrate,
                m.fps,
                m.meta_state,
                m.src_mtime,
                m.src_size,
                m.gps_lat,
                m.gps_lon,
                META_VERSION,
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Lookup cho protocol thumb:// / media://.
/// Trang id file đang hiện diện (status=0) — job warm thumb nền duyệt tuần tự.
pub fn select_present_ids(conn: &Connection, after_id: i64, limit: i64) -> Result<Vec<i64>> {
    let mut st = conn
        .prepare_cached("SELECT id FROM files WHERE status = 0 AND id > ?1 ORDER BY id LIMIT ?2")?;
    let ids = st
        .query_map(params![after_id, limit], |r| r.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ids)
}

pub fn count_present_files(conn: &Connection) -> Result<i64> {
    Ok(
        conn.query_row("SELECT COUNT(*) FROM files WHERE status = 0", [], |r| {
            r.get(0)
        })?,
    )
}

pub fn get_media_src(conn: &Connection, file_id: i64) -> Result<Option<MediaSrc>> {
    Ok(conn
        .query_row(
            "SELECT d.path, f.name, f.ext, f.kind, f.size, f.mtime, f.status, m.duration_ms
             FROM files f
             JOIN dirs d ON d.id = f.dir_id
             LEFT JOIN media_meta m ON m.file_id = f.id
             WHERE f.id = ?1",
            params![file_id],
            |r| {
                let dir: String = r.get(0)?;
                let name: String = r.get(1)?;
                Ok(MediaSrc {
                    path: join_path(&dir, &name),
                    ext: r.get(2)?,
                    kind: r.get(3)?,
                    size: r.get(4)?,
                    mtime: r.get(5)?,
                    status: r.get(6)?,
                    duration_ms: r.get(7)?,
                })
            },
        )
        .optional()?)
}

/// Ghép cặp Live Photo trong scope 1 root: ảnh (heic/heif/jpg/jpeg) + video
/// .mov cùng thư mục cùng stem (không phân biệt hoa thường) → live_pair_id
/// 2 CHIỀU (ảnh trỏ MOV, MOV trỏ ảnh). MOV đã ghép bị ẩn khỏi browse
/// (predicate trong query_ids) — cặp đi với nhau như 1 đơn vị.
/// Chạy trong writer sau mỗi scan; xóa pair cũ trong scope trước khi tính lại.
pub fn pair_live_photos(conn: &mut Connection, root_path: &str) -> Result<usize> {
    let (eq, start, end) = path_range(root_path);
    let tx = conn.transaction()?;
    tx.execute(
        &format!(
            "UPDATE files SET live_pair_id = NULL
             WHERE live_pair_id IS NOT NULL AND dir_id IN
               (SELECT d.id FROM dirs d WHERE {ROOT_SCOPE})"
        ),
        params![eq, start, end],
    )?;
    // EXISTS guard: chỉ REWRITE row ảnh thật sự có cặp — không thì mỗi scan
    // 200k ảnh bị ghi lại (WAL churn) chỉ để set NULL = NULL.
    let paired = tx.execute(
        &format!(
            "UPDATE files SET live_pair_id = (
               SELECT v.id FROM files v
               WHERE v.dir_id = files.dir_id AND v.kind = 1 AND v.ext = 'mov'
                 AND v.status IN (0, 2)
                 AND lower(substr(v.name, 1, length(v.name) - 4)) =
                     lower(substr(files.name, 1, length(files.name) - length(files.ext) - 1))
               LIMIT 1)
             WHERE files.kind = 0 AND files.ext IN ('heic', 'heif', 'jpg', 'jpeg')
               AND files.status IN (0, 2)
               AND files.dir_id IN (SELECT d.id FROM dirs d WHERE {ROOT_SCOPE})
               AND EXISTS (
                 SELECT 1 FROM files v
                 WHERE v.dir_id = files.dir_id AND v.kind = 1 AND v.ext = 'mov'
                   AND v.status IN (0, 2)
                   AND lower(substr(v.name, 1, length(v.name) - 4)) =
                       lower(substr(files.name, 1, length(files.name) - length(files.ext) - 1)))"
        ),
        params![eq, start, end],
    )?;
    // Chiều ngược: MOV trỏ về ảnh (dùng index files_live_pair)
    tx.execute(
        &format!(
            "UPDATE files SET live_pair_id = (
               SELECT i.id FROM files i WHERE i.live_pair_id = files.id LIMIT 1)
             WHERE files.kind = 1 AND files.ext = 'mov'
               AND files.dir_id IN (SELECT d.id FROM dirs d WHERE {ROOT_SCOPE})
               AND EXISTS (SELECT 1 FROM files i WHERE i.live_pair_id = files.id)"
        ),
        params![eq, start, end],
    )?;
    tx.commit()?;
    Ok(paired)
}

/// Chi tiết file + meta cho panel info lightbox.
pub fn get_file_detail(conn: &Connection, file_id: i64) -> Result<Option<FileDetail>> {
    Ok(conn
        .query_row(
            "SELECT f.id, f.name, d.path, f.kind, f.status, f.size, f.mtime,
                    m.width, m.height, m.taken_at, m.camera, m.orientation,
                    m.duration_ms, m.vcodec, m.acodec, m.fps, m.meta_state,
                    m.gps_lat, m.gps_lon
             FROM files f
             JOIN dirs d ON d.id = f.dir_id
             LEFT JOIN media_meta m ON m.file_id = f.id
             WHERE f.id = ?1",
            params![file_id],
            |r| {
                Ok(FileDetail {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    dir: r.get(2)?,
                    kind: r.get(3)?,
                    status: r.get(4)?,
                    size: r.get(5)?,
                    mtime: r.get(6)?,
                    width: r.get(7)?,
                    height: r.get(8)?,
                    taken_at: r.get(9)?,
                    camera: r.get(10)?,
                    orientation: r.get(11)?,
                    duration_ms: r.get(12)?,
                    vcodec: r.get(13)?,
                    acodec: r.get(14)?,
                    fps: r.get(15)?,
                    meta_state: r.get(16)?,
                    gps_lat: r.get(17)?,
                    gps_lon: r.get(18)?,
                    // Tầng lệnh tra tên rồi điền — xem doc của field
                    place: None,
                })
            },
        )
        .optional()?)
}

// ---------- dedup: hash pipeline + dup groups (M4) ----------

/// Điều kiện "hash quick còn thiếu/stale" dùng chung cho count + select.
/// Chỉ xét file present, size > 0, và size xuất hiện >= 2 lần (tầng 1: group
/// theo size loại sạch phần lớn — file size độc nhất không bao giờ có bản trùng).
const QUICK_PENDING_WHERE: &str = "f.status = 0 AND f.size > 0
    AND f.size IN (SELECT size FROM files WHERE status = 0 AND size > 0
                   GROUP BY size HAVING COUNT(*) >= 2)
    AND (h.file_id IS NULL OR h.quick64 IS NULL
         OR h.hashed_mtime != f.mtime OR h.hashed_size != f.size)";

pub fn count_pending_quick(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM files f LEFT JOIN hashes h ON h.file_id = f.id
             WHERE {QUICK_PENDING_WHERE}"
        ),
        [],
        |r| r.get(0),
    )?)
}

pub fn select_pending_quick(
    conn: &Connection,
    after_id: i64,
    limit: i64,
) -> Result<Vec<PendingHash>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT f.id, d.path, f.name, f.mtime, f.size
         FROM files f
         JOIN dirs d ON d.id = f.dir_id
         LEFT JOIN hashes h ON h.file_id = f.id
         WHERE {QUICK_PENDING_WHERE} AND f.id > ?1
         ORDER BY f.id LIMIT ?2"
    ))?;
    let rows = stmt
        .query_map(params![after_id, limit], map_pending_hash)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Full hash cần cho file nằm trong nhóm (size, quick64) có >= 2 thành viên
/// mà chưa có full_hash hợp lệ.
const FULL_PENDING_WHERE: &str = "f.status = 0
    AND h.quick64 IS NOT NULL AND h.hashed_mtime = f.mtime AND h.hashed_size = f.size
    AND h.full_hash IS NULL
    AND (f.size, h.quick64) IN (
        SELECT f2.size, h2.quick64 FROM files f2
        JOIN hashes h2 ON h2.file_id = f2.id
        WHERE f2.status = 0 AND h2.quick64 IS NOT NULL
          AND h2.hashed_mtime = f2.mtime AND h2.hashed_size = f2.size
        GROUP BY f2.size, h2.quick64 HAVING COUNT(*) >= 2)";

pub fn count_pending_full(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM files f JOIN hashes h ON h.file_id = f.id
             WHERE {FULL_PENDING_WHERE}"
        ),
        [],
        |r| r.get(0),
    )?)
}

pub fn select_pending_full(
    conn: &Connection,
    after_id: i64,
    limit: i64,
) -> Result<Vec<PendingHash>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT f.id, d.path, f.name, f.mtime, f.size
         FROM files f
         JOIN dirs d ON d.id = f.dir_id
         JOIN hashes h ON h.file_id = f.id
         WHERE {FULL_PENDING_WHERE} AND f.id > ?1
         ORDER BY f.id LIMIT ?2"
    ))?;
    let rows = stmt
        .query_map(params![after_id, limit], map_pending_hash)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn map_pending_hash(r: &rusqlite::Row<'_>) -> rusqlite::Result<PendingHash> {
    let dir: String = r.get(1)?;
    let name: String = r.get(2)?;
    Ok(PendingHash {
        file_id: r.get(0)?,
        path: join_path(&dir, &name),
        mtime: r.get(3)?,
        size: r.get(4)?,
    })
}

/// Ghi hash với guard id+mtime+size như meta (file đổi giữa chừng → bỏ).
/// full_hash cũ được GIỮ khi chỉ update quick64 cho cùng phiên bản file.
pub fn upsert_hash_batch(conn: &mut Connection, rows: &[HashUpsert]) -> Result<()> {
    let tx = conn.transaction()?;
    {
        let mut ins = tx.prepare_cached(
            "INSERT INTO hashes(file_id, quick64, full_hash, hashed_size, hashed_mtime)
             SELECT ?1, ?2, ?3, ?5, ?4
             WHERE EXISTS(SELECT 1 FROM files WHERE id = ?1 AND mtime = ?4 AND size = ?5)
             ON CONFLICT(file_id) DO UPDATE SET
               quick64 = COALESCE(excluded.quick64, hashes.quick64),
               full_hash = CASE
                 WHEN hashes.hashed_mtime = excluded.hashed_mtime
                  AND hashes.hashed_size = excluded.hashed_size
                 THEN COALESCE(excluded.full_hash, hashes.full_hash)
                 ELSE excluded.full_hash END,
               hashed_size = excluded.hashed_size,
               hashed_mtime = excluded.hashed_mtime",
        )?;
        for h in rows {
            ins.execute(params![
                h.file_id,
                h.quick64,
                h.full,
                h.src_mtime,
                h.src_size
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Xây lại toàn bộ nhóm trùng exact từ full_hash (thay thế bộ cũ kind=0).
/// Trả (số nhóm, tổng bytes lãng phí).
pub fn rebuild_dup_groups(conn: &mut Connection) -> Result<(i64, i64)> {
    let tx = conn.transaction()?;
    // Preserve group ids for hashes that still form the same exact group. UI state is
    // keyed by group id; deleting/reinserting every row made an unrelated organize-hash
    // preparation close the group the user was inspecting.
    let mut existing_by_hash: HashMap<Vec<u8>, i64> = {
        let mut stmt = tx.prepare(
            "SELECT g.id, h.full_hash
             FROM dup_groups g
             JOIN dup_members m ON m.group_id = g.id
             JOIN hashes h ON h.file_id = m.file_id
             WHERE g.kind = 0 AND h.full_hash IS NOT NULL
             GROUP BY g.id
             HAVING COUNT(DISTINCT h.full_hash) = 1
             ORDER BY g.id",
        )?;
        let rows: Vec<(i64, Vec<u8>)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let mut map = HashMap::new();
        for (id, hash) in rows {
            map.entry(hash).or_insert(id);
        }
        map
    };
    tx.execute(
        "DELETE FROM dup_members WHERE group_id IN (SELECT id FROM dup_groups WHERE kind = 0)",
        [],
    )?;

    let mut groups = 0i64;
    let mut waste = 0i64;
    {
        // Hash hợp lệ = hashed_* khớp file hiện tại (trigger dọn khi file đổi,
        // nhưng check thêm cho chắc)
        let mut find = tx.prepare(
            "SELECT h.full_hash, COUNT(*), MAX(f.size)
             FROM files f JOIN hashes h ON h.file_id = f.id
             WHERE f.status = 0 AND h.full_hash IS NOT NULL
               AND h.hashed_mtime = f.mtime AND h.hashed_size = f.size
             GROUP BY h.full_hash HAVING COUNT(*) >= 2",
        )?;
        let mut ins_group = tx.prepare("INSERT INTO dup_groups(kind, created_at) VALUES(0, ?1)")?;
        let mut ins_members = tx.prepare(
            "INSERT INTO dup_members(group_id, file_id, keep)
             SELECT ?1, f.id, 0 FROM files f JOIN hashes h ON h.file_id = f.id
             WHERE f.status = 0 AND h.full_hash = ?2
               AND h.hashed_mtime = f.mtime AND h.hashed_size = f.size",
        )?;

        let found: Vec<(Vec<u8>, i64, i64)> = find
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;
        let now = now_ms();
        for (fh, n, size) in found {
            let gid = if let Some(existing) = existing_by_hash.remove(&fh) {
                existing
            } else {
                ins_group.execute(params![now])?;
                tx.last_insert_rowid()
            };
            ins_members.execute(params![gid, fh])?;
            groups += 1;
            waste += (n - 1) * size;
        }
    }
    tx.execute(
        "DELETE FROM dup_groups
         WHERE kind = 0 AND NOT EXISTS(
           SELECT 1 FROM dup_members m WHERE m.group_id = dup_groups.id
         )",
        [],
    )?;
    tx.commit()?;
    Ok((groups, waste))
}

/// List nhóm trùng, lãng phí nhiều nhất trước. Cap 10k nhóm (UI ảo hóa được
/// nhưng IPC 1 phát 10k row ~ 1MB là trần hợp lý).
/// `kind`: 0 = trùng tuyệt đối (byte y hệt), 1 = gần giống (perceptual).
/// Với nhóm gần giống các bản KHÔNG cùng dung lượng nên "dọn được" tính bằng
/// tổng mọi bản trừ bản nặng nhất, chứ không phải (n-1) * size.
pub fn list_dup_groups(conn: &Connection, kind: i64) -> Result<Vec<DupGroupRow>> {
    let mut stmt = conn.prepare_cached(
        "SELECT g.id, COUNT(*), MAX(f.size), SUM(f.size) - MAX(f.size),
                substr(GROUP_CONCAT(f.id || ':' || f.mtime ORDER BY f.id), 1, 120)
         FROM dup_groups g
         JOIN dup_members m ON m.group_id = g.id
         JOIN files f ON f.id = m.file_id
         WHERE g.kind = ?1 AND f.status = 0
         GROUP BY g.id HAVING COUNT(*) >= 2
         ORDER BY 4 DESC LIMIT 10000",
    )?;
    let rows = stmt
        .query_map(params![kind], |r| {
            let concat: String = r.get(4)?;
            let samples = concat
                .split(',')
                .take(3)
                .filter_map(|p| {
                    let (id, mt) = p.split_once(':')?;
                    Some((id.parse().ok()?, mt.parse().ok()?))
                })
                .collect();
            Ok(DupGroupRow {
                id: r.get(0)?,
                count: r.get(1)?,
                size: r.get(2)?,
                waste: r.get(3)?,
                samples,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_dup_group(conn: &Connection, group_id: i64) -> Result<Vec<DupMemberRow>> {
    let mut stmt = conn.prepare_cached(
        "SELECT f.id, f.name, d.path, f.size, f.mtime, f.status, f.live_pair_id,
                mm.width, mm.height, mm.taken_at, mm.camera
         FROM dup_members m
         JOIN files f ON f.id = m.file_id
         JOIN dirs d ON d.id = f.dir_id
         LEFT JOIN media_meta mm ON mm.file_id = f.id
         WHERE m.group_id = ?1
         ORDER BY f.id",
    )?;
    let rows = stmt
        .query_map(params![group_id], |r| {
            Ok(DupMemberRow {
                file_id: r.get(0)?,
                name: r.get(1)?,
                dir: r.get(2)?,
                size: r.get(3)?,
                mtime: r.get(4)?,
                status: r.get(5)?,
                is_live: r.get::<_, Option<i64>>(6)?.is_some(),
                width: r.get(7)?,
                height: r.get(8)?,
                taken_at: r.get(9)?,
                camera: r.get(10)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ---------- perceptual dedup: dhash + nhóm gần giống (M7) ----------

/// `phashes.kind`: 0 = dhash ảnh (dùng để gom nhóm).
pub const PHASH_KIND_DHASH: i64 = 0;
/// 3 = "đã quét, không có hash dùng được" (ảnh phẳng / không decode nổi).
/// Row bia này giữ cho job hội tụ thay vì đọc lại file đó mỗi lượt.
pub const PHASH_KIND_NONE: i64 = 3;

const PHASH_PENDING_WHERE: &str = "f.kind = 0 AND f.status = 0
    AND NOT EXISTS(SELECT 1 FROM phashes p WHERE p.file_id = f.id
                   AND p.kind IN (0, 3) AND p.seq = 0)";

pub fn count_pending_phash(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row(
        &format!("SELECT COUNT(*) FROM files f WHERE {PHASH_PENDING_WHERE}"),
        [],
        |r| r.get(0),
    )?)
}

pub fn select_pending_phash(
    conn: &Connection,
    after_id: i64,
    limit: i64,
) -> Result<Vec<PendingPhash>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT f.id, d.path, f.name, f.ext, f.mtime, f.size
         FROM files f JOIN dirs d ON d.id = f.dir_id
         WHERE {PHASH_PENDING_WHERE} AND f.id > ?1
         ORDER BY f.id LIMIT ?2"
    ))?;
    let rows = stmt
        .query_map(params![after_id, limit], |r| {
            let dir: String = r.get(1)?;
            let name: String = r.get(2)?;
            Ok(PendingPhash {
                file_id: r.get(0)?,
                path: join_path(&dir, &name),
                ext: r.get(3)?,
                mtime: r.get(4)?,
                size: r.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Ghi hash với guard mtime+size như meta: file đổi nội dung giữa lúc job đang
/// decode thì hash vừa tính là của bản CŨ — bỏ, lượt sau tính lại.
///
/// 256 bit = 4 dòng `seq 0..3`. Xóa trước rồi mới chèn: file có thể đang mang
/// hash cũ ở kind khác (bia → hash thật hoặc ngược lại), để lại là gom nhóm
/// đọc phải hash mồ côi.
pub fn upsert_phash_batch(conn: &mut Connection, rows: &[PhashUpsert]) -> Result<()> {
    let tx = conn.transaction()?;
    {
        let mut fresh =
            tx.prepare_cached("SELECT 1 FROM files WHERE id = ?1 AND mtime = ?2 AND size = ?3")?;
        let mut del =
            tx.prepare_cached("DELETE FROM phashes WHERE file_id = ?1 AND kind IN (0, 3)")?;
        let mut ins = tx.prepare_cached(
            "INSERT INTO phashes(file_id, kind, seq, hash64) VALUES(?1, ?2, ?3, ?4)",
        )?;
        for r in rows {
            if !fresh.exists(params![r.file_id, r.src_mtime, r.src_size])? {
                continue;
            }
            del.execute(params![r.file_id])?;
            match r.hash {
                Some(words) => {
                    for (seq, w) in words.iter().enumerate() {
                        ins.execute(params![r.file_id, PHASH_KIND_DHASH, seq as i64, w])?;
                    }
                }
                None => {
                    ins.execute(params![r.file_id, PHASH_KIND_NONE, 0, 0])?;
                }
            }
        }
    }
    tx.commit()?;
    Ok(())
}

/// Gom cụm THUẦN (test được, không đụng DB): union-find trên các cặp qua được
/// CẢ BA chốt chặn — Hamming ≤ `max_dist`, tỉ lệ khung hình xấp xỉ, và cùng
/// giờ bấm máy.
///
/// Tỉ lệ khung hình: dhash 8x8 bỏ hết thông tin khung hình nên ảnh dọc và ảnh
/// ngang cùng bố cục có thể ra hash gần nhau.
///
/// Giờ bấm máy là chốt chặn QUAN TRỌNG NHẤT, đo trên kho thật mới thêm: một
/// loạt 13 tấm chụp liên tiếp cùng cảnh (`IMG_8194..IMG_8206`, người chỉ đổi tư
/// thế) ra hash gần như y hệt vì ở lưới 9x8 luma người chiếm 1-2 ô — chỉnh
/// `max_dist` không cứu được, hạ tới 0 thì mất luôn ca trùng thật. Nhưng 13 tấm
/// đó có 13 mốc EXIF khác nhau trải 14 giây, trong khi 4 bản của CÙNG một tấm
/// (`IMG_1463` ở icloud/iphone/anh/TienIphone, 3 độ phân giải, 4 dung lượng)
/// mang đúng một mốc `18:02:24.802`. Cùng một tấm ảnh thì bắt buộc cùng một lần
/// bấm máy — khác mili-giây là khác tấm, dù hash giống đến đâu.
pub fn cluster_similar(items: &[ClusterItem], max_dist: u32) -> Vec<Vec<i64>> {
    let n = items.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    let aspect = |it: &ClusterItem| -> Option<f64> {
        match (it.width, it.height) {
            (Some(w), Some(h)) if w > 0 && h > 0 => Some(w as f64 / h as f64),
            _ => None,
        }
    };
    let dist = |x: &ClusterItem, y: &ClusterItem| -> u32 {
        let mut d = 0;
        for i in 0..4 {
            d += (x.hash[i] ^ y.hash[i]).count_ones();
            // Vượt ngưỡng rồi thì 3 word còn lại không đổi được kết luận —
            // thoát sớm, đây là vòng trong của O(n²) trên 21k ảnh.
            if d > max_dist {
                return d;
            }
        }
        d
    };
    for a in 0..n {
        for b in (a + 1)..n {
            if dist(&items[a], &items[b]) > max_dist {
                continue;
            }
            // Thiếu kích thước (meta chưa chạy tới) → không chặn, chỉ dựa hash
            if let (Some(ra), Some(rb)) = (aspect(&items[a]), aspect(&items[b])) {
                if (ra - rb).abs() > 0.05 * ra.max(rb) {
                    continue;
                }
            }
            // Thiếu taken_at (ảnh bị app nhắn tin xóa sạch EXIF) → không chặn:
            // đó là ca M7 thật, phải gom được với bản gốc.
            if let (Some(ta), Some(tb)) = (items[a].taken_at, items[b].taken_at) {
                if ta != tb {
                    continue;
                }
            }
            let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
            if ra != rb {
                parent[ra] = rb;
            }
        }
    }
    let mut groups: HashMap<usize, Vec<ClusterItem>> = HashMap::new();
    for (i, item) in items.iter().enumerate() {
        let root = find(&mut parent, i);
        groups.entry(root).or_default().push(*item);
    }
    let mut out: Vec<Vec<i64>> = groups
        .into_values()
        .flat_map(|c| split_by_taken_at(c).into_iter())
        .filter(|g| g.len() >= 2)
        .collect();
    for g in out.iter_mut() {
        g.sort_unstable();
    }
    // Thứ tự ổn định → rebuild 2 lần cho ra cùng kết quả
    out.sort_unstable_by_key(|g| g[0]);
    out
}

/// Hậu kiểm chống BẮC CẦU qua member thiếu EXIF: chốt chặn giờ bấm máy chỉ xét
/// từng cặp, nên A(mốc 1) ~ B(không mốc) ~ C(mốc 2) vẫn chui chung cụm qua B.
///
/// - Cụm chỉ có 1 mốc (kèm bao nhiêu member không mốc cũng được) → giữ nguyên;
///   đây đúng là ca ảnh gốc + bản bị xóa EXIF, phải gom được.
/// - Cụm có ≥ 2 mốc khác nhau → chẻ theo từng mốc, member không mốc bị LOẠI:
///   không có cơ sở gán nó về mốc nào, mà đoán sai là xóa nhầm ảnh thật.
fn split_by_taken_at(cluster: Vec<ClusterItem>) -> Vec<Vec<i64>> {
    let mut stamps: Vec<i64> = cluster.iter().filter_map(|it| it.taken_at).collect();
    stamps.sort_unstable();
    stamps.dedup();
    match stamps.len() {
        0 | 1 => vec![cluster.into_iter().map(|it| it.file_id).collect()],
        _ => stamps
            .into_iter()
            .map(|ts| {
                cluster
                    .iter()
                    .filter(|it| it.taken_at == Some(ts))
                    .map(|it| it.file_id)
                    .collect()
            })
            .collect(),
    }
}

/// Dựng lại nhóm "gần giống" (kind = 1) từ dhash. Trả (số nhóm, bytes có thể
/// dọn nếu mỗi nhóm chỉ giữ bản to nhất).
pub fn rebuild_similar_groups(conn: &mut Connection, max_dist: u32) -> Result<(i64, i64)> {
    let items: Vec<ClusterItem> = {
        // 4 dòng seq/file gộp lại thành 1 item. Chỉ nhận file đủ cả 4 word:
        // thiếu word là hash dở dang (job bị kill giữa transaction cũ), gom
        // theo nó là so với 0 và kéo về nhóm sai.
        let mut stmt = conn.prepare(
            "SELECT p.file_id, p.seq, p.hash64, m.width, m.height, m.taken_at
             FROM phashes p
             JOIN files f ON f.id = p.file_id AND f.status = 0
             LEFT JOIN media_meta m ON m.file_id = p.file_id
             WHERE p.kind = 0 AND p.seq < 4
             ORDER BY p.file_id, p.seq",
        )?;
        let mut out: Vec<ClusterItem> = Vec::new();
        let mut cur: Option<(ClusterItem, usize)> = None;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let (id, seq): (i64, i64) = (r.get(0)?, r.get(1)?);
            if cur.as_ref().is_none_or(|(it, _)| it.file_id != id) {
                if let Some((it, n)) = cur.take() {
                    if n == 4 {
                        out.push(it);
                    }
                }
                cur = Some((
                    ClusterItem {
                        file_id: id,
                        hash: [0; 4],
                        width: r.get(3)?,
                        height: r.get(4)?,
                        taken_at: r.get(5)?,
                    },
                    0,
                ));
            }
            if let Some((it, n)) = cur.as_mut() {
                it.hash[seq as usize] = r.get(2)?;
                *n += 1;
            }
        }
        if let Some((it, n)) = cur {
            if n == 4 {
                out.push(it);
            }
        }
        out
    };
    let clusters = cluster_similar(&items, max_dist);

    let tx = conn.transaction()?;
    // Giữ group id qua mỗi lượt rebuild. Trước đây khóa theo file_id NHỎ NHẤT
    // của cụm, và đó là bug: quét thêm một ảnh có id nhỏ hơn vào đúng cụm đó là
    // cụm nhận id mới, UI thấy nhóm cũ biến mất và VỨT TICK user vừa đánh dấu.
    // Nay tra theo TỪNG member: cụm to thêm bao nhiêu cũng giữ nguyên id.
    let existing: HashMap<i64, i64> = {
        let mut stmt = tx.prepare(
            "SELECT m.file_id, m.group_id FROM dup_members m
             JOIN dup_groups g ON g.id = m.group_id AND g.kind = 1",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<Vec<(i64, i64)>, _>>()?;
        rows.into_iter().collect()
    };
    tx.execute(
        "DELETE FROM dup_members WHERE group_id IN (SELECT id FROM dup_groups WHERE kind = 1)",
        [],
    )?;
    let mut groups = 0i64;
    let mut waste = 0i64;
    {
        let mut ins_group = tx.prepare("INSERT INTO dup_groups(kind, created_at) VALUES(1, ?1)")?;
        let mut ins_member =
            tx.prepare("INSERT INTO dup_members(group_id, file_id, keep) VALUES(?1, ?2, 0)")?;
        let mut size_of = tx.prepare("SELECT size FROM files WHERE id = ?1")?;
        let now = now_ms();
        // Một id cũ chỉ được cấp lại cho ĐÚNG MỘT cụm: nhóm cũ có thể vỡ đôi
        // (hash mịn hơn tách ra), hai nửa cùng đòi id đó là dup_members có 2 cụm
        // chung group_id — UI hiện thành một nhóm hổ lốn.
        let mut taken: std::collections::HashSet<i64> = std::collections::HashSet::new();
        for cluster in &clusters {
            // Nhóm cũ nào giữ được nhiều member nhất của cụm này thì cụm kế thừa
            // id đó; hòa phiếu thì lấy id nhỏ nhất cho kết quả tất định.
            let mut votes: HashMap<i64, usize> = HashMap::new();
            for fid in cluster {
                if let Some(gid) = existing.get(fid) {
                    if !taken.contains(gid) {
                        *votes.entry(*gid).or_default() += 1;
                    }
                }
            }
            let gid = match votes
                .into_iter()
                .max_by_key(|(id, n)| (*n, std::cmp::Reverse(*id)))
            {
                Some((id, _)) => id,
                None => {
                    ins_group.execute(params![now])?;
                    tx.last_insert_rowid()
                }
            };
            taken.insert(gid);
            let mut sizes: Vec<i64> = Vec::with_capacity(cluster.len());
            for &fid in cluster {
                ins_member.execute(params![gid, fid])?;
                sizes.push(size_of.query_row(params![fid], |r| r.get(0))?);
            }
            sizes.sort_unstable();
            // Giữ bản NẶNG nhất → dọn được tổng phần còn lại
            waste += sizes.iter().take(sizes.len() - 1).sum::<i64>();
            groups += 1;
        }
    }
    tx.execute(
        "DELETE FROM dup_groups
         WHERE kind = 1 AND NOT EXISTS(
           SELECT 1 FROM dup_members m WHERE m.group_id = dup_groups.id
         )",
        [],
    )?;
    tx.commit()?;
    Ok((groups, waste))
}

/// Member của MỌI nhóm exact trong 1 lượt — UI cần để tick "chọn tất cả" theo
/// rule mà không phải mở lần lượt vài nghìn nhóm. Chỉ file còn present: file
/// mất/placeholder không bao giờ được chọn làm bản giữ, mà cũng không đáng để
/// đề nghị xóa. Cap 200k dòng: quá số này thì UI cũng không dùng nổi 1 payload.
pub fn list_dup_members_brief(conn: &Connection, kind: i64) -> Result<Vec<DupMemberBrief>> {
    let mut stmt = conn.prepare_cached(
        "SELECT m.group_id, f.id, f.size, f.mtime, f.status, mm.width, mm.height, mm.taken_at
         FROM dup_groups g
         JOIN dup_members m ON m.group_id = g.id
         JOIN files f ON f.id = m.file_id
         LEFT JOIN media_meta mm ON mm.file_id = f.id
         WHERE g.kind = ?1 AND f.status = 0
         ORDER BY m.group_id, f.id
         LIMIT 200000",
    )?;
    let rows = stmt
        .query_map(params![kind], |r| {
            Ok(DupMemberBrief {
                group_id: r.get(0)?,
                file_id: r.get(1)?,
                size: r.get(2)?,
                mtime: r.get(3)?,
                status: r.get(4)?,
                width: r.get(5)?,
                height: r.get(6)?,
                taken_at: r.get(7)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// (số nhóm, tổng bytes lãng phí) cho badge/status.
pub fn dedup_stats(conn: &Connection, kind: i64) -> Result<(i64, i64)> {
    Ok(conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(waste), 0) FROM (
           SELECT SUM(f.size) - MAX(f.size) AS waste
           FROM dup_groups g
           JOIN dup_members m ON m.group_id = g.id
           JOIN files f ON f.id = m.file_id
           WHERE g.kind = ?1 AND f.status = 0
           GROUP BY g.id HAVING COUNT(*) >= 2)",
        params![kind],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
}

/// Mọi member của mọi group chứa bất kỳ file nào trong danh sách xóa —
/// backend verify invariant trên context này, không tin UI.
pub fn get_delete_context(conn: &Connection, file_ids: &[i64]) -> Result<Vec<DeleteContextRow>> {
    use std::rc::Rc;
    let values: Rc<Vec<rusqlite::types::Value>> = Rc::new(
        file_ids
            .iter()
            .map(|&i| rusqlite::types::Value::Integer(i))
            .collect(),
    );
    // Chỉ group exact (kind=0) — M7 thêm group perceptual thì 1 file có thể
    // thuộc nhiều group với ngữ nghĩa khác nhau, không được trộn vào verify này.
    let mut stmt = conn.prepare_cached(
        "SELECT m.group_id, f.id, d.path, f.name, f.kind, f.size, f.mtime, f.status,
                f.live_pair_id, h.full_hash, h.hashed_size, h.hashed_mtime
         FROM dup_members m
         JOIN dup_groups g ON g.id = m.group_id AND g.kind = 0
         JOIN files f ON f.id = m.file_id
         JOIN dirs d ON d.id = f.dir_id
         LEFT JOIN hashes h ON h.file_id = f.id
         WHERE m.group_id IN (
           SELECT m2.group_id FROM dup_members m2
           JOIN dup_groups g2 ON g2.id = m2.group_id AND g2.kind = 0
           WHERE m2.file_id IN rarray(?1))",
    )?;
    let rows = stmt
        .query_map([&values], |r| {
            let dir: String = r.get(2)?;
            let name: String = r.get(3)?;
            Ok(DeleteContextRow {
                group_id: r.get(0)?,
                file_id: r.get(1)?,
                path: join_path(&dir, &name),
                kind: r.get(4)?,
                size: r.get(5)?,
                mtime: r.get(6)?,
                status: r.get(7)?,
                live_pair_id: r.get(8)?,
                full_hash: r.get(9)?,
                hashed_size: r.get(10)?,
                hashed_mtime: r.get(11)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Context xóa cho nhóm GẦN GIỐNG (kind=1). Tách hẳn khỏi `get_delete_context`
/// vì bất biến khác nhau về bản chất: ở đây KHÔNG thể chứng minh hai file cùng
/// nội dung (chúng khác độ phân giải/độ nén thật sự), nên đường xóa của nó
/// không bao giờ được đi qua cùng một hàm verify với nhóm byte-y-hệt.
pub fn get_similar_delete_context(
    conn: &Connection,
    file_ids: &[i64],
) -> Result<Vec<DeleteContextRow>> {
    use std::rc::Rc;
    let values: Rc<Vec<rusqlite::types::Value>> = Rc::new(
        file_ids
            .iter()
            .map(|&i| rusqlite::types::Value::Integer(i))
            .collect(),
    );
    let mut stmt = conn.prepare_cached(
        "SELECT m.group_id, f.id, d.path, f.name, f.kind, f.size, f.mtime, f.status,
                f.live_pair_id, h.full_hash, h.hashed_size, h.hashed_mtime
         FROM dup_members m
         JOIN dup_groups g ON g.id = m.group_id AND g.kind = 1
         JOIN files f ON f.id = m.file_id
         JOIN dirs d ON d.id = f.dir_id
         LEFT JOIN hashes h ON h.file_id = f.id
         WHERE m.group_id IN (
           SELECT m2.group_id FROM dup_members m2
           JOIN dup_groups g2 ON g2.id = m2.group_id AND g2.kind = 1
           WHERE m2.file_id IN rarray(?1))",
    )?;
    let rows = stmt
        .query_map([&values], |r| {
            let dir: String = r.get(2)?;
            let name: String = r.get(3)?;
            Ok(DeleteContextRow {
                group_id: r.get(0)?,
                file_id: r.get(1)?,
                path: join_path(&dir, &name),
                kind: r.get(4)?,
                size: r.get(5)?,
                mtime: r.get(6)?,
                status: r.get(7)?,
                live_pair_id: r.get(8)?,
                full_hash: r.get(9)?,
                hashed_size: r.get(10)?,
                hashed_mtime: r.get(11)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Context verify của đúng một file, dùng cho nửa MOV của Live Photo. Khác
/// `get_delete_context`, lookup này không yêu cầu file phải nằm trong dup group:
/// executor cần chứng minh riêng MOV victim và MOV survivor trùng BLAKE3 trước
/// khi cho MOV đi theo ảnh.
pub fn get_delete_file_context(
    conn: &Connection,
    file_id: i64,
) -> Result<Option<DeleteContextRow>> {
    Ok(conn
        .query_row(
            "SELECT f.id, d.path, f.name, f.kind, f.size, f.mtime, f.status,
                    f.live_pair_id, h.full_hash, h.hashed_size, h.hashed_mtime
             FROM files f
             JOIN dirs d ON d.id = f.dir_id
             LEFT JOIN hashes h ON h.file_id = f.id
             WHERE f.id = ?1",
            params![file_id],
            |r| {
                let dir: String = r.get(1)?;
                let name: String = r.get(2)?;
                Ok(DeleteContextRow {
                    group_id: 0,
                    file_id: r.get(0)?,
                    path: join_path(&dir, &name),
                    kind: r.get(3)?,
                    size: r.get(4)?,
                    mtime: r.get(5)?,
                    status: r.get(6)?,
                    live_pair_id: r.get(7)?,
                    full_hash: r.get(8)?,
                    hashed_size: r.get(9)?,
                    hashed_mtime: r.get(10)?,
                })
            },
        )
        .optional()?)
}

/// Id mọi ẢNH (kind=0) đang trỏ live_pair_id vào file này. Dùng ngay trước
/// khi trash 1 pair MOV: còn ảnh sống nào khác ngoài victim đã xóa → MOV
/// không được đụng (HEIC + JPG export cùng stem share chung 1 MOV).
pub fn image_refs_of_pair(conn: &Connection, pair_id: i64) -> Result<Vec<i64>> {
    let mut st =
        conn.prepare_cached("SELECT id FROM files WHERE live_pair_id = ?1 AND kind = 0")?;
    let ids = st
        .query_map([pair_id], |r| r.get(0))?
        .collect::<Result<Vec<i64>, _>>()?;
    Ok(ids)
}

/// Xóa row các file đã vào Recycle Bin thành công (CASCADE dọn hashes/meta/
/// dup_members) + gỡ live_pair_id đang trỏ vào file chết (MOV mồ côi phải
/// hiện lại trong browse thay vì ẩn vĩnh viễn) + refresh count mọi root.
pub fn remove_deleted_files(conn: &mut Connection, file_ids: &[i64]) -> Result<()> {
    let tx = conn.transaction()?;
    {
        let mut unpair =
            tx.prepare_cached("UPDATE files SET live_pair_id = NULL WHERE live_pair_id = ?1")?;
        let mut del = tx.prepare_cached("DELETE FROM files WHERE id = ?1")?;
        for id in file_ids {
            unpair.execute(params![id])?;
            del.execute(params![id])?;
        }
    }
    tx.commit()?;
    refresh_all_root_counts(conn)
}

// ---------- excluded paths (user-defined) ----------

/// Danh sách folder user tự exclude (JSON array trong kv). Path đã normalize.
pub fn get_excluded_paths(conn: &Connection) -> Result<Vec<String>> {
    let raw = kv_get(conn, "excluded_paths")?.unwrap_or_else(|| "[]".into());
    let mut list: Vec<String> = Vec::new();
    // Parse tay JSON array-of-strings đơn giản để khỏi kéo serde_json vào core-db
    // — format do chính mình ghi (set_excluded_paths), luôn hợp lệ.
    for part in raw
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split("\",\"")
    {
        let s = part.trim().trim_matches('"').replace("\\\\", "\\");
        if !s.is_empty() {
            list.push(s);
        }
    }
    Ok(list)
}

pub fn set_excluded_paths(conn: &Connection, paths: &[String]) -> Result<()> {
    let json = format!(
        "[{}]",
        paths
            .iter()
            .map(|p| format!("\"{}\"", normalize_path(p).replace('\\', "\\\\")))
            .collect::<Vec<_>>()
            .join(",")
    );
    kv_set(conn, "excluded_paths", &json)
}

// ---------- kv ----------

pub fn kv_get(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM kv WHERE key = ?1", params![key], |r| {
            r.get(0)
        })
        .optional()?)
}

pub fn kv_set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO kv(key, value) VALUES(?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}
