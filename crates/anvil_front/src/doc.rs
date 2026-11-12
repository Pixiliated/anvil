//! Doc comments. The `//` lines right above an item (above its `allow` lines), with no blank line
//! between, are its documentation: the tools show it with the item's signature. A doc line
//! `// example: EXPR` is also a test of the file, which passes when EXPR is true, so the examples
//! a reader is shown cannot go stale.

use crate::ast::{self, Arg, Block, Expr, ExprKind, Ident, Item, Stmt, StmtKind, TestDecl};
use crate::diag::Diagnostic;
use crate::span::{SourceFile, Span};

/// What a doc line starts with to be an example.
pub const EXAMPLE: &str = "example:";

/// The doc lines above the line holding `offset`: each one's text without `//` (and one space
/// after it), with the offset where that text starts, top to bottom.
fn lines_above(text: &str, offset: usize) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut line_start = text[..offset].rfind('\n').map_or(0, |i| i + 1);
    while line_start > 0 {
        let prev_start = text[..line_start - 1].rfind('\n').map_or(0, |i| i + 1);
        let line = text[prev_start..line_start - 1].trim_end_matches('\r');
        let trimmed = line.trim_start();
        if trimmed.starts_with("allow ") {
            line_start = prev_start;
            continue;
        }
        let Some(rest) = trimmed.strip_prefix("//") else { break };
        let rest = rest.strip_prefix(' ').unwrap_or(rest);
        let at = prev_start + (line.len() - rest.len());
        out.push((at, rest));
        line_start = prev_start;
    }
    out.reverse();
    out
}

/// The documentation of the item declared at `offset` (any offset on the line of its name), or
/// None when no comment lines sit right above it.
pub fn doc_above(text: &str, offset: usize) -> Option<String> {
    let lines = lines_above(text, offset);
    if lines.is_empty() {
        return None;
    }
    let joined: Vec<&str> = lines.iter().map(|(_, l)| l.trim_end()).collect();
    Some(joined.join("\n"))
}

/// The examples in the docs of a file's functions, as tests named after the function and line.
/// Each is `assert(EXPR)` with EXPR's spans in the comment, so errors and panics point there.
pub fn examples(src: &SourceFile, file: &ast::File) -> (Vec<TestDecl>, Vec<Diagnostic>) {
    let mut tests = Vec::new();
    let mut errors = Vec::new();
    for item in &file.items {
        let Item::Fn(d) = item else { continue };
        let name = match &d.receiver {
            Some(r) => format!("{}.{}", r.ty.name, d.name.name),
            None => d.name.name.clone(),
        };
        for (at, line) in lines_above(&src.text, d.name.span.start) {
            let Some(rest) = line.strip_prefix(EXAMPLE) else { continue };
            let expr_text = rest.trim();
            let start = at + EXAMPLE.len() + (rest.len() - rest.trim_start().len());
            let span = Span::new(start, start + expr_text.len());
            if expr_text.is_empty() {
                errors.push(Diagnostic::new("E0100", span, "an example needs an expression after `example:`"));
                continue;
            }
            let expr =
                match crate::lexer::lex_range(&src.text, span.start, span.end).and_then(crate::parser::parse_expr) {
                    Ok(e) => e,
                    Err(d) => {
                        errors.push(d);
                        continue;
                    }
                };
            let (line_no, _) = src.line_col(span.start);
            let call = Expr {
                kind: ExprKind::Call {
                    callee: Ident { name: "assert".into(), span },
                    args: vec![Arg { name: None, value: expr }],
                },
                span,
            };
            tests.push(TestDecl {
                name: format!("example of {name} on line {line_no}"),
                name_span: span,
                inputs: Vec::new(),
                uses: Vec::new(),
                uses_span: None,
                uses_insert: span.at_end(),
                body: Block { stmts: vec![Stmt { kind: StmtKind::Expr(call), span, allows: Vec::new() }], span },
                span,
            });
        }
    }
    (tests, errors)
}
