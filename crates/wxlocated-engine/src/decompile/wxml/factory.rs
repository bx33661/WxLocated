//! Runs one compiled `$gwx` WXML function (page factory, `<template>` body or
//! loop body) statement by statement and rebuilds the tree it would render.
//!
//! Mechanical only: every element, attribute and directive in the result
//! comes from an instruction in the compiled code. Statements it does not
//! understand are counted in `unknown`, never guessed.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::pool::{OperationPools, decode};
use crate::js::lexer::{Kind, Token, matching_token, tokenize};
use crate::js::literal::{Literal, LiteralParser};
use crate::text::character_count;

/// Half-open token index range `(start, end)`.
type TokenRange = (usize, usize);

pub type NodeRef = Rc<RefCell<Node>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    Element(String),
    Text(String),
    /// `_v()`: a holder for `wx:if` branches, a `wx:for` loop, or a template
    /// call. Disappears when the tree is normalised.
    Virtual,
}

#[derive(Debug, Clone)]
pub struct Branch {
    pub directive: String,
    pub condition: Option<String>,
    pub children: Vec<NodeRef>,
}

#[derive(Debug, Clone)]
pub struct Loop {
    pub items: String,
    pub item: String,
    pub index: String,
    pub key: String,
}

/// Attribute value: `None` is a bare attribute (`wx:else`).
pub type Attributes = HashMap<String, Option<String>>;

#[derive(Debug)]
pub struct Node {
    pub kind: NodeKind,
    pub attributes: Attributes,
    pub children: Vec<NodeRef>,
    pub branches: Vec<Branch>,
    pub active_branch: Option<usize>,
    pub loop_: Option<Loop>,
}

impl Node {
    pub fn new(kind: NodeKind) -> NodeRef {
        Self::with_attributes(kind, Attributes::new())
    }

    pub fn with_attributes(kind: NodeKind, attributes: Attributes) -> NodeRef {
        Rc::new(RefCell::new(Self {
            kind,
            attributes,
            children: Vec::new(),
            branches: Vec::new(),
            active_branch: None,
            loop_: None,
        }))
    }

    pub fn element(tag: &str) -> NodeRef {
        Self::new(NodeKind::Element(tag.to_string()))
    }

    pub fn text(value: &str) -> NodeRef {
        Self::new(NodeKind::Text(value.to_string()))
    }
}

/// Appends into the active `wx:if` branch, if any, else into the children.
pub fn append(node: &NodeRef, child: NodeRef) {
    let mut node = node.borrow_mut();
    if let Some(active) = node.active_branch {
        node.branches[active].children.push(child);
    } else {
        node.children.push(child);
    }
}

#[derive(Debug, Clone)]
enum Value {
    Node(NodeRef),
    Expression(String),
    Function { parameters: Vec<String>, open: usize, close: usize },
    Template(String),
    Opaque,
}

type Scope = HashMap<String, Value>;

#[derive(Clone, Copy)]
struct Argument {
    first: usize,
    last: usize,
}

pub struct FactoryInterpreter<'a> {
    tokens: Vec<Token>,
    bytes: Vec<u8>,
    pool: &'a OperationPools,
    group: Option<String>,
    path_pool: &'a [String],
    pub unknown: Vec<String>,
    pub imports: Vec<String>,
    returned: bool,
    return_value: Option<Value>,
}

impl<'a> FactoryInterpreter<'a> {
    pub fn new(function: &str, pool: &'a OperationPools, group: Option<String>, path_pool: &'a [String]) -> Self {
        let bytes = function.as_bytes().to_vec();
        let tokens = tokenize(&bytes);
        Self { tokens, bytes, pool, group, path_pool, unknown: Vec::new(), imports: Vec::new(), returned: false, return_value: None }
    }

    /// Runs the function and returns the rendered children of its root (the
    /// third parameter, `r`), already normalised.
    pub fn run(&mut self) -> Vec<NodeRef> {
        let function = self
            .tokens
            .iter()
            .position(|token| token.identifier() == Some("function"))
            .and_then(|start| self.parse_function(start));
        let Some((function @ Value::Function { .. }, _)) = function else {
            self.unknown.push("函数头".into());
            return Vec::new();
        };
        let Value::Function { parameters, .. } = &function else { unreachable!() };
        let root = Node::new(NodeKind::Virtual);
        let mut arguments = vec![Value::Opaque; parameters.len().max(3)];
        arguments[2] = Value::Node(root.clone());
        let returned = self.call(&function, arguments, &Scope::new());
        // Older output builds its own root and returns it.
        if root.borrow().children.is_empty()
            && let Some(Value::Node(node)) = returned
            && !Rc::ptr_eq(&node, &root)
        {
            return normalize(&node);
        }
        normalize(&root)
    }

    // MARK: Statements

    fn call(&mut self, function: &Value, arguments: Vec<Value>, outer: &Scope) -> Option<Value> {
        let Value::Function { parameters, open, close } = function else { return None };
        let mut scope = outer.clone();
        for (index, name) in parameters.iter().enumerate() {
            scope.insert(name.clone(), arguments.get(index).cloned().unwrap_or(Value::Opaque));
        }
        let saved_returned = self.returned;
        let saved_value = self.return_value.take();
        self.returned = false;
        self.execute(open + 1, *close, &mut scope);
        let result = self.return_value.take();
        self.returned = saved_returned;
        self.return_value = saved_value;
        result
    }

    fn execute(&mut self, start: usize, end: usize, scope: &mut Scope) {
        let mut index = start;
        while index < end && !self.returned {
            let next = self.statement(index, end, scope);
            index = next.max(index + 1);
        }
    }

    fn statement(&mut self, index: usize, end: usize, scope: &mut Scope) -> usize {
        match &self.tokens[index].kind {
            Kind::Comment | Kind::Punctuator(b';') => index + 1,
            Kind::Punctuator(b'{') => {
                let Some(close) = self.matching(index, b'{', b'}') else { return end };
                self.execute(index + 1, close, scope);
                close + 1
            }
            Kind::Identifier(word) if matches!(word.as_str(), "var" | "let" | "const") => self.declaration(index, end, scope),
            Kind::Identifier(word) if word == "if" => self.conditional(index, end, scope),
            Kind::Identifier(word) if word == "try" => self.try_statement(index, end, scope),
            Kind::Identifier(word) if word == "return" => {
                if let Some(next) = self.next(index)
                    && next < end
                    && let Some(name) = self.tokens[next].identifier()
                    && !self.has_newline(index, next)
                {
                    self.return_value = scope.get(name).cloned();
                }
                self.returned = true;
                self.statement_end(index + 1, end)
            }
            Kind::Identifier(name) => {
                let name = name.clone();
                self.expression_statement(&name, index, end, scope)
            }
            _ => self.statement_end(index, end),
        }
    }

    fn declaration(&mut self, index: usize, end: usize, scope: &mut Scope) -> usize {
        let parsed = (|| {
            let name_index = self.next(index)?;
            let name = self.tokens[name_index].identifier()?.to_string();
            let equal = self.next(name_index).filter(|&i| self.tokens[i].is_punctuator(b'='))?;
            Some((name, self.next(equal)?))
        })();
        let Some((name, value_index)) = parsed else { return self.statement_end(index + 1, end) };
        if self.tokens[value_index].identifier() == Some("function")
            && let Some((function, function_end)) = self.parse_function(value_index)
        {
            scope.insert(name, function);
            return function_end;
        }
        let statement_end = self.statement_end(value_index, end);
        let value = self.evaluate(value_index, scope);
        scope.insert(name, value);
        statement_end
    }

    fn expression_statement(&mut self, name: &str, index: usize, end: usize, scope: &mut Scope) -> usize {
        let statement_end = self.statement_end(index, end);
        let Some(following) = self.next(index) else { return statement_end };
        if self.tokens[following].is_punctuator(b'(') {
            if let Some(arguments) = self.arguments(index) {
                self.call_statement(name, &arguments, scope);
            }
        } else if self.tokens[following].is_punctuator(b'.') {
            self.member_statement(name, following, scope);
        }
        statement_end
    }

    /// `X.rawAttr = {...}`, `X.attr['k'] = v`; bookkeeping such as `wxXCkey`,
    /// `gg.f` and `.pop()` has no effect on the tree.
    fn member_statement(&self, name: &str, dot: usize, scope: &Scope) {
        let Some(property_index) = self.next(dot) else { return };
        let Some(property) = self.tokens[property_index].identifier() else { return };
        let Some(Value::Node(node)) = scope.get(name) else { return };
        let parser = LiteralParser::new(&self.tokens, &self.bytes);
        match property {
            "rawAttr" => {
                let Some(equal) = self.next(property_index).filter(|&i| self.tokens[i].is_punctuator(b'=')) else { return };
                let Some(value_index) = self.next(equal) else { return };
                let Some((Literal::Object(values), _)) = parser.parse(value_index) else { return };
                let mut node = node.borrow_mut();
                for (key, value) in values {
                    set_attribute(&mut node.attributes, key, attribute_value(&value));
                }
            }
            "attr" => {
                let parsed = (|| {
                    let open = self.next(property_index).filter(|&i| self.tokens[i].is_punctuator(b'['))?;
                    let key_index = self.next(open)?;
                    let key = self.tokens[key_index].string()?.to_string();
                    let close = self.next(key_index)?;
                    let equal = self.next(close).filter(|&i| self.tokens[i].is_punctuator(b'='))?;
                    let (value, _) = parser.parse(self.next(equal)?)?;
                    Some((key, value))
                })();
                if let Some((key, value)) = parsed {
                    set_attribute(&mut node.borrow_mut().attributes, key, attribute_value(&value));
                }
            }
            _ => {}
        }
    }

    fn call_statement(&mut self, name: &str, arguments: &[Argument], scope: &Scope) {
        match name {
            "_" => {
                if arguments.len() < 2 {
                    return;
                }
                let Some(Value::Node(parent)) = self.value(arguments[0], scope) else { return };
                match self.value(arguments[1], scope) {
                    Some(Value::Node(child)) => append(&parent, child),
                    Some(Value::Expression(text)) => append(&parent, Node::text(&text)),
                    _ => self.unknown.push("_ 的子节点".into()),
                }
            }
            "_rz" | "_r" => {
                let offset = usize::from(name == "_rz");
                let parsed = (|| {
                    if arguments.len() < offset + 3 {
                        return None;
                    }
                    let Some(Value::Node(node)) = self.value(arguments[offset], scope) else { return None };
                    let attribute = self.string(arguments[offset + 1])?;
                    let id = self.integer(arguments[offset + 2])?;
                    Some((node, attribute, id))
                })();
                let Some((node, attribute, id)) = parsed else {
                    self.unknown.push(name.to_string());
                    return;
                };
                let value = self.pool_value(id, name == "_r");
                node.borrow_mut().attributes.insert(attribute, Some(value));
            }
            "_2z" | "_2" => {
                let skip = usize::from(name == "_2z");
                let rest: Vec<Argument> = arguments.iter().skip(skip).copied().collect();
                self.run_loop(&rest, name == "_2", scope);
            }
            "_ic" => {
                // _ic(x[src], e_, x[me], e, s, parent, gg)
                let parsed = (|| {
                    if arguments.len() < 6 {
                        return None;
                    }
                    let source = self.path(arguments[0])?;
                    let Some(Value::Node(parent)) = self.value(arguments[5], scope) else { return None };
                    Some((source, parent))
                })();
                let Some((source, parent)) = parsed else {
                    self.unknown.push(name.to_string());
                    return;
                };
                let include = Node::with_attributes(NodeKind::Element("include".into()), [("src".to_string(), Some(source))].into());
                append(&parent, include);
            }
            "_ai" => {
                // _ai(importList, x[src], e_, x[me], line, column)
                let Some(source) = (arguments.len() >= 2).then(|| self.path(arguments[1])).flatten() else {
                    self.unknown.push(name.to_string());
                    return;
                };
                if !self.imports.contains(&source) {
                    self.imports.push(source);
                }
            }
            // Runtime bookkeeping: warnings, WXS initialisation; no effect on the tree.
            "_w" | "_wl" | "_wp" | "_tsd" | "_ca" | "_wa" | "nf_init" => {}
            _ => {
                if let Some(Value::Template(template)) = scope.get(name) {
                    // template(data, data, holder, gg)
                    let holder = (arguments.len() >= 3).then(|| self.value(arguments[2], scope)).flatten();
                    let Some(Value::Node(holder)) = holder else {
                        self.unknown.push("模板调用".into());
                        return;
                    };
                    let mut attributes = Attributes::from([("is".to_string(), Some(template.clone()))]);
                    if let Some(Value::Expression(data)) = self.value(arguments[0], scope) {
                        attributes.insert("data".into(), Some(template_data(&data)));
                    }
                    append(&holder, Node::with_attributes(NodeKind::Element("template".into()), attributes));
                } else if !name.starts_with("gz$gwx") {
                    self.unknown.push(name.to_string());
                }
            }
        }
    }

    /// `_2z(z, items, body, e, s, gg, holder, 'item', 'index', 'key')`.
    fn run_loop(&mut self, arguments: &[Argument], global: bool, scope: &Scope) {
        let parsed = (|| {
            if arguments.len() < 9 {
                return None;
            }
            let id = self.integer(arguments[0])?;
            let body = self.value(arguments[1], scope).filter(|value| matches!(value, Value::Function { .. }))?;
            let Some(Value::Node(holder)) = self.value(arguments[5], scope) else { return None };
            Some((id, body, holder))
        })();
        let Some((id, body, holder)) = parsed else {
            self.unknown.push("循环".into());
            return;
        };
        let container = Node::new(NodeKind::Virtual);
        self.call(&body, vec![Value::Opaque, Value::Opaque, Value::Node(container.clone()), Value::Opaque], scope);
        let items = self.pool_value(id, global);
        let item = self.string(arguments[6]).unwrap_or_else(|| "item".into());
        let index = self.string(arguments[7]).unwrap_or_else(|| "index".into());
        let key = self.string(arguments[8]).unwrap_or_default();
        let children = container.borrow().children.clone();
        let mut holder = holder.borrow_mut();
        holder.loop_ = Some(Loop { items, item, index, key });
        holder.children.extend(children);
    }

    /// `if (_oz(z,N,…)) { holder.wxVkey = 1 … } else if (…) { holder.wxVkey = 2 … } else { … }`
    fn conditional(&mut self, index: usize, end: usize, scope: &mut Scope) -> usize {
        // (condition token range, body token range); `None` for the final `else`.
        let mut branches: Vec<(Option<TokenRange>, TokenRange)> = Vec::new();
        let mut cursor = index;
        let mut after;
        loop {
            let mut condition = None;
            if self.tokens[cursor].identifier() == Some("if") {
                let Some(open) = self.next(cursor).filter(|&i| self.tokens[i].is_punctuator(b'(')) else {
                    return self.statement_end(index, end);
                };
                let Some(close) = self.matching(open, b'(', b')') else { return self.statement_end(index, end) };
                condition = Some((open + 1, close));
                cursor = close;
            }
            let Some(body_start) = self.next(cursor) else { return end };
            let body;
            if self.tokens[body_start].is_punctuator(b'{')
                && let Some(close) = self.matching(body_start, b'{', b'}')
            {
                body = (body_start + 1, close);
                after = close + 1;
            } else {
                let statement_end = self.statement_end(body_start, end);
                body = (body_start, statement_end);
                after = statement_end;
            }
            branches.push((condition, body));
            if condition.is_none() {
                break;
            }
            let else_index = if after < end { self.significant_at_or_after(after) } else { None };
            let Some(else_index) = else_index.filter(|&i| i < end && self.tokens[i].identifier() == Some("else")) else {
                break;
            };
            let Some(following) = self.next(else_index) else { break };
            if self.tokens[following].identifier() == Some("if") {
                cursor = following;
            } else {
                // The trailing `else` body, without a condition.
                let Some(body_start) = self.next(else_index) else { break };
                if self.tokens[body_start].is_punctuator(b'{')
                    && let Some(close) = self.matching(body_start, b'{', b'}')
                {
                    branches.push((None, (body_start + 1, close)));
                    after = close + 1;
                } else {
                    let statement_end = self.statement_end(body_start, end);
                    branches.push((None, (body_start, statement_end)));
                    after = statement_end;
                }
                break;
            }
        }

        let Some(Some(first_condition)) = branches.first().map(|branch| branch.0) else { return after };
        match self.condition_value(first_condition, scope) {
            Value::Expression(_) => {
                for (offset, (condition_range, body)) in branches.iter().enumerate() {
                    let directive = if offset == 0 {
                        "wx:if"
                    } else if condition_range.is_none() {
                        "wx:else"
                    } else {
                        "wx:elif"
                    };
                    let mut condition = None;
                    if let Some(range) = condition_range {
                        let Value::Expression(text) = self.condition_value(*range, scope) else {
                            self.unknown.push("条件".into());
                            continue;
                        };
                        condition = Some(text);
                    }
                    let Some(holder) = self.branch_holder(*body, scope) else {
                        self.unknown.push("条件分支没有容器".into());
                        continue;
                    };
                    let previous = {
                        let mut holder = holder.borrow_mut();
                        let previous = holder.active_branch;
                        holder.branches.push(Branch { directive: directive.into(), condition, children: Vec::new() });
                        holder.active_branch = Some(holder.branches.len() - 1);
                        previous
                    };
                    self.execute(body.0, body.1, scope);
                    holder.borrow_mut().active_branch = previous;
                }
            }
            Value::Template(_) => {
                // `if (template) { … call … } else _w(…)`: the call is unconditional source.
                let body = branches[0].1;
                self.execute(body.0, body.1, scope);
            }
            // `if (p_[b]) { …; return }`: the recursion guard of template bodies.
            _ => {}
        }
        after
    }

    fn condition_value(&self, range: (usize, usize), scope: &Scope) -> Value {
        if range.0 >= range.1 {
            return Value::Opaque;
        }
        let Some(first) = self.significant_at_or_after(range.0).filter(|&first| first < range.1) else { return Value::Opaque };
        if let Some(name) = self.tokens[first].identifier()
            && self.next(first) == Some(range.1)
        {
            return scope.get(name).cloned().unwrap_or(Value::Opaque);
        }
        self.evaluate(first, scope)
    }

    /// The `_v()` holder named by the branch's first `holder.wxVkey = N`.
    fn branch_holder(&self, body: (usize, usize), scope: &Scope) -> Option<NodeRef> {
        let first = self.significant_at_or_after(body.0).filter(|&first| first < body.1)?;
        let name = self.tokens[first].identifier()?;
        let dot = self.next(first).filter(|&i| self.tokens[i].is_punctuator(b'.'))?;
        self.next(dot).filter(|&i| self.tokens[i].identifier() == Some("wxVkey"))?;
        match scope.get(name) {
            Some(Value::Node(holder)) => Some(holder.clone()),
            _ => None,
        }
    }

    fn try_statement(&mut self, index: usize, end: usize, scope: &mut Scope) -> usize {
        let Some(open) = self.next(index).filter(|&i| self.tokens[i].is_punctuator(b'{')) else {
            return self.statement_end(index, end);
        };
        let Some(close) = self.matching(open, b'{', b'}') else { return self.statement_end(index, end) };
        self.execute(open + 1, close, scope);
        let mut after = close + 1;
        loop {
            let keyword = if after < end { self.significant_at_or_after(after) } else { None };
            let Some(keyword) = keyword.filter(|&k| k < end) else { break };
            let word = match self.tokens[keyword].identifier() {
                Some(word @ ("catch" | "finally")) => word.to_string(),
                _ => break,
            };
            let mut cursor = keyword;
            if word == "catch"
                && let Some(parameters) = self.next(cursor).filter(|&i| self.tokens[i].is_punctuator(b'('))
                && let Some(close) = self.matching(parameters, b'(', b')')
            {
                cursor = close;
            }
            let Some(block_open) = self.next(cursor).filter(|&i| self.tokens[i].is_punctuator(b'{')) else { break };
            let Some(block_close) = self.matching(block_open, b'{', b'}') else { break };
            if word == "finally" {
                self.execute(block_open + 1, block_close, scope);
            }
            after = block_close + 1;
        }
        after
    }

    // MARK: Values

    fn evaluate(&self, index: usize, scope: &Scope) -> Value {
        let call = (|| {
            let callee = self.tokens[index].identifier()?.to_string();
            self.next(index).filter(|&i| self.tokens[i].is_punctuator(b'('))?;
            Some((callee, self.arguments(index)?))
        })();
        let Some((callee, arguments)) = call else {
            if let Some(name) = self.tokens[index].identifier()
                && let Some(value) = scope.get(name)
            {
                return value.clone();
            }
            return Value::Opaque;
        };
        match callee.as_str() {
            "_n" => match arguments.first().and_then(|&argument| self.string(argument)) {
                Some(tag) => Value::Node(Node::element(strip_wx_prefix(&tag))),
                None => Value::Opaque,
            },
            "_v" => Value::Node(Node::new(NodeKind::Virtual)),
            "_m" | "_mz" => {
                let offset = usize::from(callee == "_mz");
                let Some(tag) = arguments.get(offset).and_then(|&argument| self.string(argument)) else { return Value::Opaque };
                let node = Node::element(strip_wx_prefix(&tag));
                if let Some(argument) = arguments.get(offset + 1) {
                    let parser = LiteralParser::new(&self.tokens, &self.bytes);
                    if let Some((list, _)) = parser.parse(argument.first) {
                        node.borrow_mut().attributes = self.attributes(&list, callee == "_m");
                    }
                }
                Value::Node(node)
            }
            "_o" | "_1" => match arguments.first().and_then(|&argument| self.integer(argument)) {
                Some(id) => Value::Expression(self.pool_value(id, true)),
                None => Value::Opaque,
            },
            "_oz" | "_1z" => match arguments.get(1).and_then(|&argument| self.integer(argument)) {
                Some(id) => Value::Expression(self.pool_value(id, false)),
                None => Value::Opaque,
            },
            "_gd" => {
                // _gd(x[me], nameValue, e_, d_): look up a template by name.
                match arguments.get(1).and_then(|&argument| self.value(argument, scope)) {
                    Some(Value::Expression(name)) => Value::Template(name),
                    _ => Value::Opaque,
                }
            }
            _ => Value::Opaque,
        }
    }

    /// `['class', 3, 'mode', 1, 'src', 2]`: the first index is absolute, the
    /// rest are relative to it; a negative index is a bare attribute.
    fn attributes(&self, list: &Literal, global: bool) -> Attributes {
        let Literal::Array(values) = list else { return Attributes::new() };
        let mut result = Attributes::new();
        let mut base = 0i64;
        let mut index = 0;
        while index + 1 < values.len() {
            if let (Literal::String(name), Some(relative)) = (&values[index], values[index + 1].integer_value()) {
                if base + relative < 0 {
                    result.insert(name.clone(), None);
                } else {
                    result.insert(name.clone(), Some(self.pool_value(base + relative, global)));
                    if base == 0 {
                        base = relative;
                    }
                }
            }
            index += 2;
        }
        result
    }

    fn pool_value(&self, id: i64, global: bool) -> String {
        self.pool.value(id, if global { None } else { self.group.as_deref() })
    }

    fn value(&self, argument: Argument, scope: &Scope) -> Option<Value> {
        if argument.first == argument.last
            && let Some(name) = self.tokens[argument.first].identifier()
        {
            return scope.get(name).cloned();
        }
        Some(self.evaluate(argument.first, scope))
    }

    fn string(&self, argument: Argument) -> Option<String> {
        if argument.first != argument.last {
            return None;
        }
        self.tokens[argument.first].string().map(str::to_string)
    }

    fn integer(&self, argument: Argument) -> Option<i64> {
        LiteralParser::new(&self.tokens, &self.bytes).parse(argument.first)?.0.integer_value()
    }

    /// `x[N]` → the segment's N-th path.
    fn path(&self, argument: Argument) -> Option<String> {
        if self.tokens[argument.first].identifier() != Some("x") {
            return None;
        }
        let open = self.next(argument.first).filter(|&i| self.tokens[i].is_punctuator(b'['))?;
        let number = self.next(open).filter(|&i| self.tokens[i].kind == Kind::Number)?;
        let index: usize = self.tokens[number].text(&self.bytes).parse().ok()?;
        self.path_pool.get(index).cloned()
    }

    // MARK: Tokens

    fn parse_function(&self, index: usize) -> Option<(Value, usize)> {
        if self.tokens[index].identifier() != Some("function") {
            return None;
        }
        let mut cursor = index;
        if let Some(name) = self.next(cursor)
            && self.tokens[name].identifier().is_some()
        {
            cursor = name;
        }
        let open = self.next(cursor).filter(|&i| self.tokens[i].is_punctuator(b'('))?;
        let close = self.matching(open, b'(', b')')?;
        let body_open = self.next(close).filter(|&i| self.tokens[i].is_punctuator(b'{'))?;
        let body_close = self.matching(body_open, b'{', b'}')?;
        let parameters = self.tokens[open + 1..close].iter().filter_map(|token| token.identifier().map(str::to_string)).collect();
        Some((Value::Function { parameters, open: body_open, close: body_close }, body_close + 1))
    }

    fn arguments(&self, callee: usize) -> Option<Vec<Argument>> {
        let open = self.next(callee).filter(|&i| self.tokens[i].is_punctuator(b'('))?;
        let close = self.matching(open, b'(', b')')?;
        let mut result = Vec::new();
        let mut start = open + 1;
        let mut depth = 0i64;
        let flush = |result: &mut Vec<Argument>, start: usize, end: usize| {
            let first = (start..end).find(|&i| !self.tokens[i].is_comment());
            let last = (start..end).rev().find(|&i| !self.tokens[i].is_comment());
            if let (Some(first), Some(last)) = (first, last) {
                result.push(Argument { first, last });
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
        Some(result)
    }

    /// First token after the statement starting at `index`: a `;`, or a line
    /// break between a finished expression and the next statement.
    fn statement_end(&self, index: usize, end: usize) -> usize {
        let mut depth = 0i64;
        let mut cursor = index;
        while cursor < end {
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
            if depth == 0
                && cursor + 1 < end
                && self.has_newline(cursor, cursor + 1)
                && self.ends_expression(cursor)
                && self.starts_statement(cursor + 1)
            {
                return cursor + 1;
            }
            cursor += 1;
        }
        end
    }

    fn ends_expression(&self, index: usize) -> bool {
        matches!(
            self.tokens[index].kind,
            Kind::Identifier(_) | Kind::String(_) | Kind::Number | Kind::Punctuator(b')' | b']' | b'}')
        )
    }

    fn starts_statement(&self, index: usize) -> bool {
        match &self.tokens[index].kind {
            Kind::Identifier(word) => !matches!(word.as_str(), "else" | "catch" | "finally" | "instanceof" | "in"),
            Kind::Punctuator(b'}') => true,
            _ => false,
        }
    }

    fn has_newline(&self, left: usize, right: usize) -> bool {
        let start = self.tokens[left].end;
        let end = self.tokens[right].start;
        start < end && self.bytes[start..end].contains(&b'\n')
    }

    fn matching(&self, index: usize, open: u8, close: u8) -> Option<usize> {
        matching_token(&self.tokens, index, open, close)
    }

    fn next(&self, index: usize) -> Option<usize> {
        (index + 1..self.tokens.len()).find(|&i| !self.tokens[i].is_comment())
    }

    fn significant_at_or_after(&self, index: usize) -> Option<usize> {
        (index..self.tokens.len()).find(|&i| !self.tokens[i].is_comment())
    }
}

/// `attributeValue`: `true` is a bare attribute, strings and numbers are
/// values; anything else removes the attribute.
fn attribute_value(value: &Literal) -> Option<Option<String>> {
    match value {
        Literal::Boolean(true) => Some(None),
        Literal::String(text) => Some(Some(text.clone())),
        Literal::Number(_) => Some(Some(decode(value))),
        _ => None,
    }
}

fn set_attribute(attributes: &mut Attributes, key: String, value: Option<Option<String>>) {
    match value {
        Some(value) => {
            attributes.insert(key, value);
        }
        None => {
            attributes.remove(&key);
        }
    }
}

/// `{{{a:b}}}` → `{{a:b}}`: template data is written without its own braces.
fn template_data(value: &str) -> String {
    if value.starts_with("{{{") && value.ends_with("}}}") && value.len() >= 6 {
        return format!("{{{{{}}}}}", &value[3..value.len() - 3]);
    }
    value.to_string()
}

fn strip_wx_prefix(tag: &str) -> &str {
    tag.strip_prefix("wx-").unwrap_or(tag)
}

// MARK: Normalisation

/// Expands holders into what was written in the source: a single child takes
/// the directive itself, several children get a `<block>`.
pub fn normalize(node: &NodeRef) -> Vec<NodeRef> {
    let kind = node.borrow().kind.clone();
    match kind {
        NodeKind::Text(_) => vec![node.clone()],
        NodeKind::Element(_) => {
            let children = std::mem::take(&mut node.borrow_mut().children);
            let normalized: Vec<NodeRef> = children.iter().flat_map(normalize).collect();
            node.borrow_mut().children = normalized;
            vec![node.clone()]
        }
        NodeKind::Virtual => {
            let (branches, children, loop_) = {
                let node = node.borrow();
                (node.branches.clone(), node.children.clone(), node.loop_.clone())
            };
            let mut result = Vec::new();
            for branch in branches {
                let children: Vec<NodeRef> = branch.children.iter().flat_map(normalize).collect();
                let attributes = Attributes::from([(branch.directive.clone(), branch.condition.clone())]);
                result.push(wrap(children, attributes, |child| !child.attributes.keys().any(|key| key.starts_with("wx:"))));
            }
            let children: Vec<NodeRef> = children.iter().flat_map(normalize).collect();
            if let Some(loop_) = loop_ {
                let mut attributes = Attributes::from([("wx:for".to_string(), Some(loop_.items.clone()))]);
                if loop_.item != "item" {
                    attributes.insert("wx:for-item".into(), Some(loop_.item.clone()));
                }
                if loop_.index != "index" {
                    attributes.insert("wx:for-index".into(), Some(loop_.index.clone()));
                }
                if !loop_.key.is_empty() {
                    attributes.insert("wx:key".into(), Some(loop_.key.clone()));
                }
                // `wx:for` is evaluated before `wx:if`, so a conditional child can carry it.
                result.push(wrap(children, attributes, |child| {
                    !child.attributes.keys().any(|key| key.starts_with("wx:for") || key == "wx:key")
                }));
            } else {
                result.extend(children);
            }
            result
        }
    }
}

fn wrap(children: Vec<NodeRef>, attributes: Attributes, can_carry: impl Fn(&Node) -> bool) -> NodeRef {
    if children.len() == 1 {
        let child = &children[0];
        let carries = {
            let borrowed = child.borrow();
            matches!(borrowed.kind, NodeKind::Element(_)) && can_carry(&borrowed)
        };
        if carries {
            child.borrow_mut().attributes.extend(attributes);
            return child.clone();
        }
    }
    let block = Node::with_attributes(NodeKind::Element("block".into()), attributes);
    block.borrow_mut().children = children;
    block
}

// MARK: Serialisation

pub fn serialize(nodes: &[NodeRef], depth: usize) -> String {
    nodes.iter().map(|node| serialize_node(node, depth)).collect()
}

fn serialize_node(node: &NodeRef, depth: usize) -> String {
    let indent = "    ".repeat(depth);
    let kind = node.borrow().kind.clone();
    match kind {
        NodeKind::Text(value) => format!("{indent}{}\n", escape_text(&value)),
        NodeKind::Virtual => serialize(&normalize(node), depth),
        NodeKind::Element(tag) => {
            let node = node.borrow();
            let mut opening = format!("{indent}<{tag}");
            let mut attributes: Vec<_> = node.attributes.iter().collect();
            attributes.sort_by(|a, b| attribute_rank(a.0).cmp(&attribute_rank(b.0)).then_with(|| a.0.cmp(b.0)));
            for (name, value) in attributes {
                opening.push(' ');
                opening.push_str(name);
                if let Some(value) = value {
                    opening.push_str("=\"");
                    opening.push_str(&escape_attribute(value));
                    opening.push('"');
                }
            }
            if node.children.is_empty() {
                return if matches!(tag.as_str(), "import" | "include" | "template") {
                    format!("{opening} />\n")
                } else {
                    format!("{opening}></{tag}>\n")
                };
            }
            if node.children.len() == 1
                && let NodeKind::Text(value) = &node.children[0].borrow().kind
                && !crate::text::contains(value, "\n")
                && character_count(value) <= 120
            {
                return format!("{opening}>{}</{tag}>\n", escape_text(value));
            }
            format!("{opening}>\n{}{indent}</{tag}>\n", serialize(&node.children, depth + 1))
        }
    }
}

const LEADING_ATTRIBUTES: &[&str] =
    &["wx:if", "wx:elif", "wx:else", "wx:for", "wx:for-item", "wx:for-index", "wx:key", "is", "name", "data", "id", "class", "style"];

fn attribute_rank(name: &str) -> usize {
    if let Some(index) = LEADING_ATTRIBUTES.iter().position(|leading| *leading == name) {
        return index;
    }
    let events = name.starts_with("bind") || name.starts_with("catch") || name.starts_with("capture-");
    LEADING_ATTRIBUTES.len() + usize::from(!events)
}

/// Escapes markup characters outside `{{ }}`; expressions stay verbatim
/// because the template compiler reads them unescaped.
fn escape_outside_mustache(value: &str, replacements: &[(&str, &str)]) -> String {
    let replace = |text: &str| replacements.iter().fold(text.to_string(), |text, (from, to)| text.replace(from, to));
    let mut result = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(open) = rest.find("{{") {
        result.push_str(&replace(&rest[..open]));
        let Some(close) = rest[open + 2..].find("}}") else {
            result.push_str(&replace(&rest[open..]));
            return result;
        };
        let close_end = open + 2 + close + 2;
        result.push_str(&rest[open..close_end]);
        rest = &rest[close_end..];
    }
    result.push_str(&replace(rest));
    result
}

pub fn escape_text(value: &str) -> String {
    escape_outside_mustache(value, &[("&", "&amp;"), ("<", "&lt;")])
}

pub fn escape_attribute(value: &str) -> String {
    // Expressions quote strings with ', so only " needs escaping inside them.
    escape_outside_mustache(value, &[("&", "&amp;"), ("<", "&lt;"), ("\"", "&quot;")]).replace('"', "&quot;")
}
