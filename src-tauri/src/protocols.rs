//! Protocol thumb:// và media:// — WebView2 truy cập qua http://thumb.localhost/…
//!
//! URL luôn kèm ?v=<mtime> → response được cache "immutable" vô hạn phía WebView2;
//! file đổi thì mtime đổi → URL đổi → tự miss. Không cần ETag/304.
//!
//! Handler KHÔNG được block thread webview: mọi việc (DB lookup, decode, đọc file)
//! dispatch sang thumb_pool rồi trả lời qua responder.

use std::path::Path;

use core_db::MediaSrc;
use tauri::http::{Request, Response};
use tauri::{AppHandle, Manager, UriSchemeResponder};

use crate::state::AppState;

/// File media:// to hơn mức này (0.5GB) → 404 thay vì nuốt RAM (ảnh hợp lệ
/// không bao giờ tới cỡ đó; video đi đường Range riêng ở M3).
const MAX_MEDIA_BYTES: i64 = 512 * 1024 * 1024;

pub fn spawn_thumb(app: AppHandle, request: Request<Vec<u8>>, responder: UriSchemeResponder) {
    let state = app.state::<AppState>();
    let db = state.db.clone();
    let thumbs = state.thumbs.clone();
    let ffmpeg = state.ffmpeg.clone();
    state.thumb_pool.spawn(move || {
        let resp = thumb_response(&db, &thumbs, ffmpeg.as_deref(), &request);
        responder.respond(resp);
    });
}

pub fn spawn_media(app: AppHandle, request: Request<Vec<u8>>, responder: UriSchemeResponder) {
    let state = app.state::<AppState>();
    let db = state.db.clone();
    state.thumb_pool.spawn(move || {
        let resp = media_response(&db, &request);
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
    // đọc là kéo hydrate cả OneDrive). Video chưa có keyframe thumb (M3).
    if src.status != 0 || src.kind != 0 {
        return status(404);
    }

    if let Some(data) = thumbs.get(id, s, src.mtime) {
        return ok(data, "image/webp");
    }

    let ext = src.ext.as_deref().unwrap_or("");
    match core_media::make_thumb(Path::new(&src.path), ext, s, ffmpeg) {
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

fn media_response(db: &core_db::Db, request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    let Some(id) = parse_id(request) else {
        return status(400);
    };
    let src = match lookup(db, id) {
        Some(s) => s,
        None => return status(404),
    };
    if src.status != 0 || src.size > MAX_MEDIA_BYTES {
        return status(404);
    }
    match std::fs::read(&src.path) {
        Ok(bytes) => ok(bytes, mime_for(src.ext.as_deref().unwrap_or(""))),
        Err(e) => {
            tracing::debug!(id, path = %src.path, "media read failed: {e}");
            status(404)
        }
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
