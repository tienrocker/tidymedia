//! core-db: SQLite index — schema, single-writer thread, read pool, queries.
//!
//! Kiến trúc concurrency:
//! - MỌI ghi đi qua [`WriterHandle`] (1 OS thread, 1 write connection) → không bao giờ
//!   có write contention, WAL cho phép reader chạy song song trong lúc scan.
//! - Đọc qua [`ReadPool`] (N connection read-only).

mod models;
pub mod ops;
mod pool;
pub mod query;
mod writer;

use std::path::{Path, PathBuf};

use anyhow::Result;

pub use models::{
    DeleteContextRow, DupGroupRow, DupMemberRow, FileDetail, FileFilter, FileRow, HashUpsert,
    JobRow, MediaSrc, MetaUpsert, PendingHash, PendingMeta, RootInfo, ScanEntry,
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
}
