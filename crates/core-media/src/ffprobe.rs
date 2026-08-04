use std::path::Path;
use std::process::Command;

use serde_json::Value;

use crate::ffmpeg::run_with_timeout;
use crate::meta::{civil_from_days, days_from_civil};

/// Metadata video từ ffprobe. `taken_at` theo cùng quy ước EXIF: wall-clock
/// encode như epoch ms khung UTC.
#[derive(Debug, Default, Clone)]
pub struct VideoMeta {
    /// Dimensions HIỂN THỊ (đã hoán đổi nếu rotation 90/270).
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<i64>,
    pub vcodec: Option<String>,
    pub acodec: Option<String>,
    pub bitrate: Option<i64>,
    pub fps: Option<f64>,
    pub taken_at: Option<i64>,
    /// 1 = QuickTime tag (creationdate có offset hoặc creation_time UTC).
    pub date_source: Option<i64>,
    /// false = probe fail / không có video stream → meta_state=2.
    pub ok: bool,
}

/// Probe 1 file video. Không Err với file hỏng — trả `ok=false` để job ghi
/// meta_state=2 và không chọn lại. Err chỉ khi không chạy được ffprobe.
pub fn probe_video(ffprobe: &Path, input: &Path, tz_offset_min: i64) -> VideoMeta {
    probe_video_with_offset(ffprobe, input, &|_| tz_offset_min)
}

/// Production path: IANA timezone + fallback offset cho dữ liệu cũ. Offset
/// được resolve tại timestamp của video nên đúng qua DST.
pub fn probe_video_in_timezone(
    ffprobe: &Path,
    input: &Path,
    timezone: Option<&str>,
    fallback_offset_min: i64,
) -> VideoMeta {
    probe_video_with_offset(ffprobe, input, &|ms| {
        timezone
            .and_then(|name| timezone_offset_minutes(name, ms))
            .map(i64::from)
            .unwrap_or(fallback_offset_min)
    })
}

fn probe_video_with_offset(
    ffprobe: &Path,
    input: &Path,
    offset_at: &dyn Fn(i64) -> i64,
) -> VideoMeta {
    let mut cmd = Command::new(ffprobe);
    cmd.args([
        "-v",
        "error",
        "-print_format",
        "json",
        "-show_format",
        "-show_streams",
    ])
    .arg(input);
    match run_with_timeout(cmd, "ffprobe") {
        Ok((stdout, _)) => {
            parse_probe_json_with_offset(&String::from_utf8_lossy(&stdout), offset_at)
        }
        Err(e) => {
            tracing::debug!(path = %input.display(), "ffprobe failed: {e:#}");
            VideoMeta::default()
        }
    }
}

/// Tách riêng để test không cần ffprobe thật.
#[cfg(test)]
pub(crate) fn parse_probe_json(json: &str, tz_offset_min: i64) -> VideoMeta {
    parse_probe_json_with_offset(json, &|_| tz_offset_min)
}

fn parse_probe_json_with_offset(json: &str, offset_at: &dyn Fn(i64) -> i64) -> VideoMeta {
    let mut m = VideoMeta::default();
    let Ok(root) = serde_json::from_str::<Value>(json) else {
        return m;
    };

    let streams = root["streams"].as_array().cloned().unwrap_or_default();
    let video = streams
        .iter()
        .find(|s| s["codec_type"].as_str() == Some("video"));
    let audio = streams
        .iter()
        .find(|s| s["codec_type"].as_str() == Some("audio"));

    if let Some(v) = video {
        m.width = v["width"].as_u64().map(|x| x as u32);
        m.height = v["height"].as_u64().map(|x| x as u32);
        m.vcodec = v["codec_name"].as_str().map(String::from);
        m.fps = parse_fraction(v["avg_frame_rate"].as_str().unwrap_or(""))
            .or_else(|| parse_fraction(v["r_frame_rate"].as_str().unwrap_or("")));
        // Rotation: side_data displaymatrix (ffmpeg 5+) hoặc tag rotate cũ
        let rotation = v["side_data_list"]
            .as_array()
            .and_then(|list| {
                list.iter()
                    .find_map(|sd| sd["rotation"].as_f64().map(|r| r as i64))
            })
            .or_else(|| {
                v["tags"]["rotate"]
                    .as_str()
                    .and_then(|s| s.parse::<i64>().ok())
            })
            .unwrap_or(0);
        if rotation.rem_euclid(180) == 90 {
            std::mem::swap(&mut m.width, &mut m.height);
        }
        m.ok = m.width.is_some() && m.height.is_some();
    }
    if let Some(a) = audio {
        m.acodec = a["codec_name"].as_str().map(String::from);
    }

    let format = &root["format"];
    m.duration_ms = format["duration"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .map(|secs| (secs * 1000.0) as i64)
        .filter(|&ms| ms >= 0);
    m.bitrate = format["bit_rate"].as_str().and_then(|s| s.parse().ok());

    // Ngày quay: ưu tiên com.apple.quicktime.creationdate (iPhone ghi kèm offset
    // "+07:00" — phần local trong chuỗi CHÍNH LÀ giờ đồng hồ máy); fallback
    // creation_time (UTC) → đổi sang wall-clock theo tz config.
    let tags = &format["tags"];
    let qt = tags["com.apple.quicktime.creationdate"].as_str();
    let ct = tags["creation_time"].as_str();
    // Chung 1 luật cho cả 2 tag: có offset → phần local là giờ đồng hồ camera,
    // dùng thẳng; không offset/Z → là UTC instant, cộng tz config ra wall-clock.
    // (iPhone ghi creationdate kèm offset, nhưng ffmpeg re-mux / app Android
    // có thể ghi tag này dạng Z — không được coi bừa là wall-clock.)
    let to_wall = |(ms, has_offset): (i64, bool)| {
        if has_offset {
            ms
        } else {
            ms + offset_at(ms) * 60_000
        }
    };
    if let Some(parsed) = qt.and_then(parse_iso_datetime) {
        m.taken_at = sanity(to_wall(parsed));
    } else if let Some(parsed) = ct.and_then(parse_iso_datetime) {
        m.taken_at = sanity(to_wall(parsed));
    }
    if m.taken_at.is_some() {
        m.date_source = Some(1);
    }
    m
}

fn timezone_offset_minutes(timezone: &str, epoch_ms: i64) -> Option<i32> {
    use chrono::{Offset, TimeZone};
    let tz: chrono_tz::Tz = timezone.parse().ok()?;
    let utc = chrono::Utc.timestamp_millis_opt(epoch_ms).single()?;
    Some(
        tz.offset_from_utc_datetime(&utc.naive_utc())
            .fix()
            .local_minus_utc()
            / 60,
    )
}

fn sanity(ms: i64) -> Option<i64> {
    // 1990-01-01 .. hôm-nay+1d, như EXIF (camera reset pin ghi 1970/2099)
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(i64::MAX);
    (631_152_000_000..=now_ms + 86_400_000)
        .contains(&ms)
        .then_some(ms)
}

/// "30000/1001" → 29.97; "0/0" → None.
fn parse_fraction(s: &str) -> Option<f64> {
    let (num, den) = s.split_once('/')?;
    let num: f64 = num.parse().ok()?;
    let den: f64 = den.parse().ok()?;
    if den == 0.0 || num <= 0.0 {
        None
    } else {
        Some(num / den)
    }
}

/// Parse ISO-8601 kiểu ffprobe: "2019-06-14T15:30:22.000000Z",
/// "2019-06-14 08:30:22", "2019-06-14T15:30:22+0700", "+07:00".
/// Trả (wall_ms_cua_phan_local, có_offset_không):
/// - Có offset → phần local là giờ đồng hồ camera, dùng thẳng.
/// - "Z"/không offset → là UTC instant, caller tự cộng tz config.
pub(crate) fn parse_iso_datetime(s: &str) -> Option<(i64, bool)> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let year: i64 = s.get(0..4)?.parse().ok()?;
    let month: i64 = s.get(5..7)?.parse().ok()?;
    let day: i64 = s.get(8..10)?.parse().ok()?;
    let hour: i64 = s.get(11..13)?.parse().ok()?;
    let minute: i64 = s.get(14..16)?.parse().ok()?;
    let second: i64 = s.get(17..19)?.parse().ok()?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let days = days_from_civil(year, month, day);
    if civil_from_days(days) != (year, month, day) {
        return None;
    }
    let mut ms = (days * 86_400 + hour * 3600 + minute * 60 + second) * 1000;

    // Phần còn lại: ".ffffff" rồi "Z" / "+HH:MM" / "+HHMM" / rỗng
    let mut rest = &s[19..];
    if let Some(stripped) = rest.strip_prefix('.') {
        let digits: String = stripped
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if !digits.is_empty() {
            let frac: i64 = digits[..digits.len().min(3)].parse().unwrap_or(0);
            ms += frac * 10_i64.pow(3 - digits.len().min(3) as u32);
        }
        rest = &stripped[digits.len()..];
    }
    let has_offset = rest.starts_with('+') || rest.starts_with('-');
    Some((ms, has_offset))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_datetime_variants() {
        // 2019-06-14T08:30:22Z = 1560501022 (UTC, không offset)
        assert_eq!(
            parse_iso_datetime("2019-06-14T08:30:22.000000Z"),
            Some((1_560_501_022_000, false))
        );
        // Space separator
        assert_eq!(
            parse_iso_datetime("2019-06-14 08:30:22"),
            Some((1_560_501_022_000, false))
        );
        // Có offset: phần local 15:30:22 là wall-clock → 1560526222
        assert_eq!(
            parse_iso_datetime("2019-06-14T15:30:22+0700"),
            Some((1_560_526_222_000, true))
        );
        assert_eq!(parse_iso_datetime("not a date"), None);
        assert_eq!(parse_iso_datetime(""), None);
        assert_eq!(parse_iso_datetime("2019-02-30T12:00:00Z"), None);
        assert_eq!(parse_iso_datetime("2021-04-31T12:00:00Z"), None);
        assert!(parse_iso_datetime("2020-02-29T12:00:00Z").is_some());
    }

    #[test]
    fn probe_json_parses_iphone_mov() {
        let json = r#"{
          "streams": [
            {"codec_type":"video","codec_name":"hevc","width":1920,"height":1080,
             "avg_frame_rate":"30000/1001",
             "side_data_list":[{"side_data_type":"Display Matrix","rotation":-90}]},
            {"codec_type":"audio","codec_name":"aac"}
          ],
          "format": {
            "duration":"12.345","bit_rate":"16000000",
            "tags":{"com.apple.quicktime.creationdate":"2019-06-14T15:30:22+0700",
                    "creation_time":"2019-06-14T08:30:22.000000Z"}
          }
        }"#;
        let m = parse_probe_json(json, 420);
        assert!(m.ok);
        // rotation -90 → dims hiển thị hoán đổi
        assert_eq!((m.width, m.height), (Some(1080), Some(1920)));
        assert_eq!(m.vcodec.as_deref(), Some("hevc"));
        assert_eq!(m.acodec.as_deref(), Some("aac"));
        assert_eq!(m.duration_ms, Some(12_345));
        assert_eq!(m.bitrate, Some(16_000_000));
        assert!((m.fps.unwrap() - 29.97).abs() < 0.01);
        // creationdate có offset thắng: wall-clock 15:30:22
        assert_eq!(m.taken_at, Some(1_560_526_222_000));
        assert_eq!(m.date_source, Some(1));
    }

    #[test]
    fn probe_json_utc_fallback_applies_tz() {
        let json = r#"{
          "streams":[{"codec_type":"video","codec_name":"h264","width":1280,"height":720,
                      "avg_frame_rate":"30/1"}],
          "format":{"duration":"5.0","tags":{"creation_time":"2019-06-14T08:30:22.000000Z"}}
        }"#;
        let m = parse_probe_json(json, 420); // UTC+7
        assert_eq!(m.taken_at, Some(1_560_526_222_000)); // 08:30 UTC + 7h = 15:30 wall
    }

    #[test]
    fn probe_json_garbage_is_not_ok() {
        assert!(!parse_probe_json("not json", 0).ok);
        assert!(!parse_probe_json(r#"{"streams":[],"format":{}}"#, 0).ok);
    }

    #[test]
    fn iana_timezone_offset_tracks_dst() {
        // 2024-01-15 / 2024-07-15 12:00:00 UTC.
        let jan = 1_705_320_000_000;
        let jul = 1_721_044_800_000;
        assert_eq!(timezone_offset_minutes("America/New_York", jan), Some(-300));
        assert_eq!(timezone_offset_minutes("America/New_York", jul), Some(-240));
        assert_eq!(timezone_offset_minutes("Asia/Ho_Chi_Minh", jan), Some(420));
        assert_eq!(timezone_offset_minutes("Asia/Ho_Chi_Minh", jul), Some(420));
    }
}
