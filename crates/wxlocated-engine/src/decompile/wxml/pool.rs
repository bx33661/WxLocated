//! The `$gwx` operation pools: `Z([...])` entries pushed inside each
//! `gz$gwx…` group function (or globally), decoded back to template text.

use std::collections::HashMap;

use crate::js::lexer::{Kind, Token, matching_token, next_significant, previous_significant, tokenize};
use crate::js::literal::{Literal, parse_swift_double};

pub struct OperationPools {
    pub global: Vec<String>,
    pub groups: HashMap<String, Vec<String>>,
}

impl OperationPools {
    pub fn value(&self, index: i64, group: Option<&str>) -> String {
        if let Some(group) = group
            && let Some(values) = self.groups.get(group)
            && index >= 0
            && (index as usize) < values.len()
        {
            return values[index as usize].clone();
        }
        if index >= 0 && (index as usize) < self.global.len() {
            return self.global[index as usize].clone();
        }
        format!("{{{{__wxlocated_op_{index}}}}}")
    }
}

pub fn extract_pools(source: &str) -> OperationPools {
    let bytes = source.as_bytes();
    let tokens = tokenize(bytes);
    let mut global = Vec::new();
    let mut groups: HashMap<String, Vec<String>> = HashMap::new();
    let mut group_ranges: Vec<(std::ops::Range<usize>, String)> = Vec::new();

    for index in 0..tokens.len() {
        if tokens[index].identifier() != Some("function") {
            continue;
        }
        let Some(name_index) = next_significant(&tokens, index) else { continue };
        let Some(name) = tokens[name_index].identifier().filter(|name| name.starts_with("gz$gwx")) else { continue };
        let Some(body_open) = first_punctuator(b'{', name_index, 128, &tokens) else { continue };
        let Some(body_close) = matching_token(&tokens, body_open, b'{', b'}') else { continue };
        group_ranges.push((body_open..body_close, name["gz$gwx".len()..].to_string()));
    }

    // Group bodies appear in source order, so one cursor is enough.
    let mut cursor = 0;
    let mut raw_global: Vec<Literal> = Vec::new();
    let mut raw_groups: HashMap<String, Vec<Literal>> = HashMap::new();
    for index in 0..tokens.len() {
        if tokens[index].identifier() != Some("Z") {
            continue;
        }
        let Some(open) = next_significant(&tokens, index).filter(|&i| tokens[i].is_punctuator(b'(')) else { continue };
        let Some(value_index) = next_significant(&tokens, open) else { continue };
        // `function Z(ops){z.push(ops)}` declares the pusher; not an entry.
        if previous_significant(&tokens, index).is_some_and(|previous| tokens[previous].identifier() == Some("function")) {
            continue;
        }
        while cursor < group_ranges.len() && group_ranges[cursor].0.end < index {
            cursor += 1;
        }
        let group = (cursor < group_ranges.len() && group_ranges[cursor].0.contains(&index)).then(|| group_ranges[cursor].1.clone());
        let earlier: &[Literal] = match &group {
            Some(group) => raw_groups.get(group).map_or(&raw_global, |values| values),
            None => &raw_global,
        };
        // An entry that cannot be read still takes its slot, so later
        // indices stay aligned.
        let value = PoolEntryParser { tokens: &tokens, bytes, earlier }.parse(value_index).map(|(value, _)| value);
        let decoded = value.as_ref().map_or_else(|| "{{__wxlocated_unreadable}}".to_string(), decode);
        let raw = value.unwrap_or(Literal::Null);
        match group {
            Some(group) => {
                groups.entry(group.clone()).or_default().push(decoded);
                raw_groups.entry(group).or_default().push(raw);
            }
            None => {
                global.push(decoded);
                raw_global.push(raw);
            }
        }
    }
    if global.is_empty() && groups.len() == 1 {
        global = groups.values().next().cloned().unwrap_or_default();
    }
    OperationPools { global, groups }
}

/// One pool entry as it reads in a template: static text stays as is,
/// expressions come back inside `{{ }}`.
pub fn decode(value: &Literal) -> String {
    let Literal::Array(values) = value else { return literal(value) };
    if values.is_empty() {
        return literal(value);
    }
    match opcode(&values[0]) {
        Some(3) => return if values.len() > 1 { string_value(&values[1]) } else { String::new() },
        Some(11) => {
            // `[a, part, part…]`: text and mustaches concatenated.
            return values[1..]
                .iter()
                .map(|part| {
                    if let Literal::Array(inner) = part
                        && inner.first().and_then(opcode) == Some(3)
                        && inner.len() > 1
                    {
                        return string_value(&inner[1]);
                    }
                    format!("{{{{{}}}}}", expression(part).0)
                })
                .collect();
        }
        _ => {}
    }
    format!("{{{{{}}}}}", expression(value).0)
}

/// `var a=11` inside every pool makes `a` the concatenation opcode.
fn opcode(value: &Literal) -> Option<i64> {
    if let Some(number) = value.integer_value() {
        return Some(number);
    }
    matches!(value, Literal::Identifier(name) if name == "a").then_some(11)
}

const PRIMARY: u8 = 17;
const UNARY: u8 = 14;

fn binary_precedence(operation: &str) -> u8 {
    match operation {
        "?:" => 3,
        "||" => 4,
        "&&" => 5,
        "|" => 6,
        "^" => 7,
        "&" => 8,
        "==" | "!=" | "===" | "!==" => 9,
        "<" | ">" | "<=" | ">=" | "instanceof" | "in" => 10,
        "<<" | ">>" | ">>>" => 10,
        "+" | "-" => 11,
        "*" | "/" | "%" => 12,
        _ => 2,
    }
}

fn wrapped(operand: (String, u8), below: u8) -> String {
    if operand.1 < below { format!("({})", operand.0) } else { operand.0 }
}

/// Expression text with the precedence of its outermost operator, so parents
/// add exactly the parentheses the source needed.
fn expression(value: &Literal) -> (String, u8) {
    let Literal::Array(values) = value else { return (literal(value), PRIMARY) };
    if values.is_empty() {
        return (literal(value), PRIMARY);
    }
    if let Some(code) = opcode(&values[0]) {
        return match code {
            1 => (if values.len() > 1 { literal(&values[1]) } else { "undefined".into() }, PRIMARY),
            3 => (
                if values.len() > 1 { literal(&Literal::String(string_value(&values[1]))) } else { "''".into() },
                PRIMARY,
            ),
            11 => {
                let parts: Vec<String> = values[1..].iter().map(|part| wrapped(expression(part), 12)).collect();
                (parts.join("+"), 11)
            }
            _ => (literal(value), PRIMARY),
        };
    }
    let Literal::Array(descriptor) = &values[0] else { return (literal(value), PRIMARY) };
    let Some(code) = descriptor.first().and_then(Literal::integer_value) else { return (literal(value), PRIMARY) };
    let operands = &values[1..];

    match code {
        2 => {
            let operation = if descriptor.len() > 1 { string_value(&descriptor[1]) } else { "?".into() };
            if operation == "?:" && operands.len() >= 3 {
                let condition = wrapped(expression(&operands[0]), 4);
                let yes = wrapped(expression(&operands[1]), 3);
                let no = wrapped(expression(&operands[2]), 3);
                return (format!("{condition}?{yes}:{no}"), 3);
            }
            if operands.len() == 1 {
                return (operation.clone() + &wrapped(expression(&operands[0]), UNARY), UNARY);
            }
            let precedence = binary_precedence(&operation);
            let Some(first) = operands.first() else { return (String::new(), PRIMARY) };
            let mut text = wrapped(expression(first), precedence);
            let separator =
                if operation.chars().next().is_some_and(char::is_alphabetic) { format!(" {operation} ") } else { operation.clone() };
            for operand in &operands[1..] {
                // Left-associative: an equal-precedence right operand keeps its parentheses.
                text += &separator;
                text += &wrapped(expression(operand), precedence + 1);
            }
            (text, precedence)
        }
        4 => (format!("[{}]", operands.first().map(array_elements).unwrap_or_default().join(",")), PRIMARY),
        5 => (format!("[{}]", array_elements(value).join(",")), PRIMARY),
        6 => {
            let object =
                operands.first().map_or_else(|| "undefined".to_string(), |operand| wrapped(expression(operand), PRIMARY));
            if operands.len() <= 1 {
                return (object, PRIMARY);
            }
            let property = &operands[1];
            if let Literal::Array(inner) = property
                && (inner.first().and_then(opcode) == Some(3) || inner.first().and_then(Literal::integer_value) == Some(1))
                && inner.len() > 1
                && let Literal::String(name) = &inner[1]
                && is_identifier_name(name)
            {
                return (format!("{object}.{name}"), PRIMARY);
            }
            (format!("{object}[{}]", expression(property).0), PRIMARY)
        }
        7 => (operands.first().map_or_else(|| "undefined".to_string(), variable_name), PRIMARY),
        8 => {
            let key = operands.first().map_or_else(|| "key".to_string(), string_value);
            let child = if operands.len() > 1 { wrapped(expression(&operands[1]), 3) } else { "undefined".into() };
            (format!("{{{key}:{child}}}"), PRIMARY)
        }
        9 => {
            let members: Vec<String> = operands
                .iter()
                .map(|member| {
                    let text = expression(member).0;
                    if text.starts_with('{') && text.ends_with('}') && text.len() >= 2 {
                        text[1..text.len() - 1].to_string()
                    } else {
                        text
                    }
                })
                .collect();
            (format!("{{{}}}", members.join(",")), PRIMARY)
        }
        10 => (format!("...{}", operands.first().map(|operand| wrapped(expression(operand), UNARY)).unwrap_or_default()), PRIMARY),
        12 => {
            let function =
                operands.first().map_or_else(|| "undefined".to_string(), |operand| wrapped(expression(operand), PRIMARY));
            let arguments = if operands.len() > 1 { array_elements(&operands[1]) } else { Vec::new() };
            (format!("{function}({})", arguments.join(",")), PRIMARY)
        }
        _ => (format!("__wxlocated_op_{code}"), PRIMARY),
    }
}

/// `[[5], x]` is `[x]`; `[[5], list, x]` appends `x` to `list`; `[[4], list]`
/// marks the finished array literal.
fn array_elements(value: &Literal) -> Vec<String> {
    let descriptor_code = match value {
        Literal::Array(values) => match values.first() {
            Some(Literal::Array(descriptor)) => descriptor.first().and_then(Literal::integer_value),
            _ => None,
        },
        _ => None,
    };
    let (Some(code), Literal::Array(values)) = (descriptor_code, value) else { return vec![expression(value).0] };
    match code {
        5 => {
            let operands = &values[1..];
            if operands.len() >= 2 {
                let mut result = array_elements(&operands[0]);
                result.extend(operands[1..].iter().map(|operand| wrapped(expression(operand), 3)));
                return result;
            }
            operands.iter().map(|operand| wrapped(expression(operand), 3)).collect()
        }
        4 => if values.len() > 1 { array_elements(&values[1]) } else { Vec::new() },
        _ => vec![expression(value).0],
    }
}

fn variable_name(value: &Literal) -> String {
    if let Literal::Array(values) = value
        && values.first().and_then(Literal::integer_value) == Some(3)
        && values.len() > 1
    {
        return string_value(&values[1]);
    }
    expression(value).0
}

fn string_value(value: &Literal) -> String {
    match value {
        Literal::String(value) | Literal::Identifier(value) => value.clone(),
        _ => literal(value),
    }
}

fn literal(value: &Literal) -> String {
    match value {
        Literal::String(value) => format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'")),
        Literal::Number(value) => swift_number_text(*value),
        Literal::Boolean(value) => value.to_string(),
        Literal::Null => "null".into(),
        Literal::Identifier(value) => value.clone(),
        Literal::Array(values) => format!("[{}]", values.iter().map(literal).collect::<Vec<_>>().join(",")),
        Literal::Object(values) => {
            let mut entries: Vec<_> = values.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            format!("{{{}}}", entries.iter().map(|(key, value)| format!("{key}:{}", literal(value))).collect::<Vec<_>>().join(","))
        }
    }
}

/// `value.rounded() == value ? String(Int(value)) : String(value)`.
pub fn swift_number_text(value: f64) -> String {
    if value.round() == value {
        return (value as i64).to_string();
    }
    swift_double_description(value)
}

/// Swift's `Double.description`: shortest round-trip digits, exponential
/// below 1e-4 as `1e-05`.
pub fn swift_double_description(value: f64) -> String {
    if value.is_nan() {
        return "nan".into();
    }
    if value.is_infinite() {
        return if value > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let magnitude = value.abs();
    if magnitude != 0.0 && !(1e-4..1e16).contains(&magnitude) {
        let text = format!("{value:e}");
        let (mantissa, exponent) = text.split_once('e').expect("exponential form");
        let (sign, digits) = exponent.strip_prefix('-').map_or(("+", exponent), |digits| ("-", digits));
        return format!("{mantissa}e{sign}{digits:0>2}");
    }
    let text = value.to_string();
    if text.contains('.') { text } else { format!("{text}.0") }
}

fn is_identifier_name(name: &str) -> bool {
    let mut characters = name.chars();
    characters.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && characters.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

fn first_punctuator(value: u8, after: usize, limit: usize, tokens: &[Token]) -> Option<usize> {
    (after + 1..tokens.len().min(after + limit)).find(|&candidate| tokens[candidate].is_punctuator(value))
}

/// Reads one `Z(...)` operand: nested arrays of literals, where `z[N]`
/// (optionally indexed further, `z[N][1]`) repeats part of an earlier entry
/// of the same pool.
struct PoolEntryParser<'a> {
    tokens: &'a [Token],
    bytes: &'a [u8],
    earlier: &'a [Literal],
}

impl PoolEntryParser<'_> {
    fn parse(&self, index: usize) -> Option<(Literal, usize)> {
        let tokens = self.tokens;
        let token = tokens.get(index)?;
        match &token.kind {
            Kind::Punctuator(b'[') => {
                let mut values = Vec::new();
                let mut cursor = index + 1;
                while cursor < tokens.len() {
                    if tokens[cursor].is_punctuator(b']') {
                        return Some((Literal::Array(values), cursor + 1));
                    }
                    if tokens[cursor].is_punctuator(b',') {
                        cursor += 1;
                        continue;
                    }
                    let (value, next) = self.parse(cursor)?;
                    values.push(value);
                    cursor = next;
                }
                None
            }
            Kind::String(value) => Some((Literal::String(value.clone()), index + 1)),
            Kind::Number => parse_swift_double(&token.text(self.bytes)).map(|number| (Literal::Number(number), index + 1)),
            Kind::Punctuator(b'-') => match self.parse(index + 1)? {
                (Literal::Number(number), next) => Some((Literal::Number(-number), next)),
                _ => None,
            },
            Kind::Identifier(name) if name == "z" => {
                if !tokens.get(index + 1).is_some_and(|t| t.is_punctuator(b'[')) {
                    return None;
                }
                let mut value: Option<Literal> = None;
                let mut cursor = index + 1;
                // z[N][M]…
                while cursor + 2 < tokens.len()
                    && tokens[cursor].is_punctuator(b'[')
                    && tokens[cursor + 1].kind == Kind::Number
                    && tokens[cursor + 2].is_punctuator(b']')
                {
                    let Ok(position) = tokens[cursor + 1].text(self.bytes).parse::<usize>() else { break };
                    value = Some(match value {
                        Some(Literal::Array(items)) => items.get(position)?.clone(),
                        Some(_) => return None,
                        None => self.earlier.get(position)?.clone(),
                    });
                    cursor += 3;
                }
                value.map(|value| (value, cursor))
            }
            Kind::Identifier(name) => Some((
                match name.as_str() {
                    "true" => Literal::Boolean(true),
                    "false" => Literal::Boolean(false),
                    "null" => Literal::Null,
                    _ => Literal::Identifier(name.clone()),
                },
                index + 1,
            )),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_source(entry: &str) -> String {
        let source = format!("function gz$gwx_1(){{Z({entry});}}");
        let pools = extract_pools(&source);
        pools.value(0, Some("_1"))
    }

    #[test]
    fn decodes_static_and_member_and_ternary() {
        assert_eq!(decode_source("[3,'hello']"), "hello");
        assert_eq!(decode_source("[[7],[3,'item']]"), "{{item}}");
        assert_eq!(decode_source("[[6],[[7],[3,'a']],[3,'b']]"), "{{a.b}}");
        assert_eq!(decode_source("[[2,'?:'],[[7],[3,'c']],[1,1],[1,2]]"), "{{c?1:2}}");
        assert_eq!(decode_source("[[2,'+'],[[2,'+'],[1,1],[1,2]],[[2,'+'],[1,3],[1,4]]]"), "{{1+2+(3+4)}}");
        assert_eq!(decode_source("[a,[3,'x-'],[[7],[3,'n']]]"), "x-{{n}}");
    }

    #[test]
    fn swift_double_text() {
        assert_eq!(swift_number_text(3.0), "3");
        assert_eq!(swift_number_text(0.5), "0.5");
        assert_eq!(swift_number_text(0.00001), "1e-05");
        assert_eq!(swift_number_text(-1.5e-7), "-1.5e-07");
    }
}
