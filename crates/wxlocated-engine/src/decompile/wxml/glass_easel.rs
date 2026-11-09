//! WXML from the glass-easel compiled format (newer WeChat builds,
//! `componentFramework: glass-easel`). Templates ship through
//! `__wxCodeSpace__.batchAddCompiledTemplate(function(G,R,D,Q,gdc,X,Y,Z,RG){ return {'<path>': <builder>, …} })`
//! instead of the `$gwx` opcode pools.
//!
//! The builder is a declarative tree of callbacks: `E(tag,{},attrsFn,childrenFn)`
//! for elements, `cond?T(x):T()` for text, `B`/`F`/`S`/`J` for if/for/slot/pure.
//! The tree is the nesting of the children builder functions, so this walks the
//! call structure statically (it never executes the closures) and reuses the
//! `$gwx` node model and serializer so output matches that path exactly.
//! Anything it cannot map is counted and the file is marked degraded.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use super::WxmlRestoration;
use super::factory::{Attributes, Branch, Loop, Node, NodeKind, NodeRef, serialize};
use crate::decompile::{ArtifactKind, Candidate, Confidence, Diagnostic, Severity, SourceRef, Stage, file_name, normalized_path};
use crate::js::lexer::{Kind, Token, matching_token, next_significant, tokenize};
use crate::js::literal::{Literal, LiteralParser};

pub fn restore(source: &str, source_path: &str, source_reference: &SourceRef) -> WxmlRestoration {
    if !source.contains("batchAddCompiledTemplate") {
        return WxmlRestoration { files: Vec::new(), diagnostics: Vec::new() };
    }
    // The service layer carries a copy of the same templates, sometimes pruned
    // to empty; the render layer is authoritative. Demote the service copy so
    // the render layer wins the same-path merge.
    let lowered = file_name(source_path).to_lowercase();
    let is_logic_layer = lowered.contains("app-service") || lowered.contains("appservice") || lowered == "common.app.js";
    let bytes = source.as_bytes();
    let tokens = tokenize(bytes);
    let mut files = Vec::new();
    let mut diagnostics = Vec::new();

    for template in template_entries(&tokens) {
        let Some(path) = normalized_template_path(&template.path) else { continue };
        let mut context = Context { tokens: &tokens, bytes, unknown: Vec::new(), branch_conditions: HashMap::new() };
        let nodes = context.render_template(&template);
        let wxml = serialize(&nodes, 0);
        let content = if wxml.is_empty() {
            "\n".to_string()
        } else if wxml.ends_with('\n') {
            wxml
        } else {
            wxml + "\n"
        };
        let confidence = if is_logic_layer {
            Confidence::Low
        } else if context.unknown.is_empty() {
            Confidence::High
        } else {
            Confidence::Medium
        };
        let mut notes = if context.unknown.is_empty() {
            vec!["由 glass-easel 编译模板还原".to_string()]
        } else {
            let mut distinct: Vec<&String> = context.unknown.iter().collect::<HashSet<_>>().into_iter().collect();
            distinct.sort();
            let sample: Vec<&str> = distinct.iter().take(5).map(|s| s.as_str()).collect();
            vec![format!("由 glass-easel 编译模板还原，{} 处未能还原：{}", context.unknown.len(), sample.join("、"))]
        };
        if is_logic_layer {
            notes.push("来自逻辑层的副本，渲染层缺失时才保留".into());
        }
        let range = tokens[template.lowest].start..tokens[template.highest].end;
        files.push(Candidate::new(path.clone(), Some(ArtifactKind::Markup), content, confidence).ranges(vec![range]).notes(notes));
        if !context.unknown.is_empty() && !is_logic_layer {
            diagnostics.push(Diagnostic::new(
                Severity::Warning,
                Stage::Markup,
                format!("{path} 含 {} 处未能还原的 glass-easel 指令", context.unknown.len()),
                Some(source_reference.clone()),
            ));
        }
    }
    WxmlRestoration { files, diagnostics }
}

// MARK: Template discovery

struct Template {
    path: String,
    /// Every `H[<key>]` render in the entry: `""` is the component's own body,
    /// a non-empty key is a `<template name=key>` definition.
    renders: Vec<Render>,
    /// Token bounds of the renders, for provenance.
    lowest: usize,
    highest: usize,
}

struct Render {
    name: String,
    functions: HashMap<String, Function>,
    root_builder: String,
}

#[derive(Clone)]
struct Function {
    parameters: Vec<String>,
    body: Range<usize>,
}

fn next(index: usize, tokens: &[Token]) -> Option<usize> {
    next_significant(tokens, index)
}

/// Every `'<path>': (()=>{ … })()` entry of each `batchAddCompiledTemplate` object.
fn template_entries(tokens: &[Token]) -> Vec<Template> {
    let mut result = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        let call = (|| {
            if tokens[index].identifier() != Some("batchAddCompiledTemplate") {
                return None;
            }
            let open = next(index, tokens).filter(|&i| tokens[i].is_punctuator(b'('))?;
            let call_close = matching_token(tokens, open, b'(', b')')?;
            let function = next(open, tokens).filter(|&i| tokens[i].identifier() == Some("function"))?;
            let parameters_open = next(function, tokens).filter(|&i| tokens[i].is_punctuator(b'('))?;
            let parameters_close = matching_token(tokens, parameters_open, b'(', b')')?;
            let object_open = next(parameters_close, tokens).filter(|&i| tokens[i].is_punctuator(b'{'))?;
            let object_close = matching_token(tokens, object_open, b'{', b'}')?;
            Some((call_close, object_open, object_close))
        })();
        let Some((call_close, object_open, object_close)) = call else {
            index += 1;
            continue;
        };
        // Walk the body for `'path': value,` entries.
        let mut cursor = object_open + 1;
        while cursor < object_close {
            let entry = (|| {
                let path = tokens[cursor].string()?;
                let colon = next(cursor, tokens).filter(|&i| tokens[i].is_punctuator(b':'))?;
                Some((path.to_string(), next(colon, tokens)?))
            })();
            let Some((path, value_start)) = entry else {
                cursor += 1;
                continue;
            };
            if let Some(template) = parse_template(path, value_start, object_close, tokens) {
                result.push(template);
            }
            // Skip to the comma that ends this entry at object depth.
            cursor = end_of_entry(value_start, object_close, tokens);
        }
        index = call_close + 1;
    }
    result
}

fn end_of_entry(start: usize, limit: usize, tokens: &[Token]) -> usize {
    let mut depth = 0i64;
    #[allow(clippy::needless_range_loop)] // `index` also marks positions for later lookups
    for index in start..limit {
        match tokens[index].kind {
            Kind::Punctuator(b'(' | b'[' | b'{') => depth += 1,
            Kind::Punctuator(b')' | b']' | b'}') => depth -= 1,
            Kind::Punctuator(b',') if depth == 0 => return index + 1,
            _ => {}
        }
    }
    limit
}

/// `(()=>{ … H[""]=(R,C,D,U)=>{…} … H["name"]=(R,C,D,U)=>{…} … })()`
fn parse_template(path: String, value_start: usize, limit: usize, tokens: &[Token]) -> Option<Template> {
    // Bound the scan to this entry's IIFE so later entries' H keys are not
    // pulled in.
    if !tokens[value_start].is_punctuator(b'(') {
        return None;
    }
    let value_close = matching_token(tokens, value_start, b'(', b')')?;
    let mut renders = Vec::new();
    let mut lowest = value_close;
    let mut highest = value_start;
    let mut index = value_start;
    while index < value_close.min(limit) {
        let render = (|| {
            if tokens[index].identifier() != Some("H") {
                return None;
            }
            let open = next(index, tokens).filter(|&i| tokens[i].is_punctuator(b'['))?;
            let key = next(open, tokens)?;
            let name = tokens[key].string()?.to_string();
            let close = next(key, tokens).filter(|&i| tokens[i].is_punctuator(b']'))?;
            let equal = next(close, tokens).filter(|&i| tokens[i].is_punctuator(b'='))?;
            let arrow = arrow_function(next(equal, tokens), tokens)?;
            let root_builder = returned_builder(&arrow.body, tokens)?;
            Some((name, arrow, root_builder))
        })();
        if let Some((name, arrow, root_builder)) = render {
            renders.push(Render { name, functions: local_functions(&arrow.body, tokens), root_builder });
            lowest = lowest.min(arrow.body.start);
            highest = highest.max(arrow.body.end);
            index = arrow.body.end;
            continue;
        }
        index += 1;
    }
    if renders.is_empty() {
        return None;
    }
    Some(Template { path, renders, lowest, highest: lowest.max(highest - 1) })
}

/// `return {C:<ident>,B:A}` → the `C` identifier.
fn returned_builder(body: &Range<usize>, tokens: &[Token]) -> Option<String> {
    for index in body.clone() {
        let found = (|| {
            if tokens[index].identifier() != Some("return") {
                return None;
            }
            let brace = next(index, tokens).filter(|&i| tokens[i].is_punctuator(b'{'))?;
            let c = next(brace, tokens).filter(|&i| tokens[i].identifier() == Some("C"))?;
            let colon = next(c, tokens).filter(|&i| tokens[i].is_punctuator(b':'))?;
            tokens[next(colon, tokens)?].identifier().map(str::to_string)
        })();
        if found.is_some() {
            return found;
        }
    }
    None
}

/// `name=(params)=>{body}` declarations at any depth inside the render function
/// (they are all hoisted into one `var` chain in practice). First one wins.
fn local_functions(body: &Range<usize>, tokens: &[Token]) -> HashMap<String, Function> {
    let mut result = HashMap::new();
    let mut index = body.start;
    while index < body.end {
        let declaration = (|| {
            let name = tokens[index].identifier()?;
            let equal = next(index, tokens).filter(|&i| tokens[i].is_punctuator(b'='))?;
            let parameters_open = next(equal, tokens).filter(|&i| tokens[i].is_punctuator(b'('))?;
            Some((name.to_string(), arrow_function(Some(parameters_open), tokens)?))
        })();
        if let Some((name, arrow)) = declaration {
            index = arrow.body.end;
            result.entry(name).or_insert(arrow);
            continue;
        }
        index += 1;
    }
    result
}

/// Parses `(params)=>{body}` starting at the `(`; returns params and the token
/// range strictly inside the `{ }`.
fn arrow_function(parameters_open: Option<usize>, tokens: &[Token]) -> Option<Function> {
    let parameters_open = parameters_open.filter(|&i| i < tokens.len() && tokens[i].is_punctuator(b'('))?;
    let parameters_close = matching_token(tokens, parameters_open, b'(', b')')?;
    let arrow = next(parameters_close, tokens).filter(|&i| tokens[i].is_punctuator(b'='))?;
    if !tokens.get(arrow + 1).is_some_and(|t| t.is_punctuator(b'>')) {
        return None;
    }
    let body_open = next(arrow + 1, tokens).filter(|&i| tokens[i].is_punctuator(b'{'))?;
    let body_close = matching_token(tokens, body_open, b'{', b'}')?;
    let parameters =
        tokens[parameters_open + 1..parameters_close].iter().filter_map(|token| token.identifier().map(str::to_string)).collect();
    Some(Function { parameters, body: body_open + 1..body_close })
}

fn normalized_template_path(raw_path: &str) -> Option<String> {
    let path = raw_path.strip_suffix(".wxml").unwrap_or(raw_path);
    normalized_path(path).map(|path| path + ".wxml")
}

fn normalized_tag(value: &str) -> &str {
    value.strip_prefix("wx-").unwrap_or(value)
}

// MARK: Rendering

#[derive(Clone, Copy)]
struct ArgRange {
    first: usize,
    last: usize,
}

struct Call {
    callee: String,
    arguments: Vec<ArgRange>,
    end: usize,
}

type Scope = HashMap<String, String>;

const CALLBACKS: &[&str] = &["C", "T", "E", "B", "F", "S", "J"];
const RUNTIME_NAMES: &[&str] = &["C", "K", "U", "Q", "Z", "R", "G", "N", "T", "E", "B", "F", "S", "J", "gdc"];

struct Context<'a> {
    tokens: &'a [Token],
    bytes: &'a [u8],
    unknown: Vec<String>,
    /// Branch variable → condition expression per 1-based index.
    branch_conditions: HashMap<String, Vec<Option<String>>>,
}

impl Context<'_> {
    fn render_template(&mut self, template: &Template) -> Vec<NodeRef> {
        let mut definitions = Vec::new();
        let mut body_nodes = Vec::new();
        // Named templates first (their definitions), then the component body.
        let mut renders: Vec<&Render> = template.renders.iter().collect();
        renders.sort_by_key(|render| render.name.is_empty());
        for render in renders {
            let Some(root) = render.functions.get(&render.root_builder) else {
                self.unknown.push("根构建函数".into());
                continue;
            };
            let nodes = self.render_content(root.body.clone(), &render.functions, &Scope::new());
            if render.name.is_empty() {
                body_nodes.extend(nodes);
            } else {
                let definition = Node::with_attributes(
                    NodeKind::Element("template".into()),
                    Attributes::from([("name".to_string(), Some(render.name.clone()))]),
                );
                definition.borrow_mut().children = nodes;
                definitions.push(definition);
            }
        }
        definitions.extend(body_nodes);
        definitions
    }

    /// A children-content function body → the WXML nodes it builds, in order.
    fn render_content(&mut self, body: Range<usize>, inherited: &HashMap<String, Function>, scope: &Scope) -> Vec<NodeRef> {
        // Sub-builders are declared local to each builder body; merge them with
        // the inherited ones so sibling/ancestor references resolve.
        let mut functions = inherited.clone();
        functions.extend(local_functions(&body, self.tokens));
        let tokens = self.tokens;
        let mut nodes = Vec::new();
        let mut index = body.start;
        while index < body.end {
            if tokens[index].is_comment() {
                index += 1;
                continue;
            }
            // Skip a local sub-builder declaration `name=(params)=>{…}`: it is
            // collected separately, and its body may contain `T(`.
            if tokens[index].identifier().is_some()
                && let Some(equal) = next(index, tokens).filter(|&i| tokens[i].is_punctuator(b'='))
                && let Some(paren) = next(equal, tokens).filter(|&i| tokens[i].is_punctuator(b'('))
                && let Some(declaration) = arrow_function(Some(paren), tokens)
            {
                index = declaration.body.end + 1;
                continue;
            }
            // Branch-variable assignment: `v=cond?1:0` / `v=c1?1:c2?2:0`.
            if let Some(name) = tokens[index].identifier()
                && let Some(equal) = next(index, tokens).filter(|&i| tokens[i].is_punctuator(b'='))
                && !matches!(name, "C" | "U" | "K")
                && let Some(value_start) = next(equal, tokens)
                && self.is_branch_assignment(value_start, &body)
            {
                let end = self.statement_end(value_start, body.end);
                let conditions = self.branch_expressions(value_start, end, scope);
                self.branch_conditions.insert(name.to_string(), conditions);
                index = end.max(index + 1);
                continue;
            }
            // Callback statement: E/B/F/S/J(...) or a text ternary `cond?T(x):T()`.
            if let Some(call) = self.call_at(index) {
                match call.callee.as_str() {
                    "E" => self.append_element(&call, &mut nodes, &functions, scope),
                    "B" => self.append_branch(&call, &mut nodes, &functions, scope),
                    "F" => self.append_loop(&call, &mut nodes, &functions, scope),
                    "S" => self.append_slot(&call, &mut nodes, scope),
                    "J" => self.append_pure(&call, &mut nodes, &functions, scope),
                    _ => {}
                }
                index = call.end;
                continue;
            }
            if let Some((node, end)) = self.text_node(index, &body, scope) {
                nodes.push(node);
                index = end.max(index + 1);
                continue;
            }
            index = self.statement_end(index, body.end).max(index + 1);
        }
        nodes
    }

    // MARK: Elements

    fn append_element(&mut self, call: &Call, nodes: &mut Vec<NodeRef>, functions: &HashMap<String, Function>, scope: &Scope) {
        let Some(tag) = call.arguments.first().and_then(|argument| self.tokens[argument.first].string()) else {
            self.unknown.push("E 标签".into());
            return;
        };
        let node = Node::element(normalized_tag(tag));
        if call.arguments.len() > 2 {
            self.apply_attributes(call.arguments[2], &node, functions, scope);
        }
        if call.arguments.len() > 3 {
            let children = self.child_nodes(call.arguments[3], functions, scope);
            node.borrow_mut().children = children;
        }
        nodes.push(node);
    }

    fn append_slot(&mut self, call: &Call, nodes: &mut Vec<NodeRef>, scope: &Scope) {
        let node = Node::element("slot");
        if let Some(&first) = call.arguments.first() {
            if let Some(name) = self.tokens[first.first].string() {
                if !name.is_empty() {
                    node.borrow_mut().attributes.insert("name".into(), Some(name.to_string()));
                }
            } else if let Some(expression) = self.render_expression(first, scope) {
                node.borrow_mut().attributes.insert("name".into(), Some(format!("{{{{{expression}}}}}")));
            }
        }
        nodes.push(node);
    }

    fn append_pure(&mut self, call: &Call, nodes: &mut Vec<NodeRef>, functions: &HashMap<String, Function>, scope: &Scope) {
        let Some(&first) = call.arguments.first() else { return };
        let virtual_node = Node::new(NodeKind::Virtual);
        let children = self.child_nodes(first, functions, scope);
        virtual_node.borrow_mut().children = children;
        nodes.push(virtual_node);
    }

    // MARK: Conditionals

    fn append_branch(&mut self, call: &Call, nodes: &mut Vec<NodeRef>, functions: &HashMap<String, Function>, scope: &Scope) {
        let parsed = (|| {
            if call.arguments.len() < 2 || call.arguments[0].first != call.arguments[0].last {
                return None;
            }
            let variable = self.tokens[call.arguments[0].first].identifier()?.to_string();
            let builder = self.builder_body(call.arguments[1], functions)?;
            Some((variable, builder))
        })();
        let Some((variable, builder)) = parsed else {
            self.unknown.push("B 分支".into());
            return;
        };
        let conditions = self.branch_conditions.get(&variable).cloned().unwrap_or_default();
        let chain = self.if_chain(&builder.body);
        let virtual_node = Node::new(NodeKind::Virtual);
        if chain.is_empty() {
            // No `if` inside: unconditional children.
            let children = self.render_content(builder.body.clone(), functions, scope);
            virtual_node.borrow_mut().children = children;
            nodes.push(virtual_node);
            return;
        }
        for (offset, (branch_index, body)) in chain.into_iter().enumerate() {
            let children = self.render_content(body, functions, scope);
            let (directive, condition) = match branch_index {
                None => ("wx:else", None),
                Some(branch_index) => {
                    let directive = if offset == 0 { "wx:if" } else { "wx:elif" };
                    let position = branch_index - 1;
                    let known = usize::try_from(position).ok().and_then(|position| conditions.get(position));
                    let condition = match known {
                        Some(Some(expression)) => format!("{{{{{expression}}}}}"),
                        Some(None) => {
                            self.unknown.push("分支条件".into());
                            "{{__wxlocated_cond}}".to_string()
                        }
                        None => "{{__wxlocated_cond}}".to_string(),
                    };
                    (directive, Some(condition))
                }
            };
            virtual_node.borrow_mut().branches.push(Branch { directive: directive.into(), condition, children });
        }
        nodes.push(virtual_node);
    }

    // MARK: Loops

    fn append_loop(&mut self, call: &Call, nodes: &mut Vec<NodeRef>, functions: &HashMap<String, Function>, scope: &Scope) {
        let builder = (call.arguments.len() >= 2).then(|| self.builder_body(*call.arguments.last().unwrap(), functions)).flatten();
        let Some(builder) = builder else {
            self.unknown.push("F 循环".into());
            return;
        };
        let list = call.arguments[0];
        let items = if list.first == list.last
            && let Some(static_list) = self.tokens[list.first].string()
        {
            static_list.to_string()
        } else if let Some(expression) = self.render_expression(list, scope) {
            format!("{{{{{expression}}}}}")
        } else {
            self.unknown.push("循环数据".into());
            "{{__wxlocated_list}}".to_string()
        };
        let key = call.arguments.get(1).and_then(|argument| self.tokens[argument.first].string()).unwrap_or_default().to_string();

        // The for-item builder's non-callback params are item, index, …
        let mut inner_scope = scope.clone();
        let scope_parameters: Vec<&String> = builder.parameters.iter().filter(|p| !CALLBACKS.contains(&p.as_str())).collect();
        if let Some(item) = scope_parameters.first() {
            inner_scope.insert((*item).clone(), "item".into());
        }
        if let Some(index) = scope_parameters.get(1) {
            inner_scope.insert((*index).clone(), "index".into());
        }

        let virtual_node = Node::new(NodeKind::Virtual);
        virtual_node.borrow_mut().loop_ = Some(Loop { items, item: "item".into(), index: "index".into(), key });
        let children = self.render_content(builder.body.clone(), functions, &inner_scope);
        virtual_node.borrow_mut().children = children;
        nodes.push(virtual_node);
    }

    // MARK: Text

    /// `cond?T(x):T()` or `C?T("static"):T()`.
    fn text_node(&mut self, index: usize, body: &Range<usize>, scope: &Scope) -> Option<(NodeRef, usize)> {
        let tokens = self.tokens;
        let end = self.statement_end(index, body.end);
        // The first `T(` call at brace depth 0 of this statement (a `T(` nested
        // inside `{…}` belongs to a closure, not this text).
        let mut depth = 0i64;
        for cursor in index..end {
            match tokens[cursor].kind {
                Kind::Punctuator(b'{') => depth += 1,
                Kind::Punctuator(b'}') => depth -= 1,
                _ => {}
            }
            if depth == 0
                && tokens[cursor].identifier() == Some("T")
                && let Some(open) = next(cursor, tokens).filter(|&i| tokens[i].is_punctuator(b'('))
                && let Some(close) = matching_token(tokens, open, b'(', b')')
            {
                let argument = *self.arguments(open, close).first()?;
                if argument.first == argument.last
                    && let Some(value) = tokens[argument.first].string()
                {
                    return Some((Node::text(value), end));
                }
                let Some(expression) = self.render_expression(argument, scope) else {
                    self.unknown.push("文本表达式".into());
                    return Some((Node::text("{{__wxlocated_text}}"), end));
                };
                return Some((Node::text(&format!("{{{{{expression}}}}}")), end));
            }
        }
        None
    }

    // MARK: Attributes

    fn apply_attributes(&mut self, argument: ArgRange, node: &NodeRef, functions: &HashMap<String, Function>, scope: &Scope) {
        let Some(builder) = self.builder_body(argument, functions) else { return };
        let mut events: Vec<(String, String)> = Vec::new();
        let mut index = builder.body.start;
        while index < builder.body.end {
            let Some(call) = self.setter_call(&mut index, builder.body.end) else { continue };
            self.apply_setter(&call, node, scope, &mut events);
        }
        let mut node = node.borrow_mut();
        // `O(N,"bindtap","__e")` + eventOpts dataset → real handler names.
        for (event_type, handler) in &events {
            let prefix = format!("bind{event_type}");
            let suffix = format!(":{event_type}");
            for (key, value) in node.attributes.iter_mut() {
                if (*key == prefix || key.ends_with(&suffix)) && value.as_deref() == Some("__e") {
                    *value = Some(handler.clone());
                }
            }
            if !node.attributes.contains_key(&prefix) && !node.attributes.contains_key(&format!("bind:{event_type}")) {
                node.attributes.insert(prefix, Some(handler.clone()));
            }
        }
        // Drop any remaining `__e` placeholders we could not resolve.
        for value in node.attributes.values_mut() {
            if value.as_deref() == Some("__e") {
                *value = Some(String::new());
            }
        }
    }

    /// One `[if(cond)] METHOD(N,...)` statement inside an attribute function.
    fn setter_call(&self, index: &mut usize, limit: usize) -> Option<Call> {
        let tokens = self.tokens;
        while *index < limit {
            if tokens[*index].is_comment() {
                *index += 1;
                continue;
            }
            // Skip an `if(...)` guard.
            if tokens[*index].identifier() == Some("if")
                && let Some(open) = next(*index, tokens).filter(|&i| tokens[i].is_punctuator(b'('))
                && let Some(close) = matching_token(tokens, open, b'(', b')')
            {
                *index = next(close, tokens).unwrap_or(close + 1);
                continue;
            }
            if let Some(call) = self.method_call_at(*index) {
                *index = call.end;
                return Some(call);
            }
            *index += 1;
        }
        None
    }

    fn apply_setter(&mut self, call: &Call, node: &NodeRef, scope: &Scope, events: &mut Vec<(String, String)>) {
        let tokens = self.tokens;
        let arguments = &call.arguments;
        let string_at = |position: usize| arguments.get(position).and_then(|argument| tokens[argument.first].string());
        match call.callee.as_str() {
            "L" => {
                if arguments.len() >= 2
                    && let Some(text) = self.attribute_text(arguments[1], scope)
                {
                    node.borrow_mut().attributes.insert("class".into(), Some(text));
                }
            }
            "O" => {
                let Some(name) = (arguments.len() >= 2).then(|| string_at(1)).flatten() else {
                    self.unknown.push("O 属性".into());
                    return;
                };
                let name = name.to_string();
                if arguments.len() >= 3 {
                    let value = arguments[2];
                    if tokens[value.first].identifier() == Some("true") && value.first == value.last {
                        node.borrow_mut().attributes.insert(name, None);
                    } else if let Some(text) = self.attribute_text(value, scope) {
                        node.borrow_mut().attributes.insert(name, Some(text));
                    } else {
                        self.unknown.push("属性值".into());
                    }
                } else {
                    node.borrow_mut().attributes.insert(name, None);
                }
            }
            "R.y" => {
                if let Some(&last) = arguments.last()
                    && let Some(text) = self.attribute_text(last, scope)
                {
                    node.borrow_mut().attributes.insert("style".into(), Some(text));
                }
            }
            "R.i" => {
                if arguments.len() >= 2
                    && let Some(text) = self.attribute_text(arguments[1], scope)
                {
                    node.borrow_mut().attributes.insert("id".into(), Some(text));
                }
            }
            "M" => {
                if arguments.len() >= 2
                    && let Some(name) = string_at(1)
                {
                    let value =
                        if arguments.len() >= 3 { self.attribute_text(arguments[2], scope).unwrap_or_default() } else { String::new() };
                    node.borrow_mut().attributes.insert(format!("mark:{name}"), Some(value));
                }
            }
            "R.p" => {
                if arguments.len() >= 3
                    && let Some(name) = string_at(1)
                {
                    let value = self.attribute_text(arguments[2], scope).unwrap_or_default();
                    node.borrow_mut().attributes.insert(name.to_string(), Some(value));
                }
            }
            "R.d" => {
                if arguments.len() >= 2
                    && let Some(name) = string_at(1)
                {
                    if name == "eventOpts" && arguments.len() >= 3 {
                        events.extend(self.parse_event_options(arguments[2]));
                    } else if arguments.len() >= 3 {
                        let value = self.attribute_text(arguments[2], scope).unwrap_or_default();
                        node.borrow_mut().attributes.insert(name.to_string(), Some(value));
                    }
                }
            }
            "R.v" => self.apply_event_binding(arguments, node, scope),
            "R.e" => {
                if let Some(&last) = arguments.last()
                    && let Some(text) = self.class_list(last, scope)
                {
                    node.borrow_mut().attributes.insert("class".into(), Some(text));
                }
            }
            "R.w" => {
                // Style lists are name/value pairs whose shape is not settled;
                // degrade rather than guess.
                self.unknown.push("style 列表".into());
            }
            // Binding-map updates, generics, slot content: no attribute.
            "R.ei" | "R.wi" | "E" | "R.s" | "R.a" | "R.devArgs" => {}
            other => self.unknown.push(other.to_string()),
        }
    }

    fn apply_event_binding(&mut self, arguments: &[ArgRange], node: &NodeRef, scope: &Scope) {
        if arguments.len() < 3 {
            return;
        }
        let Some(event) = self.tokens[arguments[1].first].string() else { return };
        let handler = match self.tokens[arguments[2].first].string() {
            Some(value) => value.to_string(),
            None => self.render_expression(arguments[2], scope).unwrap_or_default(),
        };
        let is_catch = self.boolean_argument(arguments.get(3).copied());
        let is_capture = self.boolean_argument(arguments.get(5).copied());
        let mut prefix = if is_catch { "catch".to_string() } else { "bind".to_string() };
        if is_capture {
            prefix = format!("capture-{prefix}");
        }
        node.borrow_mut().attributes.insert(format!("{prefix}:{event}"), Some(handler));
    }

    /// `[[ "tap",[["onTapTell",["$event"]]] ]]` → [(tap, onTapTell)].
    fn parse_event_options(&self, argument: ArgRange) -> Vec<(String, String)> {
        let Some((Literal::Array(entries), _)) = LiteralParser::new(self.tokens, self.bytes).parse(argument.first) else {
            return Vec::new();
        };
        entries
            .iter()
            .filter_map(|entry| {
                let Literal::Array(pair) = entry else { return None };
                if pair.len() < 2 {
                    return None;
                }
                let Literal::String(event_type) = &pair[0] else { return None };
                let Literal::Array(handlers) = &pair[1] else { return None };
                let Some(Literal::Array(first)) = handlers.first() else { return None };
                let Some(Literal::String(handler)) = first.first() else { return None };
                Some((event_type.clone(), handler.clone()))
            })
            .collect()
    }

    fn boolean_argument(&self, argument: Option<ArgRange>) -> bool {
        let Some(argument) = argument else { return false };
        // `!0` is true, `!1` is false.
        if self.tokens[argument.first].is_punctuator(b'!')
            && argument.first < argument.last
            && self.tokens[argument.first + 1].kind == Kind::Number
        {
            return self.bytes[self.tokens[argument.first + 1].start] == b'0';
        }
        self.tokens[argument.first].identifier() == Some("true")
    }

    /// A static-or-dynamic attribute value → WXML attribute text.
    fn attribute_text(&mut self, argument: ArgRange, scope: &Scope) -> Option<String> {
        if argument.first == argument.last {
            let token = &self.tokens[argument.first];
            match &token.kind {
                Kind::String(value) => return Some(value.clone()),
                Kind::Number => return Some(token.text(self.bytes).into_owned()),
                Kind::Identifier(word) if word == "true" => return Some("true".into()),
                Kind::Identifier(word) if word == "false" => return Some("false".into()),
                _ => {}
            }
        }
        self.render_expression(argument, scope).map(|expression| format!("{{{{{expression}}}}}"))
    }

    /// `R.e(N,[ "a", cond?"b":"" , null ])` → `a {{cond?'b':''}}`.
    fn class_list(&mut self, argument: ArgRange, scope: &Scope) -> Option<String> {
        if !self.tokens[argument.first].is_punctuator(b'[') {
            return None;
        }
        let close = matching_token(self.tokens, argument.first, b'[', b']')?;
        let mut parts = Vec::new();
        for element in self.arguments(argument.first, close) {
            if element.first == element.last
                && let Some(name) = self.tokens[element.first].string()
            {
                if !name.is_empty() {
                    parts.push(name.to_string());
                }
            } else if self.tokens[element.first].identifier() == Some("null") {
                continue;
            } else if let Some(expression) = self.render_expression(element, scope) {
                parts.push(format!("{{{{{expression}}}}}"));
            } else {
                self.unknown.push("class 列表项".into());
            }
        }
        (!parts.is_empty()).then(|| parts.join(" "))
    }

    // MARK: Child builders

    fn builder_body(&self, argument: ArgRange, functions: &HashMap<String, Function>) -> Option<Function> {
        if argument.first == argument.last
            && let Some(name) = self.tokens[argument.first].identifier()
        {
            return functions.get(name).cloned();
        }
        // Inline arrow builder.
        arrow_function(Some(argument.first), self.tokens)
    }

    fn child_nodes(&mut self, argument: ArgRange, functions: &HashMap<String, Function>, scope: &Scope) -> Vec<NodeRef> {
        match self.builder_body(argument, functions) {
            Some(builder) => self.render_content(builder.body, functions, scope),
            None => Vec::new(),
        }
    }

    // MARK: Branch parsing

    /// The value contains a top-level `?`.
    fn is_branch_assignment(&self, start: usize, body: &Range<usize>) -> bool {
        let end = self.statement_end(start, body.end);
        let mut depth = 0i64;
        let mut saw_ternary = false;
        for index in start..end {
            match self.tokens[index].kind {
                Kind::Punctuator(b'(' | b'[' | b'{') => depth += 1,
                Kind::Punctuator(b')' | b']' | b'}') => depth -= 1,
                Kind::Punctuator(b'?') if depth == 0 => saw_ternary = true,
                _ => {}
            }
        }
        saw_ternary
    }

    /// `c1?1:c2?2:0` → ["c1 expr", "c2 expr"] (1-based branch conditions).
    fn branch_expressions(&mut self, start: usize, end: usize, scope: &Scope) -> Vec<Option<String>> {
        let mut conditions = Vec::new();
        let mut cursor = start;
        while cursor < end {
            let Some(question) = self.top_level_punctuator(b'?', cursor, end) else { break };
            conditions.push(self.render(cursor, question, scope));
            let Some(colon) = self.top_level_punctuator(b':', question + 1, end) else { break };
            cursor = colon + 1;
        }
        conditions
    }

    /// `if(v===1){…}else if(v===2){…}else{…}` → ordered (index, body) blocks;
    /// a `None` index is the trailing `else`.
    fn if_chain(&self, body: &Range<usize>) -> Vec<(Option<i64>, Range<usize>)> {
        let tokens = self.tokens;
        let mut blocks = Vec::new();
        let mut index = body.start;
        while index < body.end {
            let block = (|| {
                if tokens[index].identifier() != Some("if") {
                    return None;
                }
                let open = next(index, tokens).filter(|&i| tokens[i].is_punctuator(b'('))?;
                let close = matching_token(tokens, open, b'(', b')')?;
                let brace_open = next(close, tokens).filter(|&i| tokens[i].is_punctuator(b'{'))?;
                let brace_close = matching_token(tokens, brace_open, b'{', b'}')?;
                Some((open, close, brace_open, brace_close))
            })();
            let Some((open, close, brace_open, brace_close)) = block else {
                index += 1;
                continue;
            };
            blocks.push((self.branch_index(open, close), brace_open + 1..brace_close));
            // `else` / `else if` / `else { }`
            if let Some(else_index) = next(brace_close, tokens).filter(|&i| i < body.end && tokens[i].identifier() == Some("else"))
            {
                if let Some(if_index) = next(else_index, tokens).filter(|&i| tokens[i].identifier() == Some("if")) {
                    index = if_index;
                } else if let Some(else_open) = next(else_index, tokens).filter(|&i| tokens[i].is_punctuator(b'{'))
                    && let Some(else_close) = matching_token(tokens, else_open, b'{', b'}')
                {
                    blocks.push((None, else_open + 1..else_close));
                    index = else_close + 1;
                }
            }
            if index <= brace_close {
                index = brace_close + 1;
            }
        }
        blocks
    }

    /// `v===2` → 2; `None` when the number does not parse (then the block is
    /// treated as `else`, as in the Swift original); 1 when there is no `==`.
    fn branch_index(&self, open: usize, close: usize) -> Option<i64> {
        for index in open..close {
            if index + 1 >= close {
                continue;
            }
            if self.tokens[index].is_punctuator(b'=')
                && self.tokens[index + 1].is_punctuator(b'=')
                && let Some(number) = next(index + 1, self.tokens).filter(|&i| i < close && self.tokens[i].kind == Kind::Number)
            {
                return self.tokens[number].text(self.bytes).parse().ok();
            }
        }
        Some(1)
    }

    // MARK: Call parsing

    /// A call whose callee is one of the builder callbacks E/B/F/S/J (not T,
    /// which only appears inside text ternaries).
    fn call_at(&self, index: usize) -> Option<Call> {
        let name = self.tokens[index].identifier().filter(|name| matches!(*name, "E" | "B" | "F" | "S" | "J"))?;
        let open = next(index, self.tokens).filter(|&i| self.tokens[i].is_punctuator(b'('))?;
        let close = matching_token(self.tokens, open, b'(', b')')?;
        Some(Call { callee: name.to_string(), arguments: self.arguments(open, close), end: close + 1 })
    }

    /// A setter call `L(…)`, `O(…)`, `R.x(…)`, `M(…)` inside an attr function.
    fn method_call_at(&self, index: usize) -> Option<Call> {
        let tokens = self.tokens;
        let head = tokens[index].identifier()?;
        let mut callee = head.to_string();
        let mut call_open = next(index, tokens);
        if let Some(dot) = call_open.filter(|&i| tokens[i].is_punctuator(b'.'))
            && let Some(member) = next(dot, tokens)
            && let Some(property) = tokens[member].identifier()
        {
            callee = format!("{head}.{property}");
            call_open = next(member, tokens);
        }
        let open = call_open.filter(|&i| tokens[i].is_punctuator(b'('))?;
        let close = matching_token(tokens, open, b'(', b')')?;
        next(open, tokens).filter(|&first| close > open && tokens[first].identifier() == Some("N"))?;
        Some(Call { callee, arguments: self.arguments(open, close), end: close + 1 })
    }

    fn arguments(&self, open: usize, close: usize) -> Vec<ArgRange> {
        let mut result = Vec::new();
        let mut start = open + 1;
        let mut depth = 0i64;
        let flush = |result: &mut Vec<ArgRange>, start: usize, end: usize| {
            let first = (start..end).find(|&i| !self.tokens[i].is_comment());
            let last = (start..end).rev().find(|&i| !self.tokens[i].is_comment());
            if let (Some(first), Some(last)) = (first, last) {
                result.push(ArgRange { first, last });
            }
        };
        for index in open + 1..close {
            match self.tokens[index].kind {
                Kind::Punctuator(b'(' | b'[' | b'{') => depth += 1,
                Kind::Punctuator(b')' | b']' | b'}') => depth -= 1,
                Kind::Punctuator(b',') if depth == 0 => {
                    flush(&mut result, start, index);
                    start = index + 1;
                }
                _ => {}
            }
        }
        flush(&mut result, start, close);
        result
    }

    // MARK: Expression rendering

    /// Renders a glass-easel value expression into WXML expression text.
    /// Unwraps the transparent runtime wrappers `X` (undefined-safe member),
    /// `Y` (toString) and `P` (function guard); strips the `D.` data prefix;
    /// maps loop scope vars to their WXML names. `None` (→ degrade) on any
    /// construct that is not a plain value.
    fn render_expression(&self, argument: ArgRange, scope: &Scope) -> Option<String> {
        self.render(argument.first, argument.last + 1, scope)
    }

    fn render(&self, low: usize, high: usize, scope: &Scope) -> Option<String> {
        let tokens = self.tokens;
        let mut out = String::new();
        let mut index = low;
        while index < high {
            let token = &tokens[index];
            match &token.kind {
                Kind::Comment => {
                    index += 1;
                }
                Kind::Identifier(name) => {
                    if matches!(name.as_str(), "X" | "Y" | "P")
                        && index + 1 < high
                        && tokens[index + 1].is_punctuator(b'(')
                        && let Some(close) = matching_token(tokens, index + 1, b'(', b')').filter(|&close| close < high)
                    {
                        out.push_str(&self.render(index + 2, close, scope)?);
                        index = close + 1;
                        continue;
                    }
                    if name == "D" && index + 1 < high && tokens[index + 1].is_punctuator(b'.') {
                        index += 2; // drop `D.`
                        continue;
                    }
                    if let Some(mapped) = scope.get(name) {
                        out.push_str(mapped);
                        index += 1;
                        continue;
                    }
                    if RUNTIME_NAMES.contains(&name.as_str()) {
                        return None; // runtime state or callbacks: not a value
                    }
                    out.push_str(name);
                    index += 1;
                }
                Kind::String(_) => {
                    out.push_str(&normalized_string_literal(&token.text(self.bytes)));
                    index += 1;
                }
                _ => {
                    out.push_str(&token.text(self.bytes));
                    index += 1;
                }
            }
        }
        (!out.is_empty()).then_some(out)
    }

    fn top_level_punctuator(&self, value: u8, from: usize, to: usize) -> Option<usize> {
        let mut depth = 0i64;
        for index in from..to {
            match self.tokens[index].kind {
                Kind::Punctuator(b'(' | b'[' | b'{') => depth += 1,
                Kind::Punctuator(b')' | b']' | b'}') => depth -= 1,
                Kind::Punctuator(byte) if byte == value && depth == 0 => return Some(index),
                _ => {}
            }
        }
        None
    }

    fn statement_end(&self, index: usize, limit: usize) -> usize {
        let mut depth = 0i64;
        for cursor in index..limit {
            match self.tokens[cursor].kind {
                Kind::Punctuator(b'(' | b'[' | b'{') => depth += 1,
                Kind::Punctuator(b')' | b']' | b'}') => {
                    if depth == 0 {
                        return cursor;
                    }
                    depth -= 1;
                }
                Kind::Punctuator(b';') if depth == 0 => return cursor + 1,
                _ => {}
            }
        }
        limit
    }
}

/// WXML expressions quote strings with single quotes.
fn normalized_string_literal(raw: &str) -> String {
    let mut characters = raw.chars();
    let Some(first) = characters.next() else { return raw.to_string() };
    if raw.chars().count() < 2 || !(first == '"' || first == '\'') {
        return raw.to_string();
    }
    let inner: String = {
        let mut inner: Vec<char> = raw.chars().skip(1).collect();
        inner.pop();
        inner.into_iter().collect()
    };
    let unescaped = inner.replace("\\\"", "\"");
    format!("'{}'", unescaped.replace('\'', "\\'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn restore_one(source: &str, path: &str) -> Candidate {
        let reference = SourceRef { package: "p".into(), entry_path: "page-frame.js".into(), start: None, end: None };
        restore(source, path, &reference).files.into_iter().next().expect("one template")
    }

    /// Same fixture as the Swift `WXMLGlassEaselTests.testElementConditionLoopAndEvent`.
    #[test]
    fn element_condition_loop_and_event() {
        let source = r#"__wxCodeSpace__.batchAddCompiledTemplate(function(G,R,D,Q,gdc,X,Y,Z,RG={}){var P=RG.P||function(a){return typeof a==='function'?a:()=>{}};return {'commonPack/tips/index':(()=>{var H={};var S;var I=(P)=>{if(!S)S=Object.assign({},H);return S[P]};H[""]=(R,C,D,U)=>{if(typeof R.setEventListenerWrapper==='function')R.setEventListenerWrapper(Q.C);var L=R.c,M=R.m,O=R.r,A={},K=U===true,b,e=(C,T)=>{C||K||U.tipsTitle?T(Y(D.tipsTitle)):T()},p=(C,T)=>{C?T("联系服务商"):T()},o=(C,T,E)=>{if(n===1){E("text",{},(N,C)=>{if(C)L(N,"link-tel");if(C)O(N,"bindtap","__e");if(C||K||Q.a([]))R.d(N,"eventOpts",[["tap",[["onTapTell",["$event"]]]]],!0)},p)}},n,f=(C,g,h,i,j,k,T,E)=>{var m=(C,T)=>{C||K||i?T(Y(g)):T()},l=(C,T,E,B)=>{E("text",{},(N,C)=>{},m);n=D.linkTel?1:0;B(n,o)};E("view",{},(N,C)=>{if(C)L(N,"tips-desc")},l)},g=(C)=>{},d=(C,T,E,B,F)=>{E("view",{},(N,C)=>{if(C)L(N,"tips-title")},e);F(D.tipsDesc,"index",C||K?undefined:U.tipsDesc,[0,"tipsDesc"],f);E("image",{},(N,C)=>{if(C)L(N,"tips-icon");if(C||K||U.tipsIcon)O(N,"src",D.tipsIcon);if(C)O(N,"mode","widthFix")},g)},c=(C,T,E)=>{if(b===1){E("view",{},(N,C)=>{if(C)L(N,"wrapper")},d)}},a=(C,T,E,B)=>{b=D.isOnload?1:0;B(b,c);C=!1;U=Object.create(null);K=!1};;return {C:a,B:A}};return Object.assign(function(R){return H[R]},{_:H})})(),}});"#;
        let file = restore_one(source, "page-frame.js");
        assert_eq!(file.path, "commonPack/tips/index.wxml");
        assert_eq!(file.confidence, Confidence::High);
        assert_eq!(
            file.content,
            "<view wx:if=\"{{isOnload}}\" class=\"wrapper\">\n    <view class=\"tips-title\">{{tipsTitle}}</view>\n    <view wx:for=\"{{tipsDesc}}\" wx:key=\"index\" class=\"tips-desc\">\n        <text>{{item}}</text>\n        <text wx:if=\"{{linkTel}}\" class=\"link-tel\" bindtap=\"onTapTell\">联系服务商</text>\n    </view>\n    <image class=\"tips-icon\" mode=\"widthFix\" src=\"{{tipsIcon}}\"></image>\n</view>\n"
        );
    }

    #[test]
    fn named_template_and_body() {
        let source = r#"__wxCodeSpace__.batchAddCompiledTemplate(function(G,R,D,Q,gdc,X,Y,Z,RG={}){var P=RG.P||function(a){return typeof a==='function'?a:()=>{}};return {'pkg/list/index':(()=>{var H={};var S;var I=(P)=>{if(!S)S=Object.assign({},H);return S[P]};H["card"]=(R,C,D,U)=>{var L=R.c,O=R.r,A={},K=U===true,b=(C,T)=>{C||K||U.title?T(Y(D.title)):T()},a=(C,T,E)=>{E("view",{},(N,C)=>{if(C)L(N,"card")},b)};return {C:a,B:A}};H[""]=(R,C,D,U)=>{var L=R.c,O=R.r,A={},K=U===true,b=(C)=>{},a=(C,T,E)=>{E("view",{},(N,C)=>{if(C)L(N,"page")},b)};return {C:a,B:A}};return Object.assign(function(R){return H[R]},{_:H})})(),}});"#;
        let file = restore_one(source, "page-frame.js");
        assert_eq!(
            file.content,
            "<template name=\"card\">\n    <view class=\"card\">{{title}}</view>\n</template>\n<view class=\"page\"></view>\n"
        );
    }

    #[test]
    fn service_layer_copy_is_low_confidence() {
        let source = r#"__wxCodeSpace__.batchAddCompiledTemplate(function(G,R,D,Q,gdc,X,Y,Z,RG={}){var P=RG.P||function(a){return a};return {'pkg/a/index':(()=>{var H={};var S;var I=(P)=>{if(!S)S=Object.assign({},H);return S[P]};H[""]=(R,C,D,U)=>{var L=R.c,A={},K=U===true,b=(C)=>{},a=(C,T,E)=>{E("view",{},(N,C)=>{if(C)L(N,"a")},b)};return {C:a,B:A}};return Object.assign(function(R){return H[R]},{_:H})})(),}});"#;
        assert_eq!(restore_one(source, "appservice.app.js").confidence, Confidence::Low);
    }
}
