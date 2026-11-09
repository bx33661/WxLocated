//! JavaScript modules split out of compiled containers (`define("x.js",
//! function(){…})`, `__wxAppCode__["x.js"] = function(){…}`, plugin scopes),
//! the JSON registered in scripts, and sources embedded in source maps.

use std::collections::HashMap;
use std::ops::Range;

use super::{Candidate, Confidence, Diagnostic, Severity, SourceRef, Stage, normalized_path, path_extension, scheme_as_folder};
use crate::archive::natural_compare;
use crate::js::formatter;
use crate::js::lexer::{Token, matching_token, next_significant, tokenize};
use crate::json::{self, Value};
use crate::text::{has_suffix, trim_whitespace_and_newlines};

pub struct Module {
    pub path: String,
    pub content: String,
    pub source_range: Range<usize>,
    pub confidence: Confidence,
}

const CALL_NAMES: &[&str] = &["define", "definePlugin", "__defineModule__", "__definePage__"];

pub fn restore_modules(source: &str) -> Vec<Module> {
    let bytes = source.as_bytes();
    let tokens = tokenize(bytes);
    let mut modules: Vec<Module> = Vec::new();
    let mut occupied: Vec<Range<usize>> = Vec::new();
    // `definePlugin("plugin://<id>", function(define, …){ define("x.js", …) })`:
    // modules defined inside belong to the plugin, not to the app root.
    let mut plugin_scopes: Vec<(Range<usize>, String)> = Vec::new();

    for index in 0..tokens.len() {
        let Some(identifier) = tokens[index].identifier() else { continue };
        if CALL_NAMES.contains(&identifier)
            && let Some(module) = module_from_call(&tokens, bytes, index)
        {
            if identifier == "definePlugin"
                && let Some(base) = normalize_module_path(&module.path)
            {
                let extension = path_extension(&base);
                let stem = if extension.is_empty() { base.as_str() } else { &base[..base.len() - extension.len() - 1] };
                plugin_scopes.push((module.source_range.clone(), format!("{stem}/")));
            } else if let Some((_, prefix)) =
                plugin_scopes.iter().rev().find(|(range, _)| range.contains(&module.source_range.start))
            {
                let rest = module.path.trim_start_matches(['/', '.']);
                occupied.push(module.source_range.clone());
                modules.push(Module { path: format!("{prefix}{rest}"), ..module });
                continue;
            }
            occupied.push(module.source_range.clone());
            modules.push(module);
            continue;
        }
        if identifier == "__wxAppCode__"
            && let Some(module) = module_from_wx_app_code(&tokens, bytes, index)
            && !occupied.iter().any(|range| range.start < module.source_range.end && module.source_range.start < range.end)
        {
            occupied.push(module.source_range.clone());
            modules.push(module);
        }
    }

    let mut selected: HashMap<String, Module> = HashMap::new();
    for module in modules {
        let Some(path) = normalize_module_path(&module.path) else { continue };
        if selected.get(&path).is_some_and(|current| current.confidence >= module.confidence) {
            continue;
        }
        let content = strip_wrapper(&module.content);
        selected.insert(path.clone(), Module { path, content, source_range: module.source_range, confidence: module.confidence });
    }
    let mut result: Vec<Module> = selected.into_values().collect();
    result.sort_by(|a, b| natural_compare(&a.path, &b.path));
    result
}

/// `__wxAppCode__["x.json"] = {...}` registrations, by normalized path.
pub fn embedded_json(source: &str) -> HashMap<String, Value> {
    let bytes = source.as_bytes();
    let tokens = tokenize(bytes);
    let next = |index| next_significant(&tokens, index);
    let mut result = HashMap::new();
    for index in 0..tokens.len() {
        let found = (|| {
            if tokens[index].identifier() != Some("__wxAppCode__") {
                return None;
            }
            let bracket = next(index).filter(|&i| tokens[i].is_punctuator(b'['))?;
            let path_index = next(bracket)?;
            let raw_path = tokens[path_index].string()?;
            if !raw_path.to_lowercase().ends_with(".json") {
                return None;
            }
            let close = next(path_index).filter(|&i| tokens[i].is_punctuator(b']'))?;
            let equal = next(close).filter(|&i| tokens[i].is_punctuator(b'='))?;
            let value_index = next(equal)?;
            let end = if tokens[value_index].is_punctuator(b'{') {
                matching_token(&tokens, value_index, b'{', b'}')?
            } else if tokens[value_index].is_punctuator(b'[') {
                matching_token(&tokens, value_index, b'[', b']')?
            } else {
                return None;
            };
            let text = String::from_utf8_lossy(&bytes[tokens[value_index].start..tokens[end].end]);
            Some((normalized_path(raw_path)?, json::parse(&text)?))
        })();
        if let Some((path, value)) = found {
            result.insert(path, value);
        }
    }
    result
}

fn module_from_call(tokens: &[Token], bytes: &[u8], identifier_index: usize) -> Option<Module> {
    let opening = next_significant(tokens, identifier_index).filter(|&i| tokens[i].is_punctuator(b'('))?;
    let closing = matching_token(tokens, opening, b'(', b')')?;
    let mut path: Option<String> = None;
    let mut function_index = None;
    #[allow(clippy::needless_range_loop)] // `index` also marks positions for later lookups
    for index in opening + 1..closing {
        if path.is_none()
            && let Some(value) = tokens[index].string()
        {
            path = Some(value.to_string());
        }
        if tokens[index].identifier() == Some("function") {
            function_index = Some(index);
            break;
        }
    }
    let raw_path = path?;
    let function_index = function_index?;
    let body_open = (function_index + 1..closing.min(tokens.len())).find(|&i| tokens[i].is_punctuator(b'{'))?;
    let body_close = matching_token(tokens, body_open, b'{', b'}')?;
    Some(Module {
        path: raw_path,
        content: String::from_utf8_lossy(&bytes[tokens[body_open].end..tokens[body_close].start]).into_owned(),
        source_range: tokens[identifier_index].start..tokens[closing].end,
        confidence: Confidence::High,
    })
}

fn module_from_wx_app_code(tokens: &[Token], bytes: &[u8], identifier_index: usize) -> Option<Module> {
    let next = |index| next_significant(tokens, index);
    let bracket = next(identifier_index).filter(|&i| tokens[i].is_punctuator(b'['))?;
    let path_index = next(bracket)?;
    let raw_path = tokens[path_index].string()?.to_string();
    let close = next(path_index).filter(|&i| tokens[i].is_punctuator(b']'))?;
    let equal = next(close).filter(|&i| tokens[i].is_punctuator(b'='))?;
    let function_index = next(equal).filter(|&i| tokens[i].identifier() == Some("function"))?;
    let limit = tokens.len().min(function_index + 128);
    let body_open = (function_index + 1..limit).find(|&i| tokens[i].is_punctuator(b'{'))?;
    let body_close = matching_token(tokens, body_open, b'{', b'}')?;
    Some(Module {
        path: raw_path,
        content: String::from_utf8_lossy(&bytes[tokens[body_open].end..tokens[body_close].start]).into_owned(),
        source_range: tokens[identifier_index].start..tokens[body_close].end,
        confidence: Confidence::Medium,
    })
}

fn normalize_module_path(raw_path: &str) -> Option<String> {
    let mut path = raw_path.split('?').next().unwrap_or(raw_path);
    path = path.strip_prefix("webpack:///").unwrap_or(path);
    let path = scheme_as_folder(path);
    let path = path.strip_prefix("./").unwrap_or(&path);
    let mut normalized = normalized_path(path)?;
    if path_extension(&normalized).is_empty() {
        normalized.push_str(".js");
    }
    Some(normalized)
}

fn strip_wrapper(source: &str) -> String {
    let mut code = trim_whitespace_and_newlines(source).to_string();
    for prefix in ["\"use strict\";", "'use strict';"] {
        if let Some(rest) = code.strip_prefix(prefix) {
            code = trim_whitespace_and_newlines(rest).to_string();
        }
    }
    formatter::format(&code)
}

// MARK: Source maps

pub struct SourceMapRestoration {
    pub files: Vec<Candidate>,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn restore_source_map(source: &str, source_path: &str, source_reference: &SourceRef) -> SourceMapRestoration {
    let root = json::parse(source).filter(|root| matches!(root, Value::Object(_)));
    let Some(sources) = root.as_ref().and_then(|root| root.get("sources")).and_then(Value::as_string_array) else {
        return SourceMapRestoration {
            files: Vec::new(),
            diagnostics: vec![Diagnostic::new(
                Severity::Warning,
                Stage::Scripts,
                format!("Source Map 结构异常：{source_path}"),
                Some(source_reference.clone()),
            )],
        };
    };
    let root = root.expect("checked above");
    let contents: Vec<Value> = match root.get("sourcesContent") {
        Some(Value::Array(items)) => items.clone(),
        _ => Vec::new(),
    };
    let source_root = root.get("sourceRoot").and_then(Value::as_str);
    let mut files = Vec::new();
    let mut diagnostics = Vec::new();

    for (index, raw_path) in sources.iter().enumerate() {
        let Some(content) = contents.get(index).and_then(Value::as_str) else {
            diagnostics.push(Diagnostic::new(
                Severity::Information,
                Stage::Scripts,
                format!("Source Map 未内嵌源码：{raw_path}"),
                Some(source_reference.clone()),
            ));
            continue;
        };
        let Some(path) = source_map_output_path(raw_path, source_root, source_path) else {
            diagnostics.push(Diagnostic::new(
                Severity::Warning,
                Stage::Scripts,
                format!("忽略 Source Map 异常路径：{raw_path}"),
                Some(source_reference.clone()),
            ));
            continue;
        };
        let content = if has_suffix(content, "\n") { content.to_string() } else { format!("{content}\n") };
        files.push(
            Candidate::new(path, None, content, Confidence::Exact).notes(vec![format!("由 {source_path} 的 sourcesContent 原样恢复")]),
        );
    }
    if sources.len() > contents.len() {
        diagnostics.push(Diagnostic::new(
            Severity::Information,
            Stage::Scripts,
            format!("Source Map 共声明 {} 个源文件，其中 {} 个包含源码内容", sources.len(), files.len()),
            Some(source_reference.clone()),
        ));
    }
    SourceMapRestoration { files, diagnostics }
}

fn source_map_output_path(raw_path: &str, source_root: Option<&str>, source_map_path: &str) -> Option<String> {
    let mut value = crate::text::removing_percent_encoding(raw_path).unwrap_or_else(|| raw_path.to_string()).replace('\\', "/");
    for prefix in ["webpack:///", "webpack://", "vite:///", "rollup:///", "file:///", "wxfile://", "app:///"] {
        if let Some(rest) = value.strip_prefix(prefix) {
            value = rest.to_string();
            break;
        }
    }
    while let Some(rest) = value.strip_prefix("./") {
        value = rest.to_string();
    }
    value = value.trim_start_matches('/').to_string();

    let ignored = ["./~/", "~/", "node_modules/", "webpack/bootstrap", "webpack/runtime/"];
    if ignored.iter().any(|prefix| value.starts_with(prefix)) {
        value = format!("__source_map__/{}", value.replace("./~/", "node_modules/"));
    } else if let Some(source_root) = source_root.filter(|root| !root.is_empty() && !root.contains("://")) {
        let root = source_root.trim_matches('/');
        if !root.is_empty() {
            value = format!("{root}/{value}");
        }
    } else if !value.contains('/') {
        // The map's own folder inside the package.
        let mut components: Vec<&str> = source_map_path.split('/').filter(|c| !c.is_empty()).collect();
        components.pop();
        let directory = components.join("/");
        if !directory.is_empty() {
            value = format!("{directory}/{value}");
        }
    }
    normalized_path(&value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_define_and_plugin_scopes() {
        let source = r#"define("pages/a.js",function(require,module){"use strict";var x=1});
definePlugin("plugin://wx1",function(define){define("./b.js",function(){var y=2})});"#;
        let modules = restore_modules(source);
        let paths: Vec<&str> = modules.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, ["__plugin__/wx1.js", "__plugin__/wx1/b.js", "pages/a.js"]);
        assert_eq!(modules[2].content, "var x = 1\n");
    }

}
