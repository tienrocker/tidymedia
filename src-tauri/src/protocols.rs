//! Protocol thumb:// và media:// — WebView2 truy cập qua http://thumb.localhost/…
//!
//! URL luôn kèm ?v=<mtime> → response được cache "immutable" vô hạn phía WebView2;
//! file đổi thì mtime đổi → URL đổi → tự miss. Không cần ETag/304.
//!
//! Handler KHÔNG được block thread webview: mọi việc (DB lookup, decode, đọc file)
//! dispatch sang thumb_pool rồi trả lời qua responder.

use std::panic::AssertUnwindSafe;
use std::path::Path;

use core_db::MediaSrc;
use tauri::http::{Request, Response};
use tauri::{AppHandle, Manager, UriSchemeResponder};

use crate::state::AppState;

/// File media:// to hơn mức này → 404 thay vì nuốt RAM (ảnh hiển thị native
/// không bao giờ tới cỡ đó; video đi đường Range riêng ở M3).
const MAX_MEDIA_BYTES: i64 = 256 * 1024 * 1024;

pub fn spawn_thumb(app: AppHandle, request: Request<Vec<u8>>, responder: UriSchemeResponder) {
    let state = app.state::<AppState>();
    let db = state.db.clone();
    let thumbs = state.thumbs.clone();
    let ffmpeg = state.ffmpeg.clone();
    state.thumb_pool.spawn(move || {
        // Decoder panic với file hỏng (image crate, webp encode unwrap) mà lọt
        // ra ngoài task rayon là process::abort CẢ APP — và grid load lại đúng
        // thumb đó ở lần mở sau = crash-loop. Bắt lại, trả 404.
        let resp = std::panic::catch_unwind(AssertUnwindSafe(|| {
            thumb_response(&db, &thumbs, ffmpeg.as_deref(), &request)
        }))
        .unwrap_or_else(|_| {
            tracing::warn!(uri = %request.uri(), "thumb handler panicked — tra 404");
            status(404)
        });
        responder.respond(resp);
    });
}

pub fn spawn_media(app: AppHandle, request: Request<Vec<u8>>, responder: UriSchemeResponder) {
    let state = app.state::<AppState>();
    let db = state.db.clone();
    state.thumb_pool.spawn(move || {
        let resp = std::panic::catch_unwind(AssertUnwindSafe(|| media_response(&db, &request)))
            .unwrap_or_else(|_| {
                tracing::warn!(uri = %request.uri(), "media handler panicked — tra 404");
                status(404)
            });
        responder.respond(resp);
    });
}

fn thumb_response(
    db: &core_db::Db,
    thumbs: &core_media::ThumbStore,
    ffmpeg: Option<&Path>,
    request: &Request<Vec<u8>>,
) -> Response<Vec<u8>> {
    let Some(id) = parse_id(request) else {
        return status(400);
    };
    let s = core_media::clamp_thumb_size(query_param(request, "s").unwrap_or(256));

    let src = match lookup(db, id) {
        Some(s) => s,
        None => return status(404),
    };
    // status != present (missing/cloud placeholder) → cấm đọc (placeholder mà
    // đọc là kéo hydrate cả OneDrive).
    if src.status != 0 {
        return status(404);
    }

    if let Some(data) = thumbs.get(id, s, src.mtime) {
        return ok(data, "image/webp");
    }

    // status trong DB có thể stale (OneDrive "Free up space" SAU lần scan cuối)
    // → check attrs thật ngay trước khi decode, không bao giờ kéo hydrate.
    match std::fs::metadata(&src.path) {
        Ok(md) if !core_media::is_cloud_placeholder(&md) => {}
        _ => return status(404),
    }

    let ext = src.ext.as_deref().unwrap_or("");
    let generated = if src.kind == 1 {
        // Video: keyframe qua ffmpeg (seek ~10% duration nếu meta đã có)
        let Some(ff) = ffmpeg else { return status(404) };
        core_media::make_video_thumb(ff, Path::new(&src.path), s, src.duration_ms)
    } else {
        core_media::make_thumb(Path::new(&src.path), ext, s, ffmpeg)
    };
    match generated {
        Ok(data) => {
            if let Err(e) = thumbs.put(id, s, src.mtime, &data) {
                tracing::warn!("thumbs.db put failed: {e:#}");
            }
            ok(data, "image/webp")
        }
        Err(e) => {
            tracing::debug!(id, ext, "thumb generate failed: {e:#}");
            status(404)
        }
    }
}

/// Chunk tối đa mỗi response video — Chromium tự request tiếp bằng Range.
const VIDEO_CHUNK: u64 = 16 * 1024 * 1024;
/// Request video đầu tiên không có Range → trả 206 chunk đầu cỡ này.
const VIDEO_FIRST_CHUNK: u64 = 8 * 1024 * 1024;

fn media_response(db: &core_db::Db, request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    let Some(id) = parse_id(request) else {
        return status(400);
    };
    let src = match lookup(db, id) {
        Some(s) => s,
        None => return status(404),
    };
    if src.status != 0 {
        return status(404);
    }
    // Như thumb: attrs thật quyết định, không tin status snapshot
    let len = match std::fs::metadata(&src.path) {
        Ok(md) if !core_media::is_cloud_placeholder(&md) => md.len(),
        _ => return status(404),
    };
    let mime = mime_for(src.ext.as_deref().unwrap_or(""));

    if src.kind == 1 {
        return video_range_response(&src.path, len, mime, request);
    }
    // Ảnh: đọc nguyên file (native display), cap chống nuốt RAM
    if len as i64 > MAX_MEDIA_BYTES {
        return status(404);
    }
    match std::fs::read(&src.path) {
        Ok(bytes) => ok(bytes, mime),
        Err(e) => {
            tracing::debug!(id, path = %src.path, "media read failed: {e}");
            status(404)
        }
    }
}

/// HTTP Range cho <video>: 206 + Content-Range, chunk cap 16MB — không bao giờ
/// đọc nguyên file 4GB vào RAM. Không có Range header → 206 chunk đầu
/// (Chromium hiểu và tự request tiếp phần còn lại).
fn video_range_response(
    path: &str,
    len: u64,
    mime: &str,
    request: &Request<Vec<u8>>,
) -> Response<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    if len == 0 {
        return status(404);
    }
    let range = request
        .headers()
        .get("range")
        .and_then(|v| v.to_str().ok())
        .and_then(parse_range);
    let (start, want_end) = match range {
        Some((s, e)) => (s, e.unwrap_or(len - 1)),
        None => (0, VIDEO_FIRST_CHUNK.saturating_sub(1)),
    };
    if start >= len {
        return Response::builder()
            .status(416)
            .header("Content-Range", format!("bytes */{len}"))
            .header("Access-Control-Allow-Origin", "*")
            .body(Vec::new())
            .unwrap();
    }
    let end = want_end.min(len - 1).min(start + VIDEO_CHUNK - 1);

    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return status(404),
    };
    let mut buf = vec![0u8; (end - start + 1) as usize];
    let read_ok = file
        .seek(SeekFrom::Start(start))
        .and_then(|_| file.read_exact(&mut buf));
    if read_ok.is_err() {
        return status(404);
    }
    Response::builder()
        .status(206)
        .header("Content-Type", mime)
        .header("Accept-Ranges", "bytes")
        .header("Content-Range", format!("bytes {start}-{end}/{len}"))
        .header("Content-Length", buf.len().to_string())
        .header("Cache-Control", "no-store")
        .header("Access-Control-Allow-Origin", "*")
        .body(buf)
        .unwrap()
}

/// "bytes=100-200" → (100, Some(200)); "bytes=100-" → (100, None).
/// Suffix range "bytes=-500" và multi-range không hỗ trợ → None (trả từ đầu).
fn parse_range(h: &str) -> Option<(u64, Option<u64>)> {
    let spec = h.trim().strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (start, end) = spec.split_once('-')?;
    let start: u64 = start.trim().parse().ok()?;
    let end = end.trim();
    if end.is_empty() {
        Some((start, None))
    } else {
        let e: u64 = end.parse().ok()?;
        if e < start {
            return None;
        }
        Some((start, Some(e)))
    }
}

fn lookup(db: &core_db::Db, id: i64) -> Option<MediaSrc> {
    db.pool
        .with(|c| core_db::ops::get_media_src(c, id))
        .ok()
        .flatten()
}

/// "/123" → 123
fn parse_id(request: &Request<Vec<u8>>) -> Option<i64> {
    request.uri().path().trim_start_matches('/').parse().ok()
}

fn query_param(request: &Request<Vec<u8>>, key: &str) -> Option<u32> {
    request
        .uri()
        .query()?
        .split('&')
        .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
        .and_then(|v| v.parse().ok())
}

fn mime_for(ext: &str) -> &'static str {
    match ext {
        "jpg" | "jpeg" | "jfif" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        // mov dùng chung container mp4 — WebView2 phát được nếu codec hỗ trợ
        "mp4" | "m4v" | "mov" => "video/mp4",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "mpg" | "mpeg" => "video/mpeg",
        "3gp" => "video/3gpp",
        _ => "application/octet-stream",
    }
}

fn ok(body: Vec<u8>, mime: &str) -> Response<Vec<u8>> {
    Response::builder()
        .status(200)
        .header("Content-Type", mime)
        .header("Cache-Control", "public, max-age=31536000, immutable")
        .header("Access-Control-Allow-Origin", "*")
        .body(body)
        .unwrap()
}

fn status(code: u16) -> Response<Vec<u8>> {
    Response::builder()
        .status(code)
        .header("Access-Control-Allow-Origin", "*")
        .body(Vec::new())
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::parse_range;

    #[test]
    fn range_header_parses() {
        assert_eq!(parse_range("bytes=0-499"), Some((0, Some(499))));
        assert_eq!(parse_range("bytes=500-"), Some((500, None)));
        assert_eq!(parse_range("bytes=-500"), None); // suffix: khong ho tro
        assert_eq!(parse_range("bytes=0-1,5-9"), None); // multi: khong ho tro
        assert_eq!(parse_range("bytes=9-5"), None); // nguoc: bo
        assert_eq!(parse_range("chunks=0-1"), None);
    }
}
