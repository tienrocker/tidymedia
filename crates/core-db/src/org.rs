//! DB ops cho M5 organize: library roots (đích do user chỉ định), journal
//! org_ops (write-ahead + undo), query ứng viên organize.
//!
//! Batch id của org_ops = jobs.id của job organize (không cần bảng batch riêng).

use anyhow::{anyhow, bail, Result};
use rusqlite::{params, Connection, OptionalExtension};

use crate::models::{LibraryRootRow, OrgBatchRow, OrgCandidateRow, OrgOpRow, OrgPairRow};
use crate::ops::{
    drive_letter, join_path, normalize_for_search, normalize_path, now_ms, path_range,
};

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

/// File ĐANG nằm đúng chỗ organize đặt nó: có một op ORGANIZE đã hoàn tất,
/// chưa bị undo, và đích của op ĐÚNG BẰNG vị trí hiện tại của file. Dùng
/// `org_ops` chứ không phải `files.original_name`, vì undo KHÔNG xoá
/// `original_name` (`update_file_location` đặt nó bằng
/// `COALESCE(original_name, name)` và không bao giờ gỡ ra) — file đã undo vẫn
/// mang cột đó, nên nó chỉ nói "đã từng bị organize đổi tên", không nói "đang
/// nằm trong kho". `org_ops` thì undo có dọn: `mark_org_op_undone` đặt
/// `undone_at`.
///
/// Vế so path không phải trang trí: lịch sử op không chứng minh HIỆN TẠI.
/// Crash giữa lượt undo để lại op organize còn active trong khi file đã về chỗ
/// cũ — thiếu vế này thì lần organize sau tự "redo" cái user vừa undo. User tự
/// tay dời file đã organize đi chỗ khác rồi rescan cũng vậy.
///
/// So sánh qua `path_key()` (Unicode-uppercase, xem `register_sql_functions`)
/// chứ KHÔNG so byte: Windows không phân biệt hoa thường, và casing hai vế
/// được sinh độc lập — `dirs.path` đóng băng theo ai insert TRƯỚC (cả scanner
/// lẫn `update_file_location_in` đều `ON CONFLICT ... DO NOTHING`), còn
/// `org_ops.new_path` mang casing template render. Scanner index `…\photos\`
/// trước rồi organize tạo `…\Photos\` là chuyện thường (`create_dir_all` no-op
/// khi thư mục đã tồn tại khác casing) — so byte thì file vừa gom xong đã rơi
/// khỏi tập managed ngay. Cùng fold với `path_key` cột và `eq_ci` của planner,
/// hai nửa của một bất biến phải dùng chung một phép so.
///
/// Vế `o.reverses_op_id IS NULL` loại op undo: đích của op undo là chỗ CŨ của
/// file, khớp vị trí hiện tại sau khi undo xong — không lọc thì chính nó lại
/// biến file thành "đang được organize giữ". Vế `NOT EXISTS(jobs org_undo)`
/// chặn nốt row undo TRƯỚC schema v11: migration gán `reverses_op_id = NULL`
/// cho mọi row cũ, nên residue undo của bản cũ (done mà chưa kịp tự đánh dấu
/// undone vì crash) sẽ giả dạng op organize nếu chỉ nhìn cột đó — batch id của
/// nó là job `org_undo` thì không thể là bằng chứng "organize đặt file ở đây".
/// Jobs bị prune (tương lai) thì vế này degrade về hành vi hiện tại, không tệ đi.
/// Mảnh WHERE dùng chung cho cả hai câu hỏi về op đó (managed? + provenance nào?)
/// — hai bản chép tay là hai cơ hội cho một bên quên thêm điều kiện.
/// Ghép path như `join_path`: dir kết thúc bằng '\' (gốc ổ) thì không chèn thêm.
const ACTIVE_ORG_OP_AT_CURRENT_PATH: &str =
    "o.file_id = f.id AND o.done_at IS NOT NULL AND o.undone_at IS NULL
               AND o.reverses_op_id IS NULL
               AND NOT EXISTS(SELECT 1 FROM jobs j
                              WHERE j.id = o.batch_id AND j.kind = 'org_undo')
               AND path_key(o.new_path) = CASE WHEN substr(d.path_key, -1) = '\\'
                                     THEN d.path_key || path_key(f.name)
                                     ELSE d.path_key || '\\' || path_key(f.name) END";

/// Giới hạn ứng viên vào các THƯ MỤC NGUỒN user chọn, CỘNG những file organize
/// đang giữ. Danh sách rỗng = cả volume, tức đúng hành vi cũ.
///
/// Vì sao cần giới hạn: không có nó thì bấm organize là ôm mọi file trên ổ. Kho
/// thật dùng để phát triển có 25.624 file, trong đó `vod` là 99 GB video tải về
/// — trộn nó vào cây ảnh theo ngày là sai, mà undo 25 nghìn file thì vẫn là một
/// mớ dù có undo được.
///
/// Vì sao vẫn phải nới cho file organize đang giữ: bỏ chúng ra thì hỏng hai thứ
/// mà phần còn lại của organize dựa vào —
///
/// 1. **Đổi template rồi gom lại**: file cũ nằm im ở đường dẫn cũ, kho thành
///    nửa theo cách đặt tên này nửa theo cách kia, mà planner vốn idempotent
///    đúng để chuyện đó không xảy ra.
/// 2. **Hàn lại cặp Live Photo gom hụt**: khi ảnh chuyển xong mà MOV fail,
///    đường sửa nằm ở nhánh `SkipOrganized` của planner (nó phát lệnh chuyển
///    nốt MOV). Ảnh không còn được chọn thì nhánh đó không bao giờ chạy, mà MOV
///    thì luôn bị ẩn khỏi ứng viên — cặp xé vĩnh viễn.
///
/// Nới theo [`ACTIVE_ORG_OP_AT_CURRENT_PATH`] chứ KHÔNG phải theo cây thư mục kho. Nới
/// theo cây kho là một lỗ thật: đặt kho đích ở `E:\images` trong khi nguồn là
/// `E:\images\icloud` thì hợp của hai tập là cả `E:\images`, và `vod` 99 GB lọt
/// vào đúng cái mà phạm vi sinh ra để chặn — im lặng, không cảnh báo. Kho ở
/// `E:\` thì phạm vi mất tác dụng trên cả ổ. Bám theo op cũng là thứ duy nhất
/// sống sót qua việc ĐỔI thư mục kho: `set_library_root` chỉ ghi đè `path` chứ
/// không dời file, nên file nằm ở kho cũ không thuộc nguồn lẫn kho mới.
///
/// Đánh đổi đã biết: file user tự tay quăng vào thư mục kho không có op nào nên
/// không được nới. Đúng nghĩa — user đặt phạm vi tức là nói "chỉ mấy thư mục
/// này", còn phạm vi rỗng (mặc định) thì cả volume vẫn được quét như cũ.
///
/// Dùng lại [`path_range`] — CÙNG phép so prefix với root scope, nên
/// `E:\images\anh` không bao giờ nuốt nhầm `E:\images\anh cuoi`: dải kết thúc
/// ở `']'` (0x5D), ngay sau `'\'` (0x5C), nên chỉ khớp đúng con trực thuộc.
///
/// `next_idx` = số thứ tự tham số kế tiếp còn trống của câu SQL gọi tới.
fn scope_sql(scopes: &[String], next_idx: usize) -> (String, Vec<String>) {
    if scopes.is_empty() {
        return (String::new(), Vec::new());
    }
    let mut parts = Vec::new();
    let mut args = Vec::new();
    let mut i = next_idx;
    for s in scopes {
        let (eq, start, end) = path_range(&normalize_path(s));
        parts.push(format!(
            "(d.path_key = ?{i} OR (d.path_key >= ?{} AND d.path_key < ?{}))",
            i + 1,
            i + 2
        ));
        args.push(eq);
        args.push(start);
        args.push(end);
        i += 3;
    }
    parts.push(format!(
        "EXISTS(SELECT 1 FROM org_ops o WHERE {ACTIVE_ORG_OP_AT_CURRENT_PATH})"
    ));
    (format!(" AND ({})", parts.join(" OR ")), args)
}

/// Ứng viên organize trên 1 volume, keyset theo f.id. Lấy CẢ file organize
/// đang giữ trong kho — planner tự trả SkipOrganized (idempotent). File user
/// tự quăng vào thư mục kho chỉ được xếp khi scope RỖNG (cả volume) hoặc kho
/// nằm trong scope user chọn — scope không rỗng nghĩa là "chỉ mấy thư mục
/// này", không âm thầm nới thêm. MOV có pair bị ẩn (đi theo
/// ảnh canonical mà MOV trỏ ngược qua cột `pair`), status=1 missing loại từ SQL.
/// Nếu HEIC + JPG cùng trỏ một MOV, chỉ canonical owner được phép move MOV;
/// ảnh còn lại vẫn organize độc lập và giữ link logic tới cùng MOV.
pub fn select_org_candidates(
    conn: &Connection,
    volume_id: i64,
    after_id: i64,
    limit: i64,
    scopes: &[String],
) -> Result<Vec<OrgCandidateRow>> {
    let (scope_where, scope_args) = scope_sql(scopes, 4);
    let sql = format!(
        "SELECT f.id, d.path, f.name, f.ext, f.kind, f.size, f.mtime, f.status,
                m.taken_at, m.date_source, m.camera,
                h.full_hash, h.hashed_size, h.hashed_mtime,
                pv.id, pd.path, pv.name, pv.ext, pv.status, pv.size, pv.mtime,
                ph.full_hash, ph.hashed_size, ph.hashed_mtime,
                f.original_name, m.gps_lat, m.gps_lon,
                (SELECT o.lib_root FROM org_ops o
                  WHERE {ACTIVE_ORG_OP_AT_CURRENT_PATH}
                  ORDER BY o.id DESC LIMIT 1),
                (SELECT o.src_rel_dir FROM org_ops o
                  WHERE {ACTIVE_ORG_OP_AT_CURRENT_PATH}
                  ORDER BY o.id DESC LIMIT 1)
         FROM files f
         JOIN dirs d ON d.id = f.dir_id
         LEFT JOIN media_meta m ON m.file_id = f.id
         LEFT JOIN hashes h ON h.file_id = f.id
         LEFT JOIN files pv ON pv.id = f.live_pair_id AND f.kind = 0 AND pv.kind = 1
                           AND pv.live_pair_id = f.id
         LEFT JOIN dirs pd ON pd.id = pv.dir_id
         LEFT JOIN hashes ph ON ph.file_id = pv.id
         WHERE {CANDIDATE_WHERE}{scope_where}
         ORDER BY f.id LIMIT ?3"
    );
    let mut st = conn.prepare_cached(&sql)?;
    // ?1 ?2 ?3 cố định, scope nối tiếp từ ?4 (xem scope_sql)
    let mut args: Vec<Box<dyn rusqlite::ToSql>> =
        vec![Box::new(volume_id), Box::new(after_id), Box::new(limit)];
    args.extend(
        scope_args
            .into_iter()
            .map(|s| Box::new(s) as Box<dyn rusqlite::ToSql>),
    );
    let rows = st
        .query_map(rusqlite::params_from_iter(args.iter()), |r| {
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
                dir_path: dir,
                original_name: r.get(24)?,
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
                // Nửa toạ độ là vô dụng — chỉ nhận khi có đủ cả hai
                gps: match (r.get::<_, Option<f64>>(25)?, r.get::<_, Option<f64>>(26)?) {
                    (Some(lat), Some(lon)) => Some((lat, lon)),
                    _ => None,
                },
                managed_lib_root: r.get(27)?,
                managed_src_rel: r.get(28)?,
                pair,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Đếm phải dùng ĐÚNG điều kiện với `select_org_candidates` — lệch nhau thì
/// progress bar của job chạy quá 100% hoặc không bao giờ tới đích.
pub fn count_org_candidates(conn: &Connection, volume_id: i64, scopes: &[String]) -> Result<i64> {
    let (scope_where, scope_args) = scope_sql(scopes, 3);
    let sql = format!(
        "SELECT COUNT(*) FROM files f JOIN dirs d ON d.id = f.dir_id
         WHERE {CANDIDATE_WHERE}{scope_where}"
    );
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(volume_id), Box::new(0i64)];
    args.extend(
        scope_args
            .into_iter()
            .map(|s| Box::new(s) as Box<dyn rusqlite::ToSql>),
    );
    Ok(conn.query_row(&sql, rusqlite::params_from_iter(args.iter()), |r| r.get(0))?)
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

/// Ghi INTENT organize trước khi đụng fs. done_at NULL = chưa xong.
/// `lib_root` = thư mục kho tại thời điểm này (xem
/// `OrgCandidateRow::managed_lib_root`). `src_rel_dir` = giá trị {relpath}
/// NGUỒN của file lúc op này đặt nó — Some("") là "nằm ngay gốc watch root",
/// None là không biết (xem `OrgCandidateRow::managed_src_rel`).
pub fn insert_org_op(
    conn: &Connection,
    batch_id: i64,
    file_id: i64,
    old_path: &str,
    new_path: &str,
    lib_root: &str,
    src_rel_dir: Option<&str>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO org_ops(batch_id, file_id, old_path, new_path, lib_root, src_rel_dir)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        params![batch_id, file_id, old_path, new_path, lib_root, src_rel_dir],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Ghi INTENT của lượt UNDO đảo op `reverses_op_id`. Recovery dựa vào liên kết
/// này để chốt nốt op gốc thành undone khi crash giữa chừng — thiếu nó thì file
/// đã về chỗ cũ vẫn mang op active và lần organize sau tự "redo" cái vừa undo.
pub fn insert_undo_op(
    conn: &Connection,
    batch_id: i64,
    file_id: i64,
    old_path: &str,
    new_path: &str,
    reverses_op_id: i64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO org_ops(batch_id, file_id, old_path, new_path, reverses_op_id)
         VALUES(?1, ?2, ?3, ?4, ?5)",
        params![batch_id, file_id, old_path, new_path, reverses_op_id],
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
        "SELECT id, batch_id, file_id, old_path, new_path, reverses_op_id
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
        reverses_op_id: r.get(5)?,
    })
}

/// Lịch sử batch cho UI undo. Chỉ đếm op ORGANIZE (`reverses_op_id IS NULL`):
/// mỗi lượt undo cũng tạo batch riêng toàn intent (moved = 0) — không lọc thì
/// ~50 lượt undo là batch thật bị đẩy khỏi LIMIT 50 trong khi UI chỉ ẩn chúng.
pub fn list_org_batches(conn: &Connection) -> Result<Vec<OrgBatchRow>> {
    let mut st = conn.prepare_cached(
        "SELECT o.batch_id, j.finished_at,
                SUM(CASE WHEN o.done_at IS NOT NULL AND o.undone_at IS NULL THEN 1 ELSE 0 END),
                SUM(CASE WHEN o.undone_at IS NOT NULL THEN 1 ELSE 0 END)
         FROM org_ops o LEFT JOIN jobs j ON j.id = o.batch_id
         WHERE o.reverses_op_id IS NULL
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
/// Chỉ op organize — op undo còn sót (crash trước khi kịp tự đánh dấu undone)
/// không phải thứ "undo được": đảo nó là organize lại, đường đó đã có sẵn.
pub fn ops_of_batch_for_undo(conn: &Connection, batch_id: i64) -> Result<Vec<OrgOpRow>> {
    let mut st = conn.prepare_cached(
        "SELECT id, batch_id, file_id, old_path, new_path, reverses_op_id
         FROM org_ops o
         WHERE batch_id = ?1 AND done_at IS NOT NULL AND undone_at IS NULL
           AND reverses_op_id IS NULL
           AND NOT EXISTS(SELECT 1 FROM jobs j
                          WHERE j.id = o.batch_id AND j.kind = 'org_undo')
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
    let tx = conn.transaction()?;
    update_file_location_in(&tx, file_id, new_path)?;
    tx.commit()?;
    Ok(())
}

/// Thân của [`update_file_location`] KHÔNG tự mở transaction — cho caller nào
/// cần gộp nó với việc chốt journal (`mark_org_op_done`/`undone`) thành MỘT
/// lượt commit: undo và recovery mà commit lắt nhắt thì crash giữa chừng để
/// lại journal kể dở câu chuyện. `conn` phải đang ở trong transaction.
pub fn update_file_location_in(conn: &Connection, file_id: i64, new_path: &str) -> Result<()> {
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

    let volume_id: i64 = conn
        .query_row(
            "SELECT id FROM volumes WHERE guid = ?1",
            params![guid],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| anyhow!("ERR_INTERNAL|volume {guid} chưa có"))?;
    let dir_name = dir_path.rsplit('\\').next().unwrap_or(&dir_path);
    let path_key = dir_path.to_uppercase();
    conn.execute(
        "INSERT INTO dirs(volume_id, name, path, path_key) VALUES(?1, ?2, ?3, ?4)
         ON CONFLICT(volume_id, path_key) DO NOTHING",
        params![volume_id, dir_name, dir_path, path_key],
    )?;
    let dir_id: i64 = conn.query_row(
        "SELECT id FROM dirs WHERE volume_id = ?1 AND path_key = ?2",
        params![volume_id, path_key],
        |r| r.get(0),
    )?;
    // Ghost row chiếm chỗ (dir_id, name) trong index nhưng fs target vừa được
    // verify là TRỐNG → row đó chắc chắn stale, dọn để khỏi vướng UNIQUE.
    conn.execute(
        "DELETE FROM files WHERE dir_id = ?1 AND name = ?2 AND id != ?3",
        params![dir_id, name, file_id],
    )?;
    let name_norm = normalize_for_search(name);
    let updated = conn.execute(
        "UPDATE files SET dir_id = ?2, volume_id = ?3, name = ?4, name_norm = ?5,
                original_name = COALESCE(original_name, name), frn = NULL
         WHERE id = ?1",
        params![file_id, dir_id, volume_id, name, name_norm],
    )?;
    if updated != 1 {
        bail!("ERR_FILE_GONE|file id {file_id} disappeared during organize");
    }
    Ok(())
}
