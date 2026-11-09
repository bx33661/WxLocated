//! A token-based JavaScript pretty printer: one statement per line, four-space
//! indentation, spaced binary operators. It never reorders or drops tokens.

use super::lexer::{Kind, Token, previous_significant, tokenize};
use crate::text::has_suffix;

const MAXIMUM_FORMATTED_BYTES: usize = 8 * 1024 * 1024;
const OPERATOR_BYTES: &[u8] = b"=+-*%<>!&|?^~/.";
const CONTROL_KEYWORDS: &[&str] = &["if", "for", "while", "switch", "catch", "with"];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Context {
    Parenthesis,
    Bracket,
    Block,
    Object,
}

struct Writer {
    output: String,
    indentation: usize,
    line_start: bool,
}

impl Writer {
    fn indent_if_needed(&mut self) {
        if self.line_start {
            for _ in 0..self.indentation {
                self.output.push_str("    ");
            }
            self.line_start = false;
        }
    }

    fn trim_trailing_whitespace(&mut self) {
        while self.output.ends_with(' ') || self.output.ends_with('\t') {
            self.output.pop();
        }
    }

    fn newline(&mut self) {
        self.trim_trailing_whitespace();
        if !has_suffix(&self.output, "\n") {
            self.output.push('\n');
        }
        self.line_start = true;
    }

    fn space(&mut self) {
        if self.line_start || self.output.ends_with(' ') || has_suffix(&self.output, "\n") {
            return;
        }
        self.output.push(' ');
    }
}

pub fn format(source: &str) -> String {
    if source.len() > MAXIMUM_FORMATTED_BYTES {
        return if has_suffix(source, "\n") { source.to_string() } else { format!("{source}\n") };
    }
    let bytes = source.as_bytes();
    let tokens = tokenize(bytes);
    if tokens.is_empty() {
        return source.to_string();
    }

    let mut writer =
        Writer { output: String::with_capacity((source.len() * 2).min(MAXIMUM_FORMATTED_BYTES * 2)), indentation: 0, line_start: true };
    let mut contexts: Vec<Context> = Vec::new();
    let mut parenthesis_depth = 0usize;
    let mut pending_ternary = 0usize;
    let mut index = 0;

    while index < tokens.len() {
        let token = &tokens[index];
        let raw = token.text(bytes);
        let previous = previous_significant(&tokens, index).map(|i| &tokens[i]);

        if let Kind::Punctuator(byte) = token.kind
            && OPERATOR_BYTES.contains(&byte)
        {
            let cluster_end = operator_cluster_end(index, &tokens);
            let cluster = String::from_utf8_lossy(&bytes[token.start..tokens[cluster_end].end]).into_owned();
            let unary = is_unary_operator(&cluster, previous, previous.map(|p| p.text(bytes)).as_deref());
            let attached = matches!(cluster.as_str(), "." | "?." | "..." | "++" | "--") || unary;
            writer.indent_if_needed();
            if !attached {
                writer.space();
            }
            writer.output.push_str(&cluster);
            if !attached {
                writer.space();
            }
            if cluster == "?" {
                pending_ternary += 1;
            }
            index = cluster_end + 1;
            continue;
        }

        match token.kind {
            Kind::Comment => {
                if raw.starts_with("//") {
                    writer.space();
                    writer.indent_if_needed();
                    writer.output.push_str(&raw);
                    writer.newline();
                } else {
                    writer.space();
                    writer.indent_if_needed();
                    writer.output.push_str(&raw);
                    if crate::text::contains(&raw, "\n") || next_significant_is_statement(index, &tokens) {
                        writer.newline();
                    } else {
                        writer.space();
                    }
                }
            }
            Kind::Punctuator(b'{') => {
                let context = if is_block_opening(index, &tokens) { Context::Block } else { Context::Object };
                writer.indent_if_needed();
                if context == Context::Block || should_space_before_object_brace(previous) {
                    writer.space();
                }
                writer.output.push('{');
                contexts.push(context);
                writer.indentation += 1;
                if next_significant_kind(index, &tokens) != Some(&Kind::Punctuator(b'}')) {
                    writer.newline();
                }
            }
            Kind::Punctuator(b'}') => {
                let context = pop_context(&mut contexts, &[Context::Block, Context::Object]).unwrap_or(Context::Block);
                writer.indentation = writer.indentation.saturating_sub(1);
                if !writer.line_start {
                    writer.newline();
                }
                writer.indent_if_needed();
                writer.output.push('}');
                if context == Context::Block && !next_token_can_continue_statement(&tokens, index) {
                    writer.newline();
                }
            }
            Kind::Punctuator(b'(') => {
                writer.indent_if_needed();
                if let Some(Kind::Identifier(value)) = previous.map(|p| &p.kind)
                    && (CONTROL_KEYWORDS.contains(&value.as_str())
                        || matches!(value.as_str(), "function" | "return" | "throw" | "yield" | "await"))
                {
                    writer.space();
                }
                writer.output.push('(');
                contexts.push(Context::Parenthesis);
                parenthesis_depth += 1;
            }
            Kind::Punctuator(b')') => {
                writer.trim_trailing_whitespace();
                writer.indent_if_needed();
                writer.output.push(')');
                pop_context(&mut contexts, &[Context::Parenthesis]);
                parenthesis_depth = parenthesis_depth.saturating_sub(1);
            }
            Kind::Punctuator(b'[') => {
                writer.indent_if_needed();
                writer.output.push('[');
                contexts.push(Context::Bracket);
            }
            Kind::Punctuator(b']') => {
                writer.trim_trailing_whitespace();
                writer.indent_if_needed();
                writer.output.push(']');
                pop_context(&mut contexts, &[Context::Bracket]);
            }
            Kind::Punctuator(b';') => {
                writer.trim_trailing_whitespace();
                writer.indent_if_needed();
                writer.output.push(';');
                if parenthesis_depth == 0 {
                    writer.newline();
                } else {
                    writer.space();
                }
            }
            Kind::Punctuator(b',') => {
                writer.trim_trailing_whitespace();
                writer.indent_if_needed();
                writer.output.push(',');
                if contexts.last() == Some(&Context::Object) {
                    writer.newline();
                } else {
                    writer.space();
                }
            }
            Kind::Punctuator(b':') => {
                writer.trim_trailing_whitespace();
                writer.indent_if_needed();
                if pending_ternary > 0 {
                    writer.space();
                    pending_ternary -= 1;
                }
                writer.output.push(':');
                writer.space();
            }
            _ => {
                writer.indent_if_needed();
                if let Some(previous) = previous
                    && requires_space(previous, token)
                {
                    writer.space();
                }
                writer.output.push_str(&raw);
            }
        }
        index += 1;
    }

    writer.trim_trailing_whitespace();
    while has_suffix(&writer.output, "\n\n") {
        writer.output.pop();
    }
    if !has_suffix(&writer.output, "\n") {
        writer.output.push('\n');
    }
    writer.output
}

fn operator_cluster_end(start: usize, tokens: &[Token]) -> usize {
    let mut end = start;
    while end + 1 < tokens.len()
        && tokens[end].end == tokens[end + 1].start
        && matches!(tokens[end + 1].kind, Kind::Punctuator(value) if OPERATOR_BYTES.contains(&value))
    {
        end += 1;
    }
    end
}

fn is_unary_operator(value: &str, previous: Option<&Token>, previous_raw: Option<&str>) -> bool {
    if value == "!" || value == "~" {
        return true;
    }
    if value != "+" && value != "-" {
        return false;
    }
    let Some(previous) = previous else { return true };
    match &previous.kind {
        Kind::Identifier(keyword) => {
            matches!(keyword.as_str(), "return" | "throw" | "case" | "typeof" | "void" | "delete" | "yield" | "await")
        }
        Kind::Punctuator(byte) => {
            !matches!(byte, b')' | b']' | b'}') && previous_raw != Some("++") && previous_raw != Some("--")
        }
        _ => false,
    }
}

fn should_space_before_object_brace(previous: Option<&Token>) -> bool {
    matches!(
        previous.and_then(Token::identifier),
        Some("const" | "let" | "var" | "import" | "export" | "return" | "yield" | "default")
    )
}

fn is_block_opening(index: usize, tokens: &[Token]) -> bool {
    let Some(previous_index) = previous_significant(tokens, index) else { return true };
    let previous = &tokens[previous_index];
    if previous.is_punctuator(b')') || previous.is_punctuator(b'}') {
        return true;
    }
    if matches!(previous.identifier(), Some("else" | "try" | "finally" | "do")) {
        return true;
    }
    if previous.is_punctuator(b'>')
        && let Some(before) = previous_significant(tokens, previous_index)
        && tokens[before].is_punctuator(b'=')
        && tokens[before].end == previous.start
    {
        return true;
    }

    let mut cursor = previous_index;
    for _ in 0..24 {
        if matches!(
            tokens[cursor].identifier(),
            Some("function" | "class" | "switch" | "try" | "catch" | "finally" | "else" | "do")
        ) {
            return true;
        }
        if b";{}=:".iter().any(|&byte| tokens[cursor].is_punctuator(byte)) {
            break;
        }
        let Some(next) = previous_significant(tokens, cursor) else { break };
        cursor = next;
    }
    false
}

fn requires_space(lhs: &Token, rhs: &Token) -> bool {
    if is_word(&lhs.kind) && is_word(&rhs.kind) {
        return true;
    }
    if matches!(
        lhs.identifier(),
        Some("return" | "throw" | "new" | "typeof" | "void" | "delete" | "yield" | "await" | "case" | "in" | "of" | "instanceof")
    ) {
        return true;
    }
    matches!(rhs.identifier(), Some("else" | "catch" | "finally" | "in" | "of" | "instanceof"))
}

fn is_word(kind: &Kind) -> bool {
    matches!(kind, Kind::Identifier(_) | Kind::String(_) | Kind::Number | Kind::Template | Kind::Regex)
}

fn next_significant_kind(index: usize, tokens: &[Token]) -> Option<&Kind> {
    tokens[index + 1..].iter().find(|token| !token.is_comment()).map(|token| &token.kind)
}

fn next_significant_is_statement(index: usize, tokens: &[Token]) -> bool {
    match next_significant_kind(index, tokens) {
        None => false,
        Some(Kind::Punctuator(value)) => !matches!(value, b')' | b']' | b'}' | b',' | b';'),
        Some(_) => true,
    }
}

fn pop_context(contexts: &mut Vec<Context>, accepted: &[Context]) -> Option<Context> {
    let index = contexts.iter().rposition(|context| accepted.contains(context))?;
    Some(contexts.remove(index))
}

fn next_token_can_continue_statement(tokens: &[Token], current: usize) -> bool {
    for token in &tokens[current + 1..] {
        match &token.kind {
            Kind::Comment => continue,
            Kind::Punctuator(value) => return matches!(value, b')' | b']' | b',' | b';' | b'.' | b':'),
            Kind::Identifier(value) => return matches!(value.as_str(), "else" | "catch" | "finally" | "while"),
            _ => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Traced by hand through the Swift rules (note `return-a`: a unary
    /// operator is attached on both sides, including after a keyword).
    #[test]
    fn formats_statements_and_blocks() {
        assert_eq!(
            format("function f(a){if(a>1){return -a}else{return {x:1,y:[2,3]}}}"),
            "function f(a) {\n    if (a > 1) {\n        return-a\n    } else {\n        return {\n            x: 1,\n            y: [2, 3]\n        }\n    }\n}\n"
        );
    }

    #[test]
    fn ternary_spacing() {
        assert_eq!(format("var a=b?c:d;"), "var a = b ? c : d;\n");
    }
}
