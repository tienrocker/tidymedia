use std::collections::HashMap;

use anyhow::{anyhow, bail, Result};
use rusqlite::{params, Connection, OptionalExtension};
use unicode_normalization::UnicodeNormalization;

use crate::models::{JobRow, RootInfo, ScanEntry};

/// Bump khi đổi schema. Lệch version → wipe & recreate (index rebuild bằng rescan,
/// chấp nhận được pre-1.0; sau 1.0 sẽ có migration thật).
const SCHEMA_VERSION: i64 = 2;

pub fn ensure_schema(conn: &mut Connection) -> Result<()> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version == SCHEMA_VERSION {
        return Ok(());
    }
    let tx = conn.transaction()?;
    // Wipe schema cũ (v0/v1). DROP TABLE tự kéo trigger + FTS shadow tables theo.
    for t in [
        "files_fts", "org_ops", "import_seen", "imports", "library_roots", "album_files",
        "albums", "file_tags", "tags", "dup_members", "dup_groups", "phashes", "hashes",
        "media_meta", "files", "dirs", "roots", "volumes", "jobs", "kv",
    ] {
        tx.execute_batch(&format!("DROP TABLE IF EXISTS {t};"))?;
    }
    tx.execute_batch(include_str!("schema.sql"))?;
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()?;
    tracing::info!("schema created/recreated at v{SCHEMA_VERSION}");
    Ok(())
}

fn now_ms() -> i64 {
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

fn drive_letter(path: &str) -> Option<char> {
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

const ROOT_SCOPE: &str =
    "(d.path_key = ?1 OR (d.path_key >= ?2 AND d.path_key < ?3))";

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
    let volume_id: i64 =
        tx.query_row("SELECT id FROM volumes WHERE guid = ?1", params![guid], |r| r.get(0))?;
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
                    let id: i64 =
                        dir_get.query_row(params![volume_id, path_key], |r| r.get(0))?;
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
    let (eq, start, end) = path_range(root_path);
    let n = conn.execute(
        &format!(
            "UPDATE files SET status = 1
             WHERE seen_gen < ?4 AND status IN (0, 2) AND dir_id IN
               (SELECT d.id FROM dirs d WHERE {ROOT_SCOPE})"
        ),
        params![eq, start, end, gen],
    )?;
    conn.execute(
        "UPDATE roots SET last_scan_at = ?1, scan_state = 'done' WHERE path = ?2",
        params![now_ms(), root_path],
    )?;
    refresh_root_count(conn, root_path)?;
    Ok(n)
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

// ---------- kv ----------

pub fn kv_get(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM kv WHERE key = ?1", params![key], |r| r.get(0))
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
