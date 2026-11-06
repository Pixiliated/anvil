use crate::diag::{Diagnostic, FixKind};
use crate::span::Span;

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Ident(String),
    Int(i64),
    Float(f64),
    Str(String),
    /// A string with `{expr}` parts; each code part is its own token list ending in Eof.
    Interp(Vec<LexPart>),
    // keywords
    Allow,
    Import,
    Pub,
    Fn,
    Enum,
    Match,
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
    Noalloc,
    Extern,
    Interface,
    SelfValue,
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
    FatArrow,
    Question,
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
    Amp,
    Pipe,
    Tilde,
    Shl,
    Shr,
    Eof,
}

impl Tok {
    pub fn describe(&self) -> String {
        match self {
            Tok::Ident(s) => format!("identifier `{s}`"),
            Tok::Int(v) => format!("integer `{v}`"),
            Tok::Float(v) => format!("float `{v}`"),
            Tok::Str(_) | Tok::Interp(_) => "string literal".into(),
            Tok::Eof => "end of file".into(),
            other => format!("`{}`", other.text()),
        }
    }

    pub fn text(&self) -> &'static str {
        match self {
            Tok::Fn => "fn",
            Tok::Enum => "enum",
            Tok::Match => "match",
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
            Tok::Allow => "allow",
            Tok::Import => "import",
            Tok::Pub => "pub",
            Tok::Requires => "requires",
            Tok::Ensures => "ensures",
            Tok::Uses => "uses",
            Tok::Noalloc => "noalloc",
            Tok::Extern => "extern",
            Tok::Interface => "interface",
            Tok::SelfValue => "self",
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
            Tok::FatArrow => "=>",
            Tok::Question => "?",
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
            Tok::Amp => "&",
            Tok::Pipe => "|",
            Tok::Tilde => "~",
            Tok::Shl => "<<",
            Tok::Shr => ">>",
            Tok::Ident(_) | Tok::Int(_) | Tok::Float(_) | Tok::Str(_) | Tok::Interp(_) | Tok::Eof => "",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum LexPart {
    Lit(String),
    Code(Vec<Token>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
}

/// Every keyword, for completion (a test checks each is one).
pub const KEYWORDS: &[&str] = &[
    "allow",
    "import",
    "pub",
    "fn",
    "enum",
    "match",
    "let",
    "var",
    "if",
    "else",
    "while",
    "for",
    "in",
    "return",
    "break",
    "continue",
    "struct",
    "unit",
    "test",
    "requires",
    "ensures",
    "uses",
    "noalloc",
    "extern",
    "interface",
    "self",
    "true",
    "false",
    "result",
];

pub(crate) fn keyword(s: &str) -> Option<Tok> {
    Some(match s {
        "allow" => Tok::Allow,
        "import" => Tok::Import,
        "pub" => Tok::Pub,
        "fn" => Tok::Fn,
        "enum" => Tok::Enum,
        "match" => Tok::Match,
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
        "noalloc" => Tok::Noalloc,
        "extern" => Tok::Extern,
        "interface" => Tok::Interface,
        "self" => Tok::SelfValue,
        "true" => Tok::True,
        "false" => Tok::False,
        "result" => Tok::Result,
        _ => return None,
    })
}

pub fn lex(text: &str) -> Result<Vec<Token>, Diagnostic> {
    if text.starts_with('\u{feff}') {
        let span = Span::new(0, '\u{feff}'.len_utf8());
        return Err(Diagnostic::new("E0001", span, "the file starts with a byte-order mark")
            .note("some editors and PowerShell 5.1's `Set-Content -Encoding UTF8` write one", None)
            .fix1(FixKind::Mechanical, "remove it", span, ""));
    }
    lex_range(text, 0, text.len())
}

/// Lexes text[start..end]; spans stay relative to the whole text. Used again for `{expr}` parts.
pub(crate) fn lex_range(text: &str, start: usize, end: usize) -> Result<Vec<Token>, Diagnostic> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = start;
    while i < end {
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
            out.push(lex_string(text, &mut i)?);
            continue;
        }
        if c == b'`' {
            out.push(lex_raw(text, &mut i)?);
            continue;
        }
        let two = if i + 1 < end { text.get(i..i + 2).unwrap_or("") } else { "" };
        let tok2 = match two {
            ".." => Some(Tok::DotDot),
            "->" => Some(Tok::Arrow),
            "=>" => Some(Tok::FatArrow),
            "==" => Some(Tok::EqEq),
            "!=" => Some(Tok::NotEq),
            "<=" => Some(Tok::Le),
            ">=" => Some(Tok::Ge),
            "&&" => Some(Tok::AndAnd),
            "||" => Some(Tok::OrOr),
            "<<" => Some(Tok::Shl),
            ">>" => Some(Tok::Shr),
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
            b'?' => Tok::Question,
            b'&' => Tok::Amp,
            b'|' => Tok::Pipe,
            b'~' => Tok::Tilde,
            _ => {
                let ch = text[i..].chars().next().unwrap();
                let span = Span::new(i, i + ch.len_utf8());
                return Err(Diagnostic::new("E0001", span, format!("unexpected character `{ch}`")));
            }
        };
        i += 1;
        out.push(Token { tok, span: Span::new(start, i) });
    }
    out.push(Token { tok: Tok::Eof, span: Span::new(end, end) });
    Ok(out)
}

/// The `//` comments of `text`, each from `//` to the end of its line (not including the line
/// break). Strings are skipped, so `"a // b"` is not a comment. The formatter puts them back.
pub fn comments(text: &str) -> Vec<Span> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                i = string_end(bytes, i).unwrap_or_else(|| text[i..].find('\n').map_or(bytes.len(), |n| i + n));
            }
            b'`' => i = text[i + 1..].find('`').map_or(bytes.len(), |n| i + n + 2),
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                let mut end = text[i..].find('\n').map_or(bytes.len(), |n| i + n);
                if end > i && bytes[end - 1] == b'\r' {
                    end -= 1;
                }
                out.push(Span::new(i, end));
                i = end;
            }
            _ => i += 1,
        }
    }
    out
}

/// Index just past the string literal starting at `start` (a `"`), or None if it does not close
/// on its line. Only finds the end; lex_string reports what is wrong inside.
fn string_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start + 1;
    loop {
        match *bytes.get(i)? {
            b'\n' => return None,
            b'"' => return Some(i + 1),
            b'\\' => i += 2,
            b'{' if bytes.get(i + 1) == Some(&b'{') => i += 2,
            b'{' => {
                i += 1;
                loop {
                    match *bytes.get(i)? {
                        b'\n' => return None,
                        b'"' => i = string_end(bytes, i)?,
                        b'}' => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
            }
            _ => i += 1,
        }
    }
}

/// A string literal starting at `"`. `{expr}` interpolates; `{{` and `}}` are literal braces.
fn lex_string(text: &str, i: &mut usize) -> Result<Token, Diagnostic> {
    let bytes = text.as_bytes();
    let start = *i;
    *i += 1;
    let mut parts = Vec::new();
    let mut s = String::new();
    loop {
        match bytes.get(*i) {
            None | Some(b'\n') => {
                // No fix: where the string should end is a guess, and a wrong guess compiles.
                return Err(Diagnostic::new("E0002", Span::new(start, *i), "unterminated string literal"));
            }
            Some(b'"') => {
                *i += 1;
                break;
            }
            Some(b'\\') => {
                let esc = match bytes.get(*i + 1) {
                    Some(b'n') => '\n',
                    Some(b'r') => '\r',
                    Some(b't') => '\t',
                    Some(b'"') => '"',
                    Some(b'\\') => '\\',
                    _ => {
                        return Err(Diagnostic::new(
                            "E0002",
                            Span::new(*i, (*i + 2).min(bytes.len())),
                            "unknown escape; valid escapes are \\n \\r \\t \\\" \\\\",
                        ))
                    }
                };
                s.push(esc);
                *i += 2;
            }
            Some(b'{') if bytes.get(*i + 1) == Some(&b'{') => {
                s.push('{');
                *i += 2;
            }
            Some(b'}') if bytes.get(*i + 1) == Some(&b'}') => {
                s.push('}');
                *i += 2;
            }
            Some(b'}') => {
                let span = Span::new(*i, *i + 1);
                return Err(Diagnostic::new("E0004", span, "a `}` in a string is written `}}`").fix1(
                    FixKind::Mechanical,
                    "double it",
                    span,
                    "}}",
                ));
            }
            Some(b'{') => {
                let open = *i;
                let mut j = open + 1;
                let unclosed = |j: usize| {
                    let d = Diagnostic::new(
                        "E0004",
                        Span::new(open, j),
                        "`{` in a string starts an interpolation that must close with `}` on the same line, \
                         with no braces inside",
                    )
                    .note("write `{{` for a literal `{`", None);
                    // With no `}` on the line it cannot be an interpolation.
                    let line = text[open..].split('\n').next().unwrap_or("");
                    if line.contains('}') {
                        return d;
                    }
                    d.fix1(FixKind::Mechanical, "double it", Span::new(open, open + 1), "{{")
                };
                loop {
                    match bytes.get(j) {
                        Some(b'}') => break,
                        // A string inside the expression, as in "{f("x")}"; it may interpolate too.
                        Some(b'"') => j = string_end(bytes, j).ok_or_else(|| unclosed(j))?,
                        None | Some(b'\n') | Some(b'{') => return Err(unclosed(j)),
                        Some(_) => j += 1,
                    }
                }
                if text[open + 1..j].trim().is_empty() {
                    return Err(Diagnostic::new("E0004", Span::new(open, j + 1), "empty `{}` in a string")
                        .note("put an expression inside, or write `{{}}` for literal braces", None));
                }
                if !s.is_empty() {
                    parts.push(LexPart::Lit(std::mem::take(&mut s)));
                }
                parts.push(LexPart::Code(lex_range(text, open + 1, j)?));
                *i = j + 1;
            }
            Some(_) => {
                let ch = text[*i..].chars().next().unwrap();
                s.push(ch);
                *i += ch.len_utf8();
            }
        }
    }
    let span = Span::new(start, *i);
    if parts.is_empty() {
        return Ok(Token { tok: Tok::Str(s), span });
    }
    if !s.is_empty() {
        parts.push(LexPart::Lit(s));
    }
    Ok(Token { tok: Tok::Interp(parts), span })
}

/// `` `text` ``: exactly the characters between the backticks, line breaks included (a CRLF as a
/// line feed, so the value does not depend on how the file was saved).
fn lex_raw(text: &str, i: &mut usize) -> Result<Token, Diagnostic> {
    let start = *i;
    let Some(n) = text[start + 1..].find('`') else {
        return Err(Diagnostic::new("E0002", Span::new(start, start + 1), "unterminated raw string: no closing backtick"));
    };
    *i = start + n + 2;
    let s = text[start + 1..start + 1 + n].replace("\r\n", "\n");
    Ok(Token { tok: Tok::Str(s), span: Span::new(start, *i) })
}

fn lex_number(text: &str, i: &mut usize) -> Result<Token, Diagnostic> {
    let bytes = text.as_bytes();
    let start = *i;
    // `0xFF`, `0b1010`: 64 bits written out, read as the i64 with those bits.
    if bytes[start] == b'0' && matches!(bytes.get(start + 1), Some(b'x' | b'X' | b'b' | b'B')) {
        let radix = if matches!(bytes[start + 1], b'x' | b'X') { 16 } else { 2 };
        *i += 2;
        while *i < bytes.len() && ((bytes[*i] as char).is_digit(radix) || bytes[*i] == b'_') {
            *i += 1;
        }
        let span = Span::new(start, *i);
        if bytes.get(*i).is_some_and(|b| b.is_ascii_alphanumeric()) {
            return Err(Diagnostic::new("E0003", span, "this digit does not belong to the literal")
                .note("hex literals use 0-9 and a-f, binary literals 0 and 1", None));
        }
        let clean: String = text[start + 2..*i].chars().filter(|c| *c != '_').collect();
        if clean.is_empty() {
            return Err(Diagnostic::new("E0003", span, "the literal has no digits"));
        }
        let bits = u64::from_str_radix(&clean, radix)
            .map_err(|_| Diagnostic::new("E0003", span, "the literal has more than 64 bits"))?;
        return Ok(Token { tok: Tok::Int(bits as i64), span });
    }
    let digits = |i: &mut usize| {
        while *i < bytes.len() && (bytes[*i].is_ascii_digit() || bytes[*i] == b'_') {
            *i += 1;
        }
    };
    digits(i);
    let mut is_float = false;
    // `1..5` is a range and `6.xor(3)` a method call, not floats.
    let after = bytes.get(*i + 1).copied();
    if bytes.get(*i) == Some(&b'.')
        && after != Some(b'.')
        && !after.is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
    {
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
