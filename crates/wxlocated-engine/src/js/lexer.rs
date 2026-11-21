//! A forgiving byte-level JavaScript tokenizer. Token offsets are UTF-8 byte
//! offsets into the source, which is what source ranges in the manifest use.

use std::borrow::Cow;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Identifier(String),
    /// Decoded value of a quoted string literal.
    String(String),
    Number,
    Template,
    Regex,
    Comment,
    Punctuator(u8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: Kind,
    pub start: usize,
    pub end: usize,
}

impl Token {
    pub fn text<'a>(&self, bytes: &'a [u8]) -> Cow<'a, str> {
        String::from_utf8_lossy(&bytes[self.start..self.end])
    }

    pub fn is_punctuator(&self, byte: u8) -> bool {
        self.kind == Kind::Punctuator(byte)
    }

    pub fn identifier(&self) -> Option<&str> {
        match &self.kind {
            Kind::Identifier(name) => Some(name),
            _ => None,
        }
    }

    pub fn string(&self) -> Option<&str> {
        match &self.kind {
            Kind::String(value) => Some(value),
            _ => None,
        }
    }

    pub fn is_comment(&self) -> bool {
        self.kind == Kind::Comment
    }
}

pub fn tokenize(bytes: &[u8]) -> Vec<Token> {
    let mut tokens: Vec<Token> = Vec::with_capacity((bytes.len() / 5).clamp(32, 50_000));
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if is_whitespace(byte) {
            index += 1;
            continue;
        }

        if byte == b'/' && index + 1 < bytes.len() {
            if bytes[index + 1] == b'/' {
                let start = index;
                index += 2;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
                tokens.push(Token { kind: Kind::Comment, start, end: index });
                continue;
            }
            if bytes[index + 1] == b'*' {
                let start = index;
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/') {
                    index += 1;
                }
                index = bytes.len().min(index + 2);
                tokens.push(Token { kind: Kind::Comment, start, end: index });
                continue;
            }
            if expects_expression(tokens.last()) {
                let end = scan_regex(bytes, index);
                if end > index + 1 {
                    tokens.push(Token { kind: Kind::Regex, start: index, end });
                    index = end;
                    continue;
                }
            }
        }

        if byte == b'"' || byte == b'\'' {
            let start = index;
            index = scan_quoted(bytes, index, byte);
            let inner_start = index.min(start + 1);
            let inner_end = inner_start.max(index.saturating_sub(1));
            let raw = String::from_utf8_lossy(&bytes[inner_start..inner_end]);
            tokens.push(Token { kind: Kind::String(decode_string_literal(&raw)), start, end: index });
            continue;
        }

        if byte == b'`' {
            let start = index;
            index = scan_template(bytes, index);
            tokens.push(Token { kind: Kind::Template, start, end: index });
            continue;
        }

        if is_identifier_start(byte) {
            let start = index;
            index += 1;
            while index < bytes.len() && is_identifier_continuation(bytes[index]) {
                index += 1;
            }
            let name = String::from_utf8_lossy(&bytes[start..index]).into_owned();
            tokens.push(Token { kind: Kind::Identifier(name), start, end: index });
            continue;
        }

        if is_digit(byte) || (byte == b'.' && index + 1 < bytes.len() && is_digit(bytes[index + 1])) {
            let start = index;
            index += 1;
            while index < bytes.len()
                && (is_digit(bytes[index]) || is_identifier_continuation(bytes[index]) || bytes[index] == b'.')
            {
                index += 1;
            }
            tokens.push(Token { kind: Kind::Number, start, end: index });
            continue;
        }

        tokens.push(Token { kind: Kind::Punctuator(byte), start: index, end: index + 1 });
        index += 1;
    }
    tokens
}

/// Index of the token closing the bracket opened at `opening`.
pub fn matching_token(tokens: &[Token], opening: usize, open: u8, close: u8) -> Option<usize> {
    if opening >= tokens.len() || !tokens[opening].is_punctuator(open) {
        return None;
    }
    let mut depth = 0i64;
    for (index, token) in tokens.iter().enumerate().skip(opening) {
        if token.is_punctuator(open) {
            depth += 1;
        } else if token.is_punctuator(close) {
            depth -= 1;
            if depth == 0 {
                return Some(index);
            }
        }
    }
    None
}

/// First non-comment token strictly after `index`.
pub fn next_significant(tokens: &[Token], index: usize) -> Option<usize> {
    (index + 1..tokens.len()).find(|&candidate| !tokens[candidate].is_comment())
}

/// First non-comment token at or after `index`.
pub fn significant_at_or_after(tokens: &[Token], index: usize) -> Option<usize> {
    (index..tokens.len()).find(|&candidate| !tokens[candidate].is_comment())
}

/// Last non-comment token strictly before `index`.
pub fn previous_significant(tokens: &[Token], index: usize) -> Option<usize> {
    (0..index.min(tokens.len())).rev().find(|&candidate| !tokens[candidate].is_comment())
}

fn scan_quoted(bytes: &[u8], start: usize, quote: u8) -> usize {
    let mut index = start + 1;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            index = bytes.len().min(index + 2);
        } else if bytes[index] == quote {
            return index + 1;
        } else {
            index += 1;
        }
    }
    bytes.len()
}

const MAXIMUM_TEMPLATE_DEPTH: usize = 128;

fn scan_template(bytes: &[u8], start: usize) -> usize {
    scan_template_inner(bytes, start, 0)
}

fn scan_template_inner(bytes: &[u8], start: usize, depth: usize) -> usize {
    let mut index = start + 1;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            index = bytes.len().min(index + 2);
        } else if bytes[index] == b'`' {
            return index + 1;
        } else if bytes[index] == b'$'
            && index + 1 < bytes.len()
            && bytes[index + 1] == b'{'
            && depth < MAXIMUM_TEMPLATE_DEPTH
        {
            index = scan_template_expression(bytes, index + 2, depth + 1);
        } else {
            index += 1;
        }
    }
    bytes.len()
}

fn scan_template_expression(bytes: &[u8], mut index: usize, depth: usize) -> usize {
    let mut brace_depth = 1usize;
    let mut expects_regex = true;
    while index < bytes.len() && brace_depth > 0 {
        let byte = bytes[index];
        if is_whitespace(byte) {
            index += 1;
            continue;
        }
        if byte == b'\'' || byte == b'"' {
            index = scan_quoted(bytes, index, byte);
            expects_regex = false;
            continue;
        }
        if byte == b'`' {
            index = scan_template_inner(bytes, index, depth);
            expects_regex = false;
            continue;
        }
        if byte == b'/' && index + 1 < bytes.len() {
            if bytes[index + 1] == b'/' {
                index += 2;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
                continue;
            }
            if bytes[index + 1] == b'*' {
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/') {
                    index += 1;
                }
                index = bytes.len().min(index + 2);
                continue;
            }
            if expects_regex {
                let end = scan_regex(bytes, index);
                if end > index + 1 {
                    index = end;
                    expects_regex = false;
                    continue;
                }
            }
        }
        if is_identifier_start(byte) {
            let start = index;
            index += 1;
            while index < bytes.len() && is_identifier_continuation(bytes[index]) {
                index += 1;
            }
            let name = &bytes[start..index];
            expects_regex = matches!(
                name,
                b"return"
                    | b"throw"
                    | b"case"
                    | b"delete"
                    | b"void"
                    | b"typeof"
                    | b"instanceof"
                    | b"in"
                    | b"of"
                    | b"yield"
                    | b"await"
            );
            continue;
        }
        if is_digit(byte) || (byte == b'.' && index + 1 < bytes.len() && is_digit(bytes[index + 1])) {
            index += 1;
            while index < bytes.len()
                && (is_digit(bytes[index]) || is_identifier_continuation(bytes[index]) || bytes[index] == b'.')
            {
                index += 1;
            }
            expects_regex = false;
            continue;
        }
        if byte == b'{' {
            brace_depth += 1;
            expects_regex = true;
        } else if byte == b'}' {
            brace_depth -= 1;
            expects_regex = false;
        } else {
            expects_regex = !matches!(byte, b')' | b']');
        }
        index += 1;
    }
    index
}

fn scan_regex(bytes: &[u8], start: usize) -> usize {
    let mut index = start + 1;
    let mut in_class = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'\\' {
            index = bytes.len().min(index + 2);
            continue;
        }
        if byte == b'[' {
            in_class = true;
        }
        if byte == b']' {
            in_class = false;
        }
        if byte == b'/' && !in_class {
            index += 1;
            while index < bytes.len() && is_identifier_continuation(bytes[index]) {
                index += 1;
            }
            return index;
        }
        if byte == b'\n' || byte == b'\r' {
            return start + 1;
        }
        index += 1;
    }
    start + 1
}

fn expects_expression(token: Option<&Token>) -> bool {
    let Some(token) = token else { return true };
    match &token.kind {
        Kind::Identifier(value) => matches!(
            value.as_str(),
            "return" | "throw" | "case" | "delete" | "void" | "typeof" | "instanceof" | "in" | "of" | "yield" | "await"
        ),
        Kind::Number | Kind::String(_) | Kind::Template | Kind::Regex => false,
        Kind::Comment => true,
        Kind::Punctuator(value) => !matches!(value, b')' | b']' | b'}'),
    }
}

fn decode_string_literal(raw: &str) -> String {
    let mut output = String::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        let character = raw[i..].chars().next().expect("valid utf8");
        if character != '\\' {
            output.push(character);
            i += character.len_utf8();
            continue;
        }
        i += 1;
        if i >= raw.len() {
            output.push('\\');
            break;
        }
        let escaped = raw[i..].chars().next().expect("valid utf8");
        i += escaped.len_utf8();
        match escaped {
            'n' => output.push('\n'),
            'r' => output.push('\r'),
            '\r' => {
                if bytes.get(i) == Some(&b'\n') {
                    i += 1;
                }
            }
            '\n' => {}
            't' => output.push('\t'),
            'b' => output.push('\u{8}'),
            'f' => output.push('\u{c}'),
            'v' => output.push('\u{b}'),
            '0' => output.push('\0'),
            'x' => {
                if let Some(hex) = parse_hex_bytes(bytes.get(i..i + 2))
                    && let Ok(value) = u8::try_from(hex)
                {
                    output.push(char::from(value));
                    i += 2;
                } else {
                    output.push('x');
                }
            }
            'u' => {
                if bytes.get(i) == Some(&b'{') {
                    if let Some(rel_close) = bytes[i + 1..].iter().position(|&b| b == b'}')
                        && rel_close > 0
                        && let Some(code) = parse_hex_bytes(Some(&bytes[i + 1..i + 1 + rel_close]))
                        && let Some(decoded) = char::from_u32(code)
                    {
                        output.push(decoded);
                        i += 1 + rel_close + 1;
                    } else {
                        output.push('u');
                    }
                } else if let Some(code) = parse_hex_bytes(bytes.get(i..i + 4)) {
                    i += 4;
                    if (0xD800..=0xDBFF).contains(&code) {
                        if bytes.get(i..i + 2) == Some(b"\\u")
                            && let Some(low) = parse_hex_bytes(bytes.get(i + 2..i + 6))
                            && (0xDC00..=0xDFFF).contains(&low)
                        {
                            let scalar = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                            output.push(char::from_u32(scalar).unwrap_or(char::REPLACEMENT_CHARACTER));
                            i += 6;
                        } else {
                            output.push(char::REPLACEMENT_CHARACTER);
                        }
                    } else if (0xDC00..=0xDFFF).contains(&code) {
                        output.push(char::REPLACEMENT_CHARACTER);
                    } else {
                        output.push(char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER));
                    }
                } else {
                    output.push('u');
                }
            }
            other => output.push(other),
        }
    }
    output
}

fn parse_hex_bytes(slice: Option<&[u8]>) -> Option<u32> {
    let slice = slice?;
    if slice.is_empty() || !slice.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let text = std::str::from_utf8(slice).ok()?;
    u32::from_str_radix(text, 16).ok()
}

fn is_whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C)
}

fn is_digit(byte: u8) -> bool {
    byte.is_ascii_digit()
}

fn is_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'$' || byte == b'_' || byte >= 0x80
}

fn is_identifier_continuation(byte: u8) -> bool {
    is_identifier_start(byte) || is_digit(byte)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_and_offsets() {
        let source = "var a='x\\n',b=/re[/]/g;// c\nf(1.5)";
        let tokens = tokenize(source.as_bytes());
        let kinds: Vec<_> = tokens.iter().map(|t| t.kind.clone()).collect();
        assert_eq!(kinds[0], Kind::Identifier("var".into()));
        assert_eq!(kinds[3], Kind::String("x\n".into()));
        assert_eq!(kinds[7], Kind::Regex);
        assert_eq!(&source[tokens[7].start..tokens[7].end], "/re[/]/g");
        assert_eq!(kinds[9], Kind::Comment);
        assert_eq!(kinds[12], Kind::Number);
    }

    #[test]
    fn division_is_not_regex() {
        let tokens = tokenize(b"a/b/c");
        assert!(tokens.iter().all(|t| t.kind != Kind::Regex));
    }

    #[test]
    fn utf8_offsets_are_bytes() {
        let source = "x='中文';y";
        let tokens = tokenize(source.as_bytes());
        assert_eq!(tokens[2].kind, Kind::String("中文".into()));
        assert_eq!(tokens[4].start, source.find('y').unwrap());
    }

    #[test]
    fn decodes_unicode_hex_surrogates_and_line_continuations() {
        assert_eq!(decode_string_literal(r"\u4e2d\u6587"), "中文");
        assert_eq!(decode_string_literal(r"\x41\x62"), "Ab");
        assert_eq!(decode_string_literal(r"\u{1F600}"), "😀");
        assert_eq!(decode_string_literal(r"\uD83D\uDE00"), "😀");
        assert_eq!(decode_string_literal("a\\\nb\\\r\nc\\\rd"), "abcd");
        assert_eq!(decode_string_literal(r"x\r\ny"), "x\r\ny");
        assert_eq!(decode_string_literal(r"\xZZ"), "xZZ");
        assert_eq!(decode_string_literal(r"\u12"), "u12");
        assert_eq!(decode_string_literal(r"\uD800"), "\u{FFFD}");
    }

    #[test]
    fn scans_nested_template_expressions() {
        let source = r#"const s = `a ${`b ${"}"} c`} d`; next"#;
        let tokens = tokenize(source.as_bytes());
        assert_eq!(tokens[3].kind, Kind::Template);
        assert_eq!(&source[tokens[3].start..tokens[3].end], r#"`a ${`b ${"}"} c`} d`"#);
        assert!(tokens[4].is_punctuator(b';'));
        assert_eq!(tokens[5].identifier(), Some("next"));
    }
}
