use crate::diag::{Diagnostic, FixKind};
use crate::span::Span;

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Ident(String),
    Int(i64),
    Float(f64),
    Str(String),
    // keywords
    Fn,
    Let,
    Var,
    If,
    Else,
    While,
    For,
    In,
    Return,
    Break,
    Continue,
    Struct,
    Unit,
    Test,
    Requires,
    Ensures,
    Uses,
    True,
    False,
    Result,
    // punctuation
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Colon,
    Semi,
    Dot,
    DotDot,
    Arrow,
    Assign,
    EqEq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Caret,
    Bang,
    AndAnd,
    OrOr,
    Eof,
}

impl Tok {
    pub fn describe(&self) -> String {
        match self {
            Tok::Ident(s) => format!("identifier `{s}`"),
            Tok::Int(v) => format!("integer `{v}`"),
            Tok::Float(v) => format!("float `{v}`"),
            Tok::Str(_) => "string literal".into(),
            Tok::Eof => "end of file".into(),
            other => format!("`{}`", other.text()),
        }
    }

    pub fn text(&self) -> &'static str {
        match self {
            Tok::Fn => "fn",
            Tok::Let => "let",
            Tok::Var => "var",
            Tok::If => "if",
            Tok::Else => "else",
            Tok::While => "while",
            Tok::For => "for",
            Tok::In => "in",
            Tok::Return => "return",
            Tok::Break => "break",
            Tok::Continue => "continue",
            Tok::Struct => "struct",
            Tok::Unit => "unit",
            Tok::Test => "test",
            Tok::Requires => "requires",
            Tok::Ensures => "ensures",
            Tok::Uses => "uses",
            Tok::True => "true",
            Tok::False => "false",
            Tok::Result => "result",
            Tok::LParen => "(",
            Tok::RParen => ")",
            Tok::LBrace => "{",
            Tok::RBrace => "}",
            Tok::LBracket => "[",
            Tok::RBracket => "]",
            Tok::Comma => ",",
            Tok::Colon => ":",
            Tok::Semi => ";",
            Tok::Dot => ".",
            Tok::DotDot => "..",
            Tok::Arrow => "->",
            Tok::Assign => "=",
            Tok::EqEq => "==",
            Tok::NotEq => "!=",
            Tok::Lt => "<",
            Tok::Le => "<=",
            Tok::Gt => ">",
            Tok::Ge => ">=",
            Tok::Plus => "+",
            Tok::Minus => "-",
            Tok::Star => "*",
            Tok::Slash => "/",
            Tok::Percent => "%",
            Tok::Caret => "^",
            Tok::Bang => "!",
            Tok::AndAnd => "&&",
            Tok::OrOr => "||",
            Tok::Ident(_) | Tok::Int(_) | Tok::Float(_) | Tok::Str(_) | Tok::Eof => "",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
}

fn keyword(s: &str) -> Option<Tok> {
    Some(match s {
        "fn" => Tok::Fn,
        "let" => Tok::Let,
        "var" => Tok::Var,
        "if" => Tok::If,
        "else" => Tok::Else,
        "while" => Tok::While,
        "for" => Tok::For,
        "in" => Tok::In,
        "return" => Tok::Return,
        "break" => Tok::Break,
        "continue" => Tok::Continue,
        "struct" => Tok::Struct,
        "unit" => Tok::Unit,
        "test" => Tok::Test,
        "requires" => Tok::Requires,
        "ensures" => Tok::Ensures,
        "uses" => Tok::Uses,
        "true" => Tok::True,
        "false" => Tok::False,
        "result" => Tok::Result,
        _ => return None,
    })
}

pub fn lex(text: &str) -> Result<Vec<Token>, Diagnostic> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b' ' || c == b'\t' || c == b'\n' || c == b'\r' {
            i += 1;
            continue;
        }
        if c == b'/' && bytes.get(i + 1) == Some(&b'/') {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let start = i;
        if c.is_ascii_alphabetic() || c == b'_' {
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &text[start..i];
            let tok = keyword(word).unwrap_or_else(|| Tok::Ident(word.to_string()));
            out.push(Token { tok, span: Span::new(start, i) });
            continue;
        }
        if c.is_ascii_digit() {
            out.push(lex_number(text, &mut i)?);
            continue;
        }
        if c == b'"' {
            i += 1;
            let mut s = String::new();
            loop {
                match bytes.get(i) {
                    None | Some(b'\n') => {
                        // No fix: where the string should end is a guess, and a wrong guess compiles.
                        return Err(Diagnostic::new("E0002", Span::new(start, i), "unterminated string literal"));
                    }
                    Some(b'"') => {
                        i += 1;
                        break;
                    }
                    Some(b'\\') => {
                        let esc = match bytes.get(i + 1) {
                            Some(b'n') => '\n',
                            Some(b't') => '\t',
                            Some(b'"') => '"',
                            Some(b'\\') => '\\',
                            _ => {
                                return Err(Diagnostic::new(
                                    "E0002",
                                    Span::new(i, (i + 2).min(bytes.len())),
                                    "unknown escape; valid escapes are \\n \\t \\\" \\\\",
                                ))
                            }
                        };
                        s.push(esc);
                        i += 2;
                    }
                    Some(_) => {
                        let ch = text[i..].chars().next().unwrap();
                        s.push(ch);
                        i += ch.len_utf8();
                    }
                }
            }
            out.push(Token { tok: Tok::Str(s), span: Span::new(start, i) });
            continue;
        }
        let two = if i + 1 < bytes.len() { &text[i..i + 2] } else { "" };
        let tok2 = match two {
            ".." => Some(Tok::DotDot),
            "->" => Some(Tok::Arrow),
            "==" => Some(Tok::EqEq),
            "!=" => Some(Tok::NotEq),
            "<=" => Some(Tok::Le),
            ">=" => Some(Tok::Ge),
            "&&" => Some(Tok::AndAnd),
            "||" => Some(Tok::OrOr),
            _ => None,
        };
        if let Some(tok) = tok2 {
            i += 2;
            out.push(Token { tok, span: Span::new(start, i) });
            continue;
        }
        let tok = match c {
            b'(' => Tok::LParen,
            b')' => Tok::RParen,
            b'{' => Tok::LBrace,
            b'}' => Tok::RBrace,
            b'[' => Tok::LBracket,
            b']' => Tok::RBracket,
            b',' => Tok::Comma,
            b':' => Tok::Colon,
            b';' => Tok::Semi,
            b'.' => Tok::Dot,
            b'=' => Tok::Assign,
            b'<' => Tok::Lt,
            b'>' => Tok::Gt,
            b'+' => Tok::Plus,
            b'-' => Tok::Minus,
            b'*' => Tok::Star,
            b'/' => Tok::Slash,
            b'%' => Tok::Percent,
            b'^' => Tok::Caret,
            b'!' => Tok::Bang,
            _ => {
                let ch = text[i..].chars().next().unwrap();
                let span = Span::new(i, i + ch.len_utf8());
                let d = Diagnostic::new("E0001", span, format!("unexpected character `{ch}`"));
                return Err(match ch {
                    '&' => d.fix1(FixKind::ChangesMeaning, "logical and is `&&`", span, "&&"),
                    '|' => d.fix1(FixKind::ChangesMeaning, "logical or is `||`", span, "||"),
                    _ => d,
                });
            }
        };
        i += 1;
        out.push(Token { tok, span: Span::new(start, i) });
    }
    out.push(Token { tok: Tok::Eof, span: Span::new(text.len(), text.len()) });
    Ok(out)
}

fn lex_number(text: &str, i: &mut usize) -> Result<Token, Diagnostic> {
    let bytes = text.as_bytes();
    let start = *i;
    let digits = |i: &mut usize| {
        while *i < bytes.len() && (bytes[*i].is_ascii_digit() || bytes[*i] == b'_') {
            *i += 1;
        }
    };
    digits(i);
    let mut is_float = false;
    // `1..5` is a range, not a float.
    if bytes.get(*i) == Some(&b'.') && bytes.get(*i + 1) != Some(&b'.') {
        is_float = true;
        *i += 1;
        if !bytes.get(*i).is_some_and(|b| b.is_ascii_digit()) {
            let span = Span::new(start, *i);
            return Err(Diagnostic::new("E0003", span, "a float needs digits after the point").fix1(
                FixKind::Mechanical,
                "add a zero",
                span.at_end(),
                "0",
            ));
        }
        digits(i);
    }
    if matches!(bytes.get(*i), Some(b'e') | Some(b'E')) {
        is_float = true;
        *i += 1;
        if matches!(bytes.get(*i), Some(b'+') | Some(b'-')) {
            *i += 1;
        }
        if !bytes.get(*i).is_some_and(|b| b.is_ascii_digit()) {
            return Err(Diagnostic::new("E0003", Span::new(start, *i), "exponent needs digits"));
        }
        digits(i);
    }
    let span = Span::new(start, *i);
    if bytes.get(*i).is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_') {
        let mut j = *i;
        while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
            j += 1;
        }
        return Err(Diagnostic::new("E0003", Span::new(start, j), "number literal followed by letters")
            .note("units on floats are written in brackets: 9.81[m/s^2]", None));
    }
    let clean: String = text[start..*i].chars().filter(|c| *c != '_').collect();
    if is_float {
        let v: f64 = clean.parse().map_err(|_| Diagnostic::new("E0003", span, "invalid float literal"))?;
        if !v.is_finite() {
            return Err(Diagnostic::new("E0003", span, "float literal is too large to represent"));
        }
        Ok(Token { tok: Tok::Float(v), span })
    } else {
        let v: i64 =
            clean.parse().map_err(|_| Diagnostic::new("E0003", span, "integer literal does not fit in i64"))?;
        Ok(Token { tok: Tok::Int(v), span })
    }
}
