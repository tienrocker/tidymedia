use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use exif::{In, Tag, Value};

/// Metadata ảnh trích không cần decode pixel (header-only — imagesize + EXIF).
#[derive(Debug, Default, Clone)]
pub struct ImageMeta {
    /// Dimensions HIỂN THỊ (đã hoán đổi nếu orientation 5-8 xoay 90°).
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Giờ đồng hồ camera encode như epoch ms khung UTC (xem lib.rs).
    pub taken_at: Option<i64>,
    /// 0 = EXIF DateTimeOriginal/Digitized. Các tầng khác (QuickTime, tên file,
    /// mtime) vào ở M3/M5.
    pub date_source: Option<i64>,
    pub camera: Option<String>,
    pub orientation: Option<u16>,
    /// false = không đọc nổi dimensions (file hỏng/format lạ) → meta_state=2.
    pub ok: bool,
}

/// Trích meta cho 1 ảnh. Không bao giờ Err — file hỏng trả `ok=false` để job
/// ghi meta_state=2 và không chọn lại file đó nữa.
pub fn extract_image_meta(path: &Path) -> ImageMeta {
    let mut m = ImageMeta::default();

    match imagesize::size(path) {
        Ok(d) => {
            m.width = Some(d.width as u32);
            m.height = Some(d.height as u32);
            m.ok = true;
        }
        Err(e) => {
            tracing::debug!(path = %path.display(), "imagesize failed: {e}");
        }
    }

    if let Ok(file) = File::open(path) {
        if let Ok(exif) = exif::Reader::new().read_from_container(&mut BufReader::new(file)) {
            m.orientation = exif
                .get_field(Tag::Orientation, In::PRIMARY)
                .and_then(|f| f.value.get_uint(0))
                .and_then(|v| u16::try_from(v).ok())
                .filter(|v| (1..=8).contains(v));

            let dt = exif
                .get_field(Tag::DateTimeOriginal, In::PRIMARY)
                .or_else(|| exif.get_field(Tag::DateTimeDigitized, In::PRIMARY));
            let subsec = exif
                .get_field(Tag::SubSecTimeOriginal, In::PRIMARY)
                .and_then(|f| ascii_string(&f.value));
            if let Some(s) = dt.and_then(|f| ascii_string(&f.value)) {
                if let Some(ms) = parse_exif_datetime(&s, subsec.as_deref()) {
                    m.taken_at = Some(ms);
                    m.date_source = Some(0);
                }
            }

            let make = exif
                .get_field(Tag::Make, In::PRIMARY)
                .and_then(|f| ascii_string(&f.value));
            let model = exif
                .get_field(Tag::Model, In::PRIMARY)
                .and_then(|f| ascii_string(&f.value));
            m.camera = match (make, model) {
                (Some(mk), Some(md)) => {
                    // "Canon" + "Canon EOS 5D" → đừng lặp hãng 2 lần
                    if md.to_lowercase().starts_with(&mk.to_lowercase()) {
                        Some(md)
                    } else {
                        Some(format!("{mk} {md}"))
                    }
                }
                (mk, md) => mk.or(md),
            };
        }
    }

    // Orientation 5-8 = xoay 90°/270° → dimensions hiển thị hoán đổi
    if matches!(m.orientation, Some(5..=8)) {
        std::mem::swap(&mut m.width, &mut m.height);
    }
    m
}

fn ascii_string(v: &Value) -> Option<String> {
    match v {
        Value::Ascii(vecs) if !vecs.is_empty() => {
            let s = String::from_utf8_lossy(&vecs[0]);
            let s = s.trim().trim_matches('\0').trim();
            if s.is_empty() {
                None
            } else {
                Some(s.to_string())
            }
        }
        _ => None,
    }
}

/// "YYYY:MM:DD HH:MM:SS" (+SubSec "1"→100ms) → epoch ms coi wall-clock là UTC.
/// Sanity range 1990..=2100 và không quá hôm-nay+1 ngày (camera reset pin ghi
/// 1970/2099 → bỏ, để tầng date thấp hơn xử lý ở M5).
pub(crate) fn parse_exif_datetime(s: &str, subsec: Option<&str>) -> Option<i64> {
    let dt = exif::DateTime::from_ascii(s.as_bytes()).ok()?;
    if !(1990..=2100).contains(&dt.year) {
        return None;
    }
    if dt.month == 0 || dt.month > 12 || dt.day == 0 || dt.day > 31 {
        return None;
    }
    if dt.hour > 23 || dt.minute > 59 || dt.second > 60 {
        return None;
    }
    let days = days_from_civil(dt.year as i64, dt.month as i64, dt.day as i64);
    let mut ms =
        (days * 86_400 + dt.hour as i64 * 3600 + dt.minute as i64 * 60 + dt.second as i64) * 1000;
    if let Some(ss) = subsec {
        let digits: String = ss.chars().filter(|c| c.is_ascii_digit()).take(3).collect();
        if !digits.is_empty() {
            let frac: i64 = digits.parse().unwrap_or(0);
            ms += frac * 10_i64.pow(3 - digits.len() as u32);
        }
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(i64::MAX);
    if ms > now_ms + 86_400_000 {
        return None;
    }
    Some(ms)
}

/// Howard Hinnant civil-days: số ngày kể từ 1970-01-01 (không cần chrono).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exif_datetime_to_epoch() {
        // 2019-06-14T15:30:22Z = 1560526222
        assert_eq!(
            parse_exif_datetime("2019:06:14 15:30:22", None),
            Some(1_560_526_222_000)
        );
        // SubSec "7" = 700ms, "73" = 730ms, "735" = 735ms
        assert_eq!(
            parse_exif_datetime("2019:06:14 15:30:22", Some("7")),
            Some(1_560_526_222_700)
        );
        assert_eq!(
            parse_exif_datetime("2019:06:14 15:30:22", Some("735")),
            Some(1_560_526_222_735)
        );
    }

    #[test]
    fn exif_datetime_sanity_range() {
        // Camera reset pin → 1970: loại
        assert_eq!(parse_exif_datetime("1970:01:01 00:00:00", None), None);
        // Tương lai xa: loại
        assert_eq!(parse_exif_datetime("2099:01:01 00:00:00", None), None);
        // Rác: loại, không panic
        assert_eq!(parse_exif_datetime("0000:00:00 00:00:00", None), None);
        assert_eq!(parse_exif_datetime("not a date", None), None);
    }
}
