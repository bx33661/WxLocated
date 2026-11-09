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

fn scan_template(bytes: &[u8], start: usize) -> usize {
    let mut index = start + 1;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            index = bytes.len().min(index + 2);
        } else if bytes[index] == b'`' {
            return index + 1;
        } else {
            index += 1;
        }
    }
    bytes.len()
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
    let mut characters = raw.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            output.push(character);
            continue;
        }
        let Some(escaped) = characters.next() else {
            output.push(character);
            continue;
        };
        match escaped {
            'n' => output.push('\n'),
            'r' => output.push('\r'),
            't' => output.push('\t'),
            'b' => output.push('\u{8}'),
            'f' => output.push('\u{c}'),
            'v' => output.push('\u{b}'),
            '0' => output.push('\0'),
            other => output.push(other),
        }
    }
    output
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
}
