//! Xác định ngày chụp cho organize - 4 tầng ưu tiên giảm dần:
//! media_meta.taken_at (EXIF/QuickTime, M2/M3 đã unify) -> parse từ tên file
//! -> mtime (flag uncertain).
//!
//! Convention taken_at (giống media_meta): wall-clock camera encode như epoch ms
//! trong khung UTC - render ngày giờ đọc với tz=0. files.mtime là instant UTC
//! thật nên tầng mtime phải cộng tz_offset_min để về cùng khung wall-clock.

use regex::Regex;
use std::sync::LazyLock;

/// date_source values - khớp comment cột media_meta.date_source trong schema.sql
pub const SRC_EXIF: i64 = 0;
pub const SRC_FILENAME: i64 = 2;
pub const SRC_MTIME_UNCERTAIN: i64 = 3;

pub const MS_PER_DAY: i64 = 86_400_000;
/// 1990-01-01T00:00:00Z - EXIF trước mốc này coi là rác (camera reset pin)
pub const SANITY_MIN_MS: i64 = 631_152_000_000;

/// days since 1970-01-01 từ (year, month, day). Thuật toán Howard Hinnant.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// (year, month, day) từ days since 1970-01-01. Đảo của days_from_civil.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Validate từng trường + ngày civil có thật (30/02 -> None) rồi ra epoch ms.
fn ymd_hms_to_ms(y: i64, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> Option<i64> {
    if !(1900..=2100).contains(&y) || !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    if h >= 24 || mi >= 60 || s >= 60 {
        return None;
    }
    let days = days_from_civil(y, mo, d);
    if civil_from_days(days) != (y, mo, d) {
        return None; // 31/04, 30/02...
    }
    Some(days * MS_PER_DAY + (h as i64 * 3600 + mi as i64 * 60 + s as i64) * 1000)
}

pub struct ParsedDate {
    /// epoch ms khung wall-clock (đọc với tz=0)
    pub ms: i64,
    /// false = tên file chỉ có ngày, giờ mặc định 00:00:00
    pub has_time: bool,
}

// Thứ tự thử: datetime đầy đủ trước, epoch, rồi mới date-only.
// Mọi group số đều chặn biên bằng (?:^|[^0-9]) ... (?:[^0-9]|$) để không cắt
// giữa chuỗi số dài.
static COMPACT_DT: LazyLock<Regex> = LazyLock::new(|| {
    // IMG_20190614_153022 / Screenshot_20190614-123456 / 20190614153022
    // / PXL_20210614_211523123 (bỏ 3 số mili)
    Regex::new(
        r"(?:^|[^0-9])((?:19|20)\d{2})(\d{2})(\d{2})[_\- .]?(\d{2})(\d{2})(\d{2})(?:\d{3})?(?:[^0-9]|$)",
    )
    .unwrap()
});
static DASHED_DT: LazyLock<Regex> = LazyLock::new(|| {
    // 2019-06-14 15.30.22 / photo_2026-08-03_09-56-47 / Screenshot_2019-06-14-12-34-56
    Regex::new(
        r"(?:^|[^0-9])((?:19|20)\d{2})-(\d{2})-(\d{2})[ _T.\-](\d{2})[.\-:](\d{2})[.\-:](\d{2})(?:[^0-9]|$)",
    )
    .unwrap()
});
static EPOCH_MS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[^0-9])(1[3-9]\d{11})(?:[^0-9]|$)").unwrap());
static EPOCH_S: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[^0-9])(1[3-9]\d{8})(?:[^0-9]|$)").unwrap());
static COMPACT_D: LazyLock<Regex> = LazyLock::new(|| {
    // IMG-20190614-WA0001 (WhatsApp) - chỉ có ngày
    Regex::new(r"(?:^|[^0-9])((?:19|20)\d{2})(\d{2})(\d{2})(?:[^0-9]|$)").unwrap()
});
static DASHED_D: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[^0-9])((?:19|20)\d{2})-(\d{2})-(\d{2})(?:[^0-9]|$)").unwrap()
});

fn g_i64(c: &regex::Captures, i: usize) -> i64 {
    c.get(i).unwrap().as_str().parse().unwrap()
}
fn g_u32(c: &regex::Captures, i: usize) -> u32 {
    c.get(i).unwrap().as_str().parse().unwrap()
}

/// Parse ngày chụp từ TÊN file (không gồm path). tz_offset_min chỉ dùng cho
/// dạng epoch (instant thật -> wall-clock); các dạng chữ số là giờ địa phương
/// sẵn rồi.
pub fn parse_filename_date(name: &str, tz_offset_min: i32) -> Option<ParsedDate> {
    for caps in COMPACT_DT.captures_iter(name) {
        if let Some(ms) = ymd_hms_to_ms(
            g_i64(&caps, 1),
            g_u32(&caps, 2),
            g_u32(&caps, 3),
            g_u32(&caps, 4),
            g_u32(&caps, 5),
            g_u32(&caps, 6),
        ) {
            return Some(ParsedDate { ms, has_time: true });
        }
    }
    for caps in DASHED_DT.captures_iter(name) {
        if let Some(ms) = ymd_hms_to_ms(
            g_i64(&caps, 1),
            g_u32(&caps, 2),
            g_u32(&caps, 3),
            g_u32(&caps, 4),
            g_u32(&caps, 5),
            g_u32(&caps, 6),
        ) {
            return Some(ParsedDate { ms, has_time: true });
        }
    }
    if let Some(caps) = EPOCH_MS.captures(name) {
        let ms = g_i64(&caps, 1) + tz_offset_min as i64 * 60_000;
        return Some(ParsedDate { ms, has_time: true });
    }
    if let Some(caps) = EPOCH_S.captures(name) {
        let ms = g_i64(&caps, 1) * 1000 + tz_offset_min as i64 * 60_000;
        return Some(ParsedDate { ms, has_time: true });
    }
    for caps in COMPACT_D.captures_iter(name) {
        if let Some(ms) = ymd_hms_to_ms(g_i64(&caps, 1), g_u32(&caps, 2), g_u32(&caps, 3), 0, 0, 0)
        {
            return Some(ParsedDate {
                ms,
                has_time: false,
            });
        }
    }
    for caps in DASHED_D.captures_iter(name) {
        if let Some(ms) = ymd_hms_to_ms(g_i64(&caps, 1), g_u32(&caps, 2), g_u32(&caps, 3), 0, 0, 0)
        {
            return Some(ParsedDate {
                ms,
                has_time: false,
            });
        }
    }
    None
}

pub struct Resolved {
    pub taken_ms: i64,
    pub source: i64,
}

fn sane(ms: i64, now_ms: i64) -> bool {
    (SANITY_MIN_MS..=now_ms + MS_PER_DAY).contains(&ms)
}

/// Resolution 4 tầng. meta_taken/meta_source lấy từ media_meta (đã là wall-clock
/// khung UTC); name = tên file; mtime_ms = files.mtime (instant UTC thật);
/// now_ms để sanity-check (không gọi Date::now trong core).
pub fn resolve_taken(
    meta_taken: Option<i64>,
    meta_source: Option<i64>,
    name: &str,
    mtime_ms: i64,
    tz_offset_min: i32,
    now_ms: i64,
) -> Resolved {
    if let Some(t) = meta_taken {
        if sane(t, now_ms) {
            return Resolved {
                taken_ms: t,
                source: meta_source.unwrap_or(SRC_EXIF),
            };
        }
    }
    if let Some(p) = parse_filename_date(name, tz_offset_min) {
        if sane(p.ms, now_ms) {
            return Resolved {
                taken_ms: p.ms,
                source: SRC_FILENAME,
            };
        }
    }
    Resolved {
        taken_ms: mtime_ms + tz_offset_min as i64 * 60_000,
        source: SRC_MTIME_UNCERTAIN,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(y: i64, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> i64 {
        ymd_hms_to_ms(y, mo, d, h, mi, s).unwrap()
    }

    #[test]
    fn civil_roundtrip() {
        for &(y, m, d) in &[
            (1970, 1, 1),
            (1999, 12, 31),
            (2000, 2, 29),
            (2019, 6, 14),
            (2026, 8, 3),
            (2100, 12, 31),
        ] {
            assert_eq!(civil_from_days(days_from_civil(y, m, d)), (y, m, d));
        }
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1970, 1, 2), 1);
    }

    #[test]
    fn invalid_civil_dates_rejected() {
        assert!(ymd_hms_to_ms(2019, 2, 30, 0, 0, 0).is_none());
        assert!(ymd_hms_to_ms(2019, 4, 31, 0, 0, 0).is_none());
        assert!(ymd_hms_to_ms(2019, 13, 1, 0, 0, 0).is_none());
        assert!(ymd_hms_to_ms(2019, 6, 14, 24, 0, 0).is_none());
        assert!(ymd_hms_to_ms(2100, 2, 29, 0, 0, 0).is_none()); // 2100 không nhuận
    }

    #[test]
    fn parse_common_patterns() {
        let cases: &[(&str, i64, bool)] = &[
            ("IMG_20190614_153022.jpg", ms(2019, 6, 14, 15, 30, 22), true),
            ("VID_20190614_153022.mp4", ms(2019, 6, 14, 15, 30, 22), true),
            ("20190614153022.jpg", ms(2019, 6, 14, 15, 30, 22), true),
            ("20190614_153022.jpg", ms(2019, 6, 14, 15, 30, 22), true),
            (
                "Screenshot_20190614-123456.png",
                ms(2019, 6, 14, 12, 34, 56),
                true,
            ),
            (
                "PXL_20210614_211523123.jpg",
                ms(2021, 6, 14, 21, 15, 23),
                true,
            ),
            ("2019-06-14 15.30.22.jpg", ms(2019, 6, 14, 15, 30, 22), true),
            (
                "photo_2026-08-03_09-56-47.jpg",
                ms(2026, 8, 3, 9, 56, 47),
                true,
            ),
            (
                "Screenshot_2019-06-14-12-34-56.png",
                ms(2019, 6, 14, 12, 34, 56),
                true,
            ),
            ("IMG-20190614-WA0001.jpg", ms(2019, 6, 14, 0, 0, 0), false),
            ("2019-06-14.jpg", ms(2019, 6, 14, 0, 0, 0), false),
        ];
        for &(name, want, has_time) in cases {
            let p =
                parse_filename_date(name, 0).unwrap_or_else(|| panic!("khong parse duoc: {name}"));
            assert_eq!(p.ms, want, "{name}");
            assert_eq!(p.has_time, has_time, "{name}");
        }
    }

    #[test]
    fn parse_epoch_names() {
        // 1560523822 = 2019-06-14T14:50:22Z; tz +420 phut (UTC+7) -> 21:50:22 wall
        let p = parse_filename_date("received_1560523822.jpeg", 420).unwrap();
        assert_eq!(p.ms, ms(2019, 6, 14, 21, 50, 22));
        let p = parse_filename_date("1560523822000.jpg", 0).unwrap();
        assert_eq!(p.ms, ms(2019, 6, 14, 14, 50, 22));
    }

    #[test]
    fn parse_rejects_non_dates() {
        for name in [
            "IMG_1234.jpg",
            "photo (2).jpg",
            "12345678.jpg", // không có prefix năm 19xx/20xx
            "DSC_9999.NEF",
            "a_20191355_999999.jpg",            // month 13, hour 99 loại
            "chuyen_khoan_123456789012345.png", // 15 số không khớp epoch nào
        ] {
            assert!(
                parse_filename_date(name, 0).is_none(),
                "false positive: {name}"
            );
        }
    }

    #[test]
    fn skips_invalid_match_then_finds_valid_one() {
        // Chuỗi đầu 20191355 (month 13) không hợp lệ -> phải tìm tiếp match sau
        let p = parse_filename_date("x20191355_y_IMG_20190614_153022.jpg", 0).unwrap();
        assert_eq!(p.ms, ms(2019, 6, 14, 15, 30, 22));
    }

    #[test]
    fn resolve_priority_and_sanity() {
        let now = ms(2026, 8, 4, 0, 0, 0);
        // meta hop le -> thang, giu nguon meta
        let r = resolve_taken(
            Some(ms(2020, 1, 1, 0, 0, 0)),
            Some(1),
            "IMG_20190614_153022.jpg",
            0,
            420,
            now,
        );
        assert_eq!((r.taken_ms, r.source), (ms(2020, 1, 1, 0, 0, 0), 1));
        // meta rac (2099, qua tuong lai) -> rot xuong filename
        let r = resolve_taken(
            ymd_hms_to_ms(2099, 1, 1, 0, 0, 0),
            Some(0),
            "IMG_20190614_153022.jpg",
            0,
            420,
            now,
        );
        assert_eq!(
            (r.taken_ms, r.source),
            (ms(2019, 6, 14, 15, 30, 22), SRC_FILENAME)
        );
        // meta rac 1970 -> filename khong co -> mtime + tz, uncertain
        let mtime = ms(2022, 5, 1, 10, 0, 0);
        let r = resolve_taken(Some(0), Some(0), "DSC_0001.jpg", mtime, 420, now);
        assert_eq!(r.source, SRC_MTIME_UNCERTAIN);
        assert_eq!(r.taken_ms, mtime + 420 * 60_000);
        // filename tuong lai xa (nam sau) -> loai, ve mtime
        let r = resolve_taken(None, None, "IMG_20270614_153022.jpg", mtime, 0, now);
        assert_eq!(r.source, SRC_MTIME_UNCERTAIN);
    }
}
