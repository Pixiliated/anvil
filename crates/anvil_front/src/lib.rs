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
