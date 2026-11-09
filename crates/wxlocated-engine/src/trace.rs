//! `wxlocated trace <material> <path>[:line]`: maps a rebuilt file (and line)
//! back to the original bytes in `raw/`, so agents cite package bytes, not the
//! reading view.
//!
//! Matching is explicit about its strength:
//! - `identical`: the file is a byte-for-byte copy of a package entry; line
//!   numbers carry over.
//! - `anchored`: the line's non-whitespace text was found inside the entry's
//!   source range (the formatter only changes whitespace, so this pins the
//!   original bytes).
//! - `range`: only the entry and byte range of the whole rebuilt file are known.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::archive::Archive;
use crate::material::report::SCHEMA_VERSION;
use crate::overlay;

pub struct TraceFailure {
    /// 64 usage, 66 not found, 70 failure.
    pub exit: u8,
    pub code: &'static str,
    pub message: String,
}

fn fail(exit: u8, code: &'static str, message: String) -> TraceFailure {
    TraceFailure { exit, code, message }
}

/// `root` is the material directory as given (already `~`-expanded and made
/// absolute by the caller).
pub fn trace(args: &[String], root: impl Fn(&str) -> PathBuf) -> Result<Value, TraceFailure> {
    let positionals: Vec<&String> = args.iter().filter(|arg| !arg.starts_with('-')).collect();
    if positionals.len() < 2 {
        return Err(fail(64, "missing_arguments", "trace 需要 <材料目录> <路径>[:行号]".into()));
    }
    let root = root(positionals[0]);
    let (requested_path, line) = split_line(positionals[1]);
    let context_bytes: i64 = args
        .iter()
        .position(|arg| arg == "--context")
        .and_then(|index| args.get(index + 1))
        .and_then(|value| value.parse().ok())
        .unwrap_or(120);

    let material: Value = std::fs::read_to_string(root.join("material.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .filter(|value: &Value| value["metadata"]["app_id"].is_string() && value["wxlocated"]["packages"].is_array())
        .ok_or_else(|| {
            fail(66, "not_material", format!("{} 不是 wxlocated export 导出的材料目录（缺少 material.json）", root.to_string_lossy()))
        })?;

    // Agent semantic layers resolve to the static rebuild file they replace;
    // the agent's line text is then anchored in that file's raw range.
    let mut path = requested_path.clone();
    let mut overlay_info: Option<Value> = None;
    let mut needle_path: Option<PathBuf> = None;
    if path.starts_with("semantic/") {
        let parts: Vec<&str> = path.splitn(3, '/').collect();
        if parts.len() != 3 {
            return Err(fail(64, "missing_arguments", "路径应为 semantic/<名称>/<文件>".into()));
        }
        let layer_directory = root.join("semantic").join(parts[1]);
        let manifest = overlay::read_manifest(&layer_directory);
        let file = manifest.as_ref().and_then(|m| m.files.iter().find(|file| file.path == parts[2]));
        let (Some(manifest), Some(file)) = (manifest.as_ref(), file) else {
            return Err(fail(66, "unknown_file", format!("重建层 {} 中没有登记 {}", parts[1], parts[2])));
        };
        let Some(base) = &file.base else {
            return Err(fail(70, "no_source", format!("{} 是 Agent 新增的文件，没有对应的静态重建文件，无法回溯原始字节", parts[2])));
        };
        overlay_info = Some(json!({ "name": manifest.name, "file": parts[2], "base": base }));
        needle_path = Some(layer_directory.join(parts[2]));
        path = base.clone();
    }

    // Which source tree (app or plugin) the path belongs to.
    let mut source_root = "source/".to_string();
    let mut decryption_id = material["metadata"]["app_id"].as_str().unwrap_or_default().to_string();
    let mut packages = material["wxlocated"]["packages"].as_array().cloned().unwrap_or_default();
    if path.starts_with("plugins/") {
        let parts: Vec<&str> = path.splitn(4, '/').collect();
        let plugin = (parts.len() >= 4)
            .then(|| {
                material["wxlocated"]["plugins"].as_array().and_then(|plugins| {
                    plugins
                        .iter()
                        .find(|p| {
                            let alias = p["alias"].as_str().unwrap_or_default();
                            alias == parts[1] || trace_safe_name(alias) == parts[1]
                        })
                        .cloned()
                })
            })
            .flatten();
        let Some(plugin) = plugin else {
            let name = if parts.len() > 1 { parts[1] } else { path.as_str() };
            return Err(fail(66, "unknown_plugin", format!("材料中没有插件 {name}")));
        };
        source_root = plugin["source_directory"].as_str().map(str::to_string).unwrap_or_else(|| format!("plugins/{}/source/", parts[1]));
        decryption_id = plugin["provider"].as_str().unwrap_or_default().to_string();
        packages = plugin["packages"].as_array().cloned().unwrap_or_default();
        path = parts[3].to_string();
    } else if let Some(rest) = path.strip_prefix("source/") {
        path = rest.to_string();
    }

    let manifest: Value = std::fs::read_to_string(root.join(&source_root).join("WxLocated-decompilation.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .filter(|value: &Value| value["artifacts"].is_array())
        .ok_or_else(|| {
            fail(66, "missing_manifest", format!("找不到 {source_root}WxLocated-decompilation.json；导出时是否用了 --no-source？"))
        })?;
    let artifacts = manifest["artifacts"].as_array().cloned().unwrap_or_default();
    let artifact = artifacts
        .iter()
        .find(|a| a["path"].as_str() == Some(path.as_str()))
        .or_else(|| artifacts.iter().find(|a| a["path"].as_str().is_some_and(|p| p.ends_with(&format!("/{path}")))))
        .ok_or_else(|| fail(66, "unknown_file", format!("{source_root}{path} 不在重建清单中")))?;
    let artifact_path = artifact["path"].as_str().unwrap_or_default().to_string();
    let generated = artifact["generated"].as_bool().unwrap_or(false);
    let source = artifact["sources"].as_array().and_then(|sources| sources.first()).cloned();
    let Some(source) = source else {
        return Err(fail(70, "no_source", format!("{artifact_path} 没有记录来源")));
    };

    // The manifest names packages by title, e.g. "主包 · __APP__.wxapkg".
    let title = source["package"].as_str().unwrap_or_default();
    let package_file = title.rsplit(" · ").next().unwrap_or(title).to_string();
    let package = packages
        .iter()
        .find(|p| p["file"].as_str() == Some(package_file.as_str()))
        .or_else(|| (packages.len() == 1).then(|| &packages[0]))
        .cloned();
    let Some((package, raw_relative)) = package.and_then(|p| p["raw_path"].as_str().map(str::to_string).map(|raw| (p, raw))) else {
        return Err(fail(66, "missing_raw", format!("材料中没有 {package_file} 的原始副本；导出时是否用了 --no-raw？")));
    };
    let raw_path = root.join(&raw_relative);
    let archive = Archive::open_with_app_id(&raw_path, Some(&decryption_id))
        .map_err(|error| fail(70, "unreadable_raw", format!("无法打开 {raw_relative}：{error}")))?;
    let entry_path = source["entryPath"].as_str().unwrap_or_default();
    let entry = archive
        .entries
        .iter()
        .find(|e| e.path == entry_path)
        .or_else(|| archive.entries.iter().find(|e| e.path.trim_matches('/') == entry_path))
        .ok_or_else(|| fail(70, "missing_entry", format!("{raw_relative} 中没有条目 {entry_path}")))?;
    let bytes = archive
        .read_limited(entry, 256 * 1024 * 1024)
        .map_err(|error| fail(70, "failed", error.to_string()))?;
    let length = bytes.len() as i64;
    let start_offset = source["startOffset"].as_i64();
    let segment_start = start_offset.unwrap_or(0).max(0).min(length);
    let segment_end = source["endOffset"].as_i64().unwrap_or(length).max(segment_start).min(length);

    let rebuilt = needle_path.unwrap_or_else(|| root.join(&source_root).join(&artifact_path));
    let reading_text = std::fs::read_to_string(&rebuilt).unwrap_or_default();
    let line_text = line.and_then(|line| rebuilt_line(&reading_text, line));
    let segment = segment_start as usize..segment_end as usize;
    let matched = locate_entry(
        &bytes,
        &reading_text,
        line,
        segment,
        overlay_info.is_none() && !generated && start_offset.is_none(),
        context_bytes,
    );

    let mut input = json!({ "material_directory": root.to_string_lossy(), "path": requested_path });
    if let Some(line) = line {
        input["line"] = json!(line);
    }
    if let Some(text) = line_text {
        input["line_text"] = json!(text);
    }
    let mut raw = json!({
        "package_file": package["file"],
        "package_path": raw_relative,
        "entry_path": entry.path,
        "segment_start": segment_start,
        "segment_end": segment_end,
    });
    if let Some(sha) = package["sha256"].as_str() {
        raw["package_sha256"] = json!(sha);
    }
    let mut output = json!({
        "schema_version": SCHEMA_VERSION,
        "input": input,
        "artifact": { "path": format!("{source_root}{artifact_path}"), "confidence": artifact["confidence"], "generated": generated },
        "raw": raw,
        "match": matched,
    });
    if let Some(overlay) = overlay_info {
        output["overlay"] = overlay;
    }
    Ok(output)
}

/// Locate rebuilt or semantic reading text in an original package entry.
/// `segment` uses byte offsets in `bytes`; `identical` is only for unchanged
/// entries, never semantic overlays. Line and column values follow CLI trace.
pub fn locate_entry(
    bytes: &[u8],
    source: &str,
    line: Option<i64>,
    segment: std::ops::Range<usize>,
    identical: bool,
    context_bytes: i64,
) -> Value {
    let start = segment.start.min(bytes.len());
    let segment = start..segment.end.max(start).min(bytes.len());
    if let Some(line) = line
        && identical
    {
        return identical_match(bytes, line, context_bytes);
    }
    // Short lines (`}`, `})`) include following lines to form a useful anchor.
    if let Some((text, lines)) = line.and_then(|line| rebuilt_needle(source, line)) {
        return anchored_match(&text, bytes, segment, context_bytes, lines);
    }
    let (line_number, column) = line_column(bytes, segment.start);
    let upper = segment.end.min((segment.start as i64 + context_bytes * 2).max(0) as usize);
    json!({
        "method": "range",
        "start_offset": segment.start,
        "end_offset": segment.end,
        "line": line_number,
        "column": column,
        "snippet": snippet(bytes, segment.start, upper),
        "note": match line {
            None => "未指定行号，返回整个文件对应的原始区间".to_string(),
            Some(line) => format!("第 {line} 行不存在或为空，只能返回整个文件对应的原始区间"),
        },
    })
}

fn identical_match(bytes: &[u8], line: i64, context_bytes: i64) -> Value {
    let mut current = 1i64;
    let mut start = 0usize;
    let mut index = 0usize;
    while index < bytes.len() && current < line {
        if bytes[index] == b'\n' {
            current += 1;
            start = index + 1;
        }
        index += 1;
    }
    if current != line {
        return json!({ "method": "range", "start_offset": 0, "end_offset": bytes.len(), "note": format!("原始条目只有 {current} 行") });
    }
    let mut end = start;
    while end < bytes.len() && bytes[end] != b'\n' {
        end += 1;
    }
    let upper = end.min((start as i64 + context_bytes * 2).max(0) as usize);
    json!({
        "method": "identical",
        "start_offset": start,
        "end_offset": end,
        "line": line,
        "column": 1,
        "snippet": snippet(bytes, start, upper),
        "note": "原样保留的文件，行号与原始条目一致",
    })
}

/// Whitespace-insensitive search of the rebuilt line inside the entry's range.
fn anchored_match(text: &str, bytes: &[u8], segment: std::ops::Range<usize>, context_bytes: i64, extended_lines: usize) -> Value {
    let needle: Vec<u8> = text.bytes().filter(|b| !is_whitespace(*b)).collect();
    let extended_note =
        if extended_lines > 1 { format!("（该行过短，连同其后 {} 行一起匹配）", extended_lines - 1) } else { String::new() };
    if needle.len() < 3 {
        return json!({
            "method": "range",
            "start_offset": segment.start,
            "end_offset": segment.end,
            "note": "该行有效字符太少，无法唯一定位；返回整个文件对应的原始区间",
        });
    }
    let mut compact = Vec::with_capacity(segment.len());
    let mut origin = Vec::with_capacity(segment.len());
    for offset in segment.clone() {
        if !is_whitespace(bytes[offset]) {
            compact.push(bytes[offset]);
            origin.push(offset);
        }
    }
    let hits = occurrences(&needle, &compact, 50);
    let Some(&first) = hits.first() else {
        return json!({
            "method": "range",
            "start_offset": segment.start,
            "end_offset": segment.end,
            "note": "在原始区间中没有找到这一行的文本（可能经过了结构化改写），返回整个文件对应的原始区间",
        });
    };
    let start = origin[first];
    let end = origin[first + needle.len() - 1] + 1;
    let (line, column) = line_column(bytes, start);
    let lower = segment.start.max((start as i64 - context_bytes).max(0) as usize);
    let upper = segment.end.min((end as i64 + context_bytes).max(0) as usize);
    let note = if hits.len() == 1 {
        "按去空白文本在原始区间中唯一定位".to_string()
    } else {
        format!("原始区间中有 {} 处相同文本，返回第一处；引用时请结合上下文确认", hits.len())
    } + &extended_note;
    json!({
        "method": "anchored",
        "start_offset": start,
        "end_offset": end,
        "line": line,
        "column": column,
        "candidates": hits.len(),
        "snippet": snippet(bytes, lower, upper),
        "note": note,
    })
}

fn occurrences(needle: &[u8], haystack: &[u8], limit: usize) -> Vec<usize> {
    let mut result = Vec::new();
    if needle.is_empty() || haystack.len() < needle.len() {
        return result;
    }
    let last = haystack.len() - needle.len();
    let mut index = 0;
    while index <= last {
        if haystack[index] == needle[0] && &haystack[index..index + needle.len()] == needle {
            result.push(index);
            if result.len() >= limit {
                break;
            }
            index += needle.len();
        } else {
            index += 1;
        }
    }
    result
}

fn is_whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r')
}

fn line_column(bytes: &[u8], offset: usize) -> (i64, i64) {
    let (mut line, mut column) = (1i64, 1i64);
    for &byte in &bytes[..offset.min(bytes.len())] {
        if byte == b'\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    (line, column)
}

fn snippet(bytes: &[u8], lower: usize, upper: usize) -> String {
    let upper = upper.min(bytes.len());
    let lower = lower.min(upper);
    String::from_utf8_lossy(&bytes[lower..upper]).into_owned()
}

/// `CharacterSet.whitespaces`: tab and Unicode `Zs` (not `\r` or `\n`).
fn is_space_or_tab(c: char) -> bool {
    matches!(c, '\t' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}')
}

fn rebuilt_line(source: &str, line: i64) -> Option<String> {
    let lines: Vec<&str> = source.split('\n').collect();
    if line < 1 || line as usize > lines.len() {
        return None;
    }
    let value = lines[line as usize - 1].trim_matches(is_space_or_tab);
    (!value.is_empty()).then(|| value.to_string())
}

fn rebuilt_needle(source: &str, line: i64) -> Option<(String, usize)> {
    let lines: Vec<&str> = source.split('\n').collect();
    if line < 1 || line as usize > lines.len() {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    let mut compact = 0;
    let mut index = line as usize - 1;
    while index < lines.len() && parts.len() < 6 {
        parts.push(&lines[index]);
        compact += lines[index].chars().filter(|c| !is_space_or_tab(*c)).count();
        if compact >= 12 {
            break;
        }
        index += 1;
    }
    let joined = parts.join("\n");
    (!crate::text::trim_whitespace_and_newlines(&joined).is_empty()).then(|| (joined.clone(), parts.len()))
}

/// `path:line` when the part after the last `:` is an integer.
fn split_line(value: &str) -> (String, Option<i64>) {
    if let Some(colon) = value.rfind(':')
        && let Ok(line) = value[colon + 1..].parse::<i64>()
    {
        return (value[..colon].to_string(), Some(line));
    }
    (value.to_string(), None)
}

/// Letters, digits, `-`, `_`, `.`; anything else `_` (no fallback name).
fn trace_safe_name(value: &str) -> String {
    value.chars().map(|c| if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' }).collect()
}

#[cfg(test)]
mod tests {
    use super::locate_entry;

    #[test]
    fn anchors_respect_utf8_byte_offsets_and_segment_boundaries() {
        let raw = "前缀\nrun( 你好 );\nrun( 你好 );尾".as_bytes();
        let start = "前缀\nrun( 你好 );\n".len();
        let end = raw.len() - "尾".len();
        let matched = locate_entry(raw, "run(你好);", Some(1), start..end, false, 120);
        assert_eq!(matched["method"], "anchored");
        assert_eq!(matched["start_offset"], start);
        assert_eq!(matched["end_offset"], end);
        assert_eq!(matched["line"], 3);
        assert_eq!(matched["candidates"], 1);
        let ambiguous = locate_entry(raw, "run(你好);", Some(1), 0..raw.len(), false, 120);
        assert_eq!(ambiguous["candidates"], 2);
        assert!(ambiguous["note"].as_str().unwrap().contains("返回第一处"));
    }

    #[test]
    fn short_lines_extend_and_semantic_text_never_uses_identical_offsets() {
        let raw = b"prefix(); } nextCall(); suffix();";
        let matched = locate_entry(raw, "}\nnextCall();", Some(1), 0..raw.len(), false, 120);
        assert_eq!(matched["method"], "anchored");
        assert_eq!(matched["start_offset"], 10);
        assert!(matched["note"].as_str().unwrap().contains("其后 1 行"));
        let absent = locate_entry(raw, "rewritten();", Some(1), 10..23, false, 120);
        assert_eq!(absent["method"], "range");
        assert_eq!(absent["start_offset"], 10);
        assert_eq!(absent["end_offset"], 23);
        let unchanged = locate_entry(b"first\nsecond", "first\nsecond", Some(2), 0..12, true, 120);
        assert_eq!(unchanged["method"], "identical");
        assert_eq!(unchanged["start_offset"], 6);
        assert_eq!(unchanged["end_offset"], 12);
        assert_eq!(locate_entry(raw, "", None, 10..23, false, 120)["method"], "range");
    }
}
