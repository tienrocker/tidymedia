use std::path::Path;

use anyhow::Result;
use crossbeam_channel::{bounded, Receiver, Sender};
use rusqlite::{Connection, OpenFlags};

/// Pool connection read-only trên WAL — checkout/checkin qua channel.
/// Connection được trả về pool qua RAII guard, kể cả khi closure panic.
pub struct ReadPool {
    tx: Sender<Connection>,
    rx: Receiver<Connection>,
}

struct PoolGuard<'a> {
    conn: Option<Connection>,
    tx: &'a Sender<Connection>,
}

impl Drop for PoolGuard<'_> {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            // Nếu pool đã đóng thì connection drop luôn — không sao.
            let _ = self.tx.send(conn);
        }
    }
}

impl ReadPool {
    pub fn new(db_path: &Path, size: usize) -> Result<Self> {
        let (tx, rx) = bounded(size);
        for _ in 0..size {
            let conn = Connection::open_with_flags(
                db_path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            conn.execute_batch("PRAGMA busy_timeout=5000; PRAGMA cache_size=-16384;")?;
            rusqlite::vtab::array::load_module(&conn)?;
            tx.send(conn).expect("fill read pool");
        }
        Ok(Self { tx, rx })
    }

    pub fn with<R>(&self, f: impl FnOnce(&Connection) -> R) -> R {
        let conn = self.rx.recv().expect("read pool closed");
        let guard = PoolGuard {
            conn: Some(conn),
            tx: &self.tx,
        };
        // Guard trả connection về pool trong Drop — panic-safe.
        f(guard.conn.as_ref().expect("guard holds connection"))
    }
}
