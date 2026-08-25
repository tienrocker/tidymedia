use std::panic::AssertUnwindSafe;
use std::path::Path;

use anyhow::Result;
use crossbeam_channel::{unbounded, Sender};
use rusqlite::Connection;

type WriteFn = Box<dyn FnOnce(&mut Connection) + Send + 'static>;

/// Handle tới writer thread duy nhất — mọi ghi DB đi qua đây, nên SQLite
/// không bao giờ gặp write contention.
#[derive(Clone)]
pub struct WriterHandle {
    tx: Sender<WriteFn>,
}

impl WriterHandle {
    /// Chạy `f` trên write-connection và block chờ kết quả.
    pub fn exec<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&mut Connection) -> Result<R> + Send + 'static,
        R: Send + 'static,
    {
        let (rtx, rrx) = std::sync::mpsc::channel();
        self.tx
            .send(Box::new(move |conn| {
                let _ = rtx.send(f(conn));
            }))
            .map_err(|_| anyhow::anyhow!("ERR_DB_WRITER_DOWN|writer thread is gone"))?;
        rrx.recv()
            .map_err(|_| anyhow::anyhow!("ERR_DB_WRITER_DOWN|writer dropped result (panic?)"))?
    }

    /// Fire-and-forget. Lỗi trong `f` do closure tự xử lý/ghi nhận (vd error
    /// counter của scan); ở đây chỉ log phòng hờ.
    pub fn exec_async<F>(&self, f: F)
    where
        F: FnOnce(&mut Connection) -> Result<()> + Send + 'static,
    {
        let _ = self.tx.send(Box::new(move |conn| {
            if let Err(e) = f(conn) {
                tracing::error!("db write failed: {e:#}");
            }
        }));
    }
}

pub fn spawn_writer(db_path: &Path) -> Result<WriterHandle> {
    let mut conn = Connection::open(db_path)?;
    apply_write_pragmas(&conn)?;
    rusqlite::vtab::array::load_module(&conn)?;
    crate::ops::register_sql_functions(&conn)?;
    crate::ops::ensure_schema(&mut conn)?;

    let (tx, rx) = unbounded::<WriteFn>();
    std::thread::Builder::new()
        .name("db-writer".into())
        .spawn(move || {
            while let Ok(op) = rx.recv() {
                // Một op panic không được giết writer — bắt lại, rollback nếu
                // đang dở transaction, đi tiếp.
                let result = std::panic::catch_unwind(AssertUnwindSafe(|| op(&mut conn)));
                if result.is_err() {
                    tracing::error!("db write op panicked — rolled back, writer continues");
                    if !conn.is_autocommit() {
                        let _ = conn.execute_batch("ROLLBACK;");
                    }
                }
            }
        })
        .expect("spawn db-writer thread");
    Ok(WriterHandle { tx })
}

fn apply_write_pragmas(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         PRAGMA busy_timeout=5000;
         PRAGMA cache_size=-131072;
         PRAGMA foreign_keys=ON;
         PRAGMA temp_store=MEMORY;",
    )?;
    let mode: String = conn.pragma_query_value(None, "journal_mode", |r| r.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        anyhow::bail!("ERR_DB_NO_WAL|journal_mode={mode} — index db phải nằm trên ổ hỗ trợ WAL");
    }
    Ok(())
}
