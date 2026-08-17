//! Nhãn (tag) và album — hai cách gom file do CHÍNH USER đặt ra, khác với mọi
//! nhóm khác trong app (nhóm trùng, nhóm theo ngày) vốn do máy suy ra.
//!
//! Hệ quả: chúng là dữ liệu KHÔNG dựng lại được bằng quét. Xoá nhầm một album
//! thì không có đường nào lấy lại, nên mọi thao tác xoá ở đây phải do user bấm
//! và phải hỏi lại — y như luật của phần xoá file.
//!
//! # Quan hệ với `files`
//!
//! `file_tags`/`album_files` có khoá ngoại ON DELETE CASCADE tới `files`: xoá
//! ảnh là nó rời khỏi mọi nhãn/album, không để lại dòng mồ côi làm sai số đếm.
//! (`files.id` là AUTOINCREMENT nên id đã xoá không bao giờ bị cấp lại — không
//! có chuyện một ảnh mới tự chui vào album cũ.)

use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use crate::ops::now_ms;

/// Một nhãn/album + số file đang nằm trong đó.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NamedCount {
    pub id: i64,
    pub name: String,
    pub count: i64,
}

/// Gọn khoảng trắng + cắt hai đầu. Tên rỗng bị từ chối chứ không lặng lẽ tạo
/// một nhãn vô hình mà user không bao giờ bấm trúng được.
fn clean_name(name: &str) -> Result<String> {
    let n = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if n.is_empty() {
        bail!("ERR_NAME_EMPTY|tên không được để trống");
    }
    if n.chars().count() > 64 {
        bail!("ERR_NAME_TOO_LONG|tên tối đa 64 ký tự");
    }
    Ok(n)
}

/// Chỉ đếm file đang hiện diện — nhãn có 300 file mà 300 file đó nằm trên ổ đã
/// tháo thì hiện "300" là nói dối; bấm vào sẽ ra danh sách rỗng.
const LIVE_FILES: &str = "SELECT 1 FROM files f WHERE f.id = x.file_id
     AND f.status IN (0, 2) AND (f.kind = 0 OR f.live_pair_id IS NULL)";

pub fn list_tags(conn: &Connection) -> Result<Vec<NamedCount>> {
    let mut st = conn.prepare_cached(&format!(
        "SELECT t.id, t.name,
                (SELECT COUNT(*) FROM file_tags x
                 WHERE x.tag_id = t.id AND EXISTS({LIVE_FILES}))
         FROM tags t ORDER BY t.name COLLATE NOCASE"
    ))?;
    let rows = st
        .query_map([], |r| {
            Ok(NamedCount {
                id: r.get(0)?,
                name: r.get(1)?,
                count: r.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn list_albums(conn: &Connection) -> Result<Vec<NamedCount>> {
    let mut st = conn.prepare_cached(&format!(
        "SELECT a.id, a.name,
                (SELECT COUNT(*) FROM album_files x
                 WHERE x.album_id = a.id AND EXISTS({LIVE_FILES}))
         FROM albums a ORDER BY a.name COLLATE NOCASE"
    ))?;
    let rows = st
        .query_map([], |r| {
            Ok(NamedCount {
                id: r.get(0)?,
                name: r.get(1)?,
                count: r.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Nhãn của 1 file, cho panel info. `count` ở đây luôn 0 — dùng chung struct
/// cho đỡ đẻ thêm kiểu, caller không đọc field đó.
pub fn tags_of_file(conn: &Connection, file_id: i64) -> Result<Vec<NamedCount>> {
    let mut st = conn.prepare_cached(
        "SELECT t.id, t.name FROM tags t
         JOIN file_tags ft ON ft.tag_id = t.id
         WHERE ft.file_id = ?1 ORDER BY t.name COLLATE NOCASE",
    )?;
    let rows = st
        .query_map([file_id], |r| {
            Ok(NamedCount {
                id: r.get(0)?,
                name: r.get(1)?,
                count: 0,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Gắn nhãn (tạo nếu chưa có) cho một loạt file. Trả id của nhãn.
///
/// Tên so khớp KHÔNG phân biệt hoa thường (collation NOCASE trên cột) nên gõ
/// "gia đình" khi đã có "Gia đình" là gắn vào nhãn cũ chứ không đẻ nhãn thứ hai
/// trông y hệt. Lưu ý NOCASE của SQLite chỉ fold ASCII: "GIA ĐÌNH" viết hoa
/// toàn bộ vẫn ra nhãn riêng vì Đ/đ nằm ngoài ASCII.
pub fn tag_files(conn: &mut Connection, name: &str, file_ids: &[i64]) -> Result<i64> {
    let name = clean_name(name)?;
    let tx = conn.transaction()?;
    let id: i64 = {
        let existing: Option<i64> = tx
            .query_row("SELECT id FROM tags WHERE name = ?1", [&name], |r| r.get(0))
            .optional()?;
        match existing {
            Some(id) => id,
            None => {
                tx.execute("INSERT INTO tags(name) VALUES(?1)", [&name])?;
                tx.last_insert_rowid()
            }
        }
    };
    {
        // Bỏ qua file đã mất: FK sẽ chặn, nhưng chặn kiểu đó là hỏng cả lô
        let mut ins = tx.prepare_cached(
            "INSERT OR IGNORE INTO file_tags(file_id, tag_id)
             SELECT ?1, ?2 WHERE EXISTS(SELECT 1 FROM files WHERE id = ?1)",
        )?;
        for &f in file_ids {
            ins.execute(params![f, id])?;
        }
    }
    tx.commit()?;
    Ok(id)
}

pub fn untag_files(conn: &mut Connection, tag_id: i64, file_ids: &[i64]) -> Result<()> {
    let tx = conn.transaction()?;
    {
        let mut del =
            tx.prepare_cached("DELETE FROM file_tags WHERE tag_id = ?1 AND file_id = ?2")?;
        for &f in file_ids {
            del.execute(params![tag_id, f])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Xoá hẳn nhãn khỏi thư viện (file thì không đụng tới).
pub fn delete_tag(conn: &mut Connection, tag_id: i64) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM file_tags WHERE tag_id = ?1", [tag_id])?;
    tx.execute("DELETE FROM tags WHERE id = ?1", [tag_id])?;
    tx.commit()?;
    Ok(())
}

pub fn rename_tag(conn: &mut Connection, tag_id: i64, name: &str) -> Result<()> {
    let name = clean_name(name)?;
    conn.execute(
        "UPDATE tags SET name = ?2 WHERE id = ?1",
        params![tag_id, name],
    )?;
    Ok(())
}

pub fn create_album(conn: &mut Connection, name: &str) -> Result<i64> {
    let name = clean_name(name)?;
    conn.execute(
        "INSERT INTO albums(name, created_at) VALUES(?1, ?2)",
        params![name, now_ms()],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Thêm file vào album, giữ THỨ TỰ caller đưa vào. `pos` nối tiếp sau phần tử
/// cuối đang có nên thêm nhiều đợt vẫn ra đúng thứ tự đã thêm.
/// Trả số file thực sự được thêm (file đã có sẵn trong album không tính).
pub fn add_to_album(conn: &mut Connection, album_id: i64, file_ids: &[i64]) -> Result<usize> {
    let tx = conn.transaction()?;
    let mut pos: i64 = tx.query_row(
        "SELECT COALESCE(MAX(pos), -1) + 1 FROM album_files WHERE album_id = ?1",
        [album_id],
        |r| r.get(0),
    )?;
    let mut added = 0usize;
    {
        let mut ins = tx.prepare_cached(
            "INSERT OR IGNORE INTO album_files(album_id, file_id, pos)
             SELECT ?1, ?2, ?3 WHERE EXISTS(SELECT 1 FROM files WHERE id = ?2)",
        )?;
        for &f in file_ids {
            if ins.execute(params![album_id, f, pos])? > 0 {
                pos += 1;
                added += 1;
            }
        }
    }
    tx.commit()?;
    Ok(added)
}

pub fn remove_from_album(conn: &mut Connection, album_id: i64, file_ids: &[i64]) -> Result<()> {
    let tx = conn.transaction()?;
    {
        let mut del =
            tx.prepare_cached("DELETE FROM album_files WHERE album_id = ?1 AND file_id = ?2")?;
        for &f in file_ids {
            del.execute(params![album_id, f])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Xoá album. KHÔNG đụng tới file — album chỉ là một cách xếp, không phải nơi
/// chứa; user xoá album mà mất ảnh thì là mất dữ liệu thật.
pub fn delete_album(conn: &mut Connection, album_id: i64) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM album_files WHERE album_id = ?1", [album_id])?;
    tx.execute("DELETE FROM albums WHERE id = ?1", [album_id])?;
    tx.commit()?;
    Ok(())
}

pub fn rename_album(conn: &mut Connection, album_id: i64, name: &str) -> Result<()> {
    let name = clean_name(name)?;
    conn.execute(
        "UPDATE albums SET name = ?2 WHERE id = ?1",
        params![album_id, name],
    )?;
    Ok(())
}
