//! Anvil front end: lexing, parsing, checking. Produces the typed IR both backends consume.

pub mod ast;
pub mod check;
pub mod diag;
pub mod fmt;
pub mod ir;
pub mod lexer;
pub mod parser;
pub mod span;
pub mod units;

use diag::Diagnostic;
use span::SourceFile;

/// Source text to checked program, or every error found.
pub fn compile(src: &SourceFile) -> Result<ir::Program, Vec<Diagnostic>> {
    let file = parse(src)?;
    check::check(src, &file)
}

/// Parses `src`; the syntax tree, or the syntax errors (a lexical error stops at once; the
/// parser reports the first error of each item).
pub fn parse(src: &SourceFile) -> Result<ast::File, Vec<Diagnostic>> {
    let toks = lexer::lex(&src.text).map_err(|d| vec![d])?;
    match parser::parse(toks) {
        (file, errors) if errors.is_empty() => Ok(file),
        (_, errors) => Err(errors),
    }
}

/// Everything the checker learns, even from a file with type errors. Syntax errors stop
/// before checking, so they come back alone with empty facts. A file that checks cleanly must
/// also be in canonical layout (E0110, see `fmt`).
pub fn analyze(src: &SourceFile) -> check::Analysis {
    match parse(src) {
        Ok(file) => check::analyze(src, &file),
        Err(diags) => check::Analysis { program: None, diags, facts: check::Facts::default() },
    }
}
