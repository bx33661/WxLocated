//! WXSS from `setCssToHead([...])` registrations: the `_C` shared style pool,
//! `__COMMON_STYLESHEETS__` shared sheets, `[0,N]` rpx values and `[2,…]`
//! imports, with compiler-inserted annotations removed.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;

use super::{ArtifactKind, Candidate, Confidence, Diagnostic, Severity, SourceRef, Stage, file_name, normalized_path};
use crate::js::lexer::{Token, matching_token, next_significant, tokenize};
use crate::js::literal::{Literal, LiteralParser};
use crate::decompile::wxml::pool::swift_double_description;
use crate::text::trim_whitespace_and_newlines;

pub struct WxssRestoration {
    pub files: Vec<Candidate>,
    pub diagnostics: Vec<Diagnostic>,
}

struct Registration {
    path: String,
    value: Literal,
    range: Range<usize>,
}

/// Synthetic pool ids for shared stylesheets; far above real `_C` indices.
const COMMON_POOL_BASE: i64 = 1_000_000;
/// Marks a self-reference to a shared sheet defined in another file.
const UNRESOLVED_SHARED_SHEET: i64 = -1;

type Pool = HashMap<i64, Literal>;

pub fn restore(source: &str, source_path: &str, source_reference: &SourceRef) -> WxssRestoration {
    let script = super::wxml::extract_scripts(source);
    let bytes = script.as_bytes();
    let tokens = tokenize(bytes);
    let parser = LiteralParser::new(&tokens, bytes);
    let mut pool = extract_style_pool(&tokens, &parser);
    // Shared stylesheets: `__COMMON_STYLESHEETS__["./a.wxss"] = [...]`, referenced
    // as `[2, "./a.wxss"]`. Each gets a synthetic pool id so the pool logic
    // applies: a reference to the sheet's own path inlines it, any other
    // becomes `@import`.
    let common_sheets = extract_common_stylesheets(&tokens, &parser);
    let mut common_ids: HashMap<String, i64> = HashMap::new();
    for (offset, (path, _)) in common_sheets.iter().enumerate() {
        common_ids.insert(path.clone(), COMMON_POOL_BASE + offset as i64);
    }
    for (offset, (_, value)) in common_sheets.iter().enumerate() {
        pool.insert(COMMON_POOL_BASE + offset as i64, rewriting_path_references(value, &common_ids));
    }

    let mut registrations = extract_registered_styles(&tokens, &parser);
    if registrations.is_empty()
        && let Some((value, range)) = first_set_css_value(&tokens, &parser)
        && let Some(path) = output_path(source_path)
    {
        registrations = vec![Registration { path, value, range }];
    }
    for registration in &mut registrations {
        registration.value = rewriting_path_references(&registration.value, &common_ids);
    }

    let mut pool_paths: HashMap<i64, String> = common_ids.iter().map(|(path, id)| (*id, path.clone())).collect();
    // A registration that is only `[[2, N]]` names an anonymous `_C` entry.
    // Shared sheets already have their own path; a file that just imports one
    // does not own it.
    for registration in &registrations {
        if let Some(id) = pure_pool_reference(&registration.value) {
            pool_paths.entry(id).or_insert_with(|| registration.path.clone());
        }
    }

    let renderer = Renderer { pool: &pool, pool_paths: &pool_paths };
    let mut files = Vec::new();
    let mut diagnostics = Vec::new();
    let mut referenced_unknown_pools: BTreeSet<i64> = BTreeSet::new();
    for registration in &registrations {
        let mut stack = HashSet::new();
        let mut local_unknown = HashSet::new();
        let rendered = renderer.render(&registration.value, &registration.path, &mut stack, &mut local_unknown);
        referenced_unknown_pools.extend(local_unknown.iter().copied());
        if registration.value == Literal::Array(Vec::new()) {
            // `setCssToHead([])`: the page or component really has no styles.
            files.push(
                Candidate::new(registration.path.clone(), Some(ArtifactKind::Style), String::new(), Confidence::Exact)
                    .ranges(vec![registration.range.clone()])
                    .notes(vec!["编译产物中该样式为空（setCssToHead([])）".into()]),
            );
            continue;
        }
        let blank = trim_whitespace_and_newlines(&rendered).is_empty();
        if blank && local_unknown.is_empty() {
            // Only references to shared sheets that are themselves empty
            // (`__COMMON_STYLESHEETS__['./x.wxss']=[]`).
            files.push(
                Candidate::new(registration.path.clone(), Some(ArtifactKind::Style), String::new(), Confidence::Exact)
                    .ranges(vec![registration.range.clone()])
                    .notes(vec!["编译产物中该样式为空（引用的共享样式表为空）".into()]),
            );
            continue;
        }
        if blank {
            diagnostics.push(Diagnostic::new(
                Severity::Warning,
                Stage::Styles,
                format!("样式注册项内容为空：{}", registration.path),
                Some(source_reference.clone()),
            ));
            continue;
        }
        let (css, removed) = removing_compiler_annotations(&clean_selectors(&rendered));
        let mut notes = vec!["由 setCssToHead 静态恢复".to_string()];
        if removed > 0 {
            notes.push(format!("已去掉编译器插入的 {removed} 处 wxcs_ 定位声明"));
        }
        if local_unknown.contains(&UNRESOLVED_SHARED_SHEET) {
            notes.push("本文件内容在其他文件定义的共享样式表中，此处未能内联".into());
        }
        let confidence = if local_unknown.is_empty() { Confidence::High } else { Confidence::Medium };
        files.push(
            Candidate::new(registration.path.clone(), Some(ArtifactKind::Style), format_css(&css), confidence)
                .ranges(vec![registration.range.clone()])
                .notes(notes),
        );
    }

    // Shared stylesheets that other files @import but that have no
    // registration of their own.
    let registered_paths: HashSet<&str> = registrations.iter().map(|registration| registration.path.as_str()).collect();
    let mut referenced_common = HashSet::new();
    for value in registrations.iter().map(|registration| &registration.value).chain(pool.values()) {
        collect_pool_references(value, &mut referenced_common);
    }
    let mut common_sorted: Vec<(&String, &i64)> = common_ids.iter().collect();
    common_sorted.sort_by(|a, b| a.0.cmp(b.0));
    for (path, id) in common_sorted {
        if !referenced_common.contains(id) || registered_paths.contains(path.as_str()) {
            continue;
        }
        let Some(value) = pool.get(id) else { continue };
        let mut stack = HashSet::from([*id]);
        let mut nested = HashSet::new();
        let content = renderer.render(value, path, &mut stack, &mut nested);
        files.push(
            Candidate::new(path.clone(), Some(ArtifactKind::Style), format_css(&clean_selectors(&content)), Confidence::High)
                .notes(vec!["由 __COMMON_STYLESHEETS__ 共享样式表恢复".into()]),
        );
    }

    for &id in referenced_unknown_pools.iter().filter(|&&id| id < COMMON_POOL_BASE) {
        let Some(value) = pool.get(&id) else { continue };
        let path = format!("__wxlocated_shared__/style-{id}.wxss");
        let mut stack = HashSet::from([id]);
        let mut nested = HashSet::new();
        let content = renderer.render(value, &path, &mut stack, &mut nested);
        let (css, _) = removing_compiler_annotations(&clean_selectors(&content));
        files.push(
            Candidate::new(path, Some(ArtifactKind::Style), format_css(&css), Confidence::Medium)
                .notes(vec![format!("共享样式池 _C[{id}] 的稳定映射")]),
        );
    }
    if registrations.is_empty() && source.contains("setCssToHead") {
        diagnostics.push(Diagnostic::new(
            Severity::Warning,
            Stage::Styles,
            "检测到 setCssToHead，当前编译变体未形成静态样式注册项",
            Some(source_reference.clone()),
        ));
    }
    WxssRestoration { files, diagnostics }
}

fn next(index: usize, tokens: &[Token]) -> Option<usize> {
    next_significant(tokens, index)
}

/// First declaration wins for a path.
fn extract_common_stylesheets(tokens: &[Token], parser: &LiteralParser) -> Vec<(String, Literal)> {
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    for index in 0..tokens.len() {
        let sheet = (|| {
            if tokens[index].identifier() != Some("__COMMON_STYLESHEETS__") {
                return None;
            }
            let open = next(index, tokens).filter(|&i| tokens[i].is_punctuator(b'['))?;
            let path_index = next(open, tokens)?;
            let raw_path = tokens[path_index].string()?;
            let close = next(path_index, tokens).filter(|&i| tokens[i].is_punctuator(b']'))?;
            let equal = next(close, tokens).filter(|&i| tokens[i].is_punctuator(b'='))?;
            let (value, _) = parser.parse(next(equal, tokens)?)?;
            if !matches!(value, Literal::Array(_)) {
                return None;
            }
            Some((normalized_path(raw_path)?, value))
        })();
        if let Some((path, value)) = sheet
            && seen.insert(path.clone())
        {
            result.push((path, value));
        }
    }
    result
}

/// Rewrites `[2, "./a.wxss"]` path references into `[2, id]` pool references.
fn rewriting_path_references(value: &Literal, ids: &HashMap<String, i64>) -> Literal {
    if ids.is_empty() {
        return value.clone();
    }
    match value {
        Literal::Array(items) => {
            if items.len() == 2
                && items[0].integer_value() == Some(2)
                && let Literal::String(raw_path) = &items[1]
                && let Some(path) = normalized_path(raw_path)
                && let Some(id) = ids.get(&path)
            {
                return Literal::Array(vec![Literal::Number(2.0), Literal::Number(*id as f64)]);
            }
            Literal::Array(items.iter().map(|item| rewriting_path_references(item, ids)).collect())
        }
        Literal::Object(values) => {
            Literal::Object(values.iter().map(|(key, value)| (key.clone(), rewriting_path_references(value, ids))).collect())
        }
        _ => value.clone(),
    }
}

fn collect_pool_references(value: &Literal, ids: &mut HashSet<i64>) {
    let Literal::Array(items) = value else { return };
    if items.len() == 2
        && items[0].integer_value() == Some(2)
        && let Some(id) = items[1].integer_value()
    {
        ids.insert(id);
        return;
    }
    for item in items {
        collect_pool_references(item, ids);
    }
}

fn extract_style_pool(tokens: &[Token], parser: &LiteralParser) -> Pool {
    for index in 0..tokens.len() {
        if tokens[index].identifier() != Some("_C") {
            continue;
        }
        let Some(equal) = next(index, tokens).filter(|&i| tokens[i].is_punctuator(b'=')) else { continue };
        let Some(value_index) = next(equal, tokens) else { continue };
        let Some((value, _)) = parser.parse(value_index) else { continue };
        match value {
            Literal::Array(values) => return values.into_iter().enumerate().map(|(offset, value)| (offset as i64, value)).collect(),
            Literal::Object(values) => {
                return values.into_iter().filter_map(|(key, value)| key.parse::<i64>().ok().map(|id| (id, value))).collect();
            }
            _ => continue,
        }
    }
    Pool::new()
}

fn extract_registered_styles(tokens: &[Token], parser: &LiteralParser) -> Vec<Registration> {
    let mut result = Vec::new();
    for index in 0..tokens.len() {
        let registration = (|| {
            if tokens[index].identifier() != Some("__wxAppCode__") {
                return None;
            }
            let open = next(index, tokens).filter(|&i| tokens[i].is_punctuator(b'['))?;
            let path_index = next(open, tokens)?;
            let raw_path = tokens[path_index].string()?;
            if !raw_path.to_lowercase().ends_with(".wxss") {
                return None;
            }
            let close = next(path_index, tokens).filter(|&i| tokens[i].is_punctuator(b']'))?;
            let call = registered_style_call(close, tokens)?;
            let (value, range) = parse_first_call_argument(call, tokens, parser)?;
            Some(Registration { path: normalized_path(raw_path)?, value, range })
        })();
        result.extend(registration);
    }
    result
}

fn first_set_css_value(tokens: &[Token], parser: &LiteralParser) -> Option<(Literal, Range<usize>)> {
    let index = tokens.iter().position(|token| token.identifier().is_some_and(is_style_call))?;
    parse_first_call_argument(index, tokens, parser)
}

fn parse_first_call_argument(call: usize, tokens: &[Token], parser: &LiteralParser) -> Option<(Literal, Range<usize>)> {
    let open = next(call, tokens).filter(|&i| tokens[i].is_punctuator(b'('))?;
    let value_index = next(open, tokens)?;
    let (literal, end) = parser.parse(value_index)?;
    Some((literal, tokens[value_index].start..tokens[end].end))
}

fn pure_pool_reference(value: &Literal) -> Option<i64> {
    if let Some(id) = value.integer_value() {
        return Some(id);
    }
    let Literal::Array(items) = value else { return None };
    if items.len() != 1 {
        return None;
    }
    let Literal::Array(instruction) = &items[0] else { return None };
    if instruction.len() < 2 || instruction[0].integer_value() != Some(2) {
        return None;
    }
    instruction[1].integer_value()
}

struct Renderer<'a> {
    pool: &'a Pool,
    pool_paths: &'a HashMap<i64, String>,
}

impl Renderer<'_> {
    fn render(&self, value: &Literal, current_path: &str, stack: &mut HashSet<i64>, unknown: &mut HashSet<i64>) -> String {
        match value {
            Literal::String(text) => text.clone(),
            Literal::Number(number) => {
                if number.round() != *number {
                    return swift_double_description(*number);
                }
                self.render_pool(*number as i64, current_path, stack, unknown)
            }
            Literal::Array(items) => {
                match (items.first().and_then(Literal::integer_value), items.len()) {
                    (Some(0), length) if length > 1 => return numeric_text(&items[1]) + "rpx",
                    (Some(1), _) => return String::new(),
                    (Some(2), length) if length > 1 => {
                        // `[2,"./x.wxss"]` whose sheet is defined in another file:
                        // still an import of that path.
                        if let Literal::String(referenced) = &items[1]
                            && let Some(target) = normalized_path(referenced)
                        {
                            if target == current_path {
                                // Own content lives in a shared sheet this file
                                // doesn't define: not restorable from here.
                                unknown.insert(UNRESOLVED_SHARED_SHEET);
                                return format!("/* WxLocated: 本文件内容登记在其他文件的共享样式表 {referenced} 中 */\n");
                            }
                            return format!("@import \"{}\";\n", relative_path(current_path, &target));
                        }
                        let Some(id) = items[1].integer_value() else { return String::new() };
                        return self.render_pool(id, current_path, stack, unknown);
                    }
                    _ => {}
                }
                items.iter().map(|item| self.render(item, current_path, stack, unknown)).collect()
            }
            Literal::Object(values) => {
                let mut sorted: Vec<_> = values.iter().collect();
                sorted.sort_by(|a, b| a.0.cmp(b.0));
                sorted.into_iter().map(|(_, value)| self.render(value, current_path, stack, unknown)).collect()
            }
            Literal::Identifier(value) => format!("/* WxLocated: {value} */"),
            Literal::Boolean(_) | Literal::Null => String::new(),
        }
    }

    fn render_pool(&self, id: i64, current_path: &str, stack: &mut HashSet<i64>, unknown: &mut HashSet<i64>) -> String {
        if let Some(mapped) = self.pool_paths.get(&id)
            && mapped != current_path
        {
            return format!("@import \"{}\";\n", relative_path(current_path, mapped));
        }
        let Some(value) = self.pool.get(&id) else {
            unknown.insert(id);
            return format!("@import \"{}\";\n", relative_path(current_path, &format!("__wxlocated_shared__/style-{id}.wxss")));
        };
        if !stack.insert(id) {
            return format!("/* WxLocated: 样式池循环引用 _C[{id}] */\n");
        }
        if !self.pool_paths.contains_key(&id) {
            unknown.insert(id);
        }
        let text = self.render(value, current_path, stack, unknown);
        stack.remove(&id);
        text
    }
}

fn numeric_text(value: &Literal) -> String {
    let Literal::Number(number) = value else { return "0".into() };
    if number.round() == *number { (*number as i64).to_string() } else { swift_double_description(*number) }
}

/// `page-frame`/`app-wxss` containers are the app stylesheet; any other file
/// keeps its own name with a `.wxss` extension.
fn output_path(source_path: &str) -> Option<String> {
    let path = normalized_path(source_path)?;
    if matches!(file_name(&path), "page-frame.html" | "page-frame.js" | "app-wxss.js") {
        return Some("app.wxss".into());
    }
    normalized_path(&super::changing_extension(&path, "wxss"))
}

/// The `setCssToHead` call a `__wxAppCode__["x.wxss"]` registration points at.
/// Two compiler forms exist:
/// - direct: `__wxAppCode__["x.wxss"] = setCssToHead([...], …)` (current caches)
/// - wrapped: `__wxAppCode__["x.wxss"] = function(){ setCssToHead([...])() }`
///
/// Extended libraries use `setCssToHead_<AppID>` in either form.
fn registered_style_call(close: usize, tokens: &[Token]) -> Option<usize> {
    if let Some(assign) = next(close, tokens).filter(|&i| tokens[i].is_punctuator(b'='))
        && let Some(target) = next(assign, tokens).filter(|&i| tokens[i].identifier().is_some_and(is_style_call))
    {
        return Some(target);
    }
    let body_open = (close + 1..tokens.len().min(close + 160)).find(|&i| tokens[i].is_punctuator(b'{'))?;
    let body_close = matching_token(tokens, body_open, b'{', b'}')?;
    (body_open + 1..body_close).find(|&i| tokens[i].identifier().is_some_and(is_style_call))
}

/// `setCssToHead`, or `setCssToHead_<AppID>` for an extended library's styles.
fn is_style_call(name: &str) -> bool {
    name == "setCssToHead" || name.starts_with("setCssToHead_")
}

static WX_SELECTOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)(^|[\s,{>+~])wx-([A-Za-z][A-Za-z0-9-]*)").expect("regex"));
static ANNOTATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*;?\s*wxcs_[A-Za-z0-9_-]+\s*:[^;{}]*;?").expect("regex"));

fn clean_selectors(source: &str) -> String {
    let result = WX_SELECTOR.replace_all(source, "${1}${2}");
    replace_body_selector(&result)
}

/// `(?m)(^|[\s,{>+~])body(?=([\s,.#:{>+~]|$))` → `$1page`, without lookahead.
fn replace_body_selector(source: &str) -> String {
    let is_before = |c: char| c.is_whitespace() || matches!(c, ',' | '{' | '>' | '+' | '~');
    let is_after = |c: char| c.is_whitespace() || matches!(c, ',' | '.' | '#' | ':' | '{' | '>' | '+' | '~');
    let mut output = String::with_capacity(source.len());
    let mut last = 0;
    let mut search = 0;
    while let Some(found) = source[search..].find("body") {
        let start = search + found;
        let end = start + 4;
        let previous = source[..start].chars().next_back();
        let at_line_start = previous.is_none_or(|c| c == '\n');
        let following = source[end..].chars().next();
        let at_line_end = following.is_none_or(|c| c == '\n');
        if (at_line_start || previous.is_some_and(is_before)) && (at_line_end || following.is_some_and(is_after)) {
            output.push_str(&source[last..start]);
            output.push_str("page");
            last = end;
        }
        search = end;
    }
    output.push_str(&source[last..]);
    output
}

/// Some compiles (extended libraries) add pseudo-declarations inside each rule:
/// `wxcs_originclass`, `wxcs_fileinfo: <file> <line> <column>` and
/// `wxcs_style_<property>`. They were never in the source.
fn removing_compiler_annotations(css: &str) -> (String, usize) {
    if !css.contains("wxcs_") {
        return (css.to_string(), 0);
    }
    let count = ANNOTATION.find_iter(css).count();
    (ANNOTATION.replace_all(css, "").into_owned(), count)
}

/// `CharacterSet.whitespaces`: tab and Unicode `Zs` (no newlines).
fn is_space_or_tab(c: char) -> bool {
    matches!(c, '\t' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}')
}

/// One declaration per line, four-space indentation. Iterates like Swift's
/// `Character` loop: a `\r\n` pair is one unit and is kept verbatim.
fn format_css(source: &str) -> String {
    let mut output = String::with_capacity(source.len() + source.len() / 4);
    let mut indentation = 0usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut characters = source.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\r' && characters.peek() == Some(&'\n') {
            characters.next();
            output.push_str("\r\n");
            if quote.is_some() {
                escaped = false;
            }
            continue;
        }
        if let Some(open) = quote {
            output.push(character);
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == open {
                quote = None;
            }
            continue;
        }
        match character {
            '"' | '\'' => {
                quote = Some(character);
                output.push(character);
            }
            '{' => {
                output = output.trim_matches(is_space_or_tab).to_string();
                output.push_str(" {\n");
                indentation += 1;
                output.push_str(&"    ".repeat(indentation));
            }
            ';' => {
                output.push_str(";\n");
                output.push_str(&"    ".repeat(indentation));
            }
            '}' => {
                output = trim_whitespace_and_newlines(&output).to_string();
                indentation = indentation.saturating_sub(1);
                output.push('\n');
                output.push_str(&"    ".repeat(indentation));
                output.push_str("}\n");
                output.push_str(&"    ".repeat(indentation));
            }
            '\n' | '\r' => {
                if !output.ends_with(' ') {
                    output.push(' ');
                }
            }
            _ => output.push(character),
        }
    }
    format!("{}\n", trim_whitespace_and_newlines(&output))
}

fn relative_path(source: &str, target: &str) -> String {
    let mut source_components: Vec<&str> = source.split('/').filter(|c| !c.is_empty()).collect();
    source_components.pop();
    let target_components: Vec<&str> = target.split('/').filter(|c| !c.is_empty()).collect();
    let mut common = 0;
    while common < source_components.len()
        && common < target_components.len()
        && source_components[common] == target_components[common]
    {
        common += 1;
    }
    let mut parts: Vec<&str> = vec![".."; source_components.len() - common];
    parts.extend(&target_components[common..]);
    let combined = parts.join("/");
    if combined.starts_with('.') { combined } else { format!("./{combined}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference() -> SourceRef {
        SourceRef { package: "p".into(), entry_path: "app-wxss.js".into(), start: None, end: None }
    }

    #[test]
    fn restores_pool_rpx_and_import() {
        let source = r#"var _C=[[".a{width:",[0,20],"}"]];__wxAppCode__['pages/x/x.wxss']=setCssToHead([[2,0],".b{color:red;}"]);__wxAppCode__['common/c.wxss']=setCssToHead([[2,0]]);"#;
        let restoration = restore(source, "app-wxss.js", &reference());
        let by_path: HashMap<_, _> = restoration.files.iter().map(|file| (file.path.as_str(), file)).collect();
        assert_eq!(by_path["common/c.wxss"].content, ".a {\n    width:20rpx\n}\n");
        // Swift's formatCSS trims only the ends of the buffer at `{`, so the
        // newline after the import leaves a leading space: kept as is.
        assert_eq!(by_path["pages/x/x.wxss"].content, "@import \"../../common/c.wxss\";\n .b {\n    color:red;\n}\n");
    }

    #[test]
    fn body_becomes_page_only_as_selector() {
        assert_eq!(replace_body_selector("body{a}\n.x body,.tbody{}"), "page{a}\n.x page,.tbody{}");
    }

    #[test]
    fn strips_wxcs_annotations() {
        let (css, removed) = removing_compiler_annotations(".a{color:red;wxcs_fileinfo: a.wxss 1 2;}");
        assert_eq!(removed, 1);
        assert_eq!(css, ".a{color:red}");
    }
}
