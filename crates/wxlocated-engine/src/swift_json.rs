//! JSON written as Swift's `JSONEncoder` writes it with `.sortedKeys`,
//! `.withoutEscapingSlashes` and (optionally) `.prettyPrinted`, the format of
//! every CLI document and `material.json`. Checked against real Foundation:
//! keys sort by code point (unlike `JSONSerialization`), doubles use the
//! shortest round-trip form (`0.1`, `1e-07`, `1e+20`; `1.0` → `1`), an empty
//! container is an opening line, a blank line and the closing bracket, and
//! dates are ISO 8601 truncated to whole seconds.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::decompile::wxml::pool::swift_double_description;

pub fn encode(value: &Value, pretty: bool) -> String {
    let mut output = String::new();
    write(value, 0, pretty, &mut output);
    output
}

fn indent(output: &mut String, level: usize) {
    for _ in 0..level {
        output.push_str("  ");
    }
}

fn write(value: &Value, level: usize, pretty: bool, output: &mut String) {
    match value {
        Value::Null => output.push_str("null"),
        Value::Bool(flag) => output.push_str(if *flag { "true" } else { "false" }),
        Value::Number(number) => {
            if let Some(integer) = number.as_i64() {
                output.push_str(&integer.to_string());
            } else if let Some(integer) = number.as_u64() {
                output.push_str(&integer.to_string());
            } else {
                output.push_str(&swift_double(number.as_f64().unwrap_or(0.0)));
            }
        }
        Value::String(text) => write_string(text, output),
        Value::Array(items) => {
            if !pretty {
                output.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        output.push(',');
                    }
                    write(item, level, pretty, output);
                }
                output.push(']');
                return;
            }
            output.push_str("[\n");
            if items.is_empty() {
                output.push('\n');
            }
            for (index, item) in items.iter().enumerate() {
                indent(output, level + 1);
                write(item, level + 1, pretty, output);
                output.push_str(if index + 1 < items.len() { ",\n" } else { "\n" });
            }
            indent(output, level);
            output.push(']');
        }
        Value::Object(map) => {
            // `.sortedKeys` orders by code point, which byte order of UTF-8
            // gives. Sorted here rather than trusting `serde_json::Map`:
            // another crate in the same build (GPUI) may turn on its
            // `preserve_order` feature, which keeps insertion order instead.
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            if !pretty {
                output.push('{');
                for (index, (key, item)) in entries.iter().enumerate() {
                    if index > 0 {
                        output.push(',');
                    }
                    write_string(key, output);
                    output.push(':');
                    write(item, level, pretty, output);
                }
                output.push('}');
                return;
            }
            output.push_str("{\n");
            if entries.is_empty() {
                output.push('\n');
            }
            for (index, (key, item)) in entries.iter().enumerate() {
                indent(output, level + 1);
                write_string(key, output);
                output.push_str(" : ");
                write(item, level + 1, pretty, output);
                output.push_str(if index + 1 < entries.len() { ",\n" } else { "\n" });
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

/// `JSONEncoder`'s double: `Double.description` without a trailing `.0`.
pub fn swift_double(value: f64) -> String {
    let text = swift_double_description(value);
    text.strip_suffix(".0").map(str::to_string).unwrap_or(text)
}

/// `JSONEncoder.dateEncodingStrategy = .iso8601`: UTC, whole seconds.
pub fn iso8601(time: SystemTime) -> String {
    let seconds = match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs() as i64,
        Err(error) => -(error.duration().as_secs_f64().ceil() as i64),
    };
    let days = seconds.div_euclid(86_400);
    let second_of_day = seconds.rem_euclid(86_400);
    // Civil from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        second_of_day / 3600,
        second_of_day / 60 % 60,
        second_of_day % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Recorded from Foundation's `JSONEncoder`.
    #[test]
    fn matches_json_encoder() {
        let value = serde_json::json!({
            "nested": {"b": 1, "a": 2, "_": 3, "-": 4, "10": 5, "9": 6, "A": 7},
            "d": [1.0, 1e-7, 123.456, 0.30000000000000004, 1e20, 2.5e-5, 0.1],
            "e": {}, "s": "a/b\"c\\d\u{1}é中"
        });
        assert_eq!(
            encode(&value, true),
            "{\n  \"d\" : [\n    1,\n    1e-07,\n    123.456,\n    0.30000000000000004,\n    1e+20,\n    2.5e-05,\n    0.1\n  ],\n  \"e\" : {\n\n  },\n  \"nested\" : {\n    \"-\" : 4,\n    \"10\" : 5,\n    \"9\" : 6,\n    \"A\" : 7,\n    \"_\" : 3,\n    \"a\" : 2,\n    \"b\" : 1\n  },\n  \"s\" : \"a/b\\\"c\\\\d\\u0001é中\"\n}"
        );
        assert_eq!(encode(&serde_json::json!({"k": [1], "e": []}), false), "{\"e\":[],\"k\":[1]}");
    }

    #[test]
    fn iso_dates() {
        let time = UNIX_EPOCH + std::time::Duration::from_millis(1_700_000_000_789);
        assert_eq!(iso8601(time), "2023-11-14T22:13:20Z");
        assert_eq!(iso8601(UNIX_EPOCH), "1970-01-01T00:00:00Z");
    }
}
