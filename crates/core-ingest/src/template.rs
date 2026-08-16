//! Template engine cho tên thư mục + tên file đích của organize.
//!
//! User tự cấu hình (lưu kv), default: dir `{YYYY}\{YYYY}-{MM}`, file
//! `{YYYYMMDD}_{hhmmss}_{hash4}`. Ext KHÔNG nằm trong template - executor tự
//! append ext gốc (lowercase). Token hash render prefix hex của BLAKE3 full;
//! khi đụng độ tên, planner render lại với hash_len lớn hơn (4 -> 8 -> 16).

use crate::date::{civil_from_days, MS_PER_DAY};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    Lit(String),
    Yyyy,
    Mm,
    Dd,
    Yyyymmdd,
    Hh,
    Min,
    Ss,
    Hhmmss,
    /// độ dài prefix hex mặc định của token (4/8/16)
    Hash(usize),
    Camera,
    /// thư mục nguồn tương đối root (Dir only) — giữ cấu trúc phân loại của user
    RelPath,
    /// tên thư mục cha trực tiếp của file (Dir only)
    Folder,
    /// stem tên file gốc (original_name ?? name, bỏ ext)
    Name,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TemplateError {
    #[error("ERR_TPL_EMPTY")]
    Empty,
    #[error("ERR_TPL_UNCLOSED")]
    Unclosed,
    #[error("ERR_TPL_UNKNOWN_TOKEN|{0}")]
    UnknownToken(String),
    #[error("ERR_TPL_BAD_CHAR|{0}")]
    BadChar(char),
    #[error("ERR_TPL_ABSOLUTE")]
    Absolute,
    #[error("ERR_TPL_PARENT_REF")]
    ParentRef,
    #[error("ERR_TPL_SEP_IN_FILE")]
    SepInFile,
    #[error("ERR_TPL_DIR_ONLY|{0}")]
    DirOnlyToken(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateKind {
    /// cho phép \ hoặc / làm ngăn thư mục
    Dir,
    /// một component duy nhất, cấm separator
    File,
}

#[derive(Debug, Clone)]
pub struct Template {
    tokens: Vec<Token>,
    kind: TemplateKind,
    pub has_hash: bool,
}

/// Ký tự cấm trong 1 component tên trên Windows (separator xét riêng theo kind)
const BAD_CHARS: &[char] = &['<', '>', ':', '"', '|', '?', '*'];

pub fn parse_template(src: &str, kind: TemplateKind) -> Result<Template, TemplateError> {
    if src.trim().is_empty() {
        return Err(TemplateError::Empty);
    }
    let mut tokens = Vec::new();
    let mut lit = String::new();
    let mut chars = src.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '{' {
            let mut name = String::new();
            loop {
                match chars.next() {
                    None => return Err(TemplateError::Unclosed),
                    Some('}') => break,
                    Some(x) => name.push(x),
                }
            }
            if !lit.is_empty() {
                tokens.push(Token::Lit(std::mem::take(&mut lit)));
            }
            let tok = match name.as_str() {
                "YYYY" => Token::Yyyy,
                "MM" => Token::Mm,
                "DD" => Token::Dd,
                "YYYYMMDD" => Token::Yyyymmdd,
                "hh" => Token::Hh,
                "mm" => Token::Min,
                "ss" => Token::Ss,
                "hhmmss" => Token::Hhmmss,
                "hash4" => Token::Hash(4),
                "hash8" => Token::Hash(8),
                "hash16" => Token::Hash(16),
                "camera" => Token::Camera,
                "relpath" => Token::RelPath,
                "folder" => Token::Folder,
                "name" => Token::Name,
                other => return Err(TemplateError::UnknownToken(other.to_string())),
            };
            // {relpath}/{folder} có thể render nhiều segment thư mục — trong
            // template TÊN FILE (1 component duy nhất) là vô nghĩa, chặn sớm.
            if kind == TemplateKind::File && matches!(tok, Token::RelPath | Token::Folder) {
                return Err(TemplateError::DirOnlyToken(name));
            }
            tokens.push(tok);
        } else {
            if BAD_CHARS.contains(&c) || (c as u32) < 0x20 {
                return Err(TemplateError::BadChar(c));
            }
            if (c == '\\' || c == '/') && kind == TemplateKind::File {
                return Err(TemplateError::SepInFile);
            }
            lit.push(c);
        }
    }
    if !lit.is_empty() {
        tokens.push(Token::Lit(lit));
    }
    if kind == TemplateKind::Dir {
        // cấm absolute + ..: xét trên chuỗi literal thô (token không sinh ra \ hay ..)
        let raw: String = tokens
            .iter()
            .map(|t| match t {
                Token::Lit(s) => s.clone(),
                _ => "x".into(), // token render không bao giờ ra separator/..
            })
            .collect();
        let norm = raw.replace('/', "\\");
        if norm.starts_with('\\') || norm.chars().nth(1) == Some(':') {
            return Err(TemplateError::Absolute);
        }
        if norm.split('\\').any(|seg| seg.trim() == "..") {
            return Err(TemplateError::ParentRef);
        }
    }
    let has_hash = tokens.iter().any(|t| matches!(t, Token::Hash(_)));
    Ok(Template {
        tokens,
        kind,
        has_hash,
    })
}

pub struct RenderCtx<'a> {
    pub y: i64,
    pub mo: u32,
    pub d: u32,
    pub hh: u32,
    pub mi: u32,
    pub ss: u32,
    /// hex đầy đủ (64 ký tự) của BLAKE3 full hash
    pub hash_hex: &'a str,
    pub camera: Option<&'a str>,
    /// thư mục nguồn tương đối root, phân cách '\' (raw — sanitize lúc render)
    pub rel_dir: Option<&'a str>,
    /// tên thư mục cha trực tiếp của file
    pub folder: Option<&'a str>,
    /// stem tên file gốc (original_name ?? name, không gồm ext)
    pub orig_stem: Option<&'a str>,
}

impl<'a> RenderCtx<'a> {
    /// taken_ms theo convention wall-clock khung UTC (đọc với tz=0)
    pub fn from_taken(taken_ms: i64, hash_hex: &'a str, camera: Option<&'a str>) -> Self {
        let days = taken_ms.div_euclid(MS_PER_DAY);
        let rem = taken_ms.rem_euclid(MS_PER_DAY) / 1000;
        let (y, mo, d) = civil_from_days(days);
        RenderCtx {
            y,
            mo,
            d,
            hh: (rem / 3600) as u32,
            mi: (rem / 60 % 60) as u32,
            ss: (rem % 60) as u32,
            hash_hex,
            camera,
            rel_dir: None,
            folder: None,
            orig_stem: None,
        }
    }

    /// Gắn dữ liệu nguồn cho token {relpath}/{folder}/{name}.
    pub fn with_source(
        mut self,
        rel_dir: Option<&'a str>,
        folder: Option<&'a str>,
        orig_stem: Option<&'a str>,
    ) -> Self {
        self.rel_dir = rel_dir;
        self.folder = folder;
        self.orig_stem = orig_stem;
        self
    }
}

/// Slug hóa tên camera: giữ chữ số + chữ cái (mọi bảng chữ) + '-', mọi
/// khoảng trắng/ký tự khác thành '_' (gộp liền nhau) — "Apple iPhone 12"
/// -> "Apple_iPhone_12", không bao giờ có space trong tên file.
fn sanitize_camera(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_alphanumeric() || c == '-' {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('_') {
            // space, '/', ':', '*'... đều thành đúng 1 dấu '_'
            out.push('_');
        }
    }
    out.trim_matches('_').to_string()
}

/// Component lấy từ DỮ LIỆU NGUỒN (tên thư mục/tên file thật trên đĩa) — khác
/// literal, giá trị này chưa từng qua cổng BAD_CHARS của parse: bỏ <>:"|?* +
/// control char + separator (token file không bao giờ được sinh ra path).
/// GIỮ NGUYÊN space + unicode — "Bác Tuấn" phải ra "Bác Tuấn", không slug.
fn sanitize_source_component(s: &str) -> String {
    s.chars()
        .filter(|&c| !BAD_CHARS.contains(&c) && (c as u32) >= 0x20 && c != '\\' && c != '/')
        .collect()
}

/// relpath nguồn: chuẩn hóa separator, sanitize từng segment, bỏ segment "."
/// ".." và rỗng. Path canonical từ index không thể chứa chúng, nhưng đây là
/// chốt chặn cuối trước một đợt move hàng loạt — giữ tuyệt đối.
fn sanitize_source_relpath(s: &str) -> String {
    s.replace('/', "\\")
        .split('\\')
        .map(sanitize_source_component)
        .filter(|seg| !seg.is_empty() && seg.as_str() != "." && seg.as_str() != "..")
        .collect::<Vec<_>>()
        .join("\\")
}

/// Dọn 1 component sau render: gộp separator chữ thừa do token rỗng ({camera}
/// không có dữ liệu), cắt đầu/đuôi _-. và space (Windows cấm tên kết thúc
/// bằng dot/space), né tên thiết bị Windows cấm.
fn clean_component(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if (c == '_' || c == '-' || c == ' ') && out.ends_with(c) {
            continue;
        }
        out.push(c);
    }
    let out = out
        .trim_matches(|c| c == '_' || c == '-' || c == ' ' || c == '.')
        .to_string();
    // CON/PRN/AUX/NUL/COM1-9/LPT1-9 (cả dạng "CON.jpg") là tên thiết bị —
    // Windows từ chối tạo → thêm '_' đầu để mọi tên render ra đều tạo được
    let stem = out.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0');
    if reserved {
        format!("_{out}")
    } else {
        out
    }
}

impl Template {
    /// hash_len: None = độ dài mặc định của từng token; Some(n) = escalation khi
    /// đụng độ tên (mọi token hash render n ký tự).
    fn render_raw(&self, ctx: &RenderCtx, hash_len: Option<usize>) -> String {
        let mut out = String::new();
        for t in &self.tokens {
            match t {
                Token::Lit(s) => out.push_str(s),
                Token::Yyyy => out.push_str(&format!("{:04}", ctx.y)),
                Token::Mm => out.push_str(&format!("{:02}", ctx.mo)),
                Token::Dd => out.push_str(&format!("{:02}", ctx.d)),
                Token::Yyyymmdd => out.push_str(&format!("{:04}{:02}{:02}", ctx.y, ctx.mo, ctx.d)),
                Token::Hh => out.push_str(&format!("{:02}", ctx.hh)),
                Token::Min => out.push_str(&format!("{:02}", ctx.mi)),
                Token::Ss => out.push_str(&format!("{:02}", ctx.ss)),
                Token::Hhmmss => out.push_str(&format!("{:02}{:02}{:02}", ctx.hh, ctx.mi, ctx.ss)),
                Token::Hash(n) => {
                    let n = hash_len.unwrap_or(*n).min(ctx.hash_hex.len());
                    out.push_str(&ctx.hash_hex[..n]);
                }
                Token::Camera => {
                    if let Some(c) = ctx.camera {
                        out.push_str(&sanitize_camera(c));
                    }
                }
                Token::RelPath => {
                    if let Some(r) = ctx.rel_dir {
                        out.push_str(&sanitize_source_relpath(r));
                    }
                }
                Token::Folder => {
                    if let Some(f) = ctx.folder {
                        out.push_str(&sanitize_source_component(f));
                    }
                }
                Token::Name => {
                    if let Some(n) = ctx.orig_stem {
                        out.push_str(&sanitize_source_component(n));
                    }
                }
            }
        }
        out
    }

    /// File: trả về 1 component sạch (chưa có ext).
    pub fn render_file(&self, ctx: &RenderCtx, hash_len: Option<usize>) -> String {
        debug_assert_eq!(self.kind, TemplateKind::File);
        clean_component(&self.render_raw(ctx, hash_len))
    }

    /// Dir: trả về danh sách segment sạch (segment rỗng sau khi dọn bị bỏ).
    pub fn render_dir(&self, ctx: &RenderCtx) -> Vec<String> {
        debug_assert_eq!(self.kind, TemplateKind::Dir);
        self.render_raw(ctx, None)
            .replace('/', "\\")
            .split('\\')
            .map(clean_component)
            .filter(|s| !s.is_empty())
            .collect()
    }
}

pub const DEFAULT_DIR_TEMPLATE: &str = r"{YYYY}\{YYYY}-{MM}";
pub const DEFAULT_FILE_TEMPLATE: &str = "{YYYYMMDD}_{hhmmss}_{hash4}";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::date::days_from_civil;

    const HASH: &str = "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";

    fn ctx(camera: Option<&'static str>) -> RenderCtx<'static> {
        // 2019-06-14 15:30:22 wall-clock
        let ms = days_from_civil(2019, 6, 14) * MS_PER_DAY + (15 * 3600 + 30 * 60 + 22) * 1000;
        RenderCtx::from_taken(ms, HASH, camera)
    }

    #[test]
    fn default_templates_render() {
        let dir = parse_template(DEFAULT_DIR_TEMPLATE, TemplateKind::Dir).unwrap();
        assert_eq!(dir.render_dir(&ctx(None)), vec!["2019", "2019-06"]);
        let file = parse_template(DEFAULT_FILE_TEMPLATE, TemplateKind::File).unwrap();
        assert!(file.has_hash);
        assert_eq!(file.render_file(&ctx(None), None), "20190614_153022_abcd");
        assert_eq!(
            file.render_file(&ctx(None), Some(8)),
            "20190614_153022_abcdef12"
        );
    }

    #[test]
    fn all_tokens() {
        let t = parse_template(
            "{YYYY}.{MM}.{DD} {hh}{mm}{ss} {hash8} {camera}",
            TemplateKind::File,
        )
        .unwrap();
        assert_eq!(
            t.render_file(&ctx(Some("Canon EOS R5")), None),
            "2019.06.14 153022 abcdef12 Canon_EOS_R5"
        );
    }

    #[test]
    fn camera_missing_and_dirty() {
        let t = parse_template("{YYYYMMDD}_{camera}_{hash4}", TemplateKind::File).unwrap();
        // camera rỗng -> không còn __ đôi
        assert_eq!(t.render_file(&ctx(None), None), "20190614_abcd");
        // camera bẩn: ký tự cấm/space đều thành slug '_'
        let t2 = parse_template("{camera}", TemplateKind::File).unwrap();
        assert_eq!(
            t2.render_file(&ctx(Some("  Apple/iPhone: 12* Pro  ")), None),
            "Apple_iPhone_12_Pro"
        );
    }

    #[test]
    fn parse_errors() {
        use TemplateError::*;
        assert_eq!(parse_template("", TemplateKind::Dir).unwrap_err(), Empty);
        assert_eq!(
            parse_template("{YYYY", TemplateKind::Dir).unwrap_err(),
            Unclosed
        );
        assert_eq!(
            parse_template("{foo}", TemplateKind::Dir).unwrap_err(),
            UnknownToken("foo".into())
        );
        assert_eq!(
            parse_template("a<b", TemplateKind::Dir).unwrap_err(),
            BadChar('<')
        );
        // ':' nằm trong BAD_CHARS nên drive-absolute rớt ngay từ đó
        assert_eq!(
            parse_template(r"C:\x\{YYYY}", TemplateKind::Dir).unwrap_err(),
            BadChar(':')
        );
        assert_eq!(
            parse_template(r"\abs", TemplateKind::Dir).unwrap_err(),
            Absolute
        );
        assert_eq!(
            parse_template(r"a\..\b", TemplateKind::Dir).unwrap_err(),
            ParentRef
        );
        assert_eq!(
            parse_template(r"a\b", TemplateKind::File).unwrap_err(),
            SepInFile
        );
        assert_eq!(
            parse_template("a/b", TemplateKind::File).unwrap_err(),
            SepInFile
        );
    }

    #[test]
    fn dir_slash_normalized_and_empty_segments_dropped() {
        let t = parse_template("{YYYY}/{camera}/{YYYY}-{MM}", TemplateKind::Dir).unwrap();
        // camera None -> segment rỗng bị bỏ, không sinh thư mục rỗng tên ""
        assert_eq!(t.render_dir(&ctx(None)), vec!["2019", "2019-06"]);
    }

    #[test]
    fn trailing_dot_space_trimmed() {
        let t = parse_template("{YYYYMMDD}.", TemplateKind::File).unwrap();
        assert_eq!(t.render_file(&ctx(None), None), "20190614");
    }

    #[test]
    fn reserved_device_names_defused() {
        // CON/NUL/COM1... là tên thiết bị Windows — mọi component render ra
        // phải né bằng prefix '_' để luôn tạo được trên fs
        let t = parse_template("{camera}", TemplateKind::File).unwrap();
        assert_eq!(t.render_file(&ctx(Some("CON")), None), "_CON");
        assert_eq!(t.render_file(&ctx(Some("com1")), None), "_com1");
        assert_eq!(t.render_file(&ctx(Some("Console")), None), "Console"); // không phải reserved
        let d = parse_template("{camera}", TemplateKind::Dir).unwrap();
        assert_eq!(d.render_dir(&ctx(Some("NUL"))), vec!["_NUL"]);
    }

    fn src_ctx(
        rel: Option<&'static str>,
        folder: Option<&'static str>,
        stem: Option<&'static str>,
    ) -> RenderCtx<'static> {
        ctx(None).with_source(rel, folder, stem)
    }

    #[test]
    fn relpath_and_folder_are_dir_only() {
        assert_eq!(
            parse_template("{relpath}", TemplateKind::File).unwrap_err(),
            TemplateError::DirOnlyToken("relpath".into())
        );
        assert_eq!(
            parse_template("x_{folder}", TemplateKind::File).unwrap_err(),
            TemplateError::DirOnlyToken("folder".into())
        );
        assert!(parse_template("{relpath}", TemplateKind::Dir).is_ok());
        assert!(parse_template("{YYYY}\\{folder}", TemplateKind::Dir).is_ok());
        // {name} hợp lệ ở CẢ hai kind
        assert!(parse_template("{name}", TemplateKind::File).is_ok());
        assert!(parse_template("{name}", TemplateKind::Dir).is_ok());
    }

    #[test]
    fn relpath_renders_all_segments_keeping_unicode_and_spaces() {
        let d = parse_template(r"{relpath}\{YYYY}-{MM}", TemplateKind::Dir).unwrap();
        assert_eq!(
            d.render_dir(&src_ctx(Some(r"Bác Tuấn\Tết 2008"), None, None)),
            vec!["Bác Tuấn", "Tết 2008", "2019-06"]
        );
        // relpath rỗng/None → segment biến mất, không sinh thư mục rỗng
        let only = parse_template("{relpath}", TemplateKind::Dir).unwrap();
        assert!(only.render_dir(&src_ctx(None, None, None)).is_empty());
        assert!(only.render_dir(&src_ctx(Some(""), None, None)).is_empty());
    }

    #[test]
    fn source_values_are_sanitized_at_render() {
        // Ký tự cấm Windows + control char trong dữ liệu nguồn phải bị lọc
        let d = parse_template("{relpath}", TemplateKind::Dir).unwrap();
        assert_eq!(
            d.render_dir(&src_ctx(Some("a<b>:c\"|?*\\x\u{7}y"), None, None)),
            vec!["abc", "xy"]
        );
        // {folder}/{name} không bao giờ được sinh separator
        let f = parse_template("{name}", TemplateKind::File).unwrap();
        assert_eq!(
            f.render_file(&src_ctx(None, None, Some(r"a\b/c")), None),
            "abc"
        );
        let fd = parse_template("{folder}", TemplateKind::Dir).unwrap();
        assert_eq!(
            fd.render_dir(&src_ctx(None, Some(r"x\y"), None)),
            vec!["xy"]
        );
    }

    #[test]
    fn relpath_drops_traversal_and_empty_segments() {
        let d = parse_template("{relpath}", TemplateKind::Dir).unwrap();
        assert_eq!(
            d.render_dir(&src_ctx(Some(r"..\x\.\\y"), None, None)),
            vec!["x", "y"]
        );
        assert!(d.render_dir(&src_ctx(Some(".."), None, None)).is_empty());
        // reserved device name trong relpath vẫn bị né như mọi component khác
        assert_eq!(
            d.render_dir(&src_ctx(Some(r"CON\a"), None, None)),
            vec!["_CON", "a"]
        );
    }

    #[test]
    fn name_token_renders_stem() {
        let f = parse_template("{name}", TemplateKind::File).unwrap();
        assert_eq!(
            f.render_file(&src_ctx(None, None, Some("Picture 039")), None),
            "Picture 039"
        );
        // kết hợp với hash vẫn chạy
        let fh = parse_template("{name}_{hash4}", TemplateKind::File).unwrap();
        assert_eq!(
            fh.render_file(&src_ctx(None, None, Some("Picture 039")), None),
            "Picture 039_abcd"
        );
        // không có stem → không render gì (dọn _ thừa như {camera})
        assert_eq!(fh.render_file(&src_ctx(None, None, None), None), "abcd");
    }
}
