//! JSON read and written exactly as Foundation's `JSONSerialization` does,
//! because restored configuration files are compared byte for byte.
//!
//! Behaviour checked against real Foundation:
//! - parsing is strict JSON, plus a trailing comma in arrays and objects and a
//!   leading BOM; a repeated key keeps its first value; lone surrogates,
//!   control characters and out-of-range numbers are errors;
//! - integers keep their exact digits; a number with a fraction or exponent is
//!   a `Double` (written `%.17g`) unless it has more than 17 significant
//!   digits, which Foundation keeps as a decimal and writes back verbatim;
//! - pretty printing indents by two spaces, writes `"key" : value`, writes an
//!   empty container as an opening line, a blank line and the closing bracket,
//!   sorts keys with Finder-style collation, escapes only `"`, `\` and control
//!   characters (lowercase `\u00xx`), and leaves `/` as is.

use crate::archive::natural_compare;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    /// Exact integer digits (`-0` normalised to `0`).
    Integer(String),
    Double(f64),
    /// More than 17 significant digits: kept as written.
    Decimal(String),
    String(String),
    Array(Vec<Value>),
    /// Insertion order of first occurrences.
    Object(Vec<(String, Value)>),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Self::Object(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&Vec<(String, Value)>> {
        match self {
            Self::Object(entries) => Some(entries),
            _ => None,
        }
    }

    /// `as? [String]`: an array whose elements are all strings.
    pub fn as_string_array(&self) -> Option<Vec<String>> {
        match self {
            Self::Array(items) => items.iter().map(|item| item.as_str().map(str::to_string)).collect(),
            _ => None,
        }
    }

    /// `as? [[String: Any]]`: an array whose elements are all objects.
    pub fn as_object_array(&self) -> Option<Vec<Vec<(String, Value)>>> {
        match self {
            Self::Array(items) => items.iter().map(|item| item.as_object().cloned()).collect(),
            _ => None,
        }
    }
}

/// Sets `key`, keeping its position if present (dictionary assignment).
pub fn set(entries: &mut Vec<(String, Value)>, key: &str, value: Value) {
    match entries.iter_mut().find(|(k, _)| k == key) {
        Some(entry) => entry.1 = value,
        None => entries.push((key.to_string(), value)),
    }
}

pub fn remove(entries: &mut Vec<(String, Value)>, key: &str) {
    entries.retain(|(k, _)| k != key);
}

// MARK: Parsing

/// `JSONSerialization.jsonObject(with:)` (no fragments: the top level must be
/// an array or an object).
pub fn parse(text: &str) -> Option<Value> {
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    let mut parser = Parser { bytes: text.as_bytes(), position: 0 };
    parser.skip_whitespace();
    let value = parser.value(0)?;
    if !matches!(value, Value::Array(_) | Value::Object(_)) {
        return None;
    }
    parser.skip_whitespace();
    (parser.position == parser.bytes.len()).then_some(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.position += 1;
        }
    }

    fn literal(&mut self, word: &str, value: Value) -> Option<Value> {
        if self.bytes[self.position..].starts_with(word.as_bytes()) {
            self.position += word.len();
            Some(value)
        } else {
            None
        }
    }

    fn value(&mut self, depth: usize) -> Option<Value> {
        if depth > 512 {
            return None;
        }
        match self.peek()? {
            b'{' => self.object(depth),
            b'[' => self.array(depth),
            b'"' => self.string().map(Value::String),
            b't' => self.literal("true", Value::Bool(true)),
            b'f' => self.literal("false", Value::Bool(false)),
            b'n' => self.literal("null", Value::Null),
            b'-' | b'0'..=b'9' => self.number(),
            _ => None,
        }
    }

    fn array(&mut self, depth: usize) -> Option<Value> {
        self.position += 1;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.position += 1;
            return Some(Value::Array(items));
        }
        loop {
            self.skip_whitespace();
            items.push(self.value(depth + 1)?);
            self.skip_whitespace();
            match self.peek()? {
                b',' => {
                    self.position += 1;
                    self.skip_whitespace();
                    // A trailing comma is accepted.
                    if self.peek() == Some(b']') {
                        self.position += 1;
                        return Some(Value::Array(items));
                    }
                }
                b']' => {
                    self.position += 1;
                    return Some(Value::Array(items));
                }
                _ => return None,
            }
        }
    }

    fn object(&mut self, depth: usize) -> Option<Value> {
        self.position += 1;
        let mut entries: Vec<(String, Value)> = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.position += 1;
            return Some(Value::Object(entries));
        }
        loop {
            self.skip_whitespace();
            if self.peek()? != b'"' {
                return None;
            }
            let key = self.string()?;
            self.skip_whitespace();
            if self.peek()? != b':' {
                return None;
            }
            self.position += 1;
            self.skip_whitespace();
            let value = self.value(depth + 1)?;
            // A repeated key keeps its first value.
            if !entries.iter().any(|(existing, _)| *existing == key) {
                entries.push((key, value));
            }
            self.skip_whitespace();
            match self.peek()? {
                b',' => {
                    self.position += 1;
                    self.skip_whitespace();
                    if self.peek() == Some(b'}') {
                        self.position += 1;
                        return Some(Value::Object(entries));
                    }
                }
                b'}' => {
                    self.position += 1;
                    return Some(Value::Object(entries));
                }
                _ => return None,
            }
        }
    }

    fn hex4(&mut self) -> Option<u32> {
        let digits = self.bytes.get(self.position..self.position + 4)?;
        let text = std::str::from_utf8(digits).ok()?;
        let value = u32::from_str_radix(text, 16).ok()?;
        if !digits.iter().all(u8::is_ascii_hexdigit) {
            return None;
        }
        self.position += 4;
        Some(value)
    }

    fn string(&mut self) -> Option<String> {
        self.position += 1;
        let mut output: Vec<u8> = Vec::new();
        loop {
            let byte = self.peek()?;
            self.position += 1;
            match byte {
                b'"' => return String::from_utf8(output).ok(),
                0x00..=0x1F => return None,
                b'\\' => {
                    let escape = self.peek()?;
                    self.position += 1;
                    let character = match escape {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let first = self.hex4()?;
                            if (0xD800..0xDC00).contains(&first) {
                                if self.bytes.get(self.position..self.position + 2) != Some(b"\\u") {
                                    return None;
                                }
                                self.position += 2;
                                let second = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&second) {
                                    return None;
                                }
                                char::from_u32(0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00))?
                            } else if (0xDC00..0xE000).contains(&first) {
                                return None;
                            } else {
                                char::from_u32(first)?
                            }
                        }
                        _ => return None,
                    };
                    let mut buffer = [0u8; 4];
                    output.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
                }
                _ => output.push(byte),
            }
        }
    }

    fn number(&mut self) -> Option<Value> {
        let start = self.position;
        if self.peek() == Some(b'-') {
            self.position += 1;
        }
        match self.peek()? {
            b'0' => self.position += 1,
            b'1'..=b'9' => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.position += 1;
                }
            }
            _ => return None,
        }
        let mut is_float = false;
        if self.peek() == Some(b'.') {
            is_float = true;
            self.position += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return None;
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.position += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            is_float = true;
            self.position += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.position += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return None;
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.position += 1;
            }
        }
        let text = std::str::from_utf8(&self.bytes[start..self.position]).ok()?;
        if !is_float {
            let digits = text.trim_start_matches('-');
            return Some(Value::Integer(if digits.bytes().all(|b| b == b'0') { "0".into() } else { text.into() }));
        }
        let mantissa = text.split(['e', 'E']).next().unwrap_or(text);
        let significant = mantissa.bytes().filter(u8::is_ascii_digit).skip_while(|&b| b == b'0').count();
        if significant > 17 && !text.contains(['e', 'E']) {
            return Some(Value::Decimal(text.into()));
        }
        let value: f64 = text.parse().ok()?;
        value.is_finite().then_some(Value::Double(value))
    }
}

// MARK: Writing

/// `JSONSerialization.data(withJSONObject:options: [.prettyPrinted,
/// .sortedKeys, .withoutEscapingSlashes])` plus a final newline, as
/// `DecompilationUtilities.prettyJSON` writes it; `None` unless the top level
/// is an array or an object (`isValidJSONObject`).
pub fn pretty(value: &Value) -> Option<String> {
    if !matches!(value, Value::Array(_) | Value::Object(_)) {
        return None;
    }
    let mut output = String::new();
    write_value(value, 0, &mut output);
    output.push('\n');
    Some(output)
}

fn indent(output: &mut String, level: usize) {
    for _ in 0..level {
        output.push_str("  ");
    }
}

fn write_value(value: &Value, level: usize, output: &mut String) {
    match value {
        Value::Null => output.push_str("null"),
        Value::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        Value::Integer(digits) | Value::Decimal(digits) => output.push_str(digits),
        Value::Double(value) => output.push_str(&format_g17(*value)),
        Value::String(text) => write_string(text, output),
        Value::Array(items) => {
            output.push_str("[\n");
            if items.is_empty() {
                output.push('\n');
            }
            for (index, item) in items.iter().enumerate() {
                indent(output, level + 1);
                write_value(item, level + 1, output);
                output.push_str(if index + 1 < items.len() { ",\n" } else { "\n" });
            }
            indent(output, level);
            output.push(']');
        }
        Value::Object(entries) => {
            output.push_str("{\n");
            if entries.is_empty() {
                output.push('\n');
            }
            let mut sorted: Vec<&(String, Value)> = entries.iter().collect();
            sorted.sort_by(|a, b| natural_compare(&a.0, &b.0));
            for (index, (key, item)) in sorted.iter().enumerate() {
                indent(output, level + 1);
                write_string(key, output);
                output.push_str(" : ");
                write_value(item, level + 1, output);
                output.push_str(if index + 1 < sorted.len() { ",\n" } else { "\n" });
            }
            indent(output, level);
            output.push('}');
        }
    }
}

fn write_string(text: &str, output: &mut String) {
    output.push('"');
    for character in text.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            '\u{8}' => output.push_str("\\b"),
            '\u{c}' => output.push_str("\\f"),
            c if (c as u32) < 0x20 => output.push_str(&format!("\\u{:04x}", c as u32)),
            c => output.push(c),
        }
    }
    output.push('"');
}

/// C's `%.17g`.
pub fn format_g17(value: f64) -> String {
    if value == 0.0 {
        return if value.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    let scientific = format!("{value:.16e}");
    let (mantissa, exponent) = scientific.split_once('e').expect("exponential form");
    let exponent: i32 = exponent.parse().expect("exponent");
    if !(-4..17).contains(&exponent) {
        let mantissa = strip_fraction_zeros(mantissa);
        let sign = if exponent < 0 { '-' } else { '+' };
        return format!("{mantissa}e{sign}{:02}", exponent.abs());
    }
    let decimals = (16 - exponent) as usize;
    strip_fraction_zeros(&format!("{value:.decimals$}")).to_string()
}

fn strip_fraction_zeros(text: &str) -> &str {
    if !text.contains('.') {
        return text;
    }
    text.trim_end_matches('0').trim_end_matches('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Outputs recorded from Foundation (`scripts`-free probe, macOS).
    #[test]
    fn numbers_match_foundation() {
        let cases = [
            ("1", "1"), ("2.5", "2.5"), ("1.0", "1"), ("-0", "0"), ("-0.0", "-0"), ("1e2", "100"),
            ("1e-7", "9.9999999999999995e-08"), ("0.1", "0.10000000000000001"), ("0.30000000000000004", "0.30000000000000004"),
            ("123456789012345678", "123456789012345678"), ("12345678901234567890", "12345678901234567890"),
            ("1.7976931348623157e308", "1.7976931348623157e+308"), ("-1.5", "-1.5"),
            ("100000000000000000000.5", "100000000000000000000.5"), ("5e-324", "4.9406564584124654e-324"),
            ("1E3", "1000"), ("2.50", "2.5"), ("1.5e300", "1.5000000000000001e+300"), ("1.10", "1.1000000000000001"),
            ("3.0e2", "300"), ("2e-400", "0"), ("1E-2", "0.01"), ("-0.0e0", "-0"),
            ("0.1234567890123456789", "0.1234567890123456789"), ("1234567890123456789012", "1234567890123456789012"),
        ];
        for (input, expected) in cases {
            let value = parse(&format!("[{input}]")).unwrap_or_else(|| panic!("parse {input}"));
            let Value::Array(items) = value else { panic!() };
            let mut text = String::new();
            write_value(&items[0], 0, &mut text);
            assert_eq!(text, expected, "for {input}");
        }
    }

    #[test]
    fn acceptance_matches_foundation() {
        for accepted in [r#"{"a":1,}"#, "[1,]", "\u{FEFF}{\"a\":1}", r#"{"a":1}  "#, r#"["\/"]"#, r#"["中😀"]"#] {
            assert!(parse(accepted).is_some(), "{accepted}");
        }
        for rejected in [
            "{a:1}", "[1,,2]", "[,1]", "[01]", "[+1]", "[.5]", "[1.]", "[0x1]", "[-]", "[1e]", r#"["\ud800"]"#, r#"["\x41"]"#,
            "[\"a\u{1}b\"]", "['a']", "[1]//c", "1", "[1e400]", r#"{"a":NaN}"#,
        ] {
            assert!(parse(rejected).is_none(), "{rejected}");
        }
    }

    #[test]
    fn pretty_matches_foundation() {
        let value = parse(r#"{"b":1,"a":{"k":[]},"e":{},"s":["x/y","\n\u0001\"\\"],"a":2}"#).unwrap();
        assert_eq!(
            pretty(&value).unwrap(),
            "{\n  \"a\" : {\n    \"k\" : [\n\n    ]\n  },\n  \"b\" : 1,\n  \"e\" : {\n\n  },\n  \"s\" : [\n    \"x/y\",\n    \"\\n\\u0001\\\"\\\\\"\n  ]\n}\n"
        );
    }

    #[test]
    fn keys_sort_like_finder() {
        let value = parse(r#"{"b":1,"_":1,"10":1,"9":1,"a":1,"A":1,"-":1,"Z":1,"e":1}"#).unwrap();
        let text = pretty(&value).unwrap();
        let keys: Vec<&str> = text.lines().filter_map(|line| line.trim().strip_prefix('"')?.split('"').next()).collect();
        assert_eq!(keys, ["_", "-", "9", "10", "a", "A", "b", "e", "Z"]);
    }
}
