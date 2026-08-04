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

struct JobInfo {
    flag: CancelFlag,
    kind: String,
    root_id: Option<i64>,
}

pub struct JobManager {
    active: Mutex<HashMap<i64, JobInfo>>,
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

    pub fn register(&self, job_id: i64, kind: &str, root_id: Option<i64>) -> CancelFlag {
        let flag: CancelFlag = Arc::new(AtomicBool::new(false));
        self.active.lock().unwrap().insert(
            job_id,
            JobInfo {
                flag: flag.clone(),
                kind: kind.to_string(),
                root_id,
            },
        );
        flag
    }

    /// Đăng ký scan ATOMIC: check scan-đang-chạy + insert trong CÙNG một lần
    /// giữ lock. Check rời rồi mới register (như cũ) thì 2 start_scan song song
    /// cùng root (double-click / StrictMode double-invoke) đều lọt: 2 scan gen
    /// chồng nhau → reconcile của gen sau đánh missing file đang tồn tại.
    pub fn try_register_scan(&self, job_id: i64, root_id: i64) -> Option<CancelFlag> {
        let mut map = self.active.lock().unwrap();
        if map
            .values()
            .any(|i| i.kind == "scan" && i.root_id == Some(root_id))
        {
            return None;
        }
        let flag: CancelFlag = Arc::new(AtomicBool::new(false));
        map.insert(
            job_id,
            JobInfo {
                flag: flag.clone(),
                kind: "scan".to_string(),
                root_id: Some(root_id),
            },
        );
        Some(flag)
    }

    /// Trả true nếu job tồn tại và đã được gắn cờ hủy.
    pub fn cancel(&self, job_id: i64) -> bool {
        if let Some(info) = self.active.lock().unwrap().get(&job_id) {
            info.flag.store(true, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    /// Job id của scan đang chạy trên root này (nếu có) — chặn scan trùng.
    pub fn active_scan_for_root(&self, root_id: i64) -> Option<i64> {
        self.active
            .lock()
            .unwrap()
            .iter()
            .find(|(_, info)| info.kind == "scan" && info.root_id == Some(root_id))
            .map(|(id, _)| *id)
    }

    /// Job id đầu tiên đang chạy theo kind (vd "meta") — chặn job trùng loại.
    pub fn active_job_of_kind(&self, kind: &str) -> Option<i64> {
        self.active
            .lock()
            .unwrap()
            .iter()
            .find(|(_, info)| info.kind == kind)
            .map(|(id, _)| *id)
    }

    /// Gắn cờ hủy mọi scan của root; trả về danh sách job id bị hủy.
    pub fn cancel_scans_for_root(&self, root_id: i64) -> Vec<i64> {
        let map = self.active.lock().unwrap();
        map.iter()
            .filter(|(_, info)| info.kind == "scan" && info.root_id == Some(root_id))
            .map(|(id, info)| {
                info.flag.store(true, Ordering::Relaxed);
                *id
            })
            .collect()
    }

    /// Còn job nào trong danh sách đang active không (để đợi cancel xong).
    pub fn any_active(&self, ids: &[i64]) -> bool {
        let map = self.active.lock().unwrap();
        ids.iter().any(|id| map.contains_key(id))
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
/// Lần gọi đầu tiên luôn ready (không trừ lùi Instant — trừ lùi panic khi
/// uptime máy nhỏ hơn offset, vì Instant trên Windows là QPC tính từ boot).
pub struct Throttle {
    last: Option<std::time::Instant>,
    interval: std::time::Duration,
}

impl Throttle {
    pub fn new(interval_ms: u64) -> Self {
        Self {
            last: None,
            interval: std::time::Duration::from_millis(interval_ms),
        }
    }

    pub fn ready(&mut self) -> bool {
        match self.last {
            Some(t) if t.elapsed() < self.interval => false,
            _ => {
                self.last = Some(std::time::Instant::now());
                true
            }
        }
    }
}
