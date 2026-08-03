//! core-index: quét filesystem → đẩy batch vào db writer.
//!
//! M1: jwalk parallel walk (không cần admin). M8 sẽ thêm MFT/USN qua elevated helper.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use core_db::{ops, ScanEntry, WriterHandle};
use core_jobs_types::CancelFlag;

// Re-export kiểu cancel flag mà không kéo cả core-jobs vào dep graph.
mod core_jobs_types {
    pub type CancelFlag = std::sync::Arc<std::sync::atomic::AtomicBool>;
}

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

const ATTR_SYSTEM: u32 = 0x4;
const ATTR_REPARSE_POINT: u32 = 0x400;
const BATCH_SIZE: usize = 5000;

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

/// Quét `root`, upsert theo batch 5k qua writer, reconcile khi xong.
/// `on_progress(files_indexed)` được gọi sau mỗi batch — caller tự throttle.
pub fn scan_root(
    root: &Path,
    volume_id: i64,
    gen: i64,
    writer: &WriterHandle,
    cancel: &CancelFlag,
    mut on_progress: impl FnMut(u64),
) -> Result<ScanSummary> {
    let root_str = ops::normalize_path(&root.to_string_lossy());
    let dir_cache: Arc<Mutex<HashMap<String, i64>>> = Arc::new(Mutex::new(HashMap::new()));
    let cancel_walk = cancel.clone();

    let walk = jwalk::WalkDir::new(root)
        .skip_hidden(false)
        .follow_links(false)
        .process_read_dir(move |_depth, _path, _state, children| {
            if cancel_walk.load(Ordering::Relaxed) {
                children.clear();
                return;
            }
            children.retain(|res| {
                let Ok(entry) = res else { return false };
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                if entry.file_type().is_dir() {
                    if EXCLUDED_DIR_NAMES.contains(&name.as_str()) {
                        return false;
                    }
                    // Không recurse vào reparse point (junction/symlink/OneDrive)
                    // và thư mục system-attr.
                    if let Ok(md) = entry.metadata() {
                        let attrs = file_attrs(&md);
                        if attrs & (ATTR_REPARSE_POINT | ATTR_SYSTEM) != 0 {
                            return false;
                        }
                    }
                    true
                } else {
                    match name.rsplit('.').next() {
                        Some(ext) if ext.len() < name.len() => classify_ext(ext).is_some(),
                        _ => false,
                    }
                }
            });
        });

    let mut batch: Vec<ScanEntry> = Vec::with_capacity(BATCH_SIZE);
    let mut indexed: u64 = 0;

    let flush = |batch: &mut Vec<ScanEntry>| {
        if batch.is_empty() {
            return;
        }
        let entries = std::mem::take(batch);
        let cache = dir_cache.clone();
        writer.exec_async(move |conn| {
            let mut cache = cache.lock().unwrap();
            ops::upsert_scan_batch(conn, volume_id, gen, &entries, &mut cache)
        });
    };

    for item in walk {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let Ok(entry) = item else { continue };
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(ext) = name
            .rsplit('.')
            .next()
            .filter(|e| e.len() < name.len())
            .map(str::to_ascii_lowercase)
        else {
            continue;
        };
        let Some(kind) = classify_ext(&ext) else {
            continue;
        };
        let Ok(md) = entry.metadata() else { continue };
        let dir_path = ops::normalize_path(&entry.parent_path().to_string_lossy());
        batch.push(ScanEntry {
            dir_path,
            name,
            ext,
            kind,
            size: md.len() as i64,
            mtime: unix_ms(md.modified()),
            attrs: file_attrs(&md),
        });
        if batch.len() >= BATCH_SIZE {
            indexed += batch.len() as u64;
            flush(&mut batch);
            on_progress(indexed);
        }
    }
    indexed += batch.len() as u64;
    flush(&mut batch);
    on_progress(indexed);

    if cancel.load(Ordering::Relaxed) {
        // Không reconcile khi cancel — scan dở dang không được phép đánh dấu missing.
        return Ok(ScanSummary {
            indexed,
            marked_missing: 0,
        });
    }

    // exec() block đến khi writer xử lý xong message này ⇒ mọi batch trước đã ghi.
    let marked =
        writer.exec(move |conn| ops::reconcile_scan(conn, &root_str, gen).map(|n| n as u64))?;

    Ok(ScanSummary {
        indexed,
        marked_missing: marked,
    })
}
