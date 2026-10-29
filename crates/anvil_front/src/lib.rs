//! Anvil front end: lexing, parsing, checking. Produces the typed IR both backends consume.

pub mod ast;
pub mod check;
pub mod diag;
pub mod ir;
pub mod lexer;
pub mod parser;
pub mod span;
pub mod units;

use diag::Diagnostic;
use span::SourceFile;

/// Source text to checked program, or every error found.
pub fn compile(src: &SourceFile) -> Result<ir::Program, Vec<Diagnostic>> {
    let toks = lexer::lex(&src.text).map_err(|d| vec![d])?;
    let file = parser::parse(toks).map_err(|d| vec![d])?;
    check::check(src, &file)
}

/// Parses `src`; the syntax tree, or the first syntax error.
pub fn parse(src: &SourceFile) -> Result<ast::File, Diagnostic> {
    parser::parse(lexer::lex(&src.text)?)
}

/// Everything the checker learns, even from a file with type errors. A syntax error stops
/// before checking, so it comes back alone with empty facts.
pub fn analyze(src: &SourceFile) -> check::Analysis {
    match parse(src) {
        Ok(file) => check::analyze(src, &file),
        Err(d) => check::Analysis { program: None, diags: vec![d], facts: check::Facts::default() },
    }
}
