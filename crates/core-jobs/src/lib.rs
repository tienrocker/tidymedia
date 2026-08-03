//! core-jobs: registry job đang chạy + cancellation + event stream về UI.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crossbeam_channel::{unbounded, Receiver, Sender};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobProgress {
    pub job_id: i64,
    pub kind: String,
    pub done: u64,
    pub total: Option<u64>,
    pub message: Option<String>,
}

#[derive(Debug, Clone)]
pub enum JobEvent {
    Progress(JobProgress),
    Done {
        job_id: i64,
        kind: String,
        message: Option<String>,
    },
    Failed {
        job_id: i64,
        kind: String,
        error: String,
    },
    Cancelled {
        job_id: i64,
        kind: String,
    },
}

/// Cờ hủy — job thread poll cờ này giữa các batch.
pub type CancelFlag = Arc<AtomicBool>;

pub struct JobManager {
    active: Mutex<HashMap<i64, CancelFlag>>,
    tx: Sender<JobEvent>,
    rx: Receiver<JobEvent>,
}

impl Default for JobManager {
    fn default() -> Self {
        Self::new()
    }
}

impl JobManager {
    pub fn new() -> Self {
        let (tx, rx) = unbounded();
        Self {
            active: Mutex::new(HashMap::new()),
            tx,
            rx,
        }
    }

    pub fn register(&self, job_id: i64) -> CancelFlag {
        let flag: CancelFlag = Arc::new(AtomicBool::new(false));
        self.active.lock().unwrap().insert(job_id, flag.clone());
        flag
    }

    /// Trả true nếu job tồn tại và đã được gắn cờ hủy.
    pub fn cancel(&self, job_id: i64) -> bool {
        if let Some(flag) = self.active.lock().unwrap().get(&job_id) {
            flag.store(true, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    pub fn unregister(&self, job_id: i64) {
        self.active.lock().unwrap().remove(&job_id);
    }

    pub fn sender(&self) -> Sender<JobEvent> {
        self.tx.clone()
    }

    /// Receiver cho event pump (src-tauri drain → emit UI + ghi jobs table).
    pub fn receiver(&self) -> Receiver<JobEvent> {
        self.rx.clone()
    }
}

/// Helper throttle: chỉ cho qua tối đa 1 lần mỗi `interval`.
pub struct Throttle {
    last: std::time::Instant,
    interval: std::time::Duration,
}

impl Throttle {
    pub fn new(interval_ms: u64) -> Self {
        Self {
            last: std::time::Instant::now() - std::time::Duration::from_secs(3600),
            interval: std::time::Duration::from_millis(interval_ms),
        }
    }

    pub fn ready(&mut self) -> bool {
        if self.last.elapsed() >= self.interval {
            self.last = std::time::Instant::now();
            true
        } else {
            false
        }
    }
}
