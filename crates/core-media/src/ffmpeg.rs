use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Result};
use image::DynamicImage;

/// Tìm ffmpeg: cạnh exe → exe\binaries → PATH. M3 sẽ bundle sidecar vào
/// binaries\ — code này tự nhặt được mà không cần sửa.
pub fn find_ffmpeg() -> Option<PathBuf> {
    let exe_name = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for cand in [dir.join(exe_name), dir.join("binaries").join(exe_name)] {
                if cand.is_file() {
                    return Some(cand);
                }
            }
        }
    }
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|d| d.join(exe_name))
        .find(|c| c.is_file())
}

/// Decode 1 frame ảnh qua ffmpeg (HEIC/AVIF/JXL... mà crate `image` không đọc),
/// scale xuống lọt hộp `box_px` ngay trong ffmpeg (không kéo 48MP qua pipe).
/// `min(iw,N)` để không bao giờ upscale ảnh nhỏ.
pub fn decode_scaled(ffmpeg: &Path, input: &Path, box_px: u32) -> Result<DynamicImage> {
    let filter = format!(
        "scale=w='min(iw\\,{box_px})':h='min(ih\\,{box_px})':force_original_aspect_ratio=decrease"
    );
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-v", "error", "-nostdin", "-i"])
        .arg(input)
        .args(["-frames:v", "1", "-vf", &filter])
        .args(["-f", "image2pipe", "-c:v", "png", "-"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd.output()?;
    if !out.status.success() || out.stdout.is_empty() {
        bail!(
            "ffmpeg decode failed ({}): {}",
            input.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(image::load_from_memory(&out.stdout)?)
}
