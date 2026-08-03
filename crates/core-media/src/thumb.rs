use std::path::Path;

use anyhow::{bail, Context, Result};
use fast_image_resize::images::Image;
use fast_image_resize::{FilterType, IntoImageView, ResizeAlg, ResizeOptions, Resizer};
use image::{DynamicImage, ImageReader, RgbaImage};

use crate::ffmpeg;

/// Format crate `image` decode được trực tiếp. Còn lại (heic/avif/jxl/RAW...)
/// đi đường ffmpeg — không có ffmpeg thì Err → protocol trả 404, UI hiện icon.
const IMAGE_CRATE_EXTS: &[&str] = &[
    "jpg", "jpeg", "jfif", "png", "gif", "bmp", "webp", "tif", "tiff",
];

/// Ảnh quá to (pixel bomb / panorama khổng lồ) không decode bằng image crate
/// trong process — đẩy sang ffmpeg (process riêng, chết cũng không sao).
/// 80MP RGBA ≈ 320MB/decode; pool tối đa 6 thread → trần RAM tạm ~2GB.
const MAX_IN_PROCESS_PIXELS: u64 = 80_000_000;

/// Sinh thumbnail WebP q75 lọt hộp `target`×`target` (không upscale).
/// EXIF orientation được áp SAU resize (rẻ hơn 50 lần so với xoay full-res).
pub fn make_thumb(
    path: &Path,
    ext: &str,
    target: u32,
    ffmpeg_bin: Option<&Path>,
) -> Result<Vec<u8>> {
    let use_image_crate = IMAGE_CRATE_EXTS.contains(&ext) && !is_oversized(path);

    let (img, orientation) = if use_image_crate {
        let reader = ImageReader::open(path)
            .with_context(|| format!("open {}", path.display()))?
            .with_guessed_format()?;
        let img = reader
            .decode()
            .with_context(|| format!("decode {}", path.display()))?;
        // Orientation đọc từ EXIF gốc — image crate không tự xoay
        let orientation = crate::extract_image_meta(path).orientation.unwrap_or(1);
        (img, orientation)
    } else {
        let Some(ff) = ffmpeg_bin else {
            bail!("no decoder for .{ext} (ffmpeg not found)");
        };
        // ffmpeg tự áp rotation metadata (HEIC irot, EXIF) từ bản 6+
        (ffmpeg::decode_scaled(ff, path, target, None)?, 1)
    };

    let img = resize_to_fit(&img, target)?;
    let img = apply_orientation(img, orientation);
    encode_webp(&img)
}

/// Thumb keyframe cho video: seek ~10% thời lượng (kẹp 1..=30s — tránh frame
/// đen đầu clip và tránh seek quá xa với video dài), decode + scale trong
/// ffmpeg, encode WebP. ffmpeg tự áp rotation metadata.
pub fn make_video_thumb(
    ffmpeg_bin: &Path,
    path: &Path,
    target: u32,
    duration_ms: Option<i64>,
) -> Result<Vec<u8>> {
    let ss = duration_ms
        .filter(|&d| d > 0)
        .map(|d| (d as f64 / 1000.0 * 0.1).clamp(1.0, 30.0))
        .unwrap_or(1.0);
    let img = match ffmpeg::decode_scaled(ffmpeg_bin, path, target, Some(ss)) {
        Ok(img) => img,
        // Clip ngắn hơn điểm seek → thử lại từ frame đầu
        Err(_) => ffmpeg::decode_scaled(ffmpeg_bin, path, target, None)?,
    };
    let img = resize_to_fit(&img, target)?;
    encode_webp(&img)
}

fn encode_webp(img: &DynamicImage) -> Result<Vec<u8>> {
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    let mem = webp::Encoder::from_rgba(rgba.as_raw(), w, h).encode(75.0);
    Ok(mem.to_vec())
}

fn is_oversized(path: &Path) -> bool {
    match imagesize::size(path) {
        Ok(d) => (d.width as u64) * (d.height as u64) > MAX_IN_PROCESS_PIXELS,
        Err(_) => false, // để decoder tự báo lỗi cụ thể
    }
}

/// Downscale lọt hộp target×target bằng fast_image_resize (SIMD, Lanczos3).
/// Ảnh đã nhỏ hơn target → giữ nguyên.
fn resize_to_fit(img: &DynamicImage, target: u32) -> Result<DynamicImage> {
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        bail!("zero-dimension image");
    }
    if w.max(h) <= target {
        return Ok(DynamicImage::ImageRgba8(img.to_rgba8()));
    }
    let scale = target as f64 / w.max(h) as f64;
    let dst_w = ((w as f64 * scale).round() as u32).max(1);
    let dst_h = ((h as f64 * scale).round() as u32).max(1);

    // Ép về RGBA8 trước — pixel type ổn định, fir tự premultiply alpha khi resize
    let src = DynamicImage::ImageRgba8(img.to_rgba8());
    let mut dst = Image::new(dst_w, dst_h, src.pixel_type().unwrap());
    Resizer::new().resize(
        &src,
        &mut dst,
        &ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Lanczos3)),
    )?;
    let out =
        RgbaImage::from_raw(dst_w, dst_h, dst.into_vec()).context("resize buffer size mismatch")?;
    Ok(DynamicImage::ImageRgba8(out))
}

fn apply_orientation(img: DynamicImage, o: u16) -> DynamicImage {
    match o {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate270().fliph(),
        8 => img.rotate270(),
        _ => img,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_png(w: u32, h: u32) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("tidymedia-test-thumb");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(format!("t_{w}x{h}_{}.png", std::process::id()));
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        });
        img.save(&p).unwrap();
        p
    }

    #[test]
    fn thumb_downscales_and_encodes_webp() {
        let p = temp_png(640, 480);
        let bytes = make_thumb(&p, "png", 256, None).unwrap();
        assert!(!bytes.is_empty());
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!(decoded.width(), 256);
        assert_eq!(decoded.height(), 192);
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn thumb_no_upscale_small_image() {
        let p = temp_png(100, 80);
        let bytes = make_thumb(&p, "png", 256, None).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (100, 80));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn thumb_unsupported_ext_without_ffmpeg_errors() {
        let p = temp_png(64, 64);
        // ext "heic" ép đường ffmpeg; ffmpeg=None → phải Err chứ không panic
        assert!(make_thumb(&p, "heic", 256, None).is_err());
        std::fs::remove_file(p).ok();
    }
}
