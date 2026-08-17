//! Đổi toạ độ GPS thành tên địa điểm, HOÀN TOÀN OFFLINE.
//!
//! Không gọi mạng, không API key, không rate limit — một app dọn ảnh không có
//! quyền gửi toạ độ nhà user lên máy chủ của người khác chỉ để đặt tên thư mục.
//! Dữ liệu là bộ `cities1000` của GeoNames (CC BY 4.0) đã cắt gọn, kèm tầng
//! phường/xã cho Việt Nam — tổng 6,3 MB nhúng thẳng vào binary. Sinh lại bằng
//! `devtool gen-geo` (xem `devtool/geo.rs`).
//!
//! # Đây là "điểm dân cư gần nhất", KHÔNG phải ranh giới hành chính
//!
//! Bộ dữ liệu là các ĐIỂM, không phải đa giác. Nên câu trả lời luôn là "thành
//! phố đáng kể gần đây nhất", chứ không phải "ảnh này nằm trong địa phận nào" —
//! khác hẳn cách Apple Photos gắn nhãn. Ảnh chụp giữa rừng sẽ mang tên thị trấn
//! cách đó 20 km. UI phải nói rõ điều này chứ đừng để user tưởng là địa giới.
//!
//! # Vì sao chọn theo DÂN SỐ chứ không phải gần nhất
//!
//! `cities1000` dày tới mức trong nội đô, điểm gần nhất thường là tên một
//! phường vô danh. Ảnh chụp ở trung tâm Hà Nội mà ra thư mục tên phường thì vô
//! dụng. Nên trong bán kính [`CITY_RADIUS_KM`] ta lấy nơi ĐÔNG DÂN NHẤT, hoà
//! thì lấy gần nhất — cho ra cái tên mà con người thật sự dùng để nói về nơi đó.

use std::sync::OnceLock;

use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization;

/// Bán kính coi là "ảnh chụp ở đây". Bên ngoài nó thì tên thành phố là bịa.
pub const CITY_RADIUS_KM: f64 = 25.0;
/// Ngoài bán kính thành phố nhưng còn trong khoảng này thì chỉ dám nói quốc gia
/// (biển, sa mạc, vùng núi). Xa hơn nữa là không nói gì cả.
pub const COUNTRY_RADIUS_KM: f64 = 300.0;
/// Bán kính nhận phường/xã. Chặt hơn hẳn thành phố vì đây là điểm trụ sở hành
/// chính: ở nội đô chúng cách nhau ~1 km nên "gần nhất" gần như luôn đúng, còn
/// ở nông thôn xã rộng hàng chục km² nên xa quá là đoán bừa sang xã bên.
pub const WARD_RADIUS_KM: f64 = 10.0;
/// Toạ độ trong bộ dữ liệu lưu theo đơn vị 1e-4 độ (~11 m).
const COORD_SCALE: f64 = 10_000.0;
const EARTH_R_KM: f64 = 6371.0;

const RAW: &str = include_str!("../data/places.tsv");

struct City {
    lat_e4: i32,
    lon_e4: i32,
    name: &'static str,
    country: u16,
    /// -1 = dump không có admin1 cho nơi này (đảo nhỏ, lãnh thổ đặc biệt).
    admin1: i32,
    pop_k: u32,
}

/// Phường/xã (GeoNames ADM3). Chỉ có cho các nước được sinh kèm `--fine`.
struct Fine {
    lat_e4: i32,
    lon_e4: i32,
    name: &'static str,
    /// -1 = dump không nối được sang quận/huyện.
    district: i32,
}

struct Data {
    countries: Vec<&'static str>,
    admin1: Vec<&'static str>,
    districts: Vec<&'static str>,
    /// Sắp theo vĩ độ tăng dần — tra cứu nhị phân ra dải rồi mới quét ngang.
    cities: Vec<City>,
    fine: Vec<Fine>,
}

/// Tên nơi chụp. Field nào `None` nghĩa là không đủ căn cứ để nói, và caller
/// phải bỏ hẳn đoạn đó đi chứ đừng thay bằng "Unknown" — thư mục tên "Unknown"
/// là rác, còn thiếu một tầng thư mục thì vẫn dùng được.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Place {
    pub city: Option<&'static str>,
    pub province: Option<&'static str>,
    pub country: Option<&'static str>,
    /// Phường/xã. Chỉ có ở nước được sinh kèm dữ liệu chi tiết (hiện: VN).
    pub ward: Option<&'static str>,
    /// Quận/huyện CỦA CHÍNH phường ở trên (tra theo mã hành chính, không phải
    /// "quận gần nhất") — nên nó luôn khớp với `ward`.
    pub district: Option<&'static str>,
}

impl Place {
    pub fn is_empty(&self) -> bool {
        self.city.is_none()
            && self.province.is_none()
            && self.country.is_none()
            && self.ward.is_none()
    }
}

/// Bỏ dấu để ĐẶT TÊN THƯ MỤC: "Phường Lý Thái Tổ" → "Phuong Ly Thai To".
/// Tên trong [`Place`] giữ nguyên dấu — đó là tên để HIỂN THỊ cho người đọc.
///
/// # Vì sao thư mục không được có dấu
///
/// NTFS lưu UTF-16 nên tự nó không sao, nhưng công cụ chạy TRÊN nó thì có. Đo
/// trên đúng máy dev, thư mục thật tên `Phường Lý Thái Tổ`:
///
/// | Việc | Kết quả |
/// |---|---|
/// | `dir /b` ở codepage 437 hoặc 1258 | ra `Ph?ng L? Th�i T?` |
/// | batch đọc tên từ `dir` rồi `cd` vào lại | **fail cả ở codepage 65001** |
///
/// Cái thứ hai mới là lý do thật: `for /f` của cmd đọc pipe theo codepage ANSI
/// nên tên có dấu KHÔNG round-trip, kể cả khi console đang ở UTF-8. Nghĩa là mọi
/// script batch duyệt thư mục ảnh sẽ trượt — mà kho ảnh là chỗ người ta hay
/// viết script để backup/đổi tên/đồng bộ. Thêm nữa: ZIP không cờ UTF-8, và
/// robocopy log ở OEM codepage, cũng mất tên như vậy.
///
/// # Chỉ áp cho tên do CHÍNH TA sinh
///
/// Token `{relpath}`/`{folder}`/`{name}` vẫn giữ nguyên dấu: đó là tên thư mục
/// SẴN CÓ của user ("Bác Tuấn"), bỏ dấu nghĩa là tự ý đổi tên dữ liệu của người
/// ta. Tên địa điểm thì do ta sinh ra từ toạ độ nên ta được chọn cách viết.
///
/// # Hợp đồng
///
/// Kết quả **hoặc** là ASCII thuần, **hoặc** đúng bằng chuỗi vào (tên không có
/// phần Latin nào để bỏ dấu). Không bao giờ có trường hợp thứ ba, và không bao
/// giờ rỗng khi chuỗi vào không rỗng.
pub fn fold_ascii(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.nfd() {
        if is_combining_mark(c) {
            continue;
        }
        match c {
            // NFD không tách được mấy chữ này (dấu nằm THÂN chữ chứ không phải
            // dấu tổ hợp) nên phải kê tay. `ð`/`Ð` U+00F0/U+00D0 là eth của
            // tiếng Iceland, nhưng cũng chính là chữ Đ bị GeoNames ghi sai mã.
            'đ' | 'ð' => out.push('d'),
            'Đ' | 'Ð' => out.push('D'),
            'ø' => out.push('o'),
            'Ø' => out.push('O'),
            'ł' => out.push('l'),
            'Ł' => out.push('L'),
            'ħ' => out.push('h'),
            'Ħ' => out.push('H'),
            'ŧ' => out.push('t'),
            'Ŧ' => out.push('T'),
            'ı' => out.push('i'),
            // schwa của tiếng Azerbaijan — 69 tên cấp tỉnh dùng nó
            'ə' | 'ǝ' => out.push('e'),
            'Ə' | 'Ǝ' => out.push('E'),
            'æ' => out.push_str("ae"),
            'Æ' => out.push_str("Ae"),
            'œ' => out.push_str("oe"),
            'Œ' => out.push_str("Oe"),
            'þ' => out.push_str("th"),
            'Þ' => out.push_str("Th"),
            'ß' | 'ẞ' => out.push_str("ss"),
            // 1.053 tên trong bộ dữ liệu dùng nháy CONG ("Qacha’s Nek"), và
            // ʻokina của tiếng Hawaii là U+02BB ("Kakaʻako") — chữ thật trong
            // tên, không phải rác, nên đổi thành nháy thẳng chứ không bỏ.
            '\u{2019}' | '\u{2018}' | '\u{02bb}' | '\u{02bc}' | '\u{02bd}' | '\u{00b4}'
            | '\u{0060}' => out.push('\''),
            '\u{2013}' | '\u{2014}' | '\u{2212}' => out.push('-'),
            _ => out.push(c),
        }
    }
    if out.is_ascii() {
        return out;
    }
    // Còn ký tự ngoài ASCII = chữ không phải Latin, không có khái niệm "bỏ dấu".
    // Tên TRỘN hai bảng chữ thì phần Latin đã đủ ("Al-Medy Village, قرية المدي"
    // → "Al-Medy Village"); tên thuần Hán/Thái/Kirin thì bỏ hết sẽ ra rỗng, nên
    // trả lại nguyên bản — thư mục tên tiếng Trung vẫn mở được, tên rỗng thì
    // không, mà bịa ra phiên âm còn tệ hơn cả hai.
    let ascii_only: String = out.chars().filter(char::is_ascii).collect();
    if ascii_only.chars().any(|c| c.is_ascii_alphanumeric()) {
        // Bỏ chữ giữa câu hay để lại dấu phẩy/space lẻ ở hai đầu
        ascii_only
            .trim_matches(|c: char| c.is_whitespace() || c == ',' || c == ';')
            .to_string()
    } else {
        s.to_string()
    }
}

fn data() -> &'static Data {
    static DATA: OnceLock<Data> = OnceLock::new();
    DATA.get_or_init(parse)
}

fn parse() -> Data {
    let mut countries = Vec::new();
    let mut admin1 = Vec::new();
    let mut districts = Vec::new();
    let mut cities = Vec::new();
    let mut fine = Vec::new();
    let mut section = "";
    for line in RAW.lines() {
        if let Some(rest) = line.strip_prefix('#') {
            section = rest.split('\t').next().unwrap_or("");
            continue;
        }
        match section {
            "COUNTRIES" => {
                // "CC<TAB>Tên" — mã ISO chỉ dùng lúc sinh, ở đây bỏ
                if let Some((_, name)) = line.split_once('\t') {
                    countries.push(name);
                }
            }
            "ADMIN1" => admin1.push(line),
            "DISTRICTS" => districts.push(line),
            "FINE" => {
                let mut f = line.split('\t');
                let (Some(lat), Some(lon), Some(name), Some(_cc), Some(di)) =
                    (f.next(), f.next(), f.next(), f.next(), f.next())
                else {
                    continue;
                };
                let (Ok(lat_e4), Ok(lon_e4), Ok(district)) =
                    (lat.parse::<i32>(), lon.parse::<i32>(), di.parse::<i32>())
                else {
                    continue;
                };
                fine.push(Fine {
                    lat_e4,
                    lon_e4,
                    name,
                    district,
                });
            }
            "CITIES" => {
                let mut f = line.split('\t');
                let (Some(lat), Some(lon), Some(name), Some(cc), Some(a1), Some(pop)) =
                    (f.next(), f.next(), f.next(), f.next(), f.next(), f.next())
                else {
                    continue;
                };
                let (Ok(lat_e4), Ok(lon_e4), Ok(country), Ok(admin1_i), Ok(pop_k)) = (
                    lat.parse::<i32>(),
                    lon.parse::<i32>(),
                    cc.parse::<u16>(),
                    a1.parse::<i32>(),
                    pop.parse::<u32>(),
                ) else {
                    continue;
                };
                cities.push(City {
                    lat_e4,
                    lon_e4,
                    name,
                    country,
                    admin1: admin1_i,
                    pop_k,
                });
            }
            _ => {}
        }
    }
    Data {
        countries,
        admin1,
        districts,
        cities,
        fine,
    }
}

/// Xấp xỉ equirectangular: sai số < 0,2% ở khoảng cách vài trăm km, mà rẻ hơn
/// haversine hẳn — và ta đang so hàng nghìn điểm cho mỗi tấm ảnh.
fn dist_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (r1, r2) = (lat1.to_radians(), lat2.to_radians());
    let x = (lon2 - lon1).to_radians() * ((r1 + r2) / 2.0).cos();
    let y = r2 - r1;
    EARTH_R_KM * (x * x + y * y).sqrt()
}

/// Tra tên nơi chụp. Toạ độ vô lý (ngoài dải hợp lệ, hoặc đúng 0,0 — giá trị
/// rác mà vài app ghi vào EXIF khi không có định vị) trả về rỗng.
pub fn lookup(lat: f64, lon: f64) -> Place {
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Place::default();
    }
    if lat.abs() < 1e-6 && lon.abs() < 1e-6 {
        return Place::default();
    }
    let d = data();
    if d.cities.is_empty() {
        return Place::default();
    }

    // Dải vĩ độ đủ rộng cho bán kính quốc gia; kinh độ lọc sau vì độ rộng của
    // 1 độ kinh tuyến co lại theo cos(vĩ độ) — ở gần cực thì rất rộng.
    let lat_pad = COUNTRY_RADIUS_KM / 111.32;
    let lo = ((lat - lat_pad) * COORD_SCALE) as i32;
    let hi = ((lat + lat_pad) * COORD_SCALE) as i32;
    let start = d.cities.partition_point(|c| c.lat_e4 < lo);
    let end = d.cities.partition_point(|c| c.lat_e4 <= hi);

    let mut best_city: Option<(&City, f64)> = None;
    let mut nearest: Option<(&City, f64)> = None;
    for c in &d.cities[start..end] {
        let (clat, clon) = (c.lat_e4 as f64 / COORD_SCALE, c.lon_e4 as f64 / COORD_SCALE);
        let km = dist_km(lat, lon, clat, clon);
        if km > COUNTRY_RADIUS_KM {
            continue;
        }
        if nearest.is_none_or(|(_, best)| km < best) {
            nearest = Some((c, km));
        }
        if km <= CITY_RADIUS_KM
            // Đông dân hơn thì thắng; hoà dân số thì gần hơn thắng
            && best_city.is_none_or(|(b, bkm)| (c.pop_k, -km) > (b.pop_k, -bkm))
        {
            best_city = Some((c, km));
        }
    }

    // Phường/xã tra độc lập với thành phố: hai tầng dữ liệu khác nhau, và một
    // tấm ảnh có thể có phường mà không có thành phố nào đủ gần (và ngược lại).
    let (mut ward, mut district) = (None, None);
    let wpad = WARD_RADIUS_KM / 111.32;
    let wlo = ((lat - wpad) * COORD_SCALE) as i32;
    let whi = ((lat + wpad) * COORD_SCALE) as i32;
    let ws = d.fine.partition_point(|f| f.lat_e4 < wlo);
    let we = d.fine.partition_point(|f| f.lat_e4 <= whi);
    let mut best_w = f64::MAX;
    for f in &d.fine[ws..we] {
        let km = dist_km(
            lat,
            lon,
            f.lat_e4 as f64 / COORD_SCALE,
            f.lon_e4 as f64 / COORD_SCALE,
        );
        if km <= WARD_RADIUS_KM && km < best_w {
            best_w = km;
            ward = Some(f.name);
            district = usize::try_from(f.district)
                .ok()
                .and_then(|i| d.districts.get(i))
                .copied();
        }
    }

    match best_city.or(nearest) {
        None => Place {
            ward,
            district,
            ..Place::default()
        },
        Some((c, km)) => {
            let country = d.countries.get(c.country as usize).copied();
            if km > CITY_RADIUS_KM {
                // Quá xa để gọi tên thành phố, nhưng còn đủ gần để chắc quốc gia
                return Place {
                    city: None,
                    province: None,
                    country,
                    ward,
                    district,
                };
            }
            Place {
                city: Some(c.name),
                province: usize::try_from(c.admin1)
                    .ok()
                    .and_then(|i| d.admin1.get(i))
                    .copied(),
                country,
                ward,
                district,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dataset_loads() {
        let d = data();
        assert!(
            d.cities.len() > 100_000,
            "bo du lieu hong hoac chua sinh: {} thanh pho",
            d.cities.len()
        );
        assert!(d.countries.len() > 200);
        assert!(d.admin1.len() > 1_000);
        assert!(
            d.cities.windows(2).all(|w| w[0].lat_e4 <= w[1].lat_e4),
            "phai sap theo vi do, khong thi tra cuu nhi phan sai"
        );
    }

    #[test]
    fn city_center_resolves_to_the_city_not_a_ward() {
        // Hồ Hoàn Kiếm, trung tâm Hà Nội. cities1000 có hàng chục phường quanh
        // đây; luật "đông dân nhất" phải cho ra chính Hà Nội.
        let p = lookup(21.0287, 105.8524);
        assert_eq!(p.city, Some("Hà Nội"), "{p:?}");
        assert_eq!(p.country, Some("Vietnam"));
    }

    /// MỌI tầng phải cùng một ngôn ngữ. GeoNames để `name` của thành phố lớn ở
    /// dạng phổ thông ("Hanoi") nên bộ sinh dữ liệu ghi đè bằng tên tiếng Việt
    /// từ dump `alternatenames` (`--fine VN:vi`). Thiếu bước đó thì một đường
    /// dẫn duy nhất trộn hai ngôn ngữ: `Hanoi\Quận Hoàn Kiếm\Phường Lý Thái Tổ`.
    #[test]
    fn every_level_uses_the_same_language() {
        let p = lookup(21.0287, 105.8524);
        assert_eq!(p.city, Some("Hà Nội"));
        assert_eq!(p.province, Some("Hà Nội"));
        assert_eq!(p.ward, Some("Phường Lý Thái Tổ"));
        assert_eq!(p.district, Some("Quận Hoàn Kiếm"));
    }

    /// Dữ liệu phải ở dạng NFC. Dump gốc TRỘN hai dạng ("Xã An Hải" tách dấu),
    /// mà hai chuỗi nhìn y hệt nhưng khác byte thì sinh ra hai thư mục trông
    /// giống hệt nhau cạnh nhau — và so sánh đường dẫn thì trượt.
    ///
    /// Kiểm bằng chính thư viện chuẩn hoá chứ không đoán qua "có dấu tổ hợp hay
    /// không": vài ký tự (`n̈` trong "Iharan̈a") KHÔNG có dạng liền, nên ở NFC
    /// chúng vẫn phải giữ dấu rời.
    #[test]
    fn names_are_nfc_normalized() {
        use unicode_normalization::UnicodeNormalization;
        let d = data();
        let bad = d
            .fine
            .iter()
            .map(|f| f.name)
            .chain(d.cities.iter().map(|c| c.name))
            .chain(d.admin1.iter().copied())
            .chain(d.districts.iter().copied())
            .find(|s| s.nfc().collect::<String>() != **s);
        assert_eq!(bad, None, "ten chua o dang NFC");
    }

    /// `Ð` U+00D0 (eth) và `Đ` U+0110 (D có gạch) HIỂN THỊ y hệt nhau nhưng là
    /// hai ký tự khác nhau, và GeoNames nhầm chúng có hệ thống — không riêng
    /// tiếng Việt, cả tên Croatia. Bộ sinh dữ liệu chữa theo mã nước, nên trong
    /// dữ liệu đã ghi ra không được còn eth ở nước không dùng eth.
    #[test]
    fn no_eth_outside_iceland_and_faroe() {
        let d = data();
        // Iceland/Faroe dùng ð thật, ở đây chỉ kiểm tên VN
        let bad: Vec<_> = d
            .fine
            .iter()
            .map(|f| f.name)
            .chain(d.districts.iter().copied())
            .filter(|s| s.contains('\u{d0}') || s.contains('\u{f0}'))
            .collect();
        assert!(bad.is_empty(), "ten Viet con dung eth: {bad:?}");
        // Bất kể dữ liệu thế nào, fold vẫn phải quy cả hai về 'D'
        assert_eq!(fold_ascii("Huyện \u{d0}ông Hưng"), "Huyen Dong Hung");
        assert_eq!(fold_ascii("Huyện \u{110}ông Hưng"), "Huyen Dong Hung");
    }

    /// Tên thư mục phải bỏ dấu — xem [`fold_ascii`] để biết vì sao (batch của
    /// cmd không round-trip được tên có dấu, kể cả ở codepage 65001).
    #[test]
    fn folding_strips_vietnamese_marks() {
        assert_eq!(fold_ascii("Phường Lý Thái Tổ"), "Phuong Ly Thai To");
        assert_eq!(fold_ascii("Quận Hoàn Kiếm"), "Quan Hoan Kiem");
        assert_eq!(fold_ascii("Hà Nội"), "Ha Noi");
        assert_eq!(fold_ascii("Đà Nẵng"), "Da Nang");
        assert_eq!(fold_ascii("Xã Đạ M’Ri"), "Xa Da M'Ri");
        // Tra thật rồi fold: đúng đường mà organize sẽ đi
        let p = lookup(21.0287, 105.8524);
        let path: Vec<String> = [p.province, p.district, p.ward]
            .into_iter()
            .flatten()
            .map(fold_ascii)
            .collect();
        assert_eq!(path, ["Ha Noi", "Quan Hoan Kiem", "Phuong Ly Thai To"]);
    }

    /// Chữ Latin của nước khác cũng phải ra ASCII: dữ liệu là toàn cầu, mà ảnh
    /// đi du lịch thì đường dẫn vẫn phải gõ được.
    #[test]
    fn folding_handles_other_latin_alphabets() {
        assert_eq!(fold_ascii("Zürich"), "Zurich");
        assert_eq!(fold_ascii("São Paulo"), "Sao Paulo");
        assert_eq!(fold_ascii("Kraków"), "Krakow");
        assert_eq!(fold_ascii("Malmö"), "Malmo");
        assert_eq!(fold_ascii("Tromsø"), "Tromso");
        assert_eq!(fold_ascii("Straßburg"), "Strassburg");
        assert_eq!(fold_ascii("Þórshöfn"), "Thorshofn");
        assert_eq!(fold_ascii("Beyləqan"), "Beyleqan");
        assert_eq!(fold_ascii("Ærøskøbing"), "Aeroskobing");
        assert_eq!(fold_ascii("Łódź"), "Lodz");
        // Đã ASCII thì không được đổi gì
        assert_eq!(fold_ascii("New York City"), "New York City");
    }

    /// Chữ không phải Latin không có cách bỏ dấu. Trả nguyên bản chứ đừng ra
    /// rỗng — thư mục tên tiếng Trung vẫn mở được, tên rỗng thì không.
    #[test]
    fn folding_keeps_non_latin_names_intact() {
        assert_eq!(fold_ascii("東京"), "東京");
        assert_eq!(fold_ascii("Владивосток"), "Владивосток");
        assert_eq!(fold_ascii(""), "");
        // ʻokina của tiếng Hawaii là chữ trong tên, không phải rác
        assert_eq!(fold_ascii("Kakaʻako"), "Kaka'ako");
        // Tên TRỘN hai bảng chữ: phần Latin đã đủ để đặt thư mục
        assert_eq!(fold_ascii("Al-Medy Village, قرية المدي"), "Al-Medy Village");
    }

    /// Kiểm trên TOÀN BỘ dữ liệu, không phải vài ví dụ tự chọn.
    #[test]
    fn folding_covers_the_whole_dataset() {
        let d = data();
        let all = || {
            d.fine
                .iter()
                .map(|f| f.name)
                .chain(d.cities.iter().map(|c| c.name))
                .chain(d.admin1.iter().copied())
                .chain(d.districts.iter().copied())
        };
        // Hợp đồng: hoặc ASCII thuần, hoặc y nguyên chuỗi vào — không có ca thứ ba
        let broken: Vec<&str> = all()
            .filter(|s| {
                let f = fold_ascii(s);
                !f.is_ascii() && f != **s
            })
            .collect();
        assert!(
            broken.is_empty(),
            "{} ten khong ASCII ma cung khong nguyen ban: {:?}",
            broken.len(),
            &broken[..broken.len().min(10)]
        );
        // Và tên không phải ASCII còn lại phải là tên KHÔNG có chữ Latin nào
        let latin_left: Vec<&str> = all()
            .filter(|s| !fold_ascii(s).is_ascii())
            .filter(|s| s.chars().any(|c| c.is_ascii_alphanumeric()))
            .collect();
        assert!(
            latin_left.is_empty(),
            "{} ten co phan Latin ma fold ra van ngoai ASCII: {:?}",
            latin_left.len(),
            &latin_left[..latin_left.len().min(10)]
        );
        // fold rồi fold nữa không đổi gì — nếu không thì render 2 lần ra 2 tên
        assert!(all().all(|s| fold_ascii(&fold_ascii(s)) == fold_ascii(s)));
        // Không tên nào biến thành rỗng: segment rỗng là mất một tầng thư mục
        assert!(all().all(|s| s.is_empty() || !fold_ascii(s).trim().is_empty()));
    }

    /// Tầng phường/xã đi RIÊNG với tầng thành phố: cùng một toạ độ cho ra cả
    /// "Hà Nội" lẫn phường cụ thể, user tự chọn dùng cái nào trong template.
    #[test]
    fn ward_level_is_available_for_vietnam() {
        let p = lookup(21.0287, 105.8524);
        let w = p.ward.expect("phai co phuong");
        assert!(
            w.starts_with("Phường"),
            "ten phuong phai giu nguyen dau, khong phai asciiname kieu 'GJao': {w}"
        );
        assert!(p.district.is_some(), "{p:?}");

        // Ngoại thành: vẫn ra xã, và xã đó phải KHÁC phường nội đô
        let far = lookup(21.069258, 105.505722);
        assert!(far.ward.is_some(), "{far:?}");
        assert_ne!(far.ward, p.ward);
    }

    /// Dữ liệu chi tiết chỉ sinh cho VN — nước khác không được bịa ra phường.
    #[test]
    fn ward_is_absent_outside_countries_with_fine_data() {
        let p = lookup(48.8566, 2.3522); // Paris
        assert!(p.city.is_some(), "{p:?}");
        assert_eq!(p.ward, None, "{p:?}");
        assert_eq!(p.district, None, "{p:?}");
    }

    #[test]
    fn known_coordinates_from_the_real_library() {
        // Toạ độ đọc từ EXIF của chính kho ảnh dùng để phát triển
        let p = lookup(21.049403, 105.806739); // IMG_2366
        assert_eq!(p.country, Some("Vietnam"));
        assert!(p.city.is_some(), "{p:?}");
        let p = lookup(21.069258, 105.505722); // IMG_5950, ngoại thành
        assert_eq!(p.country, Some("Vietnam"));
        assert!(p.city.is_some(), "{p:?}");
    }

    #[test]
    fn open_ocean_gives_nothing() {
        // Giữa nam Thái Bình Dương, cách bờ hàng nghìn km
        assert_eq!(lookup(-40.0, -140.0), Place::default());
    }

    #[test]
    fn far_from_any_city_keeps_only_the_country() {
        // Sa mạc Sahara, Algeria: không thành phố nào trong 25 km nhưng chắc
        // chắn vẫn là Algeria
        let p = lookup(24.0, 3.0);
        assert_eq!(p.city, None, "{p:?}");
        assert!(p.country.is_some(), "{p:?}");
    }

    #[test]
    fn junk_coordinates_are_rejected() {
        // 0,0 giữa vịnh Guinea — vài app ghi giá trị này khi KHÔNG có định vị
        assert_eq!(lookup(0.0, 0.0), Place::default());
        assert_eq!(lookup(91.0, 10.0), Place::default());
        assert_eq!(lookup(10.0, 999.0), Place::default());
    }
}
