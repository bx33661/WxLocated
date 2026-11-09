//! WXS modules from the page-frame factories (`np_N`/`nv_` functions bound
//! through `f_[...] = nv_require(...)`), with the compiler's renames reversed.

use std::collections::HashMap;
use std::ops::Range;

use super::pipeline::WxsModules;
use super::{Candidate, Confidence, Diagnostic, Severity, SourceRef, Stage, normalized_path};
use crate::js::formatter;
use crate::js::lexer::{Token, matching_token, next_significant, tokenize};
use crate::text::has_suffix;

pub struct WxsRestoration {
    pub files: Vec<Candidate>,
    pub inline_modules: WxsModules,
    pub external_modules: WxsModules,
    pub diagnostics: Vec<Diagnostic>,
}

struct FunctionBody {
    body: String,
    range: Range<usize>,
}

pub fn restore(source: &str, source_reference: &SourceRef) -> WxsRestoration {
    let bytes = source.as_bytes();
    let tokens = tokenize(bytes);
    let functions = extract_functions(&tokens, bytes);
    let references = extract_require_map(&tokens);
    let bindings = extract_bindings(&tokens);

    let mut files = Vec::new();
    let mut inline: WxsModules = HashMap::new();
    let mut external: WxsModules = HashMap::new();
    let mut diagnostics = Vec::new();

    for (binding_path, reference) in &bindings {
        let lookup = references.get(reference).unwrap_or(reference);
        let Some(function) = functions.get(lookup).or_else(|| functions.get(reference)) else {
            diagnostics.push(Diagnostic::new(
                Severity::Warning,
                Stage::Wxs,
                format!("WXS 引用缺少函数定义：{reference}"),
                Some(source_reference.clone()),
            ));
            continue;
        };
        let content = unwrap(&function.body);
        if let Some((page_path, module_name)) = parse_inline_binding(binding_path) {
            inline.entry(page_path).or_default().insert(module_name, content);
            continue;
        }
        let Some(path) = external_path(binding_path) else { continue };
        // The compiler's renames (`nv_`, key guards) are reversed exactly;
        // anything left over keeps the file degraded.
        let confidence = if content.contains("\"nv_\"+nt_") { Confidence::Medium } else { Confidence::High };
        files.push(
            Candidate::new(path, Some(super::ArtifactKind::Wxs), content, confidence)
                .ranges(vec![function.range.clone()])
                .notes(vec!["由 page-frame WXS 工厂静态恢复".into()]),
        );
    }

    for (page_path, module_name, source_path) in extract_page_external_relations(&tokens) {
        external.entry(page_path).or_default().insert(module_name, source_path);
    }
    WxsRestoration { files, inline_modules: inline, external_modules: external, diagnostics }
}

fn extract_functions(tokens: &[Token], bytes: &[u8]) -> HashMap<String, FunctionBody> {
    let mut result = HashMap::new();
    for index in 0..tokens.len() {
        if tokens[index].identifier() != Some("function") {
            continue;
        }
        let Some(name_index) = next_significant(tokens, index) else { continue };
        let Some(name) = tokens[name_index].identifier() else { continue };
        if !(name.starts_with("np_") || name.starts_with("wxs_") || name.starts_with("nv_")) {
            continue;
        }
        let limit = tokens.len().min(name_index + 128);
        let mut cursor = name_index;
        let mut body_open = None;
        while let Some(next) = next_significant(tokens, cursor) {
            if next >= limit {
                break;
            }
            cursor = next;
            if tokens[next].is_punctuator(b'{') {
                body_open = Some(next);
                break;
            }
        }
        let Some(body_open) = body_open else { continue };
        let Some(body_close) = matching_token(tokens, body_open, b'{', b'}') else { continue };
        let range = tokens[body_open].end..tokens[body_close].start;
        result.insert(
            name.to_string(),
            FunctionBody { body: String::from_utf8_lossy(&bytes[range.clone()]).into_owned(), range },
        );
    }
    result
}

fn extract_require_map(tokens: &[Token]) -> HashMap<String, String> {
    let mut result = HashMap::new();
    if tokens.len() < 3 {
        return result;
    }
    for index in 0..tokens.len() - 2 {
        let Some(key) = tokens[index].identifier().or(tokens[index].string()) else { continue };
        if !(key.starts_with("p_") || key.starts_with("m_")) || !tokens[index + 1].is_punctuator(b':') {
            continue;
        }
        let Some(function) = tokens[index + 2].identifier() else { continue };
        if function.starts_with("np_") || function.starts_with("wxs_") {
            result.insert(key.to_string(), function.to_string());
        }
    }
    result
}

/// `f_['path']['module'] = nv_require('ref')` (the module part is optional).
fn extract_bindings(tokens: &[Token]) -> Vec<(String, String)> {
    let next = |index| next_significant(tokens, index);
    let mut result = Vec::new();
    for index in 0..tokens.len() {
        if tokens[index].identifier() != Some("f_") {
            continue;
        }
        let parsed = (|| {
            let first_open = next(index).filter(|&i| tokens[i].is_punctuator(b'['))?;
            let first_value = next(first_open)?;
            let primary = tokens[first_value].string()?;
            let first_close = next(first_value).filter(|&i| tokens[i].is_punctuator(b']'))?;
            let mut binding = primary.to_string();
            let mut cursor = first_close;
            if let Some(second_open) = next(cursor).filter(|&i| tokens[i].is_punctuator(b'['))
                && let Some(second_value) = next(second_open)
                && let Some(module) = tokens[second_value].string()
                && let Some(second_close) = next(second_value).filter(|&i| tokens[i].is_punctuator(b']'))
            {
                binding.push(':');
                binding.push_str(module);
                cursor = second_close;
            }
            let equal = next(cursor).filter(|&i| tokens[i].is_punctuator(b'='))?;
            let require = next(equal).filter(|&i| tokens[i].identifier() == Some("nv_require"))?;
            let open = next(require).filter(|&i| tokens[i].is_punctuator(b'('))?;
            let reference = tokens[next(open)?].string()?;
            Some((binding, reference.to_string()))
        })();
        if let Some(binding) = parsed {
            result.push(binding);
        }
    }
    result
}

fn extract_page_external_relations(tokens: &[Token]) -> Vec<(String, String, String)> {
    extract_bindings(tokens)
        .into_iter()
        .filter_map(|(binding, reference)| {
            if !binding.contains(':') || reference.contains(':') {
                return None;
            }
            let split = binding.rfind(':')?;
            let page = &binding[..split];
            let module = &binding[split + 1..];
            let source = reference.strip_prefix("p_").unwrap_or(&reference);
            Some((normalized_path(page)?, module.to_string(), normalized_path(source)?))
        })
        .collect()
}

fn unwrap(source: &str) -> String {
    let mut text = source;
    if let Some(position) = text.find("nv_module={nv_exports:{}};") {
        text = &text[position + "nv_module={nv_exports:{}};".len()..];
    }
    if let Some(position) = text.rfind("return nv_module.nv_exports") {
        text = &text[..position];
    }
    let text = removing_key_guards(text).replace("\\x5c", "\\\\");
    remove_identifier_prefix(&formatter::format(&text))
}

/// The WXS compiler rewrites every computed key `a[k]` as
/// `a[((nt_N=(k),null==nt_N?undefined:'number'=== typeof nt_N?nt_N:"nv_"+nt_N))]`
/// so string keys get its `nv_` prefix. Reverse it to `a[k]`; guards nest.
pub fn removing_key_guards(source: &str) -> String {
    if !source.contains("nt_") {
        return source.to_string();
    }
    let bytes = source.as_bytes();
    let tokens = tokenize(bytes);
    let mut output = String::new();
    let mut cursor = 0;
    let mut index = 0;
    while index + 4 < tokens.len() {
        let matched = (|| {
            if !(tokens[index].is_punctuator(b'(') && tokens[index + 1].is_punctuator(b'(')) {
                return None;
            }
            let name = tokens[index + 2].identifier().filter(|name| name.starts_with("nt_"))?;
            if !(tokens[index + 3].is_punctuator(b'=') && tokens[index + 4].is_punctuator(b'(')) {
                return None;
            }
            let expression_close = matching_token(&tokens, index + 4, b'(', b')')?;
            let inner_close = matching_token(&tokens, index + 1, b'(', b')')?;
            if !(inner_close + 1 < tokens.len()
                && tokens[inner_close + 1].is_punctuator(b')')
                && expression_close + 1 < inner_close)
            {
                return None;
            }
            let guard_text: String = String::from_utf8_lossy(&bytes[tokens[expression_close + 1].start..tokens[inner_close].start])
                .chars()
                .filter(|c| !crate::text::is_whitespace(*c))
                .collect();
            let expected = format!(",null=={name}?undefined:'number'===typeof{name}?{name}:\"nv_\"+{name}");
            (guard_text == expected).then_some((expression_close, inner_close))
        })();
        let Some((expression_close, inner_close)) = matched else {
            index += 1;
            continue;
        };
        output.push_str(&String::from_utf8_lossy(&bytes[cursor..tokens[index].start]));
        let expression = String::from_utf8_lossy(&bytes[tokens[index + 4].end..tokens[expression_close].start]);
        output.push_str(&removing_key_guards(&expression));
        cursor = tokens[inner_close + 1].end;
        index = inner_close + 2;
    }
    output.push_str(&String::from_utf8_lossy(&bytes[cursor..]));
    output
}

fn remove_identifier_prefix(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut result = String::with_capacity(source.len());
    let mut cursor = 0;
    for token in tokenize(bytes) {
        let Some(name) = token.identifier() else { continue };
        if !name.starts_with("nv_") || name.chars().count() <= 3 {
            continue;
        }
        result.push_str(&String::from_utf8_lossy(&bytes[cursor..token.start]));
        result.push_str(&name[3..]);
        cursor = token.end;
    }
    result.push_str(&String::from_utf8_lossy(&bytes[cursor..]));
    if !has_suffix(&result, "\n") {
        result.push('\n');
    }
    result
}

fn parse_inline_binding(binding: &str) -> Option<(String, String)> {
    let split = binding.rfind(':')?;
    let page = &binding[..split];
    let module = &binding[split + 1..];
    if !page.to_lowercase().ends_with(".wxml") || module.is_empty() {
        return None;
    }
    Some((normalized_path(page)?, module.to_string()))
}

fn external_path(binding: &str) -> Option<String> {
    if binding.contains(':') {
        return None;
    }
    let mut path = normalized_path(binding)?;
    if !path.to_lowercase().ends_with(".wxs") {
        path.push_str(".wxs");
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reverses_key_guards() {
        let source = "a[((nt_0=(k),null==nt_0?undefined:'number'=== typeof nt_0?nt_0:\"nv_\"+nt_0))]";
        assert_eq!(removing_key_guards(source), "a[k]");
    }

    #[test]
    fn restores_external_module() {
        let source = "f_['utils/fmt.wxs'] = nv_require('p_utils/fmt.wxs');\
            var x={'p_utils/fmt.wxs':np_0};\
            function np_0(){var nv_module={nv_exports:{}};nv_module.nv_exports={nv_f:1};return nv_module.nv_exports;}";
        let reference = SourceRef { package: "p".into(), entry_path: "page-frame.js".into(), start: None, end: None };
        let restoration = restore(source, &reference);
        assert_eq!(restoration.files.len(), 1);
        assert_eq!(restoration.files[0].path, "utils/fmt.wxs");
        assert_eq!(restoration.files[0].content, "module.exports = {\n    f: 1\n};\n");
    }
}
