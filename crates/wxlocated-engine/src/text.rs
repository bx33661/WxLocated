//! String checks that reproduce Swift's `Character` (grapheme) semantics where
//! they differ from byte comparison and matter for our output.
//!
//! The case that occurs in real input is CRLF: Swift treats `"\r\n"` as one
//! `Character`, so `"a\r\n".hasSuffix("\n")` is false. Combining-mark clusters
//! (`" " + U+0301`) are not reproduced; they do not occur in compiled packages.

/// Swift `String.hasSuffix` for the suffixes the restorers use.
pub fn has_suffix(text: &str, suffix: &str) -> bool {
    if !text.ends_with(suffix) {
        return false;
    }
    // The suffix must start on a Character boundary: a leading "\n" that is
    // the second half of "\r\n" does not.
    !(suffix.starts_with('\n') && text[..text.len() - suffix.len()].ends_with('\r'))
}

/// Swift `String.hasPrefix` (the mirror case: a trailing "\r" of the prefix
/// that is the first half of "\r\n" in the text).
pub fn has_prefix(text: &str, prefix: &str) -> bool {
    text.starts_with(prefix) && !(prefix.ends_with('\r') && text[prefix.len()..].starts_with('\n'))
}

/// Swift `String.contains(_: String)` (the stdlib, Character-based form): an
/// occurrence may not start on the `\n` of a `\r\n` or end on its `\r`.
pub fn contains(text: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    text.match_indices(needle).any(|(start, _)| {
        let end = start + needle.len();
        let splits_start = needle.starts_with('\n') && text[..start].ends_with('\r');
        let splits_end = needle.ends_with('\r') && text[end..].starts_with('\n');
        !splits_start && !splits_end
    })
}

/// Swift `Character.isWhitespace`.
pub fn is_whitespace(c: char) -> bool {
    c.is_whitespace()
}

/// Number of Swift `Character`s, counting "\r\n" once. Used where Swift code
/// compares `count`/`prefix(n)`.
pub fn character_count(text: &str) -> usize {
    text.chars().count() - text.matches("\r\n").count()
}

/// Swift `String.trimmingCharacters(in: .whitespacesAndNewlines)`.
pub fn trim_whitespace_and_newlines(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace())
}

/// `String.removingPercentEncoding`: `None` for a `%` not followed by two hex
/// digits, or bytes that are not UTF-8.
pub fn removing_percent_encoding(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = bytes.get(index + 1..index + 3)?;
            let value = u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
            if !hex.iter().all(u8::is_ascii_hexdigit) {
                return None;
            }
            output.push(value);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(output).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crlf_is_one_character() {
        assert!(!has_suffix("a\r\n", "\n"));
        assert!(has_suffix("a\n", "\n"));
        assert!(has_suffix("a\r\n", "\r\n"));
        assert!(!has_suffix("a\r\n\n", "\n\n"));
        assert!(has_suffix("a\n\n", "\n\n"));
        assert_eq!(character_count("a\r\nb"), 3);
        assert!(!contains("a\r\nb", "\n"));
        assert!(contains("a\r\n\nb", "\n"));
        assert!(contains("a\nb", "\n"));
        assert_eq!(removing_percent_encoding("a%20b").as_deref(), Some("a b"));
        assert_eq!(removing_percent_encoding("a%2").as_deref(), None);
        assert_eq!(removing_percent_encoding("%E4%B8%AD").as_deref(), Some("中"));
        assert_eq!(removing_percent_encoding("%FF").as_deref(), None);
    }
}
