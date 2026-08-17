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
    /// EXIF `Artist` — tên người chụp, do chủ máy tự đặt trong cài đặt camera.
    ///
    /// CỐ Ý KHÔNG lưu vào DB và KHÔNG có token `{author}`. Đo trên kho thật
    /// (`devtool probe-meta`): 4/6560 file có tag này — iPhone 3, một ảnh tải
    /// về của người khác 1, máy ảnh rời 0. Gần như không ai đụng vào ô "Artist"
    /// trong cài đặt máy. Thêm cột thì phải bump `META_VERSION`, tức bắt 25.597
    /// file trích lại chỉ để điền một cột rỗng 99,94%.
    ///
    /// Giữ lại phần đọc để `probe-meta` đo được: kho ảnh của dân chụp chuyên có
    /// thể khác hẳn, và lúc đó đo lại là ra ngay chứ không phải đoán.
    pub author: Option<String>,
    pub orientation: Option<u16>,
    /// Toạ độ nơi chụp, độ thập phân (bắc/đông dương). Cả hai cùng có hoặc
    /// cùng không — một nửa toạ độ thì vô dụng.
    pub gps_lat: Option<f64>,
    pub gps_lon: Option<f64>,
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

            // Nhiều máy ghi sẵn chuỗi rỗng/placeholder thay vì bỏ hẳn tag —
            // "unknown" mà thành một tầng thư mục thì tệ hơn là không có tầng.
            m.author = exif
                .get_field(Tag::Artist, In::PRIMARY)
                .and_then(|f| ascii_string(&f.value))
                .filter(|s| !is_placeholder(s));

            let lat = gps_degrees(&exif, Tag::GPSLatitude, Tag::GPSLatitudeRef, 'S');
            let lon = gps_degrees(&exif, Tag::GPSLongitude, Tag::GPSLongitudeRef, 'W');
            if let (Some(lat), Some(lon)) = (lat, lon) {
                if let Some((lat, lon)) = sane_coord(lat, lon) {
                    m.gps_lat = Some(lat);
                    m.gps_lon = Some(lon);
                }
            }
        }
    }

    // Orientation 5-8 = xoay 90°/270° → dimensions hiển thị hoán đổi
    if matches!(m.orientation, Some(5..=8)) {
        std::mem::swap(&mut m.width, &mut m.height);
    }
    m
}

/// Giá trị "có tag nhưng không có nội dung". Máy/phần mềm hay ghi sẵn mấy chuỗi
/// này thay vì bỏ hẳn tag; để nguyên thì đẻ ra thư mục tên "unknown".
///
/// Danh sách CỐ TÌNH ngắn: đây là tên người, đoán thừa là xoá mất tên thật của
/// một ai đó. Chỉ chặn thứ chắc chắn không phải tên.
fn is_placeholder(s: &str) -> bool {
    let t = s.trim().trim_matches(['-', '_', '.']).to_ascii_lowercase();
    t.is_empty()
        || matches!(
            t.as_str(),
            "unknown" | "none" | "null" | "n/a" | "na" | "unknown artist" | "camera owner"
        )
}

/// EXIF ghi toạ độ thành 3 phân số độ/phút/giây, dấu nằm ở tag `*Ref` riêng
/// ("N"/"S", "E"/"W"). Máy nào ghi thiếu phần giây (chỉ độ + phút thập phân)
/// vẫn đọc được — cộng đúng những phần có.
fn gps_degrees(exif: &exif::Exif, tag: Tag, ref_tag: Tag, negative: char) -> Option<f64> {
    let Value::Rational(parts) = &exif.get_field(tag, In::PRIMARY)?.value else {
        return None;
    };
    if parts.is_empty() {
        return None;
    }
    let mut deg = 0.0;
    for (i, r) in parts.iter().take(3).enumerate() {
        if r.denom == 0 {
            return None;
        }
        deg += r.to_f64() / 60_f64.powi(i as i32);
    }
    // Thiếu tag Ref thì KHÔNG đoán bán cầu — toạ độ sai dấu đưa ảnh sang châu
    // lục khác, tệ hơn hẳn việc không có toạ độ.
    let hemi = exif
        .get_field(ref_tag, In::PRIMARY)
        .and_then(|f| ascii_string(&f.value))?;
    let sign = if hemi.to_ascii_uppercase().starts_with(negative) {
        -1.0
    } else {
        1.0
    };
    Some(sign * deg)
}

/// Chặn toạ độ rác trước khi ghi vào DB. Đúng 0,0 là giá trị vài app điền khi
/// KHÔNG có định vị (giữa vịnh Guinea) — `core_geo::lookup` cũng từ chối nó,
/// nhưng chặn ngay đây để bộ đếm "ảnh có toạ độ" không bị thổi phồng.
pub(crate) fn sane_coord(lat: f64, lon: f64) -> Option<(f64, f64)> {
    if !lat.is_finite() || !lon.is_finite() {
        return None;
    }
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return None;
    }
    if lat.abs() < 1e-6 && lon.abs() < 1e-6 {
        return None;
    }
    Some((lat, lon))
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
    if civil_from_days(days) != (dt.year as i64, dt.month as i64, dt.day as i64) {
        return None;
    }
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
pub(crate) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Đảo của `days_from_civil`, dùng để reject 30/02, 31/04... thay vì để
/// thuật toán civil-days normalize âm thầm sang tháng kế.
pub(crate) fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
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
        assert_eq!(parse_exif_datetime("2019:02:30 12:00:00", None), None);
        assert_eq!(parse_exif_datetime("2021:04:31 12:00:00", None), None);
        assert!(parse_exif_datetime("2020:02:29 12:00:00", None).is_some());
    }
}
