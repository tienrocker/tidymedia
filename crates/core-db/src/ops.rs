use std::collections::HashMap;

use anyhow::{anyhow, Result};
use rusqlite::{params, Connection, OptionalExtension};

use crate::models::{JobRow, RootInfo, ScanEntry};

pub fn ensure_schema(conn: &mut Connection) -> Result<()> {
    let has_kv: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='kv'",
        [],
        |r| r.get(0),
    )?;
    if has_kv == 0 {
        conn.execute_batch(include_str!("schema.sql"))?;
        conn.execute(
            "INSERT INTO kv(key, value) VALUES('schema_version', '1')",
            [],
        )?;
    }
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// "D:\Photos\" -> "D:\Photos"; giữ nguyên "D:\" cho drive root.
pub fn normalize_path(p: &str) -> String {
    let mut s = p.replace('/', "\\");
    while s.len() > 3 && s.ends_with('\\') {
        s.pop();
    }
    s
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

/// Escape cho LIKE với ESCAPE '!' (tránh dùng '\' vì đường dẫn Windows).
pub fn like_escape(s: &str) -> String {
    s.replace('!', "!!").replace('%', "!%").replace('_', "!_")
}

fn prefix_pattern(root: &str) -> String {
    let mut base = root.to_string();
    if !base.ends_with('\\') {
        base.push('\\');
    }
    format!("{}%", like_escape(&base))
}

// ---------- roots / volumes ----------

pub fn upsert_root(conn: &mut Connection, path: &str) -> Result<i64> {
    let path = normalize_path(path);
    let letter = drive_letter(&path)
        .ok_or_else(|| anyhow!("root phải nằm trên ổ có ký tự (vd D:\\...): {path}"))?;
    let guid = format!("{letter}:");
    conn.execute(
        "INSERT INTO volumes(guid, letter, added_at) VALUES(?1, ?1, ?2)
         ON CONFLICT(guid) DO NOTHING",
        params![guid, now_ms()],
    )?;
    let volume_id: i64 = conn.query_row(
        "SELECT id FROM volumes WHERE guid = ?1",
        params![guid],
        |r| r.get(0),
    )?;
    conn.execute(
        "INSERT INTO roots(volume_id, path) VALUES(?1, ?2) ON CONFLICT(path) DO NOTHING",
        params![volume_id, path],
    )?;
    let root_id: i64 =
        conn.query_row("SELECT id FROM roots WHERE path = ?1", params![path], |r| {
            r.get(0)
        })?;
    Ok(root_id)
}

pub fn get_root(conn: &Connection, root_id: i64) -> Result<(String, i64)> {
    conn.query_row(
        "SELECT path, volume_id FROM roots WHERE id = ?1",
        params![root_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .map_err(|e| anyhow!("root {root_id} not found: {e}"))
}

pub fn list_roots(conn: &Connection) -> Result<Vec<RootInfo>> {
    let mut stmt =
        conn.prepare_cached("SELECT id, volume_id, path, last_scan_at FROM roots ORDER BY path")?;
    let mut roots: Vec<RootInfo> = stmt
        .query_map([], |r| {
            Ok(RootInfo {
                id: r.get(0)?,
                volume_id: r.get(1)?,
                path: r.get(2)?,
                last_scan_at: r.get(3)?,
                file_count: 0,
            })
        })?
        .collect::<Result<_, _>>()?;
    for root in &mut roots {
        root.file_count = conn.query_row(
            "SELECT COUNT(*) FROM files f JOIN dirs d ON d.id = f.dir_id
             WHERE f.status = 0 AND (d.path = ?1 OR d.path LIKE ?2 ESCAPE '!')",
            params![root.path, prefix_pattern(&root.path)],
            |r| r.get(0),
        )?;
    }
    Ok(roots)
}

pub fn remove_root(conn: &mut Connection, root_id: i64) -> Result<()> {
    let (path, _) = get_root(conn, root_id)?;
    let tx = conn.transaction()?;
    tx.execute(
        "DELETE FROM files WHERE dir_id IN
           (SELECT id FROM dirs WHERE path = ?1 OR path LIKE ?2 ESCAPE '!')",
        params![path, prefix_pattern(&path)],
    )?;
    tx.execute(
        "DELETE FROM dirs WHERE path = ?1 OR path LIKE ?2 ESCAPE '!'",
        params![path, prefix_pattern(&path)],
    )?;
    tx.execute("DELETE FROM roots WHERE id = ?1", params![root_id])?;
    tx.commit()?;
    Ok(())
}

// ---------- scan writes ----------

/// Upsert 1 batch entries trong 1 transaction. `dir_cache` sống theo scan job.
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
            "INSERT INTO dirs(volume_id, name, path) VALUES(?1, ?2, ?3)
             ON CONFLICT(volume_id, path) DO NOTHING",
        )?;
        let mut dir_get =
            tx.prepare_cached("SELECT id FROM dirs WHERE volume_id = ?1 AND path = ?2")?;
        let mut file_ins = tx.prepare_cached(
            "INSERT INTO files(dir_id, volume_id, name, ext, kind, size, mtime, attrs, status, seen_gen)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9)
             ON CONFLICT(dir_id, name) DO UPDATE SET
               ext = excluded.ext, kind = excluded.kind, size = excluded.size,
               mtime = excluded.mtime, attrs = excluded.attrs, status = 0,
               seen_gen = excluded.seen_gen",
        )?;
        for e in entries {
            let dir_id = match dir_cache.get(&e.dir_path) {
                Some(id) => *id,
                None => {
                    let dir_name = e
                        .dir_path
                        .rsplit('\\')
                        .next()
                        .unwrap_or(&e.dir_path)
                        .to_string();
                    dir_ins.execute(params![volume_id, dir_name, e.dir_path])?;
                    let id: i64 =
                        dir_get.query_row(params![volume_id, e.dir_path], |r| r.get(0))?;
                    dir_cache.insert(e.dir_path.clone(), id);
                    id
                }
            };
            file_ins.execute(params![
                dir_id, volume_id, e.name, e.ext, e.kind, e.size, e.mtime, e.attrs, gen
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Đánh dấu missing những file dưới `root_path` không được thấy trong generation này.
/// Trả về số file bị đánh dấu.
pub fn reconcile_scan(conn: &mut Connection, root_path: &str, gen: i64) -> Result<usize> {
    let n = conn.execute(
        "UPDATE files SET status = 1
         WHERE seen_gen < ?1 AND status = 0 AND dir_id IN
           (SELECT id FROM dirs WHERE path = ?2 OR path LIKE ?3 ESCAPE '!')",
        params![gen, root_path, prefix_pattern(root_path)],
    )?;
    conn.execute(
        "UPDATE roots SET last_scan_at = ?1, scan_state = 'done' WHERE path = ?2",
        params![now_ms(), root_path],
    )?;
    Ok(n)
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

/// Đọc 1 giá trị kv (settings nhỏ).
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
