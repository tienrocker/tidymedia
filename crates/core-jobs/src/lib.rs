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
    /// Nhóm trùng vừa được dựng lại GIỮA CHỪNG một job hash — UI nạp lại danh
    /// sách nhóm ngay thay vì đợi job xong (quét 34k file trên HDD là hàng giờ).
    /// Không gắn với job row nào: pump chỉ emit sự kiện, không ghi bảng jobs.
    DupGroupsChanged {
        groups: i64,
        waste: i64,
    },
}

/// Cờ hủy — job thread poll cờ này giữa các batch.
pub type CancelFlag = Arc<AtomicBool>;
/// Cờ tạm dừng — job ngủ tại chỗ, KHÔNG mất phần đã làm và vẫn nằm trong panel.
pub type PauseFlag = Arc<AtomicBool>;

/// Job được phép tạm dừng: chỉ job nền đọc-là-chính. Job đụng file thật
/// (organize/org_undo/dedup_delete/recovery) cố tình KHÔNG pausable — chúng ôm
/// fs_lock/delete_lock, dừng giữa chừng là chặn mọi thứ khác vô thời hạn mà
/// nhìn như app treo.
pub const PAUSABLE_KINDS: &[&str] = &["hash", "meta", "org_hash", "thumb_warm"];

pub fn is_pausable(kind: &str) -> bool {
    PAUSABLE_KINDS.contains(&kind)
}

/// Ngủ trong lúc bị pause. Trả `false` nghĩa là job phải dừng hẳn (đã bị hủy) —
/// caller thoát vòng lặp như gặp cancel. Poll cả 2 cờ nên bấm ✕ lúc đang pause
/// vẫn thoát trong ≤1 nhịp, không bao giờ kẹt luồng.
pub fn wait_while_paused(pause: &PauseFlag, cancel: &CancelFlag) -> bool {
    while pause.load(Ordering::Relaxed) {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    !cancel.load(Ordering::Relaxed)
}

struct JobInfo {
    flag: CancelFlag,
    pause: PauseFlag,
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
        self.register_pausable(job_id, kind, root_id).0
    }

    /// Như `register` nhưng trả thêm cờ pause cho job nền tự poll.
    pub fn register_pausable(
        &self,
        job_id: i64,
        kind: &str,
        root_id: Option<i64>,
    ) -> (CancelFlag, PauseFlag) {
        let flag: CancelFlag = Arc::new(AtomicBool::new(false));
        let pause: PauseFlag = Arc::new(AtomicBool::new(false));
        self.active.lock().unwrap().insert(
            job_id,
            JobInfo {
                flag: flag.clone(),
                pause: pause.clone(),
                kind: kind.to_string(),
                root_id,
            },
        );
        (flag, pause)
    }

    /// Bật/tắt pause. Trả false khi job không tồn tại hoặc kind không được phép
    /// tạm dừng — caller chỉ cần bỏ qua, không phải lỗi.
    pub fn set_paused(&self, job_id: i64, paused: bool) -> bool {
        match self.active.lock().unwrap().get(&job_id) {
            Some(info) if is_pausable(&info.kind) => {
                info.pause.store(paused, Ordering::Relaxed);
                true
            }
            _ => false,
        }
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
                pause: Arc::new(AtomicBool::new(false)),
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

    /// Snapshot jobs owned by this process. Used when frontend listeners attach
    /// after an early startup job (notably crash recovery) has already registered.
    pub fn active_jobs(&self) -> Vec<JobProgress> {
        let map = self.active.lock().unwrap();
        let mut rows: Vec<_> = map
            .iter()
            .map(|(job_id, info)| JobProgress {
                job_id: *job_id,
                kind: info.kind.clone(),
                done: 0,
                total: None,
                message: None,
            })
            .collect();
        rows.sort_by_key(|row| row.job_id);
        rows
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_snapshot_contains_only_jobs_owned_by_this_manager() {
        let jobs = JobManager::new();
        jobs.register(9, "recovery", None);
        jobs.register(3, "scan", Some(1));
        let snapshot = jobs.active_jobs();
        assert_eq!(
            snapshot.iter().map(|j| j.job_id).collect::<Vec<_>>(),
            [3, 9]
        );
        assert_eq!(snapshot[1].kind, "recovery");
        jobs.unregister(9);
        assert_eq!(jobs.active_jobs().len(), 1);
    }

    #[test]
    fn pause_only_applies_to_background_kinds() {
        let jobs = JobManager::new();
        jobs.register(1, "hash", None);
        jobs.register(2, "dedup_delete", None);
        assert!(jobs.set_paused(1, true), "job nen phai pause duoc");
        assert!(
            !jobs.set_paused(2, true),
            "job dung file that khong duoc pause (om lock)"
        );
        assert!(!jobs.set_paused(99, true), "job khong ton tai");
    }

    #[test]
    fn wait_while_paused_returns_immediately_when_idle() {
        let pause: PauseFlag = Arc::new(AtomicBool::new(false));
        let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
        assert!(wait_while_paused(&pause, &cancel));
        cancel.store(true, Ordering::Relaxed);
        assert!(!wait_while_paused(&pause, &cancel), "da huy -> dung han");
    }

    #[test]
    fn cancel_during_pause_unblocks_the_job() {
        let pause: PauseFlag = Arc::new(AtomicBool::new(true));
        let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
        let (p2, c2) = (pause.clone(), cancel.clone());
        let waiter = std::thread::spawn(move || wait_while_paused(&p2, &c2));
        std::thread::sleep(std::time::Duration::from_millis(50));
        cancel.store(true, Ordering::Relaxed);
        assert!(
            !waiter.join().unwrap(),
            "bam huy luc dang pause phai thoat, khong ket luong"
        );
    }
}
