//! Parses JavaScript literal values (strings, numbers, booleans, null, arrays,
//! objects, bare identifiers) out of a token stream.

use std::collections::HashMap;

use super::lexer::{Kind, Token, significant_at_or_after};

#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    String(String),
    Number(f64),
    Boolean(bool),
    Null,
    Array(Vec<Literal>),
    Object(HashMap<String, Literal>),
    Identifier(String),
}

impl Literal {
    pub fn integer_value(&self) -> Option<i64> {
        match self {
            Self::Number(value) if value.round() == *value && *value >= i64::MIN as f64 && *value <= i64::MAX as f64 => {
                Some(*value as i64)
            }
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }
}

pub struct LiteralParser<'a> {
    pub tokens: &'a [Token],
    pub bytes: &'a [u8],
}

impl<'a> LiteralParser<'a> {
    pub fn new(tokens: &'a [Token], bytes: &'a [u8]) -> Self {
        Self { tokens, bytes }
    }

    /// The literal starting at (the first significant token at or after)
    /// `start`, and the index of its last token.
    pub fn parse(&self, start: usize) -> Option<(Literal, usize)> {
        self.parse_value(significant_at_or_after(self.tokens, start), 0)
    }

    fn next(&self, index: usize) -> Option<usize> {
        significant_at_or_after(self.tokens, index + 1)
    }

    fn parse_value(&self, index: Option<usize>, depth: usize) -> Option<(Literal, usize)> {
        let index = index?;
        if depth >= 128 || index >= self.tokens.len() {
            return None;
        }
        match &self.tokens[index].kind {
            Kind::String(value) => Some((Literal::String(value.clone()), index)),
            Kind::Number => {
                let raw = self.tokens[index].text(self.bytes).replace('_', "");
                parse_swift_double(&raw).map(|value| (Literal::Number(value), index))
            }
            Kind::Identifier(value) => Some((
                match value.as_str() {
                    "true" => Literal::Boolean(true),
                    "false" => Literal::Boolean(false),
                    "null" | "undefined" => Literal::Null,
                    _ => Literal::Identifier(value.clone()),
                },
                index,
            )),
            Kind::Punctuator(b'-') => match self.parse_value(self.next(index), depth + 1)? {
                (Literal::Number(number), end) => Some((Literal::Number(-number), end)),
                _ => None,
            },
            // Unary plus, e.g. WXSS `[0,+40]` (an rpx value written `+40rpx`).
            Kind::Punctuator(b'+') => match self.parse_value(self.next(index), depth + 1)? {
                (value @ Literal::Number(_), end) => Some((value, end)),
                _ => None,
            },
            Kind::Punctuator(b'[') => self.parse_array(index, depth + 1),
            Kind::Punctuator(b'{') => self.parse_object(index, depth + 1),
            _ => None,
        }
    }

    fn parse_array(&self, opening: usize, depth: usize) -> Option<(Literal, usize)> {
        let mut result = Vec::new();
        let mut cursor = opening;
        while let Some(next) = self.next(cursor) {
            if self.tokens[next].is_punctuator(b']') {
                return Some((Literal::Array(result), next));
            }
            let (value, end) = self.parse_value(Some(next), depth)?;
            result.push(value);
            let separator = self.next(end)?;
            if self.tokens[separator].is_punctuator(b']') {
                return Some((Literal::Array(result), separator));
            }
            if !self.tokens[separator].is_punctuator(b',') {
                return None;
            }
            cursor = separator;
        }
        None
    }

    fn parse_object(&self, opening: usize, depth: usize) -> Option<(Literal, usize)> {
        let mut result = HashMap::new();
        let mut cursor = opening;
        while let Some(key_index) = self.next(cursor) {
            if self.tokens[key_index].is_punctuator(b'}') {
                return Some((Literal::Object(result), key_index));
            }
            let key = match &self.tokens[key_index].kind {
                Kind::String(value) | Kind::Identifier(value) => value.clone(),
                Kind::Number => self.tokens[key_index].text(self.bytes).into_owned(),
                _ => return None,
            };
            let colon = self.next(key_index)?;
            if !self.tokens[colon].is_punctuator(b':') {
                return None;
            }
            let (value, end) = self.parse_value(self.next(colon), depth)?;
            result.insert(key, value);
            let separator = self.next(end)?;
            if self.tokens[separator].is_punctuator(b'}') {
                return Some((Literal::Object(result), separator));
            }
            if !self.tokens[separator].is_punctuator(b',') {
                return None;
            }
            cursor = separator;
        }
        None
    }
}

/// Swift `Double(String)`: decimal and hex floats, `inf`/`nan`, no leading
/// `+` ambiguity issues for our inputs. JS forms Swift rejects (`0b…`, `0o…`,
/// `1n`, `.5` is accepted by both) yield `None` as in Swift.
pub fn parse_swift_double(raw: &str) -> Option<f64> {
    let lower = raw.to_ascii_lowercase();
    if let Some(hex) = lower.strip_prefix("0x") {
        // Swift parses hex integers (and hex floats with a `p` exponent).
        if hex.is_empty() || hex.contains('.') && !hex.contains('p') {
            return None;
        }
        if !hex.contains('p') {
            return u128::from_str_radix(hex, 16).ok().map(|value| value as f64);
        }
        return None;
    }
    if lower.ends_with('n') || lower.starts_with("0b") || lower.starts_with("0o") {
        return None;
    }
    raw.parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::js::lexer::tokenize;

    #[test]
    fn parses_nested_literal() {
        let source = b"x=[1,-2,+3,'a',{b:true,'c':null},[ ]]";
        let tokens = tokenize(source);
        let parser = LiteralParser::new(&tokens, source);
        let (value, end) = parser.parse(2).unwrap();
        assert_eq!(end, tokens.len() - 1);
        let Literal::Array(items) = value else { panic!() };
        assert_eq!(items[1], Literal::Number(-2.0));
        assert_eq!(items[2], Literal::Number(3.0));
        let Literal::Object(object) = &items[4] else { panic!() };
        assert_eq!(object["b"], Literal::Boolean(true));
        assert_eq!(object["c"], Literal::Null);
    }
}
