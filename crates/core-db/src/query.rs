use std::collections::HashMap;
use std::rc::Rc;

use anyhow::Result;
use rusqlite::types::Value;
use rusqlite::{params_from_iter, Connection, ToSql};

use crate::models::{FileFilter, FileRow};
use crate::ops::like_escape;

/// Chạy filter → trả toàn bộ ID khớp theo thứ tự sort (Everything-style result cache).
/// UI sẽ fetch cửa sổ bằng `fetch_rows`, không bao giờ serialize cả list.
pub fn query_ids(conn: &Connection, f: &FileFilter) -> Result<Vec<i64>> {
    let mut sql = String::from("SELECT f.id FROM files f");
    let mut wheres: Vec<String> = Vec::new();
    let mut params: Vec<Box<dyn ToSql>> = Vec::new();

    if f.root_path.is_some() {
        sql.push_str(" JOIN dirs d ON d.id = f.dir_id");
    }
    if f.include_missing != Some(true) {
        wheres.push("f.status = 0".into());
    }
    if let Some(text) = f.text.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        if text.chars().count() >= 3 {
            // FTS5 trigram: substring match thật sự, <10ms trên 1M rows
            wheres.push("f.id IN (SELECT rowid FROM files_fts WHERE files_fts MATCH ?)".into());
            params.push(Box::new(format!("\"{}\"", text.replace('"', "\"\""))));
        } else {
            // Query ngắn: prefix LIKE trên index NOCASE
            wheres.push("f.name LIKE ? ESCAPE '!'".into());
            params.push(Box::new(format!("{}%", like_escape(text))));
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
    if let Some(root) = f.root_path.as_deref() {
        let root = crate::ops::normalize_path(root);
        let mut base = root.clone();
        if !base.ends_with('\\') {
            base.push('\\');
        }
        wheres.push("(d.path = ? OR d.path LIKE ? ESCAPE '!')".into());
        params.push(Box::new(root));
        params.push(Box::new(format!("{}%", like_escape(&base))));
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

/// Hydrate 1 cửa sổ ID → FileRow, giữ nguyên thứ tự input. `ids` đã được cap ở caller.
pub fn fetch_rows(conn: &Connection, ids: &[i64]) -> Result<Vec<FileRow>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let values: Rc<Vec<Value>> = Rc::new(ids.iter().map(|&i| Value::Integer(i)).collect());
    let mut stmt = conn.prepare_cached(
        "SELECT f.id, f.name, d.path, f.ext, f.kind, f.size, f.mtime, f.status
         FROM files f JOIN dirs d ON d.id = f.dir_id
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
            })
        })?
        .filter_map(|r| r.ok().map(|row| (row.id, row)))
        .collect();
    Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
}
