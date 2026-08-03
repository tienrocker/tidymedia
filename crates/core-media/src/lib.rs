//! core-media: metadata (EXIF/dimensions), thumbnail pipeline, HEIC qua ffmpeg,
//! thumbs.db (LRU cache). Zero Tauri dependency — test headless được.
//!
//! Quy ước `taken_at`: EXIF không có timezone → lưu "giờ đồng hồ máy ảnh" encode
//! như epoch ms ở khung UTC. Hiển thị/organize đọc lại với tz=0 là ra đúng giờ
//! camera ghi — không bao giờ convert 2 lần.

mod ffmpeg;
mod meta;
mod store;
mod thumb;

pub use ffmpeg::find_ffmpeg;
pub use meta::{extract_image_meta, ImageMeta};
pub use store::ThumbStore;
pub use thumb::make_thumb;

/// Bucket size hợp lệ cho thumb — mọi request bị ép về 1 trong 2 (cache không nổ
/// theo từng pixel size UI yêu cầu). 256 = grid, 1600 = lightbox preview.
pub fn clamp_thumb_size(s: u32) -> u32 {
    if s <= 256 {
        256
    } else {
        1600
    }
}
