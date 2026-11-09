//! wxapkg archives: the plain format and WeChat's encrypted local cache copy.
//! Only the index is parsed on open; entry bodies are read on demand.
//!
//! Plain layout: a 14-byte header (`0xBE`, 4 unused bytes, big-endian index
//! size, big-endian body size, `0xED`), then the index (big-endian entry count,
//! then per entry: name length, UTF-8 name, offset, size), then the body.

mod collection;
mod reader;

pub use collection::{PackageSet, PackageSource, SetEntry, SourceKind};

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use reader::{ByteReader, is_encrypted_header};

const MAXIMUM_INDEX_SIZE: usize = 8 * 1024 * 1024;
const DUPLICATE_COMPARE_LIMIT: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ArchiveError {
    #[error("包文件读取失败")]
    Unreadable,
    #[error("当前包格式需要兼容适配")]
    UnrecognizedFormat,
    #[error("包头数据不完整")]
    TruncatedHeader,
    #[error("包索引长度异常")]
    InvalidIndexSize,
    #[error("包内文件数量异常")]
    InvalidFileCount,
    #[error("包索引结构异常")]
    MalformedIndex,
    #[error("包内存在异常文件路径")]
    InvalidPath,
    #[error("文件超出包边界：{0}")]
    EntryOutOfBounds(String),
    #[error("文件超过读取上限：{0}")]
    EntryTooLarge(String),
    #[error("文件大小为 {0} 字节，超过预览上限")]
    PreviewTooLarge(u64),
    #[error("读取包内文件失败：{0}")]
    ReadFailed(String),
    #[error("加密包缺少可用于读取的 AppID")]
    MissingAppId,
    #[error("微信缓存包解密校验失败")]
    DecryptionFailed,
}

impl ArchiveError {
    /// Stable machine-readable code (matches the Swift oracle's codes).
    pub fn code(&self) -> String {
        match self {
            Self::Unreadable => "unreadable".into(),
            Self::UnrecognizedFormat => "unrecognized_format".into(),
            Self::TruncatedHeader => "truncated_header".into(),
            Self::InvalidIndexSize => "invalid_index_size".into(),
            Self::InvalidFileCount => "invalid_file_count".into(),
            Self::MalformedIndex => "malformed_index".into(),
            Self::InvalidPath => "invalid_path".into(),
            Self::EntryOutOfBounds(path) => format!("entry_out_of_bounds:{path}"),
            Self::EntryTooLarge(path) => format!("entry_too_large:{path}"),
            Self::PreviewTooLarge(size) => format!("preview_too_large:{size}"),
            Self::ReadFailed(path) => format!("read_failed:{path}"),
            Self::MissingAppId => "missing_app_id".into(),
            Self::DecryptionFailed => "decryption_failed".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Standard,
    Encrypted,
}

impl Format {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standardWxapkg",
            Self::Encrypted => "encryptedWxapkg",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub offset: u64,
    pub size: u64,
}

pub struct Archive {
    pub path: PathBuf,
    pub format: Format,
    /// Size of the logical (decrypted) package.
    pub file_size: u64,
    pub index_size: usize,
    pub declared_body_size: u64,
    /// One entry per path (a repeated name keeps its last index entry), in
    /// natural path order.
    pub entries: Vec<Entry>,
    pub notes: Vec<String>,
    reader: ByteReader,
}

impl Archive {
    /// Opens a package, inferring the AppID needed for encrypted packages from
    /// the nearest `wx` + 16 hex path component.
    pub fn open(path: &Path) -> Result<Self, ArchiveError> {
        Self::open_with_app_id(path, inferred_app_id(path).as_deref())
    }

    pub fn open_with_app_id(path: &Path, app_id: Option<&str>) -> Result<Self, ArchiveError> {
        let mut header = [0u8; 14];
        let mut file = File::open(path).map_err(|_| ArchiveError::Unreadable)?;
        let read = read_up_to(&mut file, &mut header).map_err(|_| ArchiveError::Unreadable)?;
        if read != header.len() {
            return Err(ArchiveError::TruncatedHeader);
        }
        drop(file);

        if is_standard_header(&header) {
            Self::parse(path, ByteReader::plain(path)?, Format::Standard, Vec::new())
        } else if is_encrypted_header(&header) {
            let app_id = app_id.filter(|id| !id.is_empty()).ok_or(ArchiveError::MissingAppId)?;
            Self::parse(
                path,
                ByteReader::encrypted(path, app_id)?,
                Format::Encrypted,
                vec!["已在内存中读取微信缓存加密层，原包保持不变".into()],
            )
        } else {
            Err(ArchiveError::UnrecognizedFormat)
        }
    }

    /// Reads one entry's bytes, refusing entries larger than `maximum_size`.
    pub fn read_limited(&self, entry: &Entry, maximum_size: u64) -> Result<Vec<u8>, ArchiveError> {
        if entry.size > maximum_size {
            return Err(ArchiveError::PreviewTooLarge(entry.size));
        }
        self.read(entry)
    }

    pub fn read(&self, entry: &Entry) -> Result<Vec<u8>, ArchiveError> {
        self.reader.read(entry.offset, entry.size as usize)
    }

    fn parse(path: &Path, reader: ByteReader, format: Format, mut notes: Vec<String>) -> Result<Self, ArchiveError> {
        let file_size = reader.size;
        let header = reader.read(0, 14)?;
        if !is_standard_header(&header) {
            return Err(match format {
                Format::Encrypted => ArchiveError::DecryptionFailed,
                Format::Standard => ArchiveError::UnrecognizedFormat,
            });
        }

        let index_size = big_endian_u32(&header, 5) as usize;
        let declared_body_size = u64::from(big_endian_u32(&header, 9));
        if !(4..=MAXIMUM_INDEX_SIZE).contains(&index_size) {
            return Err(ArchiveError::InvalidIndexSize);
        }
        let index = reader.read(14, index_size)?;

        let file_count = big_endian_u32(&index, 0) as usize;
        if file_count == 0 || file_count >= 100_000 {
            return Err(ArchiveError::InvalidFileCount);
        }

        let body_offset = 14 + index_size as u64;
        let mut entries: Vec<Entry> = Vec::with_capacity(file_count.min(2_000));
        let mut positions: HashMap<String, usize> = HashMap::new();
        let mut duplicates: HashMap<String, Vec<Entry>> = HashMap::new();
        let mut cursor = 4usize;

        for _ in 0..file_count {
            if cursor + 4 > index.len() {
                return Err(ArchiveError::MalformedIndex);
            }
            let name_length = big_endian_u32(&index, cursor) as usize;
            cursor += 4;
            if name_length == 0 || name_length >= 16_384 || cursor + name_length + 8 > index.len() {
                return Err(ArchiveError::MalformedIndex);
            }
            let raw_name =
                std::str::from_utf8(&index[cursor..cursor + name_length]).map_err(|_| ArchiveError::InvalidPath)?;
            cursor += name_length;
            let path = normalized_archive_path(raw_name);
            if !is_safe_archive_path(&path) {
                return Err(ArchiveError::InvalidPath);
            }
            let offset = u64::from(big_endian_u32(&index, cursor));
            cursor += 4;
            let size = u64::from(big_endian_u32(&index, cursor));
            cursor += 4;
            if offset < body_offset || offset + size > file_size {
                return Err(ArchiveError::EntryOutOfBounds(path));
            }

            let entry = Entry { path: path.clone(), offset, size };
            // A repeated name is kept, not rejected: an index read in order
            // resolves the name to its last entry.
            if let Some(&position) = positions.get(&path) {
                duplicates.entry(path).or_insert_with(|| vec![entries[position].clone()]).push(entry.clone());
                entries[position] = entry;
            } else {
                positions.insert(path, entries.len());
                entries.push(entry);
            }
        }

        let mut duplicate_paths: Vec<_> = duplicates.into_iter().collect();
        duplicate_paths.sort_by(|a, b| a.0.cmp(&b.0));
        for (path, copies) in &duplicate_paths {
            notes.push(duplicate_note(path, copies, &reader));
        }
        let actual_body_size = file_size.saturating_sub(body_offset);
        if declared_body_size != actual_body_size {
            notes.push("包头声明的数据区大小与实际文件存在差异".into());
        }
        if cursor != index.len() {
            notes.push(format!("索引末尾包含 {} 字节扩展数据", index.len() - cursor));
        }

        entries.sort_by(|a, b| natural_compare(&a.path, &b.path));
        Ok(Self {
            path: path.to_path_buf(),
            format,
            file_size,
            index_size,
            declared_body_size,
            entries,
            notes,
            reader,
        })
    }
}

fn duplicate_note(path: &str, copies: &[Entry], reader: &ByteReader) -> String {
    let ranges: Vec<String> = copies.iter().map(|copy| format!("{}+{}", copy.offset, copy.size)).collect();
    let first = &copies[0];
    let identical = if copies.iter().all(|copy| copy.offset == first.offset && copy.size == first.size) {
        true
    } else if copies.iter().all(|copy| copy.size == first.size) && first.size <= DUPLICATE_COMPARE_LIMIT {
        let contents: Vec<_> = copies.iter().filter_map(|copy| reader.read(copy.offset, copy.size as usize).ok()).collect();
        contents.len() == copies.len() && contents.iter().all(|content| *content == contents[0])
    } else {
        false
    };
    format!(
        "包内路径重复：{path} 出现 {} 次（偏移+大小 {}），{}；按顺序读取索引时以最后一条为准，已保留最后一条",
        copies.len(),
        ranges.join("、"),
        if identical { "内容相同" } else { "内容不同" }
    )
}

fn is_standard_header(header: &[u8]) -> bool {
    header.len() >= 14 && header[0] == 0xBE && header[13] == 0xED
}

fn big_endian_u32(data: &[u8], offset: usize) -> u32 {
    data.get(offset..offset + 4).map_or(0, |bytes| u32::from_be_bytes(bytes.try_into().expect("4 bytes")))
}

fn read_up_to(file: &mut File, buffer: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

/// `\` becomes `/`; leading `/` and `./` are dropped.
pub fn normalized_archive_path(path: &str) -> String {
    let replaced = path.replace('\\', "/");
    let mut rest = replaced.as_str();
    loop {
        if let Some(stripped) = rest.strip_prefix("./") {
            rest = stripped;
        } else if let Some(stripped) = rest.strip_prefix('/') {
            rest = stripped;
        } else {
            return rest.to_string();
        }
    }
}

/// Relative, no empty / `.` / `..` components, no NUL, components ≤ 255 bytes.
pub fn is_safe_archive_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\0')
        && path.split('/').all(|component| !component.is_empty() && component != "." && component != ".." && component.len() <= 255)
}

/// The nearest path component shaped like an AppID (`wx` + 16 hex digits).
pub fn inferred_app_id(path: &Path) -> Option<String> {
    path.components().rev().filter_map(|component| component.as_os_str().to_str()).find(|name| is_app_id(name)).map(str::to_string)
}

fn is_app_id(name: &str) -> bool {
    name.len() == 18 && name.starts_with("wx") && name[2..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// File-browser ordering in the CLDR root collation, as the Swift side sorts
/// outputs (`standardCompare`): case- and width-insensitive, digit runs by
/// value, punctuation (`_` < `-` < `.` < `/` …) before digits before Latin
/// letters before other scripts (Han by code point), lowercase first on ties.
/// Deliberately not the user's locale, so every machine lists files alike.
pub fn natural_compare(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (mut left, mut right) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (left.peek().copied(), right.peek().copied()) {
            (None, None) => return case_tie_break(a, b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let take = |iter: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut digits = String::new();
                    while let Some(&c) = iter.peek().filter(|c| c.is_ascii_digit()) {
                        digits.push(c);
                        iter.next();
                    }
                    digits
                };
                let (x, y) = (take(&mut left), take(&mut right));
                let (xt, yt) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
                let ordering = xt.len().cmp(&yt.len()).then_with(|| xt.cmp(yt));
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
            (Some(x), Some(y)) => {
                let ordering = collation_weight(x).cmp(&collation_weight(y));
                if ordering != Ordering::Equal {
                    return ordering;
                }
                left.next();
                right.next();
            }
        }
    }
}

/// CLDR root order of ASCII whitespace and punctuation (non-ignorable).
const PUNCTUATION_ORDER: &str = "\t\n\u{0B}\u{0C}\r _-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$";

/// Primary weight: punctuation and symbols < digits < letters (case-, width-
/// and accent-folded) < other scripts (by code point).
fn collation_weight(c: char) -> (u8, u32) {
    let c = fold_width(c);
    if let Some(position) = PUNCTUATION_ORDER.find(c) {
        (0, position as u32)
    } else if c.is_ascii_digit() {
        (1, c as u32)
    } else if c.is_ascii_alphabetic() {
        (2, c.to_ascii_lowercase() as u32)
    } else if let Some(base) = latin_base(c) {
        (2, base as u32)
    } else if is_symbol(c) {
        (0, 0x1000 + c as u32)
    } else {
        (3, c.to_lowercase().next().unwrap_or(c) as u32)
    }
}

/// Fullwidth ASCII (`ｚ`, `１`) collates with its ASCII form.
fn fold_width(c: char) -> char {
    match c as u32 {
        0xFF01..=0xFF5E => char::from_u32(c as u32 - 0xFF01 + 0x21).unwrap_or(c),
        0x3000 => ' ',
        _ => c,
    }
}

/// The base letter of a Latin-1 accented letter (`é` → `e`).
fn latin_base(c: char) -> Option<char> {
    const TABLE: &str = "aaaaaaaceeeeiiii.nooooo.ouuuuy.saaaaaaaceeeeiiii.nooooo.ouuuuy.y";
    let code = c as u32;
    if !(0xC0..=0xFF).contains(&code) {
        return None;
    }
    TABLE.chars().nth((code - 0xC0) as usize).filter(|&base| base != '.')
}

/// Emoji, arrows, shapes and general punctuation: collate before digits.
fn is_symbol(c: char) -> bool {
    matches!(c as u32,
        0x2000..=0x2BFF | 0x3001..=0x303F | 0x1F000..=0x1FAFF | 0xFE30..=0xFE4F)
}

/// Equal at the primary level: lowercase sorts before uppercase, then by
/// code point, so the order is total.
fn case_tie_break(a: &str, b: &str) -> std::cmp::Ordering {
    let lower_first = |s: &str| s.chars().map(|c| if c.is_uppercase() { (1, c) } else { (0, c) }).collect::<Vec<_>>();
    lower_first(a).cmp(&lower_first(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain_package(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut index = (entries.len() as u32).to_be_bytes().to_vec();
        let index_size = 4 + entries.iter().map(|(name, _)| 12 + name.len()).sum::<usize>();
        let mut offset = 14 + index_size as u32;
        let mut body = Vec::new();
        for (name, content) in entries {
            index.extend((name.len() as u32).to_be_bytes());
            index.extend(name.as_bytes());
            index.extend(offset.to_be_bytes());
            index.extend((content.len() as u32).to_be_bytes());
            offset += content.len() as u32;
            body.extend_from_slice(content);
        }
        let mut package = vec![0xBE, 0, 0, 0, 0];
        package.extend((index_size as u32).to_be_bytes());
        package.extend((body.len() as u32).to_be_bytes());
        package.push(0xED);
        package.extend(index);
        package.extend(body);
        package
    }

    fn write_temp(name: &str, bytes: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wxlocated-engine-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn reads_plain_package_and_keeps_last_duplicate() {
        let path = write_temp(
            "dup.wxapkg",
            &plain_package(&[("/app.js", b"one"), ("./pages/a.js", b"aa"), ("/app.js", b"two")]),
        );
        let archive = Archive::open(&path).unwrap();
        assert_eq!(archive.format, Format::Standard);
        let paths: Vec<_> = archive.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["app.js", "pages/a.js"]);
        assert_eq!(archive.read(&archive.entries[0]).unwrap(), b"two");
        assert_eq!(archive.notes.len(), 1);
        assert!(archive.notes[0].starts_with("包内路径重复：app.js 出现 2 次"), "{}", archive.notes[0]);
        assert!(archive.notes[0].contains("内容不同"));
    }

    #[test]
    fn rejects_traversal() {
        let path = write_temp("evil.wxapkg", &plain_package(&[("../x.js", b"x")]));
        assert_eq!(Archive::open(&path).err(), Some(ArchiveError::InvalidPath));
    }

    #[test]
    fn infers_nearest_app_id() {
        let path = Path::new("/c/wx1111111111111111/plugin/wxaaaaaaaaaaaaaaaa/3/__APP__.wxapkg");
        assert_eq!(inferred_app_id(path).as_deref(), Some("wxaaaaaaaaaaaaaaaa"));
    }

    #[test]
    fn natural_order() {
        let mut names = vec!["p10.js", "P2.js", "p1.js"];
        names.sort_by(|a, b| natural_compare(a, b));
        assert_eq!(names, ["p1.js", "P2.js", "p10.js"]);
    }

    /// Orders observed from Swift's `localizedStandardCompare` on real packages.
    #[test]
    fn punctuation_follows_collation_order() {
        let mut names = vec![
            "score/score.html",
            "score/score_old.html",
            "captcha/captcha.html",
            "captcha_min/captcha.html",
            "annual-arrow-right.svg",
            "annual_entry_2025.png",
        ];
        names.sort_by(|a, b| natural_compare(a, b));
        assert_eq!(
            names,
            [
                "annual_entry_2025.png",
                "annual-arrow-right.svg",
                "captcha_min/captcha.html",
                "captcha/captcha.html",
                "score/score_old.html",
                "score/score.html",
            ]
        );
    }
}
