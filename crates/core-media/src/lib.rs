//! core-media: metadata (EXIF/dimensions), thumbnail pipeline, HEIC qua ffmpeg,
//! thumbs.db (LRU cache). Zero Tauri dependency — test headless được.
//!
//! Quy ước `taken_at`: EXIF không có timezone → lưu "giờ đồng hồ máy ảnh" encode
//! như epoch ms ở khung UTC. Hiển thị/organize đọc lại với tz=0 là ra đúng giờ
//! camera ghi — không bao giờ convert 2 lần.

mod ffmpeg;
mod ffprobe;
mod meta;
pub mod phash;
mod store;
mod thumb;

pub use ffmpeg::{find_ffmpeg, find_ffprobe};
pub use ffprobe::{probe_video, probe_video_in_timezone, VideoMeta};
pub use meta::{extract_image_meta, ImageMeta};
pub use phash::{dhash, dhash_bytes, hamming};
pub use store::ThumbStore;
pub use thumb::{make_thumb, make_video_thumb};

/// Bucket size hợp lệ cho thumb — mọi request bị ép về 1 trong 2 (cache không nổ
/// theo từng pixel size UI yêu cầu). 256 = grid, 1600 = lightbox preview.
pub fn clamp_thumb_size(s: u32) -> u32 {
    if s <= 256 {
        256
    } else {
        1600
    }
}

/// FILE_ATTRIBUTE_OFFLINE (0x1000) | RECALL_ON_OPEN (0x40000) |
/// RECALL_ON_DATA_ACCESS (0x400000) — invariant dự án: KHÔNG BAO GIỜ đọc nội
/// dung cloud placeholder (đọc là kéo hydrate cả OneDrive/iCloud). Check này
/// phải gọi TẠI CHỖ ngay trước khi mở file — status trong DB có thể stale
/// (file bị dehydrate sau lần scan cuối).
pub fn is_cloud_placeholder(md: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        md.file_attributes() & (0x1000 | 0x0004_0000 | 0x0040_0000) != 0
    }
    #[cfg(not(windows))]
    {
        let _ = md;
        false
    }
}
