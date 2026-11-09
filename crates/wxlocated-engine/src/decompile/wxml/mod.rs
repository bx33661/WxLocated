//! WXML from compiled `$gwx` factories: one `.wxml` per page factory, with
//! its `<template name>` definitions, imports and WXS modules.

pub mod factory;
pub mod glass_easel;
pub mod pool;

use std::collections::HashMap;
use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;

use super::pipeline::WxsModules;
use super::{ArtifactKind, Candidate, Confidence, Diagnostic, Severity, SourceRef, Stage, file_name, normalized_path, path_extension};
use crate::archive::natural_compare;
use crate::js::lexer::{Kind, Token, matching_token, next_significant, tokenize};
use crate::js::literal::{Literal, LiteralParser};
use factory::{FactoryInterpreter, serialize};
use pool::extract_pools;

pub struct WxmlRestoration {
    pub files: Vec<Candidate>,
    pub diagnostics: Vec<Diagnostic>,
}

/// A compiled function that renders into one `.wxml` file: the page factory
/// itself, or one of its `<template name>` definitions.
#[derive(Clone)]
struct Factory {
    path: String,
    function_range: Range<usize>,
    group: Option<String>,
    path_pool: Vec<String>,
    /// `None` for the page factory, the template name otherwise.
    template: Option<String>,
    /// Token index of the function, to find its enclosing `$gwx…` wrapper.
    function_token: usize,
}

pub fn restore(
    source: &str,
    source_path: &str,
    source_reference: &SourceRef,
    inline_wxs: &WxsModules,
    external_wxs: &WxsModules,
) -> WxmlRestoration {
    if let Some((path, content)) = restore_html(source, source_path) {
        return WxmlRestoration {
            files: vec![
                Candidate::new(path, Some(ArtifactKind::Markup), content, Confidence::Medium)
                    .notes(vec!["由包内页面 HTML 恢复".into()]),
            ],
            diagnostics: Vec::new(),
        };
    }

    let script = extract_scripts(source);
    let pools = extract_pools(&script);
    let bytes = script.as_bytes();
    let tokens = tokenize(bytes);
    let owners = plugin_owners(&tokens);
    let factories: Vec<Factory> = extract_factories(&tokens, bytes)
        .into_iter()
        .map(|mut factory| {
            // A plugin compiled into this package: `$gwx_<id>` registers its
            // templates as `plugin-private://<id>/x.wxml`; keep them there.
            if let Some(target) = owners.path(&factory.path, factory.function_token) {
                factory.path = target;
            }
            factory
        })
        .collect();

    let mut files = Vec::new();
    let mut diagnostics = Vec::new();
    let mut ordered: Vec<&str> = Vec::new();
    for factory in &factories {
        if !ordered.contains(&factory.path.as_str()) {
            ordered.push(&factory.path);
        }
    }
    // The logic layer (app-service.js) carries a pruned copy of the compile
    // with only conditions and slots; the render layer has the real template.
    let lowered = file_name(source_path).to_lowercase();
    let is_logic_layer = lowered.contains("app-service") || lowered.contains("appservice") || lowered == "common.app.js";

    for path in ordered {
        let mut parts: Vec<&Factory> = factories.iter().filter(|factory| factory.path == path).collect();
        parts.sort_by_key(|part| if part.template.is_some() { 0 } else { 1 });
        let mut imports: Vec<String> = Vec::new();
        let mut sections: Vec<String> = Vec::new();
        let mut unknown: Vec<String> = Vec::new();
        let mut ranges = Vec::new();
        for part in parts {
            let function = String::from_utf8_lossy(&bytes[part.function_range.clone()]);
            let mut interpreter = FactoryInterpreter::new(&function, &pools, part.group.clone(), &part.path_pool);
            let nodes = interpreter.run();
            for import in &interpreter.imports {
                if !imports.contains(import) {
                    imports.push(import.clone());
                }
            }
            unknown.extend(interpreter.unknown.iter().cloned());
            ranges.push(part.function_range.clone());
            match &part.template {
                Some(name) => {
                    let body = serialize(&nodes, 1);
                    sections.push(format!("<template name=\"{}\">\n{body}</template>\n", escape_attribute(name)));
                }
                None => sections.push(serialize(&nodes, 0)),
            }
        }
        let header: String = imports.iter().map(|import| format!("<import src=\"{}\" />\n", escape_attribute(import))).collect::<String>()
            + &wxs_markup(path, inline_wxs, external_wxs);
        let body: Vec<&str> = sections.iter().map(String::as_str).filter(|section| !section.is_empty()).collect();
        let content = header + &body.join("\n");
        let content = if content.ends_with('\n') || content.is_empty() { content } else { content + "\n" };
        let confidence = if is_logic_layer {
            Confidence::Low
        } else if unknown.is_empty() {
            Confidence::High
        } else {
            Confidence::Medium
        };
        let mut notes = if unknown.is_empty() {
            vec!["由页面工厂逐条还原".to_string()]
        } else {
            let mut distinct: Vec<&String> = unknown.iter().collect();
            distinct.sort();
            distinct.dedup();
            let sample: Vec<&str> = distinct.iter().take(5).map(|s| s.as_str()).collect();
            vec![format!("由页面工厂逐条还原，{} 条指令未能还原：{}", unknown.len(), sample.join("、"))]
        };
        if is_logic_layer {
            notes.push("来自逻辑层的精简副本（只含条件与插槽），渲染层没有这份模板时才保留".into());
        }
        files.push(Candidate::new(path, Some(ArtifactKind::Markup), content, confidence).ranges(ranges).notes(notes));
        if !unknown.is_empty() && !is_logic_layer {
            diagnostics.push(Diagnostic::new(
                Severity::Warning,
                Stage::Markup,
                format!("{path} 含 {} 条未能还原的 WXML 指令", unknown.len()),
                Some(source_reference.clone()),
            ));
        }
    }

    if factories.is_empty()
        && matches!(file_name(source_path), "page-frame.html" | "page-frame.js" | "app-wxss.js")
        && source.contains("$gwx")
    {
        diagnostics.push(Diagnostic::new(
            Severity::Warning,
            Stage::Markup,
            "检测到 WXML 编译器输出，当前变体未提取出页面工厂",
            Some(source_reference.clone()),
        ));
    }
    WxmlRestoration { files, diagnostics }
}

/// Page factories in every compile variant: `e_["./p.wxml"] = function…`,
/// `e_["./p.wxml"] = {f: function…}`, `e_["./p.wxml"].f = function…`, and the
/// current one where paths come from a per-segment `var x = [...]` and the
/// factory is a named function declared earlier: `var m0 = function(e,s,r,gg){…}`
/// then `e_[x[0]] = {f: m0, …}`.
fn extract_factories(tokens: &[Token], bytes: &[u8]) -> Vec<Factory> {
    let next = |index| next_significant(tokens, index);
    let parser = LiteralParser::new(tokens, bytes);
    let mut current_paths: Vec<String> = Vec::new();
    let mut functions: HashMap<String, usize> = HashMap::new();
    let mut result: Vec<Factory> = Vec::new();

    for index in 0..tokens.len() {
        let Some(name) = tokens[index].identifier() else { continue };

        // `x = [...]` path pool for the following segment.
        if name == "x"
            && let Some(equal) = next(index).filter(|&i| tokens[i].is_punctuator(b'='))
            && let Some(value_index) = next(equal).filter(|&i| tokens[i].is_punctuator(b'['))
            && let Some((Literal::Array(values), _)) = parser.parse(value_index)
        {
            current_paths = values.iter().filter_map(|value| value.as_str().map(str::to_string)).collect();
            continue;
        }

        // `d_[x[N]]["name"] = function…`: a `<template name>` of file N.
        if name == "d_" {
            let parsed = (|| {
                let open = next(index).filter(|&i| tokens[i].is_punctuator(b'['))?;
                let (raw_path, close) = path_key(open, tokens, bytes, &current_paths)?;
                let path = normalized_wxml_path(&raw_path)?;
                let name_open = next(close).filter(|&i| tokens[i].is_punctuator(b'['))?;
                let name_index = next(name_open)?;
                let template = tokens[name_index].string()?.to_string();
                let name_close = next(name_index).filter(|&i| tokens[i].is_punctuator(b']'))?;
                let equal = next(name_close).filter(|&i| tokens[i].is_punctuator(b'='))?;
                let function = next(equal).filter(|&i| tokens[i].identifier() == Some("function"))?;
                if result.iter().any(|factory| factory.path == path && factory.template.as_deref() == Some(template.as_str())) {
                    return None;
                }
                factory(path, function, Some(template), &current_paths, tokens, bytes)
            })();
            if let Some(factory) = parsed {
                result.push(factory);
            }
            continue;
        }

        // `name = function` declarations, latest wins.
        if !matches!(name, "e_" | "entrys" | "__wxAppCode__") {
            if let Some(equal) = next(index).filter(|&i| tokens[i].is_punctuator(b'='))
                && let Some(value) = next(equal).filter(|&i| tokens[i].identifier() == Some("function"))
            {
                functions.insert(name.to_string(), value);
            }
            continue;
        }

        let Some(open) = next(index).filter(|&i| tokens[i].is_punctuator(b'[')) else { continue };
        let Some((raw_path, close)) = path_key(open, tokens, bytes, &current_paths) else { continue };
        let Some(path) = normalized_wxml_path(&raw_path) else { continue };

        let mut function_index = None;
        if let Some(equal) = next(close).filter(|&i| tokens[i].is_punctuator(b'='))
            && let Some(value) = next(equal)
        {
            match &tokens[value].kind {
                Kind::Identifier(word) if word == "function" => function_index = Some(value),
                Kind::Identifier(reference) => function_index = functions.get(reference).copied(),
                Kind::Punctuator(b'{') => {
                    if let Some(object_close) = matching_token(tokens, value, b'{', b'}') {
                        function_index = first_function_property(value, object_close, tokens, &functions);
                    }
                }
                _ => {}
            }
        } else if let Some(dot) = next(close).filter(|&i| tokens[i].is_punctuator(b'.'))
            && let Some(property) = next(dot).filter(|&i| tokens[i].identifier() == Some("f"))
            && let Some(equal) = next(property).filter(|&i| tokens[i].is_punctuator(b'='))
            && let Some(value) = next(equal)
        {
            match &tokens[value].kind {
                Kind::Identifier(word) if word == "function" => function_index = Some(value),
                Kind::Identifier(reference) => function_index = functions.get(reference).copied(),
                _ => {}
            }
        }

        let Some(function_index) = function_index else { continue };
        if result.iter().any(|factory| factory.path == path && factory.template.is_none()) {
            continue;
        }
        if let Some(factory) = factory(path, function_index, None, &current_paths, tokens, bytes) {
            result.push(factory);
        }
    }
    result.sort_by(|a, b| natural_compare(&a.path, &b.path));
    result
}

fn factory(
    path: String,
    function_index: usize,
    template: Option<String>,
    paths: &[String],
    tokens: &[Token],
    bytes: &[u8],
) -> Option<Factory> {
    let body_open = first_punctuator(b'{', function_index, 256, tokens)?;
    let body_close = matching_token(tokens, body_open, b'{', b'}')?;
    let body = String::from_utf8_lossy(&bytes[tokens[body_open].end..tokens[body_close].start]);
    Some(Factory {
        path,
        function_range: tokens[function_index].start..tokens[body_close].end,
        group: group_name(&body),
        path_pool: paths.to_vec(),
        template,
        function_token: function_index,
    })
}

#[derive(Default)]
struct PluginOwners {
    /// `$gwx…=function(…){…}` wrapper bodies (token ranges) by name.
    wrappers: Vec<(Range<usize>, String)>,
    /// "wrapper|compiled path" → plugin-scoped output path.
    targets: HashMap<String, String>,
}

impl PluginOwners {
    fn path(&self, compiled: &str, token: usize) -> Option<String> {
        if self.targets.is_empty() {
            return None;
        }
        let (_, wrapper) = self.wrappers.iter().rev().find(|(range, _)| range.contains(&token))?;
        self.targets.get(&format!("{wrapper}|{compiled}")).cloned()
    }
}

/// `__wxAppCode__['plugin-private://<id>/x.wxml'] = $gwx_<id>('./x.wxml')` (or
/// `= [$gwx_<id>, './x.wxml']`) ties a wrapper's compiled path to a
/// plugin-scoped one.
fn plugin_owners(tokens: &[Token]) -> PluginOwners {
    let next = |index| next_significant(tokens, index);
    let mut owners = PluginOwners::default();
    for index in 0..tokens.len() {
        let Some(name) = tokens[index].identifier() else { continue };
        if name.starts_with("$gwx")
            && let Some(equal) = next(index).filter(|&i| tokens[i].is_punctuator(b'='))
            && let Some(function) = next(equal).filter(|&i| tokens[i].identifier() == Some("function"))
            && let Some(open) = first_punctuator(b'{', function, 512, tokens)
            && let Some(close) = matching_token(tokens, open, b'{', b'}')
        {
            owners.wrappers.push((open..close, name.to_string()));
            continue;
        }
        if name != "__wxAppCode__" {
            continue;
        }
        let parsed = (|| {
            let open = next(index).filter(|&i| tokens[i].is_punctuator(b'['))?;
            let key_index = next(open)?;
            let key = tokens[key_index].string()?;
            if !(key.starts_with("plugin") && key.contains("://") && key.ends_with(".wxml")) {
                return None;
            }
            let close = next(key_index).filter(|&i| tokens[i].is_punctuator(b']'))?;
            let equal = next(close).filter(|&i| tokens[i].is_punctuator(b'='))?;
            let mut cursor = next(equal)?;
            if tokens[cursor].is_punctuator(b'[')
                && let Some(following) = next(cursor)
            {
                cursor = following;
            }
            let wrapper = tokens[cursor].identifier().filter(|wrapper| wrapper.starts_with("$gwx"))?;
            let separator =
                next(cursor).filter(|&i| tokens[i].is_punctuator(b'(') || tokens[i].is_punctuator(b','))?;
            let compiled = tokens[next(separator)?].string()?;
            let compiled_path = normalized_wxml_path(compiled)?;
            let target = normalized_wxml_path(key)?;
            Some((format!("{wrapper}|{compiled_path}"), target))
        })();
        if let Some((key, target)) = parsed {
            owners.targets.insert(key, target);
        }
    }
    owners
}

/// `["./p.wxml"]` or `[x[3]]`; returns the path and the closing `]`.
fn path_key(open: usize, tokens: &[Token], bytes: &[u8], paths: &[String]) -> Option<(String, usize)> {
    let next = |index| next_significant(tokens, index);
    let first = next(open)?;
    if let Some(raw_path) = tokens[first].string()
        && let Some(close) = next(first).filter(|&i| tokens[i].is_punctuator(b']'))
    {
        return Some((raw_path.to_string(), close));
    }
    if tokens[first].identifier() != Some("x") {
        return None;
    }
    let inner_open = next(first).filter(|&i| tokens[i].is_punctuator(b'['))?;
    let number = next(inner_open).filter(|&i| tokens[i].kind == Kind::Number)?;
    let position: usize = tokens[number].text(bytes).parse().ok()?;
    let path = paths.get(position)?;
    let inner_close = next(number).filter(|&i| tokens[i].is_punctuator(b']'))?;
    let close = next(inner_close).filter(|&i| tokens[i].is_punctuator(b']'))?;
    Some((path.clone(), close))
}

static BODY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<body\b[^>]*>(.*?)</body\s*>").expect("regex"));
static SCRIPT_BLOCK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<script\b[^>]*>.*?</script\s*>").expect("regex"));
static STYLE_BLOCK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<style\b[^>]*>.*?</style\s*>").expect("regex"));
static WX_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(</?)wx-([A-Za-z][A-Za-z0-9-]*)").expect("regex"));
static WHITESPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("regex"));
static SCRIPT_BODY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<script\b[^>]*>(.*?)</script\s*>").expect("regex"));
static GROUP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"gz\$gwx([A-Za-z0-9_]*)\s*\(").expect("regex"));

fn restore_html(source: &str, source_path: &str) -> Option<(String, String)> {
    let filename = file_name(source_path);
    if !filename.to_lowercase().ends_with(".html") || filename == "page-frame.html" || source.contains("__WXML_GLOBAL__") {
        return None;
    }
    let path = normalized_wxml_path(source_path)?;
    let mut body = BODY.captures(source).and_then(|captures| captures.get(1)).map_or(source, |m| m.as_str()).to_string();
    body = SCRIPT_BLOCK.replace_all(&body, "").into_owned();
    body = STYLE_BLOCK.replace_all(&body, "").into_owned();
    body = WX_TAG.replace_all(&body, "${1}${2}").into_owned();
    let trimmed = crate::text::trim_whitespace_and_newlines(&body);
    // Per-page loader stubs hold only `<page></page>` and a script that
    // fetches the compiled template; there is no template here.
    let markup = WHITESPACE.replace_all(trimmed, "").to_lowercase();
    if trimmed.is_empty() || markup == "<page></page>" || markup == "<page/>" {
        return None;
    }
    Some((path, format!("{trimmed}\n")))
}

fn wxs_markup(page_path: &str, inline: &WxsModules, external: &WxsModules) -> String {
    let mut lines = Vec::new();
    if let Some(modules) = external.get(page_path) {
        let mut sorted: Vec<_> = modules.iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(b.0));
        for (module, source) in sorted {
            lines.push(format!("<wxs module=\"{}\" src=\"{}\" />", escape_attribute(module), escape_attribute(source)));
        }
    }
    if let Some(modules) = inline.get(page_path) {
        let mut sorted: Vec<_> = modules.iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(b.0));
        for (module, code) in sorted {
            lines.push(format!(
                "<wxs module=\"{}\">\n{}\n</wxs>",
                escape_attribute(module),
                crate::text::trim_whitespace_and_newlines(code)
            ));
        }
    }
    if lines.is_empty() { String::new() } else { lines.join("\n") + "\n" }
}

pub fn extract_scripts(source: &str) -> String {
    if !source.to_lowercase().contains("<script") {
        return source.to_string();
    }
    let blocks: Vec<&str> =
        SCRIPT_BODY.captures_iter(source).filter_map(|captures| captures.get(1).map(|m| m.as_str())).collect();
    if blocks.is_empty() { source.to_string() } else { blocks.join("\n") }
}

fn group_name(body: &str) -> Option<String> {
    GROUP.captures(body).and_then(|captures| captures.get(1)).map(|m| m.as_str().to_string())
}

/// `./p.html`/`./p.js`/`./p` → `p.wxml`; `None` for any other extension.
pub(crate) fn normalized_wxml_path(raw_path: &str) -> Option<String> {
    let mut path = raw_path.to_string();
    for suffix in [".html", ".js"] {
        if path.ends_with(suffix) {
            path.truncate(path.len() - suffix.len());
            path.push_str(".wxml");
            break;
        }
    }
    if path_extension(&path).is_empty() {
        path.push_str(".wxml");
    }
    if !path.to_lowercase().ends_with(".wxml") {
        return None;
    }
    normalized_path(&path)
}

fn escape_attribute(value: &str) -> String {
    value.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;")
}

fn first_function_property(start: usize, end: usize, tokens: &[Token], functions: &HashMap<String, usize>) -> Option<usize> {
    for index in start + 1..end {
        if tokens[index].identifier() != Some("f") {
            continue;
        }
        let Some(colon) = next_significant(tokens, index).filter(|&i| tokens[i].is_punctuator(b':')) else { continue };
        let Some(function) = next_significant(tokens, colon) else { continue };
        let Some(target) = tokens[function].identifier() else { continue };
        if target == "function" {
            return Some(function);
        }
        if let Some(&declared) = functions.get(target) {
            return Some(declared);
        }
    }
    None
}

fn first_punctuator(value: u8, after: usize, limit: usize, tokens: &[Token]) -> Option<usize> {
    (after + 1..tokens.len().min(after + limit)).find(|&candidate| tokens[candidate].is_punctuator(value))
}
