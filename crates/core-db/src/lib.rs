//! core-db: SQLite index — schema, single-writer thread, read pool, queries.
//!
//! Kiến trúc concurrency:
//! - MỌI ghi đi qua [`WriterHandle`] (1 OS thread, 1 write connection) → không bao giờ
//!   có write contention, WAL cho phép reader chạy song song trong lúc scan.
//! - Đọc qua [`ReadPool`] (N connection read-only).

mod models;
pub mod ops;
pub mod org;
mod pool;
pub mod query;
mod writer;

use std::path::{Path, PathBuf};

use anyhow::Result;

pub use models::{
    CameraCount, ClusterItem, DeleteContextRow, DupGroupRow, DupMemberBrief, DupMemberRow,
    FileDetail, FileFilter, FileRow, HashUpsert, JobRow, LibraryRootRow, MediaSrc, MetaUpsert,
    OrgBatchRow, OrgCandidateRow, OrgOpRow, OrgPairRow, PendingHash, PendingMeta, PendingPhash,
    PhashUpsert, RootInfo, ScanEntry,
};
pub use pool::ReadPool;
pub use writer::WriterHandle;

pub struct Db {
    pub writer: WriterHandle,
    pub pool: ReadPool,
    pub path: PathBuf,
}

impl Db {
    /// Mở (tạo nếu chưa có) `index.db` trong thư mục `data_dir`.
    pub fn open(data_dir: &Path) -> Result<Db> {
        std::fs::create_dir_all(data_dir)?;
        let db_path = data_dir.join("index.db");
        let writer = writer::spawn_writer(&db_path)?;
        let pool = ReadPool::new(&db_path, 4)?;
        Ok(Db {
            writer,
            pool,
            path: db_path,
        })
    }

    /// Chép index sang 1 file mới, gộp luôn WAL nên bản xuất ra là MỘT file
    /// tự đủ (không cần kèm -wal/-shm). Chạy trên connection đọc: `VACUUM INTO`
    /// không sửa gì nguồn, và đích phải chưa tồn tại — SQLite tự từ chối đè.
    pub fn vacuum_into(&self, dest: &Path) -> Result<()> {
        let dest = dest.to_string_lossy().to_string();
        self.pool.with(move |c| {
            c.execute("VACUUM INTO ?1", [dest.as_str()])?;
            Ok(())
        })
    }
}
