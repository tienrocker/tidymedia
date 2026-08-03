use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use image::DynamicImage;

/// ffmpeg/ffprobe treo (file hỏng, ổ mạng chập chờn) không được phép chiếm
/// thread của thumb pool vĩnh viễn — quá hạn là kill child.
const TOOL_TIMEOUT: Duration = Duration::from_secs(20);

/// Tìm tool cạnh exe → exe\binaries (bundle resources) → PATH.
fn find_tool(name: &str) -> Option<PathBuf> {
    let exe_name = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for cand in [dir.join(&exe_name), dir.join("binaries").join(&exe_name)] {
                if cand.is_file() {
                    return Some(cand);
                }
            }
        }
    }
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|d| d.join(&exe_name))
        .find(|c| c.is_file())
}

pub fn find_ffmpeg() -> Option<PathBuf> {
    find_tool("ffmpeg")
}

pub fn find_ffprobe() -> Option<PathBuf> {
    find_tool("ffprobe")
}

/// Chạy tool với timeout + kill; stdout/stderr đọc trên thread riêng (child ghi
/// nhiều hơn buffer pipe mà mình chỉ ngồi wait là deadlock kinh điển).
pub(crate) fn run_with_timeout(mut cmd: Command, what: &str) -> Result<(Vec<u8>, String)> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn()?;

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

    let deadline = Instant::now() + TOOL_TIMEOUT;
    let status = loop {
        match child.try_wait()? {
            Some(st) => break st,
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    bail!("{what} timeout sau {}s", TOOL_TIMEOUT.as_secs());
                }
                std::thread::sleep(Duration::from_millis(30));
            }
        }
    };
    let stdout_data = out_reader.join().unwrap_or_default();
    let stderr_text = err_reader.join().unwrap_or_default();
    if !status.success() {
        bail!("{what} failed: {}", stderr_text.trim());
    }
    Ok((stdout_data, stderr_text))
}

/// Decode 1 frame qua ffmpeg (HEIC/AVIF/JXL... hoặc keyframe video khi có `ss`),
/// scale lọt hộp `box_px` ngay trong ffmpeg (không kéo full-res qua pipe).
/// `min(iw,N)` để không bao giờ upscale ảnh nhỏ.
pub fn decode_scaled(
    ffmpeg: &Path,
    input: &Path,
    box_px: u32,
    ss_secs: Option<f64>,
) -> Result<DynamicImage> {
    let filter = format!(
        "scale=w='min(iw\\,{box_px})':h='min(ih\\,{box_px})':force_original_aspect_ratio=decrease"
    );
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-v", "error", "-nostdin"]);
    if let Some(ss) = ss_secs {
        // -ss TRƯỚC -i = seek theo keyframe, nhanh cả với file 4GB
        cmd.args(["-ss", &format!("{ss:.2}")]);
    }
    cmd.arg("-i")
        .arg(input)
        .args(["-frames:v", "1", "-an", "-vf", &filter])
        .args(["-f", "image2pipe", "-c:v", "png", "-"]);
    let (stdout_data, _) = run_with_timeout(cmd, "ffmpeg decode")?;
    if stdout_data.is_empty() {
        bail!("ffmpeg decode: khong ra frame nao ({})", input.display());
    }
    Ok(image::load_from_memory(&stdout_data)?)
}
