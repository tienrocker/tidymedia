//! Perceptual hash (dHash 256-bit) — bắt "cùng một tấm ảnh nhưng khác byte":
//! xuất lại qua app đồng bộ, nén lại, thu nhỏ. Dedup tuyệt đối (BLAKE3) không
//! bao giờ thấy những cặp này vì chỉ lệch 1 byte EXIF là hash đã khác.
//!
//! dHash thay vì pHash-DCT: ca cần bắt là cùng ảnh gốc qua vài lần nén/scale
//! nên độ bền của dHash là quá đủ, mà rẻ hơn hẳn — quan trọng vì input là
//! thumb 256px ĐÃ CACHE, tính được cả kho trong vài phút thay vì decode lại
//! hàng trăm GB từ ổ cứng.
//!
//! # Vì sao 256 bit chứ không phải 64
//!
//! Với ảnh CÓ EXIF thì chốt giờ bấm máy trong `cluster_similar` làm gần hết
//! việc. Nhưng **2.081 ảnh trong kho thật không có EXIF** (bị app nhắn tin lột
//! sạch) — với chúng hash là chốt chặn DUY NHẤT, mà ở bản 64-bit biên chỉ còn
//! 3 bit: ca `5.jpg` cách ảnh khác gần nhất đúng 9 bit trên ngưỡng 6.
//!
//! Lưới 17x16 KHÔNG làm loạt ảnh chụp liên tiếp tách ra — đo rồi: chúng vẫn
//! cách nhau 0-8 bit, vì vật thể di chuyển vẫn chỉ chiếm vài ô kể cả ở 16x16.
//! Đừng kỳ vọng hash mịn hơn thay được chốt giờ bấm máy. Cái nó làm được là
//! đẩy toàn bộ đường đánh đổi lên: ở CÙNG tỉ lệ bỏ sót, tỉ lệ gom nhầm giảm
//! khoảng một nửa. Xem bảng số trong [`MAX_DIST`].

use anyhow::{bail, Result};
use image::imageops::FilterType;
use image::DynamicImage;

/// Lưới so sánh: 17 cột (16 cặp ngang) x 16 hàng = 256 bit.
const GRID_W: u32 = 17;
const GRID_H: u32 = 16;

/// Số u64 chứa hash. Đổi số này là đổi luôn số dòng `seq` trong bảng `phashes`.
pub const WORDS: usize = 4;

/// Ảnh gần như phẳng (frame đen, ảnh chụp màn hình nền trơn) cho ra hash toàn
/// 0 và sẽ "giống" mọi ảnh phẳng khác. Không có tương phản tối thiểu này thì
/// KHÔNG lưu hash — thà bỏ sót còn hơn dựng một nhóm rác khổng lồ.
const MIN_CONTRAST: u8 = 12;

/// Ngưỡng Hamming để coi là cùng một tấm ảnh.
///
/// ĐO trên kho thật 19.472 ảnh, dùng chuẩn ĐỘC LẬP VỚI HASH: hai file mang
/// đúng một mốc EXIF tới mili-giây thì chắc chắn ra từ cùng một lần bấm máy.
///
/// - **cùng lần bấm máy** (9.456 cặp trong 3.001 bộ): p50 = 4, p90 = 8,
///   p99 = 20 bit;
/// - **lần bấm máy khác**, khoảng cách tới hàng xóm gần nhất (2.782 mẫu):
///   p1 = 4, p5 = 10, p50 = 69 bit.
///
/// Hai phân bố CHỒNG NHAU ở đuôi thấp — loạt chụp liên tiếp cùng cảnh cách
/// nhau 0-4 bit dù là ảnh khác nhau. Không ngưỡng nào tách được chúng, kể cả
/// hash mịn hơn nữa; đó là việc của chốt chặn giờ bấm máy trong
/// `cluster_similar`, không phải của hằng số này.
///
/// Chọn 16 (6,25% số bit) từ bảng đánh đổi đo được — so ở CÙNG tỉ lệ bỏ sót
/// thì bản 256-bit gom nhầm ít hơn bản 64-bit cũ khoảng 2 lần:
///
/// | bỏ sót cặp cùng ảnh | 64-bit cũ | 256-bit |
/// |---|---|---|
/// | ~3,8 % | ngưỡng 2 → gom nhầm 15,5 % | ngưỡng 11 → **6,3 %** |
/// | ~1,4 % | ngưỡng 3 → gom nhầm 20,6 % | ngưỡng 16 → **11,4 %** |
/// | ~0,4 % | ngưỡng 6 → gom nhầm 33,4 % | ngưỡng >25 → ~22 % |
///
/// Ngưỡng 6/64 đang chạy nằm ở ô cuối: bỏ sót 0,44 % nhưng gom nhầm 33,4 %.
/// Đổi sang 16/256 là bỏ sót 1,42 % để gom nhầm còn 11,4 % — đúng thứ tự ưu
/// tiên của cả dự án (thà bỏ sót còn hơn gom nhầm).
///
/// Lưu ý đọc số: cột "gom nhầm" đo trên các file ĐỀU CÓ mốc thời gian, mà
/// trong production chốt giờ bấm máy đã chặn hết chúng. Việc thật của ngưỡng
/// này là 2.081 ảnh KHÔNG có EXIF (bị app nhắn tin lột sạch) — với chúng hash
/// là chốt duy nhất. Các bản đã biết chắc thuộc loại đó (`photo_2018-12-02_*`
/// so với `IMG_2366`/`IMG_2367`) nằm ở 9 bit, tức còn 7 bit dư.
pub const MAX_DIST: u32 = 16;

/// dHash: thu về 17x16 xám rồi so từng cặp pixel ngang → 256 bit.
/// `None` = ảnh phẳng, không đủ tương phản để so sánh có nghĩa.
pub fn dhash(img: &DynamicImage) -> Option<[u64; WORDS]> {
    let small = img
        .resize_exact(GRID_W, GRID_H, FilterType::Triangle)
        .to_luma8();
    let (mut lo, mut hi) = (u8::MAX, u8::MIN);
    for p in small.pixels() {
        lo = lo.min(p[0]);
        hi = hi.max(p[0]);
    }
    if hi.saturating_sub(lo) < MIN_CONTRAST {
        return None;
    }
    let mut bits = [0u64; WORDS];
    let mut idx = 0usize;
    for y in 0..GRID_H {
        for x in 0..(GRID_W - 1) {
            let left = small.get_pixel(x, y)[0];
            let right = small.get_pixel(x + 1, y)[0];
            bits[idx / 64] |= u64::from(left > right) << (63 - idx % 64);
            idx += 1;
        }
    }
    Some(bits)
}

/// dHash từ bytes ảnh đã encode (thumb WebP trong thumbs.db, hoặc file gốc).
pub fn dhash_bytes(data: &[u8]) -> Result<Option<[u64; WORDS]>> {
    if data.is_empty() {
        bail!("empty image data");
    }
    let img = image::load_from_memory(data)?;
    Ok(dhash(&img))
}

/// Số bit khác nhau trên toàn bộ 256 bit.
pub fn hamming(a: &[u64; WORDS], b: &[u64; WORDS]) -> u32 {
    let mut d = 0;
    for i in 0..WORDS {
        d += (a[i] ^ b[i]).count_ones();
    }
    d
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
            hamming(&hash, &dhash(&resized).unwrap()) <= MAX_DIST,
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
        assert!(hamming(&hash, &dhash_bytes(&jpeg).unwrap().unwrap()) <= MAX_DIST);
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
            hamming(&a, &b) > MAX_DIST,
            "anh khac han phai xa nhau, khong duoc gom nham"
        );
    }

    /// Cùng bối cảnh, một vật thể đổi chỗ. Đây là ca mà lưới mịn CÓ tách được
    /// (vật thể rộng 10% khung, dịch 15% khung). Đừng đọc nhầm thành "hash mịn
    /// tách được mọi loạt chụp liên tiếp": đo trên kho thật, burst thường dịch
    /// ít hơn nhiều và vẫn chỉ cách nhau 0-8 bit — việc đó thuộc về chốt giờ
    /// bấm máy.
    #[test]
    fn small_moving_subject_is_not_a_duplicate() {
        let bg = |cx: u32, cy: u32| -> DynamicImage {
            let mut img = RgbImage::new(320, 320);
            for (x, y, p) in img.enumerate_pixels_mut() {
                let v = ((x * 200 / 320) as u8).wrapping_add((y * 55 / 320) as u8);
                *p = Rgb([v, v, v]);
            }
            // "người" cao 1/4 khung, rộng 1/10 — đổi tư thế = dịch ngang
            for y in (320 / 2)..(320 * 3 / 4) {
                for x in cx..(cx + 32) {
                    img.put_pixel(x, cy + y - 320 / 2, Rgb([250, 250, 250]));
                }
            }
            DynamicImage::ImageRgb8(img)
        };
        let a = dhash(&bg(100, 0)).unwrap();
        let b = dhash(&bg(150, 0)).unwrap();
        assert!(
            hamming(&a, &b) > MAX_DIST,
            "vat the nho doi cho phai tach duoc - day la bug cua ban 64-bit"
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
