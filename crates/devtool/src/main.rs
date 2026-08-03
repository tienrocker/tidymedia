//! devtool: sinh corpus synthetic để test scan/search/dedup.
//!
//! usage:
//!   devtool gen-tree --root <path> --files <n> [--dupe-sets <n>]
//!
//! Tạo cây <root>/<year>/<month>/ với tên file kiểu iPhone/máy ảnh/chat-app,
//! mtime rải 2010–2025. Với --dupe-sets, tạo các bộ file nội dung giống hệt
//! nằm rải rác + ghi manifest `dupes-manifest.jsonl` ở root để test M4.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use filetime::FileTime;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64* — đủ tốt cho data test, không cần crate rand
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const MTIME_MIN: i64 = 1_262_304_000; // 2010-01-01
const MTIME_MAX: i64 = 1_767_139_200; // 2025-12-31

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("gen-tree") => gen_tree(&args[1..]),
        _ => {
            eprintln!("usage: devtool gen-tree --root <path> --files <n> [--dupe-sets <n>]");
            std::process::exit(2);
        }
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn gen_tree(args: &[String]) -> Result<()> {
    let root = PathBuf::from(flag(args, "--root").context("--root <path> là bắt buộc")?);
    let files: u64 = flag(args, "--files")
        .context("--files <n> là bắt buộc")?
        .parse()?;
    let dupe_sets: u64 = flag(args, "--dupe-sets")
        .unwrap_or_default()
        .parse()
        .unwrap_or(0);

    if root.as_os_str().is_empty() {
        bail!("--root rỗng");
    }
    fs::create_dir_all(&root)?;
    let mut rng = Rng(0x9E3779B97F4A7C15);

    let started = std::time::Instant::now();
    for i in 0..files {
        let (path, mtime) = plan_file(&root, i, &mut rng)?;
        let mut content = vec![0u8; 64 + (rng.below(192)) as usize];
        rng_fill(&mut rng, &mut content, i);
        write_with_mtime(&path, &content, mtime)?;
        if i > 0 && i % 20_000 == 0 {
            eprintln!(
                "  {i}/{files} files… ({:.1}s)",
                started.elapsed().as_secs_f32()
            );
        }
    }

    if dupe_sets > 0 {
        let manifest_path = root.join("dupes-manifest.jsonl");
        let mut manifest = fs::File::create(&manifest_path)?;
        for s in 0..dupe_sets {
            let copies = 2 + rng.below(3); // 2..=4 bản
            let mut content = vec![0u8; 256 + rng.below(512) as usize];
            rng_fill(&mut rng, &mut content, u64::MAX - s);
            let mut paths = Vec::new();
            for c in 0..copies {
                let (path, mtime) = plan_file(&root, files + s * 8 + c, &mut rng)?;
                write_with_mtime(&path, &content, mtime)?;
                paths.push(path.to_string_lossy().replace('\\', "\\\\"));
            }
            writeln!(
                manifest,
                "{{\"set\":{s},\"paths\":[{}]}}",
                paths
                    .iter()
                    .map(|p| format!("\"{p}\""))
                    .collect::<Vec<_>>()
                    .join(",")
            )?;
        }
        eprintln!("dupes manifest: {}", manifest_path.display());
    }

    eprintln!(
        "done: {} files (+{} dupe sets) in {:.1}s at {}",
        files,
        dupe_sets,
        started.elapsed().as_secs_f32(),
        root.display()
    );
    Ok(())
}

/// Chọn thư mục năm/tháng + tên file "đời thật" (không nhất quán có chủ đích).
fn plan_file(root: &Path, i: u64, rng: &mut Rng) -> Result<(PathBuf, i64)> {
    let mtime = MTIME_MIN + (rng.below((MTIME_MAX - MTIME_MIN) as u64)) as i64;
    let days = mtime / 86_400;
    let year = 1970 + days / 365; // xấp xỉ là đủ cho cây thư mục test
    let month = 1 + (days % 365) / 31;
    let dir = root
        .join(format!("{year}"))
        .join(format!("{year}-{month:02}"));
    fs::create_dir_all(&dir)?;

    let name = match i % 7 {
        0 => format!("IMG_{:04}_{i}.JPG", rng.below(10_000)),
        1 => format!("IMG_{:04}_{i}.HEIC", rng.below(10_000)),
        2 => format!("DSC_{:05}_{i}.jpg", rng.below(100_000)),
        3 => format!(
            "IMG-{year}{month:02}{:02}-WA{:04}_{i}.jpg",
            1 + rng.below(28),
            rng.below(10_000)
        ),
        4 => format!("video_{i}.mp4"),
        5 => format!(
            "Screenshot {year}-{month:02}-{:02} {i}.png",
            1 + rng.below(28)
        ),
        _ => format!("ảnh chụp {i}.jpg"),
    };
    Ok((dir.join(name), mtime))
}

fn rng_fill(rng: &mut Rng, buf: &mut [u8], seed: u64) {
    let mut local = Rng(rng.next() ^ seed.wrapping_mul(0x9E37_79B9));
    for chunk in buf.chunks_mut(8) {
        let v = local.next().to_le_bytes();
        let n = chunk.len();
        chunk.copy_from_slice(&v[..n]);
    }
}

fn write_with_mtime(path: &Path, content: &[u8], mtime_secs: i64) -> Result<()> {
    fs::write(path, content)?;
    filetime::set_file_mtime(path, FileTime::from_unix_time(mtime_secs, 0))?;
    Ok(())
}
