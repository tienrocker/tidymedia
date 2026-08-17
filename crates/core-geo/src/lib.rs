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
