//! Sinh bộ dữ liệu địa điểm offline cho `core-geo` từ dump của GeoNames.
//!
//! Chạy TAY khi muốn cập nhật dữ liệu, không phải mỗi lần build — kết quả được
//! commit vào repo để build không cần mạng và ai clone về cũng dựng được y hệt.
//!
//! ```text
//! # tải 3 file từ https://download.geonames.org/export/dump/
//! #   cities1000.zip (giải nén), admin1CodesASCII.txt, countryInfo.txt
//! cargo run -p devtool --release -- gen-geo --src <thư mục> --out crates/core-geo/data/places.tsv
//! ```
//!
//! Dữ liệu GeoNames dùng theo giấy phép CC BY 4.0 — ghi nguồn trong README.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};

/// Toạ độ lưu dưới dạng số nguyên đơn vị 1e-4 độ (~11 m) — đủ xa hơn nhiều so
/// với sai số của chính việc "chọn thành phố gần nhất", mà ngắn hơn 2 ký tự
/// mỗi số so với 1e-5 (tiết kiệm ~700 KB trên 170k dòng).
pub const COORD_SCALE: f64 = 10_000.0;

pub fn gen_geo(args: &[String]) -> Result<()> {
    let src = PathBuf::from(super::flag(args, "--src").context("--src <thư mục dump> bắt buộc")?);
    let out = PathBuf::from(super::flag(args, "--out").context("--out <file> bắt buộc")?);

    // countryInfo.txt: dòng comment bắt đầu bằng '#'; cột 0 = ISO, 4 = tên
    let mut countries: Vec<(String, String)> = Vec::new();
    let mut country_idx: HashMap<String, usize> = HashMap::new();
    for line in read_lines(&src.join("countryInfo.txt"))? {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 5 {
            continue;
        }
        country_idx.insert(f[0].to_string(), countries.len());
        countries.push((f[0].to_string(), f[4].to_string()));
    }

    // admin1CodesASCII.txt: "CC.ADM1<TAB>name<TAB>asciiname<TAB>geonameid".
    // Lấy cột `name` (có dấu) chứ KHÔNG phải asciiname: GeoNames chuyển chữ Đ
    // thành "GJ" ("Hàng Đào" -> "Hang GJao"), đặt tên thư mục kiểu đó là rác.
    let mut admin1: Vec<String> = Vec::new();
    let mut admin1_idx: HashMap<String, usize> = HashMap::new();
    for line in read_lines(&src.join("admin1CodesASCII.txt"))? {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 3 {
            continue;
        }
        admin1_idx.insert(f[0].to_string(), admin1.len());
        admin1.push(f[1].to_string());
    }

    // cities1000.txt: 1=name 4=lat 5=lon 8=country 10=admin1 14=population
    let mut cities: Vec<(i32, i32, String, usize, usize, u32)> = Vec::new();
    let mut skipped = 0usize;
    for line in read_lines(&src.join("cities1000.txt"))? {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 15 {
            continue;
        }
        let (name, cc) = (f[1].trim(), f[8].trim());
        // Tên rỗng hoặc có TAB/xuống dòng sẽ phá format; nước lạ thì bỏ
        let (Some(&ci), false) = (country_idx.get(cc), name.is_empty()) else {
            skipped += 1;
            continue;
        };
        let (Ok(lat), Ok(lon)) = (f[4].parse::<f64>(), f[5].parse::<f64>()) else {
            skipped += 1;
            continue;
        };
        let ai = admin1_idx
            .get(&format!("{cc}.{}", f[10].trim()))
            .copied()
            .unwrap_or(usize::MAX);
        cities.push((
            (lat * COORD_SCALE).round() as i32,
            (lon * COORD_SCALE).round() as i32,
            name.to_string(),
            ci,
            ai,
            // Lưu theo NGHÌN người: chỉ dùng để so ai to hơn, mất chi tiết
            // dưới 1000 dân không đổi kết quả mà ngắn được 3 ký tự mỗi dòng.
            (f[14].parse::<u64>().unwrap_or(0) / 1000) as u32,
        ));
    }
    if cities.is_empty() {
        bail!("khong doc duoc thanh pho nao tu {}", src.display());
    }
    // Sắp theo vĩ độ: `core-geo` nhị phân tìm dải vĩ độ rồi mới quét ngang
    cities.sort_by_key(|c| c.0);

    // Tầng CHI TIẾT (phường/xã) cho các nước liệt kê ở --fine, lấy từ dump
    // riêng của nước đó (`VN.txt`). Bộ cities1000 toàn cầu chỉ có 905 điểm ở
    // VN (~19 km/điểm) trong khi ADM3 có đủ 10.579 phường/xã (~5 km/điểm),
    // mà chỉ tốn thêm ~0,5 MB. Không làm cho cả thế giới: allCountries là
    // 400 MB, và ngoài nước user đang ở thì độ chi tiết đó vô dụng.
    // (lat, lon, tên phường/xã, quốc gia, quận/huyện) — quận là INDEX vào bảng
    // tên riêng, không phải vào chính mảng này: bên dưới còn sort lại theo vĩ độ.
    let mut fine: Vec<(i32, i32, String, usize, i64)> = Vec::new();
    let mut districts: Vec<String> = Vec::new();
    for cc in super::flag(args, "--fine")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let Some(&ci) = country_idx.get(cc) else {
            bail!("--fine {cc}: khong co trong countryInfo.txt");
        };
        let lines = read_lines(&src.join(format!("{cc}.txt")))?;
        // Quận/huyện trước: phường/xã tra ngược qua cặp mã (admin1, admin2)
        let mut by_code: HashMap<(String, String), usize> = HashMap::new();
        for line in &lines {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() < 15 || f[7] != "ADM2" || f[1].trim().is_empty() {
                continue;
            }
            by_code.insert(
                (f[10].trim().to_string(), f[11].trim().to_string()),
                districts.len(),
            );
            districts.push(f[1].trim().to_string());
        }
        for line in &lines {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() < 15 || f[7] != "ADM3" || f[1].trim().is_empty() {
                continue;
            }
            let (Ok(lat), Ok(lon)) = (f[4].parse::<f64>(), f[5].parse::<f64>()) else {
                skipped += 1;
                continue;
            };
            let d = by_code
                .get(&(f[10].trim().to_string(), f[11].trim().to_string()))
                .map(|i| *i as i64)
                .unwrap_or(-1);
            fine.push((
                (lat * COORD_SCALE).round() as i32,
                (lon * COORD_SCALE).round() as i32,
                f[1].trim().to_string(),
                ci,
                d,
            ));
        }
    }
    fine.sort_by_key(|c| c.0);

    if let Some(dir) = out.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut w = std::io::BufWriter::new(fs::File::create(&out)?);
    writeln!(w, "#TIDYMEDIA-PLACES\t1")?;
    writeln!(w, "#COUNTRIES\t{}", countries.len())?;
    for (code, name) in &countries {
        writeln!(w, "{code}\t{name}")?;
    }
    writeln!(w, "#ADMIN1\t{}", admin1.len())?;
    for name in &admin1 {
        writeln!(w, "{name}")?;
    }
    writeln!(w, "#DISTRICTS\t{}", districts.len())?;
    for name in &districts {
        writeln!(w, "{name}")?;
    }
    writeln!(w, "#CITIES\t{}", cities.len())?;
    for (lat, lon, name, ci, ai, pop_k) in &cities {
        let ai = if *ai == usize::MAX { -1i64 } else { *ai as i64 };
        writeln!(w, "{lat}\t{lon}\t{name}\t{ci}\t{ai}\t{pop_k}")?;
    }
    writeln!(w, "#FINE\t{}", fine.len())?;
    for (lat, lon, name, ci, di) in &fine {
        writeln!(w, "{lat}\t{lon}\t{name}\t{ci}\t{di}")?;
    }
    w.flush()?;
    let bytes = fs::metadata(&out)?.len();
    println!(
        "{} thanh pho, {} quoc gia, {} tinh, {} phuong/xa, {} quan/huyen -> {} ({:.2} MB), bo qua {skipped}",
        cities.len(),
        countries.len(),
        admin1.len(),
        fine.len(),
        districts.len(),
        out.display(),
        bytes as f64 / 1_048_576.0
    );
    Ok(())
}

/// GeoNames là UTF-8, nhưng dump thỉnh thoảng có dòng lỗi encoding — đọc
/// lossy để một dòng hỏng không giết cả lượt sinh.
fn read_lines(p: &std::path::Path) -> Result<Vec<String>> {
    let raw = fs::read(p).with_context(|| format!("doc {}", p.display()))?;
    Ok(String::from_utf8_lossy(&raw)
        .lines()
        .map(|s| s.to_string())
        .collect())
}
