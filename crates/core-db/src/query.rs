use std::collections::HashMap;
use std::rc::Rc;

use anyhow::Result;
use rusqlite::types::Value;
use rusqlite::{params_from_iter, Connection, ToSql};

use crate::models::{FileFilter, FileRow};
use crate::ops::{normalize_for_search, normalize_path, path_range};

/// Chạy filter → trả toàn bộ ID khớp theo thứ tự sort (Everything-style result cache).
/// UI fetch cửa sổ bằng `fetch_rows`, không bao giờ serialize cả list.
pub fn query_ids(conn: &Connection, f: &FileFilter) -> Result<Vec<i64>> {
    let mut sql = String::from("SELECT f.id FROM files f");
    let mut wheres: Vec<String> = Vec::new();
    let mut params: Vec<Box<dyn ToSql>> = Vec::new();

    if f.root_path.is_some() {
        sql.push_str(" JOIN dirs d ON d.id = f.dir_id");
    }
    if f.min_px.is_some() {
        sql.push_str(" JOIN media_meta m ON m.file_id = f.id");
    }
    if f.include_missing != Some(true) {
        // 0 = present, 2 = cloud placeholder (vẫn browse được, chỉ cấm hash/thumb)
        wheres.push("f.status IN (0, 2)".into());
    }
    if let Some(text) = f.text.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        let norm = normalize_for_search(text);
        if norm.chars().count() >= 3 {
            // FTS5 trigram trên name_norm: substring + không phân biệt dấu
            wheres.push("f.id IN (SELECT rowid FROM files_fts WHERE files_fts MATCH ?)".into());
            params.push(Box::new(format!("\"{}\"", norm.replace('"', "\"\""))));
        } else {
            // Query ngắn: substring LIKE trên name_norm (full scan chấp nhận được —
            // semantics nhất quán với nhánh FTS, không nhảy kết quả ở mốc 3 ký tự)
            wheres.push("f.name_norm LIKE ? ESCAPE '!'".into());
            let escaped = norm
                .replace('!', "!!")
                .replace('%', "!%")
                .replace('_', "!_");
            params.push(Box::new(format!("%{escaped}%")));
        }
    }
    if let Some(kind) = f.kind {
        wheres.push("f.kind = ?".into());
        params.push(Box::new(kind));
    }
    if let Some(exts) = f.exts.as_ref().filter(|v| !v.is_empty()) {
        let marks = vec!["?"; exts.len()].join(",");
        wheres.push(format!("f.ext IN ({marks})"));
        for e in exts {
            params.push(Box::new(e.trim().trim_start_matches('.').to_lowercase()));
        }
    }
    if let Some(v) = f.size_min {
        wheres.push("f.size >= ?".into());
        params.push(Box::new(v));
    }
    if let Some(v) = f.size_max {
        wheres.push("f.size <= ?".into());
        params.push(Box::new(v));
    }
    if let Some(v) = f.mtime_from {
        wheres.push("f.mtime >= ?".into());
        params.push(Box::new(v));
    }
    if let Some(v) = f.mtime_to {
        wheres.push("f.mtime <= ?".into());
        params.push(Box::new(v));
    }
    if let Some(px) = f.min_px {
        // Chỉ file đã có meta mới khớp — meta job chạy nền tự lấp dần
        wheres.push("m.width * m.height >= ?".into());
        params.push(Box::new(px));
    }
    if let Some(root) = f.root_path.as_deref() {
        let (eq, start, end) = path_range(&normalize_path(root));
        wheres.push("(d.path_key = ? OR (d.path_key >= ? AND d.path_key < ?))".into());
        params.push(Box::new(eq));
        params.push(Box::new(start));
        params.push(Box::new(end));
    }

    if !wheres.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&wheres.join(" AND "));
    }
    sql.push_str(" ORDER BY ");
    sql.push_str(match f.sort.as_deref() {
        Some("mtime_asc") => "f.mtime ASC",
        Some("name") => "f.name COLLATE NOCASE ASC",
        Some("size_desc") => "f.size DESC",
        Some("size_asc") => "f.size ASC",
        _ => "f.mtime DESC",
    });

    let started = std::time::Instant::now();
    let mut stmt = conn.prepare_cached(&sql)?;
    let ids: Vec<i64> = stmt
        .query_map(
            params_from_iter(params.iter().map(|p| p.as_ref() as &dyn ToSql)),
            |r| r.get(0),
        )?
        .collect::<Result<_, _>>()?;
    tracing::debug!(
        elapsed_ms = started.elapsed().as_millis() as u64,
        rows = ids.len(),
        "query_ids"
    );
    Ok(ids)
}

/// Hydrate 1 cửa sổ ID → đúng 1 slot cho MỖI id yêu cầu, giữ nguyên thứ tự.
/// Id đã biến mất giữa chừng (remove_root đang chạy...) → slot None — frontend
/// không bao giờ bị lệch index vì thiếu row.
pub fn fetch_rows(conn: &Connection, ids: &[i64]) -> Result<Vec<Option<FileRow>>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let values: Rc<Vec<Value>> = Rc::new(ids.iter().map(|&i| Value::Integer(i)).collect());
    let mut stmt = conn.prepare_cached(
        "SELECT f.id, f.name, d.path, f.ext, f.kind, f.size, f.mtime, f.status,
                m.width, m.height, m.taken_at
         FROM files f
         JOIN dirs d ON d.id = f.dir_id
         LEFT JOIN media_meta m ON m.file_id = f.id
         WHERE f.id IN rarray(?1)",
    )?;
    let mut by_id: HashMap<i64, FileRow> = stmt
        .query_map([&values], |r| {
            Ok(FileRow {
                id: r.get(0)?,
                name: r.get(1)?,
                dir: r.get(2)?,
                ext: r.get(3)?,
                kind: r.get(4)?,
                size: r.get(5)?,
                mtime: r.get(6)?,
                status: r.get(7)?,
                width: r.get(8)?,
                height: r.get(9)?,
                taken_at: r.get(10)?,
            })
        })?
        .filter_map(|r| r.ok().map(|row| (row.id, row)))
        .collect();
    Ok(ids.iter().map(|id| by_id.remove(id)).collect())
}
