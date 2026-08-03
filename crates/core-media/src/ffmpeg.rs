use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use image::DynamicImage;

/// ffmpeg treo (file hỏng, ổ mạng chập chờn) không được phép chiếm thread
/// của thumb pool vĩnh viễn — quá hạn là kill child.
const FFMPEG_TIMEOUT: Duration = Duration::from_secs(20);

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
        .args(["-f", "image2pipe", "-c:v", "png", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn()?;

    // Đọc pipe trên thread riêng — child ghi nhiều hơn buffer pipe mà mình
    // chỉ ngồi wait là deadlock kinh điển.
    let mut stdout_pipe = child.stdout.take().expect("stdout piped");
    let out_reader = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut v);
        v
    });
    let mut stderr_pipe = child.stderr.take().expect("stderr piped");
    let err_reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr_pipe.read_to_string(&mut s);
        s
    });

    let deadline = Instant::now() + FFMPEG_TIMEOUT;
    let status = loop {
        match child.try_wait()? {
            Some(st) => break st,
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    bail!(
                        "ffmpeg timeout sau {}s ({})",
                        FFMPEG_TIMEOUT.as_secs(),
                        input.display()
                    );
                }
                std::thread::sleep(Duration::from_millis(30));
            }
        }
    };
    let stdout_data = out_reader.join().unwrap_or_default();
    let stderr_text = err_reader.join().unwrap_or_default();
    if !status.success() || stdout_data.is_empty() {
        bail!(
            "ffmpeg decode failed ({}): {}",
            input.display(),
            stderr_text.trim()
        );
    }
    Ok(image::load_from_memory(&stdout_data)?)
}
