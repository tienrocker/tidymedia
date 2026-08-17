use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

/// thumbs.db — cache WebP blob riêng biệt với index.db (mất cũng chỉ tốn CPU
/// re-generate, không bao giờ đụng dữ liệu thật). LRU theo `last_used`, cap bytes.
pub struct ThumbStore {
    conn: Mutex<Connection>,
    total: AtomicI64,
    cap: i64,
}

impl ThumbStore {
    pub fn open(db_path: &Path, cap_bytes: i64) -> Result<Self> {
        if let Some(dir) = db_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(db_path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA busy_timeout=5000;
             CREATE TABLE IF NOT EXISTS thumbs(
               file_id INTEGER NOT NULL,
               s INTEGER NOT NULL,
               src_mtime INTEGER NOT NULL,
               data BLOB NOT NULL,
               bytes INTEGER NOT NULL,
               last_used INTEGER NOT NULL,
               PRIMARY KEY(file_id, s)
             );
             CREATE INDEX IF NOT EXISTS thumbs_lru ON thumbs(last_used);",
        )?;
        let total: i64 = conn.query_row("SELECT COALESCE(SUM(bytes), 0) FROM thumbs", [], |r| {
            r.get(0)
        })?;
        Ok(Self {
            conn: Mutex::new(conn),
            total: AtomicI64::new(total),
            cap: cap_bytes,
        })
    }

    /// Chép cache sang 1 file mới, gộp luôn WAL — dùng cho Export.
    /// `VACUUM INTO` chạy trên DB đang mở bình thường và không khoá ghi lâu;
    /// đích PHẢI chưa tồn tại (SQLite tự từ chối, không đè file của ai cả).
    pub fn vacuum_into(&self, dest: &Path) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("VACUUM INTO ?1", [dest.to_string_lossy().as_ref()])?;
        Ok(())
    }

    /// Cache hit chỉ khi src_mtime khớp — file bị sửa/thay là miss, caller
    /// regenerate rồi `put` đè. Touch last_used thưa (>1h) để đọc không thành ghi.
    pub fn get(&self, file_id: i64, s: u32, src_mtime: i64) -> Option<Vec<u8>> {
        let conn = self.conn.lock().unwrap();
        let row: Option<(Vec<u8>, i64, i64)> = conn
            .query_row(
                "SELECT data, src_mtime, last_used FROM thumbs WHERE file_id = ?1 AND s = ?2",
                params![file_id, s],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .ok()
            .flatten();
        let (data, stored_mtime, last_used) = row?;
        if stored_mtime != src_mtime {
            return None;
        }
        let now = now_s();
        if now - last_used > 3600 {
            let _ = conn.execute(
                "UPDATE thumbs SET last_used = ?1 WHERE file_id = ?2 AND s = ?3",
                params![now, file_id, s],
            );
        }
        Some(data)
    }

    /// Đã có thumb hợp lệ (đúng src_mtime) chưa — KHÔNG đọc blob, không touch
    /// last_used. Job warm nền dùng để bỏ qua nhanh file đã có sẵn.
    pub fn has(&self, file_id: i64, s: u32, src_mtime: i64) -> bool {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT 1 FROM thumbs WHERE file_id = ?1 AND s = ?2 AND src_mtime = ?3",
            params![file_id, s, src_mtime],
            |_| Ok(()),
        )
        .optional()
        .ok()
        .flatten()
        .is_some()
    }

    pub fn put(&self, file_id: i64, s: u32, src_mtime: i64, data: &[u8]) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let old: Option<i64> = conn
            .query_row(
                "SELECT bytes FROM thumbs WHERE file_id = ?1 AND s = ?2",
                params![file_id, s],
                |r| r.get(0),
            )
            .optional()?;
        conn.execute(
            "INSERT OR REPLACE INTO thumbs(file_id, s, src_mtime, data, bytes, last_used)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![file_id, s, src_mtime, data, data.len() as i64, now_s()],
        )?;
        let delta = data.len() as i64 - old.unwrap_or(0);
        let total = self.total.fetch_add(delta, Ordering::Relaxed) + delta;
        if total > self.cap {
            self.evict(&conn)?;
        }
        Ok(())
    }

    /// Đuổi LRU theo lô 256 row tới khi còn 90% cap — hysteresis, không đuổi
    /// từng row một mỗi lần put.
    fn evict(&self, conn: &Connection) -> Result<()> {
        let target = self.cap / 10 * 9;
        while self.total.load(Ordering::Relaxed) > target {
            let freed: i64 = conn.query_row(
                "SELECT COALESCE(SUM(bytes), 0) FROM thumbs WHERE rowid IN
                   (SELECT rowid FROM thumbs ORDER BY last_used ASC LIMIT 256)",
                [],
                |r| r.get(0),
            )?;
            if freed == 0 {
                break;
            }
            conn.execute(
                "DELETE FROM thumbs WHERE rowid IN
                   (SELECT rowid FROM thumbs ORDER BY last_used ASC LIMIT 256)",
                [],
            )?;
            self.total.fetch_sub(freed, Ordering::Relaxed);
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn total_bytes(&self) -> i64 {
        self.total.load(Ordering::Relaxed)
    }
}

fn now_s() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(cap: i64) -> (ThumbStore, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "tidymedia-test-thumbstore-{}-{cap}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        let db = dir.join("thumbs.db");
        (ThumbStore::open(&db, cap).unwrap(), dir)
    }

    #[test]
    fn get_put_roundtrip_and_mtime_invalidation() {
        let (store, dir) = temp_store(1 << 20);
        store.put(1, 256, 1000, b"webpdata").unwrap();
        assert_eq!(store.get(1, 256, 1000).as_deref(), Some(&b"webpdata"[..]));
        // mtime khác → stale → miss
        assert_eq!(store.get(1, 256, 2000), None);
        // put đè bản mới → hit lại, total không đếm đôi
        store.put(1, 256, 2000, b"newdata!").unwrap();
        assert_eq!(store.get(1, 256, 2000).as_deref(), Some(&b"newdata!"[..]));
        assert_eq!(store.total_bytes(), 8);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn lru_eviction_keeps_total_under_cap() {
        // cap 10KB, chèn 40 thumb × 1KB → phải evict, total ≤ cap
        let (store, dir) = temp_store(10 * 1024);
        let blob = vec![0u8; 1024];
        for id in 0..40 {
            store.put(id, 256, 1, &blob).unwrap();
        }
        assert!(
            store.total_bytes() <= 10 * 1024,
            "total={}",
            store.total_bytes()
        );
        // Bản chèn sớm nhất phải đã bị đuổi, bản mới nhất còn
        assert_eq!(store.get(0, 256, 1), None);
        assert!(store.get(39, 256, 1).is_some());
        std::fs::remove_dir_all(dir).ok();
    }
}
