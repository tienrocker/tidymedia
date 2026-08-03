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
        Some("bench-scan") => bench_scan(&args[1..]),
        _ => {
            eprintln!(
                "usage:\n  devtool gen-tree --root <path> --files <n> [--dupe-sets <n>]\n  devtool bench-scan --root <path> --db <dir>"
            );
            std::process::exit(2);
        }
    }
}

/// M1 verification: scan root vào 1 db tạm + đo query latency.
fn bench_scan(args: &[String]) -> Result<()> {
    let root = PathBuf::from(flag(args, "--root").context("--root <path> là bắt buộc")?);
    let db_dir = PathBuf::from(flag(args, "--db").context("--db <dir> là bắt buộc")?);

    let db = core_db::Db::open(&db_dir)?;
    // Canonical hóa như app thật (giải alias/case, bỏ \\?\ prefix)
    let canon = fs::canonicalize(&root)?;
    let canon_str = canon.to_string_lossy();
    let root_str = canon_str
        .strip_prefix(r"\\?\")
        .unwrap_or(&canon_str)
        .to_string();
    let root_id = {
        let rs = root_str.clone();
        db.writer.exec(move |c| core_db::ops::upsert_root(c, &rs))?
    };
    let (root_path, volume_id) = db.pool.with(|c| core_db::ops::get_root(c, root_id))?;

    let gen = db.writer.exec(|c| core_db::ops::next_scan_gen(c))?;
    let cancel = core_index::CancelFlag::default();
    let t0 = std::time::Instant::now();
    let summary = core_index::scan_root(
        Path::new(&root_path),
        volume_id,
        gen,
        &db.writer,
        &cancel,
        &[],
        |done| eprintln!("  scanned {done} ({:.1}s)", t0.elapsed().as_secs_f32()),
    )?;
    let scan_s = t0.elapsed().as_secs_f32();
    println!(
        "SCAN: {} files indexed, {} missing, {} walk-errors, {} lossy-skipped, {:.2}s ({:.0} files/s)",
        summary.indexed,
        summary.marked_missing,
        summary.walk_errors,
        summary.skipped_lossy_names,
        scan_s,
        summary.indexed as f32 / scan_s.max(0.001)
    );

    // Query benchmarks: các pattern search chính
    let cases: &[(&str, core_db::FileFilter)] = &[
        ("all (no filter)", core_db::FileFilter::default()),
        (
            "text 1 char 'i'",
            core_db::FileFilter {
                text: Some("i".into()),
                ..Default::default()
            },
        ),
        (
            "text 3 chars 'img'",
            core_db::FileFilter {
                text: Some("img".into()),
                ..Default::default()
            },
        ),
        (
            "text 8 chars 'creensho'",
            core_db::FileFilter {
                text: Some("creensho".into()),
                ..Default::default()
            },
        ),
        (
            "videos > 100 bytes, sort size",
            core_db::FileFilter {
                kind: Some(1),
                size_min: Some(100),
                sort: Some("size_desc".into()),
                ..Default::default()
            },
        ),
    ];
    for (label, filter) in cases {
        // warm + đo lần 2 (lần 1 dính page-cache lạnh)
        let _ = db.pool.with(|c| core_db::query::query_ids(c, filter))?;
        let t = std::time::Instant::now();
        let ids = db.pool.with(|c| core_db::query::query_ids(c, filter))?;
        println!(
            "QUERY [{label}]: {} rows in {:.1}ms",
            ids.len(),
            t.elapsed().as_secs_f64() * 1000.0
        );
    }

    // fetch_rows cửa sổ giữa list
    let ids = db
        .pool
        .with(|c| core_db::query::query_ids(c, &core_db::FileFilter::default()))?;
    if ids.len() > 300 {
        let mid = ids.len() / 2;
        let window = ids[mid..mid + 200].to_vec();
        let t = std::time::Instant::now();
        let rows = db.pool.with(|c| core_db::query::fetch_rows(c, &window))?;
        println!(
            "FETCH window 200 rows @ middle: {} rows in {:.1}ms",
            rows.len(),
            t.elapsed().as_secs_f64() * 1000.0
        );
    }
    Ok(())
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
