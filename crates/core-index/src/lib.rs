//! core-index: quét filesystem → đẩy batch vào db writer.
//!
//! M1.5: KHÔNG đưa drive root ("D:\") thẳng vào jwalk — jwalk fail âm thầm với
//! path dạng trailing-backslash (repro: 0 file/0s trên cả ổ). Thay vào đó tầng
//! đầu tiên tự read_dir (đồng nhất mọi shape path), rồi jwalk từng thư mục con
//! (luôn là dạng "D:\Foo" an toàn). Mọi lỗi walk/ghi đều được ĐẾM — không còn
//! "done 0 files" láo.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use anyhow::{anyhow, bail, Result};
use core_db::{ops, ScanEntry, WriterHandle};

pub type CancelFlag = std::sync::Arc<std::sync::atomic::AtomicBool>;

pub const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "bmp", "webp", "heic", "heif", "hif", "tif", "tiff", "avif",
    "jxl", "dng", "cr2", "cr3", "nef", "arw", "orf", "rw2", "raf", "srw", "pef",
];
pub const VIDEO_EXTS: &[&str] = &[
    "mp4", "mov", "m4v", "avi", "mkv", "wmv", "flv", "webm", "mts", "m2ts", "ts", "3gp", "3g2",
    "mpg", "mpeg", "vob", "divx",
];

/// Thư mục bỏ qua (so sánh không phân biệt hoa thường, theo tên component).
pub const EXCLUDED_DIR_NAMES: &[&str] = &[
    "$recycle.bin",
    "system volume information",
    "$extend",
    "windows",
    "program files",
    "program files (x86)",
    "programdata",
    ".staging",
    ".quarantine",
    "node_modules",
];

const ATTR_REPARSE_POINT: u32 = 0x400;
// Cloud placeholder (OneDrive Files On-Demand...): index được nhưng status=2,
// tuyệt đối không hash/thumb (sẽ kéo hydrate cả cloud).
const ATTR_OFFLINE: u32 = 0x1000;
const ATTR_RECALL_ON_OPEN: u32 = 0x40000;
const ATTR_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
const CLOUD_ATTRS: u32 = ATTR_OFFLINE | ATTR_RECALL_ON_OPEN | ATTR_RECALL_ON_DATA_ACCESS;

const BATCH_SIZE: usize = 5000;
/// Backpressure: tối đa 8 batch (~8MB strings) đang chờ trong writer queue.
const MAX_INFLIGHT_BATCHES: usize = 8;

/// 0 = image, 1 = video, None = không phải media.
pub fn classify_ext(ext: &str) -> Option<i64> {
    let e = ext.to_ascii_lowercase();
    if IMAGE_EXTS.contains(&e.as_str()) {
        Some(0)
    } else if VIDEO_EXTS.contains(&e.as_str()) {
        Some(1)
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ScanSummary {
    pub indexed: u64,
    pub marked_missing: u64,
    pub walk_errors: u64,
    pub skipped_lossy_names: u64,
}

#[derive(Default)]
struct ScanTrack {
    walk_errors: AtomicU64,
    lossy_names: AtomicU64,
    write_errors: AtomicU64,
    first_write_error: Mutex<Option<String>>,
    inflight: Mutex<usize>,
    inflight_cv: Condvar,
}

/// Mutex lock bỏ qua poison — batch trước panic không được phép giết scan.
fn lock_ignore_poison<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn file_attrs(md: &std::fs::Metadata) -> u32 {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        md.file_attributes()
    }
    #[cfg(not(windows))]
    {
        let _ = md;
        0
    }
}

fn unix_ms(t: std::io::Result<std::time::SystemTime>) -> i64 {
    t.ok()
        .and_then(|st| st.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Dir có được scan không: loại theo tên + reparse point (junction/symlink/OneDrive
/// dir). KHÔNG loại theo ATTR_SYSTEM nữa — folder có icon tùy chỉnh cũng mang
/// SYSTEM và từng làm biến mất cả cây.
fn keep_dir(name_lower: &str, attrs: u32) -> bool {
    !EXCLUDED_DIR_NAMES.contains(&name_lower) && attrs & ATTR_REPARSE_POINT == 0
}

/// OsString → ScanEntry nếu là media. Tên không phải Unicode hợp lệ (unpaired
/// surrogate) → bỏ + đếm: đường dẫn không round-trip được thì mọi thao tác
/// move/delete sau này đều nguy hiểm.
fn make_entry(
    dir_path: &str,
    name_os: OsString,
    md: &std::fs::Metadata,
    track: &ScanTrack,
) -> Option<ScanEntry> {
    let name = match name_os.into_string() {
        Ok(s) => s,
        Err(_) => {
            track.lossy_names.fetch_add(1, Ordering::Relaxed);
            return None;
        }
    };
    let ext = name
        .rsplit('.')
        .next()
        .filter(|e| e.len() < name.len())?
        .to_ascii_lowercase();
    let kind = classify_ext(&ext)?;
    let attrs = file_attrs(md);
    let status = if attrs & CLOUD_ATTRS != 0 { 2 } else { 0 };
    Some(ScanEntry {
        dir_path: dir_path.to_string(),
        name,
        ext,
        kind,
        size: md.len() as i64,
        mtime: unix_ms(md.modified()),
        attrs,
        status,
    })
}

#[allow(clippy::too_many_arguments)]
fn flush_batch(
    batch: &mut Vec<ScanEntry>,
    writer: &WriterHandle,
    volume_id: i64,
    gen: i64,
    dir_cache: &Arc<Mutex<HashMap<String, i64>>>,
    track: &Arc<ScanTrack>,
) -> u64 {
    if batch.is_empty() {
        return 0;
    }
    let entries = std::mem::take(batch);
    let n = entries.len() as u64;

    // Backpressure: chờ nếu writer đang ngập
    {
        let mut inflight = lock_ignore_poison(&track.inflight);
        while *inflight >= MAX_INFLIGHT_BATCHES {
            inflight = track
                .inflight_cv
                .wait(inflight)
                .unwrap_or_else(|p| p.into_inner());
        }
        *inflight += 1;
    }

    let cache = dir_cache.clone();
    let track = track.clone();
    writer.exec_async(move |conn| {
        let res = {
            let mut c = lock_ignore_poison(&cache);
            ops::upsert_scan_batch(conn, volume_id, gen, &entries, &mut c)
        };
        if let Err(e) = &res {
            track.write_errors.fetch_add(1, Ordering::Relaxed);
            let mut first = lock_ignore_poison(&track.first_write_error);
            if first.is_none() {
                *first = Some(format!("{e:#}"));
            }
            // Cache có thể chứa dir id đã chết (remove_root đua với scan) —
            // vứt hết, batch sau tự tra lại từ DB.
            lock_ignore_poison(&cache).clear();
        }
        {
            let mut inflight = lock_ignore_poison(&track.inflight);
            *inflight = inflight.saturating_sub(1);
            track.inflight_cv.notify_one();
        }
        res
    });
    n
}

/// Quét `root`, upsert theo batch 5k qua writer, reconcile khi xong (chỉ khi
/// không cancel và không có lỗi GHI). `on_progress(files_indexed)` gọi sau mỗi
/// batch — caller tự throttle.
pub fn scan_root(
    root: &Path,
    volume_id: i64,
    gen: i64,
    writer: &WriterHandle,
    cancel: &CancelFlag,
    mut on_progress: impl FnMut(u64),
) -> Result<ScanSummary> {
    let root_str = ops::normalize_path(&root.to_string_lossy());
    let root = Path::new(&root_str);
    let root_md = std::fs::metadata(root)
        .map_err(|e| anyhow!("ERR_ROOT_UNREADABLE|{root_str}: {e}"))?;
    if !root_md.is_dir() {
        bail!("ERR_ROOT_UNREADABLE|{root_str}: not a directory");
    }

    let track = Arc::new(ScanTrack::default());
    let dir_cache: Arc<Mutex<HashMap<String, i64>>> = Arc::new(Mutex::new(HashMap::new()));
    let mut batch: Vec<ScanEntry> = Vec::with_capacity(BATCH_SIZE);
    let mut indexed: u64 = 0;

    // ---- Tầng đầu: tự read_dir (đồng nhất "D:\" và folder thường) ----
    let read_dir = std::fs::read_dir(root)
        .map_err(|e| anyhow!("ERR_ROOT_UNREADABLE|{root_str}: {e}"))?;
    let mut subdirs: Vec<PathBuf> = Vec::new();
    for dent in read_dir {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let Ok(dent) = dent else {
            track.walk_errors.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        let Ok(md) = dent.metadata() else {
            track.walk_errors.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        if md.is_dir() {
            let name_lower = dent.file_name().to_string_lossy().to_ascii_lowercase();
            if keep_dir(&name_lower, file_attrs(&md)) {
                subdirs.push(dent.path());
            }
        } else if md.is_file() {
            if let Some(e) = make_entry(&root_str, dent.file_name(), &md, &track) {
                batch.push(e);
                if batch.len() >= BATCH_SIZE {
                    indexed += flush_batch(&mut batch, writer, volume_id, gen, &dir_cache, &track);
                    on_progress(indexed);
                }
            }
        }
    }

    // ---- Từng thư mục con: jwalk (path dạng "D:\Foo" — shape an toàn) ----
    for sub in subdirs {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        walk_subtree(
            &sub, writer, volume_id, gen, cancel, &dir_cache, &track, &mut batch, &mut indexed,
            &mut on_progress,
        );
    }

    indexed += flush_batch(&mut batch, writer, volume_id, gen, &dir_cache, &track);
    on_progress(indexed);

    let summary_base = |marked: u64, track: &ScanTrack| ScanSummary {
        indexed,
        marked_missing: marked,
        walk_errors: track.walk_errors.load(Ordering::Relaxed),
        skipped_lossy_names: track.lossy_names.load(Ordering::Relaxed),
    };

    if cancel.load(Ordering::Relaxed) {
        // Không reconcile khi cancel — scan dở dang không được đánh dấu missing.
        return Ok(summary_base(0, &track));
    }

    // Fence: exec() FIFO sau mọi exec_async ⇒ mọi batch đã được xử lý xong.
    writer.exec(|_| Ok(()))?;

    let write_errors = track.write_errors.load(Ordering::Relaxed);
    if write_errors > 0 {
        let first = lock_ignore_poison(&track.first_write_error)
            .clone()
            .unwrap_or_default();
        // Index thiếu dữ liệu → KHÔNG reconcile (sẽ đánh missing oan), job phải FAIL.
        bail!("ERR_SCAN_WRITE_FAILED|{write_errors} batch failed; first: {first}");
    }

    let rs = root_str.clone();
    let marked = writer.exec(move |c| ops::reconcile_scan(c, &rs, gen).map(|n| n as u64))?;
    Ok(summary_base(marked, &track))
}

#[allow(clippy::too_many_arguments)]
fn walk_subtree(
    sub: &Path,
    writer: &WriterHandle,
    volume_id: i64,
    gen: i64,
    cancel: &CancelFlag,
    dir_cache: &Arc<Mutex<HashMap<String, i64>>>,
    track: &Arc<ScanTrack>,
    batch: &mut Vec<ScanEntry>,
    indexed: &mut u64,
    on_progress: &mut impl FnMut(u64),
) {
    let cancel_walk = cancel.clone();
    let track_walk = track.clone();
    let walk = jwalk::WalkDir::new(sub)
        .skip_hidden(false)
        .follow_links(false)
        .process_read_dir(move |_depth, _path, _state, children| {
            if cancel_walk.load(Ordering::Relaxed) {
                children.clear();
                return;
            }
            children.retain(|res| {
                let Ok(entry) = res else {
                    track_walk.walk_errors.fetch_add(1, Ordering::Relaxed);
                    return false;
                };
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                if entry.file_type().is_dir() {
                    if EXCLUDED_DIR_NAMES.contains(&name.as_str()) {
                        return false;
                    }
                    match entry.metadata() {
                        Ok(md) => file_attrs(&md) & ATTR_REPARSE_POINT == 0,
                        Err(_) => {
                            track_walk.walk_errors.fetch_add(1, Ordering::Relaxed);
                            false
                        }
                    }
                } else {
                    match name.rsplit('.').next() {
                        Some(ext) if ext.len() < name.len() => classify_ext(ext).is_some(),
                        _ => false,
                    }
                }
            });
        });

    for item in walk {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let entry = match item {
            Ok(e) => e,
            Err(_) => {
                track.walk_errors.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let md = match entry.metadata() {
            Ok(m) => m,
            Err(_) => {
                track.walk_errors.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        };
        let dir_path = ops::normalize_path(&entry.parent_path().to_string_lossy());
        if let Some(e) = make_entry(&dir_path, entry.file_name().to_os_string(), &md, track) {
            batch.push(e);
            if batch.len() >= BATCH_SIZE {
                *indexed += flush_batch(batch, writer, volume_id, gen, dir_cache, track);
                on_progress(*indexed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression cho bug "scan D:\ ra 0 file": root truyền vào với trailing
    /// backslash phải index được y như không có.
    #[test]
    fn scan_handles_trailing_backslash_root() {
        let tmp = tempfile::tempdir().unwrap();
        let db = core_db::Db::open(tmp.path()).unwrap();
        let data = tmp.path().join("data");
        std::fs::create_dir_all(data.join("sub")).unwrap();
        std::fs::write(data.join("root_photo.jpg"), b"x").unwrap();
        std::fs::write(data.join("sub").join("nested.mp4"), b"y").unwrap();
        std::fs::write(data.join("sub").join("not_media.txt"), b"z").unwrap();

        let with_slash = format!("{}\\", data.display());
        let cancel = CancelFlag::default();
        let summary = scan_root(
            Path::new(&with_slash),
            1,
            10,
            &db.writer,
            &cancel,
            |_| {},
        )
        .unwrap();
        assert_eq!(summary.indexed, 2, "phải thấy cả file ở root lẫn file lồng sâu");
        assert_eq!(summary.walk_errors, 0);

        let ids = db
            .pool
            .with(|c| core_db::query::query_ids(c, &core_db::FileFilter::default()))
            .unwrap();
        assert_eq!(ids.len(), 2);
    }

    /// Root không tồn tại (ổ rời bị rút) phải FAIL, không được "done 0 files".
    #[test]
    fn scan_missing_root_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let db = core_db::Db::open(tmp.path()).unwrap();
        let cancel = CancelFlag::default();
        let err = scan_root(
            Path::new("Q:\\khong-ton-tai-dau"),
            1,
            10,
            &db.writer,
            &cancel,
            |_| {},
        )
        .unwrap_err();
        assert!(err.to_string().contains("ERR_ROOT_UNREADABLE"));
    }
}
