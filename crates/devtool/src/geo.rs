//! Sinh bộ dữ liệu địa điểm offline cho `core-geo` từ dump của GeoNames.
//!
//! Chạy TAY khi muốn cập nhật dữ liệu, không phải mỗi lần build — kết quả được
//! commit vào repo để build không cần mạng và ai clone về cũng dựng được y hệt.
//!
//! Tải về cùng một thư mục, từ <https://download.geonames.org/export/dump/>:
//!
//! | File | Dùng cho |
//! |---|---|
//! | `cities1000.zip` (giải nén) | tầng thành phố toàn cầu |
//! | `admin1CodesASCII.txt` | tên cấp tỉnh |
//! | `countryInfo.txt` | tên quốc gia |
//! | `VN.zip` (giải nén, cho `--fine VN`) | phường/xã + quận/huyện |
//! | `alternatenames/VN.txt` (từ `alternatenames/VN.zip`) | tên bản địa |
//!
//! ```text
//! cargo run -p devtool --release -- gen-geo \
//!     --src <thư mục dump> \
//!     --fine VN:vi \
//!     --admin1-override crates/core-geo/data/admin1-override.tsv \
//!     --out crates/core-geo/data/places.tsv
//! ```
//!
//! `--fine CC:lang` = thêm tầng phường/xã cho nước CC và ưu tiên tên theo ngôn
//! ngữ `lang`. Thiếu nó thì `{place}` ra "Hanoi" mà `{ward}` ra
//! "Phường Lý Thái Tổ" — lệch ngôn ngữ ngay trong một đường dẫn.
//!
//! Dữ liệu GeoNames dùng theo giấy phép CC BY 4.0 — ghi nguồn trong README.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use unicode_normalization::UnicodeNormalization;

/// Toạ độ lưu dưới dạng số nguyên đơn vị 1e-4 độ (~11 m) — đủ xa hơn nhiều so
/// với sai số của chính việc "chọn thành phố gần nhất", mà ngắn hơn 2 ký tự
/// mỗi số so với 1e-5 (tiết kiệm ~700 KB trên 170k dòng).
pub const COORD_SCALE: f64 = 10_000.0;

/// Chuẩn hoá tên trước khi ghi ra. Dump của GeoNames TRỘN hai dạng Unicode:
/// `Xã An Hải` nằm ở dạng tách dấu (base + dấu tổ hợp), chỗ khác lại liền.
/// Hai chuỗi đó nhìn y hệt nhau nhưng KHÔNG bằng nhau — đủ để sinh ra hai thư
/// mục trông giống hệt cạnh nhau, và để so sánh đường dẫn trượt. NFC là dạng
/// Windows và mọi công cụ khác sinh ra, nên ép hết về NFC ngay tại nguồn.
///
/// Cũng gộp khoảng trắng thừa: tên có 2 space liên tiếp render ra thư mục nhìn
/// như lỗi đánh máy.
fn norm(s: &str) -> String {
    let nfc: String = s.trim().nfc().collect();
    nfc.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Bảng chữ có `ð` là chữ THẬT — không được "sửa" chữ của người ta.
const HAS_ETH: &[&str] = &["IS", "FO"];

/// [`norm`] + chữa chữ Đ bị ghi bằng mã sai, cho tầng dữ liệu bản địa.
///
/// 5 tên Việt trong dump viết Đ bằng `Ð` U+00D0 — eth của tiếng Iceland — thay
/// vì `Đ` U+0110: "Huyện Ðông Hưng". Hai ký tự HIỂN THỊ y hệt nhau nhưng khác
/// byte, đúng cái bẫy NFC/NFD ở trên, nên `Huyện Ðông Hưng` và
/// `Huyện Đông Hưng` là hai thư mục mà mắt không phân biệt được.
///
/// Không riêng tiếng Việt: `Ðurići`, `Selci Ðakovački`, `Ðurđevo` của Croatia
/// cũng vậy — bảng chữ Croatia có Đ chứ không có eth. Nên chữa theo mã nước của
/// TỪNG dòng, chỉ chừa các nước dùng eth thật ([`HAS_ETH`]).
fn norm_native(s: &str, cc: &str) -> String {
    let n = norm(s);
    if HAS_ETH.contains(&cc) {
        n
    } else {
        n.replace('\u{d0}', "\u{110}").replace('\u{f0}', "\u{111}")
    }
}

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
        countries.push((f[0].to_string(), norm(f[4])));
    }

    // Tên bản địa: GeoNames để `name` của thành phố lớn ở dạng phổ thông
    // ("Hanoi"), tên tiếng Việt nằm trong dump alternatenames. Không có bảng
    // này thì `{place}` ra "Hanoi" còn `{ward}` ra "Phường Lý Thái Tổ" — lệch
    // ngôn ngữ ngay trong một đường dẫn.
    let mut local: HashMap<i64, String> = HashMap::new();
    for spec in super::flag(args, "--fine")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let Some((cc, lang)) = spec.split_once(':') else {
            continue;
        };
        let path = src.join("alternatenames").join(format!("{cc}.txt"));
        if !path.is_file() {
            tracing_warn(&format!("khong co {} - bo qua ten ban dia", path.display()));
            continue;
        }
        // alternateNameId, geonameid, isolanguage, name, isPreferredName, ...
        for line in read_lines(&path)? {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() < 4 || f[2] != lang || f[3].trim().is_empty() {
                continue;
            }
            let Ok(id) = f[1].parse::<i64>() else {
                continue;
            };
            let preferred = f.get(4).is_some_and(|v| *v == "1");
            // Bản "preferred" thắng tuyệt đối; ngoài ra lấy bản đầu gặp
            if preferred || !local.contains_key(&id) {
                local.insert(id, norm_native(f[3], cc));
            }
        }
    }

    // admin1CodesASCII.txt: "CC.ADM1<TAB>name<TAB>asciiname<TAB>geonameid".
    // Lấy cột `name` (có dấu) chứ KHÔNG phải asciiname: GeoNames chuyển chữ Đ
    // thành "GJ" ("Hàng Đào" -> "Hang GJao"), đặt tên thư mục kiểu đó là rác.
    // Bảng chuẩn hoá tay, khoá theo mã admin1 — xem đầu file override để biết
    // vì sao cần: GeoNames ghi cấp tỉnh không đồng nhất tới mức không luật tự
    // động nào gỡ được (20/34 tỉnh VN không có dấu, số còn lại dính "Province").
    let mut override_admin1: HashMap<String, String> = HashMap::new();
    if let Some(p) = super::flag(args, "--admin1-override") {
        for line in read_lines(std::path::Path::new(&p))? {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            if let Some((code, name)) = line.split_once('\t') {
                override_admin1.insert(code.trim().to_string(), norm(name));
            }
        }
    }

    let mut admin1: Vec<String> = Vec::new();
    let mut admin1_idx: HashMap<String, usize> = HashMap::new();
    for line in read_lines(&src.join("admin1CodesASCII.txt"))? {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 3 {
            continue;
        }
        let id = f.get(3).and_then(|s| s.parse::<i64>().ok()).unwrap_or(-1);
        let cc = f[0].split('.').next().unwrap_or("");
        // Thứ tự ưu tiên: bảng tay > tên bản địa từ alternatenames > tên gốc
        let name = override_admin1
            .get(f[0])
            .cloned()
            .or_else(|| local.get(&id).cloned())
            .unwrap_or_else(|| strip_admin_suffix(&norm_native(f[1], cc)));
        admin1_idx.insert(f[0].to_string(), admin1.len());
        admin1.push(name);
    }

    // cities1000.txt: 1=name 4=lat 5=lon 8=country 10=admin1 14=population
    let mut cities: Vec<(i32, i32, String, usize, usize, u32)> = Vec::new();
    let mut skipped = 0usize;
    for line in read_lines(&src.join("cities1000.txt"))? {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 15 {
            continue;
        }
        let id = f[0].parse::<i64>().unwrap_or(-1);
        let cc = f[8].trim();
        // Tên bản địa nếu có, không thì tên phổ thông của GeoNames
        let name = local
            .get(&id)
            .cloned()
            .unwrap_or_else(|| norm_native(f[1], cc));
        // Tên rỗng thì bỏ (nước lạ cũng bỏ)
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
            name,
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
    for spec in super::flag(args, "--fine")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let cc = spec.split(':').next().unwrap_or(spec);
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
            districts.push(norm_native(f[1], cc));
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
                norm_native(f[1], cc),
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

/// devtool không dựng tracing subscriber — in thẳng ra stderr.
fn tracing_warn(msg: &str) {
    eprintln!("canh bao: {msg}");
}

/// Bỏ hậu tố tiếng Anh mà GeoNames đính vào tên cấp tỉnh ("Nghệ An Province").
/// Chỉ cắt đuôi, không đụng vào tên — "Province" là phần mô tả cấp hành chính
/// chứ không phải phần tên, mà cấp đó đã nằm ở vị trí thư mục rồi.
fn strip_admin_suffix(s: &str) -> String {
    for suffix in [
        " Province",
        " Municipality",
        " City",
        " Prefecture",
        " Region",
        " County",
    ] {
        if let Some(head) = s.strip_suffix(suffix) {
            if !head.trim().is_empty() {
                return head.trim().to_string();
            }
        }
    }
    s.to_string()
}
