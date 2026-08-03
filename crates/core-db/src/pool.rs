use std::path::Path;

use anyhow::Result;
use crossbeam_channel::{bounded, Receiver, Sender};
use rusqlite::{Connection, OpenFlags};

/// Pool connection read-only trên WAL — checkout/checkin qua channel.
pub struct ReadPool {
    tx: Sender<Connection>,
    rx: Receiver<Connection>,
}

impl ReadPool {
    pub fn new(db_path: &Path, size: usize) -> Result<Self> {
        let (tx, rx) = bounded(size);
        for _ in 0..size {
            let conn = Connection::open_with_flags(
                db_path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            conn.execute_batch("PRAGMA busy_timeout=5000; PRAGMA cache_size=-65536;")?;
            rusqlite::vtab::array::load_module(&conn)?;
            tx.send(conn).expect("fill read pool");
        }
        Ok(Self { tx, rx })
    }

    pub fn with<R>(&self, f: impl FnOnce(&Connection) -> R) -> R {
        let conn = self.rx.recv().expect("read pool empty/poisoned");
        let out = f(&conn);
        self.tx.send(conn).expect("return conn to pool");
        out
    }
}
