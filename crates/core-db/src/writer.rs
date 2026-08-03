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
            .map_err(|_| anyhow::anyhow!("db writer thread is gone"))?;
        rrx.recv()
            .map_err(|_| anyhow::anyhow!("db writer dropped result"))?
    }

    /// Fire-and-forget (dùng cho batch scan). Lỗi được log, không propagate.
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
    crate::ops::ensure_schema(&mut conn)?;

    let (tx, rx) = unbounded::<WriteFn>();
    std::thread::Builder::new()
        .name("db-writer".into())
        .spawn(move || {
            while let Ok(op) = rx.recv() {
                op(&mut conn);
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
         PRAGMA cache_size=-262144;
         PRAGMA foreign_keys=ON;
         PRAGMA temp_store=MEMORY;",
    )?;
    Ok(())
}
