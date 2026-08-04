//! DB ops cho M5 organize: library roots (đích do user chỉ định), journal
//! org_ops (write-ahead + undo), query ứng viên organize.
//!
//! Batch id của org_ops = jobs.id của job organize (không cần bảng batch riêng).

use anyhow::{anyhow, bail, Result};
use rusqlite::{params, Connection, OptionalExtension};

use crate::models::{LibraryRootRow, OrgBatchRow, OrgCandidateRow, OrgOpRow, OrgPairRow};
use crate::ops::{drive_letter, join_path, normalize_for_search, normalize_path, now_ms};

// ---------- library roots ----------

pub fn list_library_roots(conn: &Connection) -> Result<Vec<LibraryRootRow>> {
    let mut st = conn.prepare_cached(
        "SELECT lr.id, lr.volume_id, v.letter, lr.path, lr.is_primary
         FROM library_roots lr JOIN volumes v ON v.id = lr.volume_id
         ORDER BY lr.path",
    )?;
    let rows = st
        .query_map([], |r| {
            Ok(LibraryRootRow {
                id: r.get(0)?,
                volume_id: r.get(1)?,
                letter: r.get(2)?,
                path: r.get(3)?,
                is_primary: r.get::<_, i64>(4)? != 0,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Đặt library root cho volume của `canonical` (đường dẫn ĐÃ canonical hóa từ
/// caller). Mỗi volume tối đa 1 root — gọi lại là ĐỔI path. Root đầu tiên của
/// cả app tự thành primary.
pub fn set_library_root(conn: &mut Connection, canonical: &str) -> Result<i64> {
    let path = normalize_path(canonical);
    let letter = drive_letter(&path).ok_or_else(|| anyhow!("ERR_ROOT_NO_DRIVE|{path}"))?;
    if path.len() < 3 || !path[2..].starts_with('\\') {
        bail!("ERR_ROOT_DRIVE_RELATIVE|{path}");
    }
    let tx = conn.transaction()?;
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
    let any_exists: i64 = tx.query_row("SELECT COUNT(*) FROM library_roots", [], |r| r.get(0))?;
    tx.execute(
        "INSERT INTO library_roots(volume_id, path, is_primary) VALUES(?1, ?2, ?3)
         ON CONFLICT(volume_id) DO UPDATE SET path = excluded.path",
        params![volume_id, path, i64::from(any_exists == 0)],
    )?;
    let id: i64 = tx.query_row(
        "SELECT id FROM library_roots WHERE volume_id = ?1",
        params![volume_id],
        |r| r.get(0),
    )?;
    tx.commit()?;
    Ok(id)
}

pub fn remove_library_root(conn: &mut Connection, id: i64) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM library_roots WHERE id = ?1", params![id])?;
    // Không còn primary → đôn root nhỏ id nhất lên (nếu còn root nào)
    tx.execute(
        "UPDATE library_roots SET is_primary = 1
         WHERE id = (SELECT MIN(id) FROM library_roots)
           AND NOT EXISTS(SELECT 1 FROM library_roots WHERE is_primary = 1)",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

// ---------- candidates ----------

const CANDIDATE_WHERE: &str = "f.volume_id = ?1 AND f.id > ?2
       AND f.status IN (0, 2)
       AND (f.kind = 0 OR f.live_pair_id IS NULL)";

/// Ứng viên organize trên 1 volume, keyset theo f.id. Lấy CẢ file đang nằm
/// trong library root — planner tự trả SkipOrganized (idempotent, và file
/// user tự quăng vào root vẫn được xếp chỗ đúng). MOV có pair bị ẩn (đi theo
/// ảnh canonical mà MOV trỏ ngược qua cột `pair`), status=1 missing loại từ SQL.
/// Nếu HEIC + JPG cùng trỏ một MOV, chỉ canonical owner được phép move MOV;
/// ảnh còn lại vẫn organize độc lập và giữ link logic tới cùng MOV.
pub fn select_org_candidates(
    conn: &Connection,
    volume_id: i64,
    after_id: i64,
    limit: i64,
) -> Result<Vec<OrgCandidateRow>> {
    let sql = format!(
        "SELECT f.id, d.path, f.name, f.ext, f.kind, f.size, f.mtime, f.status,
                m.taken_at, m.date_source, m.camera,
                h.full_hash, h.hashed_size, h.hashed_mtime,
                pv.id, pd.path, pv.name, pv.ext, pv.status, pv.size, pv.mtime,
                ph.full_hash, ph.hashed_size, ph.hashed_mtime
         FROM files f
         JOIN dirs d ON d.id = f.dir_id
         LEFT JOIN media_meta m ON m.file_id = f.id
         LEFT JOIN hashes h ON h.file_id = f.id
         LEFT JOIN files pv ON pv.id = f.live_pair_id AND f.kind = 0 AND pv.kind = 1
                           AND pv.live_pair_id = f.id
         LEFT JOIN dirs pd ON pd.id = pv.dir_id
         LEFT JOIN hashes ph ON ph.file_id = pv.id
         WHERE {CANDIDATE_WHERE}
         ORDER BY f.id LIMIT ?3"
    );
    let mut st = conn.prepare_cached(&sql)?;
    let rows = st
        .query_map(params![volume_id, after_id, limit], |r| {
            let dir: String = r.get(1)?;
            let name: String = r.get(2)?;
            let pair = match (
                r.get::<_, Option<i64>>(14)?,
                r.get::<_, Option<String>>(15)?,
                r.get::<_, Option<String>>(16)?,
            ) {
                (Some(pid), Some(pdir), Some(pname)) => Some(OrgPairRow {
                    file_id: pid,
                    path: join_path(&pdir, &pname),
                    ext: r.get::<_, Option<String>>(17)?.unwrap_or_default(),
                    status: r.get::<_, Option<i64>>(18)?.unwrap_or(1),
                    size: r.get::<_, Option<i64>>(19)?.unwrap_or(-1),
                    mtime: r.get::<_, Option<i64>>(20)?.unwrap_or(-1),
                    full_hash: r.get(21)?,
                    hashed_size: r.get(22)?,
                    hashed_mtime: r.get(23)?,
                }),
                _ => None,
            };
            Ok(OrgCandidateRow {
                file_id: r.get(0)?,
                path: join_path(&dir, &name),
                ext: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                kind: r.get(4)?,
                size: r.get(5)?,
                mtime: r.get(6)?,
                status: r.get(7)?,
                taken_at: r.get(8)?,
                date_source: r.get(9)?,
                camera: r.get(10)?,
                full_hash: r.get(11)?,
                hashed_size: r.get(12)?,
                hashed_mtime: r.get(13)?,
                pair,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn count_org_candidates(conn: &Connection, volume_id: i64) -> Result<i64> {
    let sql = format!("SELECT COUNT(*) FROM files f WHERE {CANDIDATE_WHERE}");
    Ok(conn.query_row(&sql, params![volume_id, 0i64], |r| r.get(0))?)
}

/// Cached full hash of an indexed file plus the index snapshot it was hashed against.
/// The caller must still confirm the snapshot matches the file on disk: a valid hash only
/// proves index and hash agree with each other, not that either matches current content.
pub struct CachedHash {
    pub full_hash: Vec<u8>,
    pub size: i64,
    pub mtime: i64,
}

/// Return the still-valid cached full hash for an indexed file at `path`.
/// Organize Preview uses this only after filesystem metadata says the target exists;
/// no file content is read here.
pub fn valid_full_hash_at_path(conn: &Connection, path: &str) -> Result<Option<CachedHash>> {
    let normalized = normalize_path(path);
    let target = std::path::Path::new(&normalized);
    let Some(parent) = target.parent() else {
        return Ok(None);
    };
    let Some(name) = target.file_name().and_then(|n| n.to_str()) else {
        return Ok(None);
    };
    let parent_key = normalize_path(&parent.to_string_lossy()).to_uppercase();
    conn.query_row(
        "SELECT h.full_hash, f.size, f.mtime
         FROM files f
         JOIN dirs d ON d.id = f.dir_id
         JOIN hashes h ON h.file_id = f.id
         WHERE d.path_key = ?1 AND f.name = ?2 AND f.status = 0
           AND h.full_hash IS NOT NULL
           AND h.hashed_size = f.size AND h.hashed_mtime = f.mtime
         LIMIT 1",
        params![parent_key, name],
        |r| {
            Ok(CachedHash {
                full_hash: r.get(0)?,
                size: r.get(1)?,
                mtime: r.get(2)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

// ---------- journal (write-ahead) ----------

/// Ghi INTENT trước khi đụng fs. done_at NULL = chưa xong.
pub fn insert_org_op(
    conn: &Connection,
    batch_id: i64,
    file_id: i64,
    old_path: &str,
    new_path: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO org_ops(batch_id, file_id, old_path, new_path) VALUES(?1, ?2, ?3, ?4)",
        params![batch_id, file_id, old_path, new_path],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn mark_org_op_done(conn: &Connection, op_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE org_ops SET done_at = ?2 WHERE id = ?1",
        params![op_id, now_ms()],
    )?;
    Ok(())
}

/// Op không thực hiện được (fs từ chối trước khi đổi gì) → xóa intent.
pub fn delete_org_op(conn: &Connection, op_id: i64) -> Result<()> {
    conn.execute("DELETE FROM org_ops WHERE id = ?1", params![op_id])?;
    Ok(())
}

/// Crash recovery: các op đã ghi intent nhưng chưa chốt done — đối chiếu fs
/// trước khi cho đợt organize/undo kế chạy.
pub fn pending_org_ops(conn: &Connection) -> Result<Vec<OrgOpRow>> {
    let mut st = conn.prepare_cached(
        "SELECT id, batch_id, file_id, old_path, new_path
         FROM org_ops
         WHERE done_at IS NULL AND recovery_attempted_at IS NULL
         ORDER BY id",
    )?;
    let rows = st
        .query_map([], map_org_op)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn mark_org_op_recovery_failed(conn: &Connection, op_id: i64, reason: &str) -> Result<()> {
    conn.execute(
        "UPDATE org_ops
         SET recovery_error = ?2, recovery_attempted_at = ?3
         WHERE id = ?1 AND done_at IS NULL",
        params![op_id, reason, now_ms()],
    )?;
    Ok(())
}

pub fn org_op_recovery_error(conn: &Connection, op_id: i64) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT recovery_error FROM org_ops WHERE id = ?1",
            params![op_id],
            |r| r.get(0),
        )
        .optional()?
        .flatten())
}

fn map_org_op(r: &rusqlite::Row) -> rusqlite::Result<OrgOpRow> {
    Ok(OrgOpRow {
        id: r.get(0)?,
        batch_id: r.get(1)?,
        file_id: r.get(2)?,
        old_path: r.get(3)?,
        new_path: r.get(4)?,
    })
}

pub fn list_org_batches(conn: &Connection) -> Result<Vec<OrgBatchRow>> {
    let mut st = conn.prepare_cached(
        "SELECT o.batch_id, j.finished_at,
                SUM(CASE WHEN o.done_at IS NOT NULL AND o.undone_at IS NULL THEN 1 ELSE 0 END),
                SUM(CASE WHEN o.undone_at IS NOT NULL THEN 1 ELSE 0 END)
         FROM org_ops o LEFT JOIN jobs j ON j.id = o.batch_id
         GROUP BY o.batch_id ORDER BY o.batch_id DESC LIMIT 50",
    )?;
    let rows = st
        .query_map([], |r| {
            Ok(OrgBatchRow {
                batch_id: r.get(0)?,
                finished_at: r.get(1)?,
                moved: r.get(2)?,
                undone: r.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Op của 1 batch theo thứ tự NGƯỢC (undo replay từ op cuối về đầu).
pub fn ops_of_batch_for_undo(conn: &Connection, batch_id: i64) -> Result<Vec<OrgOpRow>> {
    let mut st = conn.prepare_cached(
        "SELECT id, batch_id, file_id, old_path, new_path
         FROM org_ops
         WHERE batch_id = ?1 AND done_at IS NOT NULL AND undone_at IS NULL
         ORDER BY id DESC",
    )?;
    let rows = st
        .query_map(params![batch_id], map_org_op)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn mark_org_op_undone(conn: &Connection, op_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE org_ops SET undone_at = ?2 WHERE id = ?1",
        params![op_id, now_ms()],
    )?;
    Ok(())
}

/// (size, mtime) hiện tại của 1 file trong index — undo dùng để fs re-check.
pub fn file_size_mtime(conn: &Connection, file_id: i64) -> Result<Option<(i64, i64)>> {
    Ok(conn
        .query_row(
            "SELECT size, mtime FROM files WHERE id = ?1",
            params![file_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}

/// Snapshot xác minh cho crash recovery. Hash chỉ hợp lệ khi hashed_* còn
/// khớp file row; caller luôn phải check size+mtime filesystem trước.
pub struct FileVerifyContext {
    pub size: i64,
    pub mtime: i64,
    pub full_hash: Option<Vec<u8>>,
}

pub fn file_verify_context(conn: &Connection, file_id: i64) -> Result<Option<FileVerifyContext>> {
    Ok(conn
        .query_row(
            "SELECT f.size, f.mtime,
                    CASE WHEN h.hashed_size = f.size AND h.hashed_mtime = f.mtime
                         THEN h.full_hash ELSE NULL END
             FROM files f LEFT JOIN hashes h ON h.file_id = f.id
             WHERE f.id = ?1",
            params![file_id],
            |r| {
                Ok(FileVerifyContext {
                    size: r.get(0)?,
                    mtime: r.get(1)?,
                    full_hash: r.get(2)?,
                })
            },
        )
        .optional()?)
}

// ---------- cập nhật index sau move ----------

/// Cập nhật vị trí file trong index sau khi fs move THÀNH CÔNG — giữ nguyên
/// id/tags/hashes/meta (rename không đổi mtime nên trigger invalidate không
/// bắn; thumb key theo file_id + mtime nên cache còn nguyên).
pub fn update_file_location(conn: &mut Connection, file_id: i64, new_path: &str) -> Result<()> {
    let new_path = normalize_path(new_path);
    let (dir_path, name) = new_path
        .rsplit_once('\\')
        .ok_or_else(|| anyhow!("ERR_INTERNAL|bad path {new_path}"))?;
    // "D:\x" rsplit ra ("D:", "x") — dir phải là "D:\"
    let dir_path = if dir_path.len() == 2 && dir_path.ends_with(':') {
        format!("{dir_path}\\")
    } else {
        dir_path.to_string()
    };
    let letter = drive_letter(&new_path).ok_or_else(|| anyhow!("ERR_ROOT_NO_DRIVE|{new_path}"))?;
    let guid = format!("{letter}:");

    let tx = conn.transaction()?;
    let volume_id: i64 = tx
        .query_row(
            "SELECT id FROM volumes WHERE guid = ?1",
            params![guid],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| anyhow!("ERR_INTERNAL|volume {guid} chưa có"))?;
    let dir_name = dir_path.rsplit('\\').next().unwrap_or(&dir_path);
    let path_key = dir_path.to_uppercase();
    tx.execute(
        "INSERT INTO dirs(volume_id, name, path, path_key) VALUES(?1, ?2, ?3, ?4)
         ON CONFLICT(volume_id, path_key) DO NOTHING",
        params![volume_id, dir_name, dir_path, path_key],
    )?;
    let dir_id: i64 = tx.query_row(
        "SELECT id FROM dirs WHERE volume_id = ?1 AND path_key = ?2",
        params![volume_id, path_key],
        |r| r.get(0),
    )?;
    // Ghost row chiếm chỗ (dir_id, name) trong index nhưng fs target vừa được
    // verify là TRỐNG → row đó chắc chắn stale, dọn để khỏi vướng UNIQUE.
    tx.execute(
        "DELETE FROM files WHERE dir_id = ?1 AND name = ?2 AND id != ?3",
        params![dir_id, name, file_id],
    )?;
    let name_norm = normalize_for_search(name);
    let updated = tx.execute(
        "UPDATE files SET dir_id = ?2, volume_id = ?3, name = ?4, name_norm = ?5,
                original_name = COALESCE(original_name, name), frn = NULL
         WHERE id = ?1",
        params![file_id, dir_id, volume_id, name, name_norm],
    )?;
    if updated != 1 {
        bail!("ERR_FILE_GONE|file id {file_id} disappeared during organize");
    }
    tx.commit()?;
    Ok(())
}
