//! Perceptual hash (dHash 64-bit) — bắt "cùng một tấm ảnh nhưng khác byte":
//! xuất lại qua app đồng bộ, nén lại, thu nhỏ. Dedup tuyệt đối (BLAKE3) không
//! bao giờ thấy những cặp này vì chỉ lệch 1 byte EXIF là hash đã khác.
//!
//! dHash thay vì pHash-DCT: ca cần bắt là cùng ảnh gốc qua vài lần nén/scale
//! nên độ bền của dHash là quá đủ, mà rẻ hơn hẳn — quan trọng vì input là
//! thumb 256px ĐÃ CACHE, tính được cả kho trong vài phút thay vì decode lại
//! hàng trăm GB từ ổ cứng.

use anyhow::{bail, Result};
use image::imageops::FilterType;
use image::DynamicImage;

/// Ảnh gần như phẳng (frame đen, ảnh chụp màn hình nền trơn) cho ra hash toàn
/// 0 và sẽ "giống" mọi ảnh phẳng khác. Không có tương phản tối thiểu này thì
/// KHÔNG lưu hash — thà bỏ sót còn hơn dựng một nhóm rác khổng lồ.
const MIN_CONTRAST: u8 = 12;

/// dHash: thu về 9x8 xám rồi so từng cặp pixel ngang → 64 bit.
/// `None` = ảnh phẳng, không đủ tương phản để so sánh có nghĩa.
pub fn dhash(img: &DynamicImage) -> Option<u64> {
    let small = img.resize_exact(9, 8, FilterType::Triangle).to_luma8();
    let (mut lo, mut hi) = (u8::MAX, u8::MIN);
    for p in small.pixels() {
        lo = lo.min(p[0]);
        hi = hi.max(p[0]);
    }
    if hi.saturating_sub(lo) < MIN_CONTRAST {
        return None;
    }
    let mut bits = 0u64;
    for y in 0..8u32 {
        for x in 0..8u32 {
            let left = small.get_pixel(x, y)[0];
            let right = small.get_pixel(x + 1, y)[0];
            bits = (bits << 1) | u64::from(left > right);
        }
    }
    Some(bits)
}

/// dHash từ bytes ảnh đã encode (thumb WebP trong thumbs.db, hoặc file gốc).
pub fn dhash_bytes(data: &[u8]) -> Result<Option<u64>> {
    if data.is_empty() {
        bail!("empty image data");
    }
    let img = image::load_from_memory(data)?;
    Ok(dhash(&img))
}

/// Số bit khác nhau. 0 = hash y hệt; cùng một ảnh qua vài lần nén thường 0-4.
pub fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    /// Ảnh gradient + vài khối — đủ tương phản, hash ổn định.
    fn sample(w: u32, h: u32) -> DynamicImage {
        let mut img = RgbImage::new(w, h);
        for (x, y, p) in img.enumerate_pixels_mut() {
            let v = ((x * 255 / w.max(1)) as u8).wrapping_add((y * 91 / h.max(1)) as u8);
            *p = Rgb([v, v.wrapping_mul(3), 255 - v]);
        }
        DynamicImage::ImageRgb8(img)
    }

    #[test]
    fn same_photo_survives_resize_and_recompression() {
        let original = sample(600, 400);
        let hash = dhash(&original).expect("anh co tuong phan");

        // Thu nhỏ mạnh: đúng ca anh\2019 bi resize 3024 -> 1772
        let resized = original.resize_exact(150, 100, FilterType::Lanczos3);
        assert!(
            hamming(hash, dhash(&resized).unwrap()) <= 6,
            "thu nho khong duoc lam doi hash"
        );

        // Nén JPEG chất lượng thấp: đúng ca iphone 2.0MB -> 0.79MB
        let mut jpeg = Vec::new();
        original
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        assert!(hamming(hash, dhash_bytes(&jpeg).unwrap().unwrap()) <= 6);
    }

    #[test]
    fn different_photos_are_far_apart() {
        let a = dhash(&sample(400, 400)).unwrap();
        let mut img = RgbImage::new(400, 400);
        for (x, y, p) in img.enumerate_pixels_mut() {
            // Hoa văn caro — khác hẳn gradient
            let v = if (x / 40 + y / 40) % 2 == 0 {
                20u8
            } else {
                230
            };
            *p = Rgb([v, v, v]);
        }
        let b = dhash(&DynamicImage::ImageRgb8(img)).unwrap();
        assert!(
            hamming(a, b) > 12,
            "anh khac han phai xa nhau, khong duoc gom nham"
        );
    }

    #[test]
    fn flat_image_has_no_hash() {
        let flat = DynamicImage::ImageRgb8(RgbImage::from_pixel(300, 300, Rgb([17, 17, 17])));
        assert!(
            dhash(&flat).is_none(),
            "anh phang phai bi bo qua, khong duoc gom thanh 1 nhom rac"
        );
    }
}
