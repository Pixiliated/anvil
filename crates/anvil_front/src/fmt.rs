//! The canonical layout. `format` prints a file from its syntax tree, so there is one layout per
//! program: four-space indents, one statement per line, `{` at the end of the line that opens it,
//! one blank line between items and at most one between statements. Literals, strings and
//! units are copied from the source (minus spaces inside units), so `1_000` and `2.5e-3` keep
//! their spelling. Comments are kept: on their own line before an item, statement, field,
//! variant or arm, or at the end of the line one ends on. A comment anywhere else (inside an
//! expression or a signature) cannot be placed, and that is an error, never a silent drop.

use crate::ast::*;
use crate::diag::{Diagnostic, FixKind};
use crate::lexer;
use crate::span::{SourceFile, Span};
use std::cell::RefCell;

const INDENT: &str = "    ";
/// Lines longer than this split a statement's list value, one element per line, and keep a
/// match arm with one statement from staying on one line.
const WIDTH: usize = 100;

/// The file in canonical layout, or why it cannot be formatted: its first syntax error, or a
/// comment that has no place in the layout (E0111).
pub fn format(src: &SourceFile) -> Result<String, Diagnostic> {
    let file = crate::parse(src).map_err(|mut d| d.remove(0))?;
    format_file(src, &file)
}

/// E0110 when `src` is not in canonical layout, with the whole-file replacement as a mechanical
/// fix; E0111 when it cannot be formatted. Only meaningful for a file that parses.
pub fn check_layout(src: &SourceFile, file: &File) -> Option<Diagnostic> {
    let formatted = match format_file(src, file) {
        Ok(f) => f,
        Err(d) => return Some(d),
    };
    if formatted == src.text {
        return None;
    }
    // Point at the first line that differs.
    let (mut offset, mut line) = (0, 1);
    for (a, b) in src.text.split_inclusive('\n').zip(formatted.split_inclusive('\n')) {
        if a != b {
            break;
        }
        offset += a.len();
        line += 1;
    }
    let end = src.text[offset..].find('\n').map_or(src.text.len(), |n| offset + n);
    let span = Span::new(offset, end.max(offset));
    let whole = Span::new(0, src.text.len());
    Some(
        Diagnostic::new("E0110", span, format!("not in canonical layout; the first difference is on line {line}"))
            .fix1(FixKind::Mechanical, "format the file (anvil fmt)", whole, formatted),
    )
}

pub fn format_file(src: &SourceFile, file: &File) -> Result<String, Diagnostic> {
    let mut p = Printer {
        text: &src.text,
        comments: lexer::comments(&src.text),
        used: RefCell::new(Vec::new()),
        out: String::new(),
        depth: 0,
    };
    p.used = RefCell::new(vec![false; p.comments.len()]);
    p.items(&file.items);
    if let Some(i) = p.used.borrow().iter().position(|u| !u) {
        let span = p.comments[i];
        return Err(Diagnostic::new(
            "E0111",
            span,
            "this comment is inside a statement or signature, where the layout has no place for it",
        )
        .note("put it on its own line before the statement, or at the end of the line the statement ends on", None));
    }
    Ok(p.out)
}

struct Printer<'a> {
    text: &'a str,
    comments: Vec<Span>,
    /// Which comments are printed; shared with the printers of function literals' bodies.
    used: RefCell<Vec<bool>>,
    out: String,
    depth: usize,
}

/// Something printed as one entry of a list (an item, statement, field, variant or arm).
trait Entry {
    fn span(&self) -> Span;
}

impl Printer<'_> {
    fn line(&mut self, text: &str) {
        for _ in 0..self.depth {
            self.out.push_str(INDENT);
        }
        self.out.push_str(text);
        self.out.push('\n');
    }

    fn blank(&mut self) {
        if !self.out.is_empty() && !self.out.ends_with("\n\n") && !self.out.ends_with("{\n") {
            self.out.push('\n');
        }
    }

    /// Whether the source has an empty line between offsets a and b.
    fn blank_between(&self, a: usize, b: usize) -> bool {
        let gap = &self.text[a.min(b)..b];
        let parts: Vec<&str> = gap.split('\n').collect();
        parts.len() >= 3 && parts[1..parts.len() - 1].iter().any(|p| p.trim().is_empty())
    }

    fn comment_text(&self, i: usize) -> &str {
        self.text[self.comments[i].start..self.comments[i].end].trim_end()
    }

    /// Prints the unused comments that start in `from..to` on their own lines; returns where the
    /// last one ended (or `from`).
    fn own_line_comments(&mut self, from: usize, to: usize, first: bool) -> usize {
        let mut prev = from;
        let mut first = first;
        for i in 0..self.comments.len() {
            let c = self.comments[i];
            if self.used.borrow()[i] || c.start < from || c.start >= to {
                continue;
            }
            if !first && self.blank_between(prev, c.start) {
                self.blank();
            }
            let t = self.comment_text(i).to_string();
            self.line(&t);
            self.used.borrow_mut()[i] = true;
            prev = c.end;
            first = false;
        }
        prev
    }

    /// The unused comment starting after `end` on the same source line, appended to the last line.
    fn trailing_comment(&mut self, end: usize) -> usize {
        let line_end = self.text[end..].find('\n').map_or(self.text.len(), |n| end + n);
        for i in 0..self.comments.len() {
            let c = self.comments[i];
            if !self.used.borrow()[i] && c.start >= end && c.start < line_end {
                let t = self.comment_text(i).to_string();
                self.out.pop();
                self.out.push(' ');
                self.out.push_str(&t);
                self.out.push('\n');
                self.used.borrow_mut()[i] = true;
                return c.end;
            }
        }
        end
    }

    /// A list of entries between `open` (just after `{`, or 0) and `close` (the `}`, or the end
    /// of the file), with the comments around them. `gap(prev, next)` says whether a blank line
    /// must separate two entries; otherwise the source's blank lines (at most one) are kept.
    fn entries<E: Entry>(
        &mut self,
        list: &[E],
        open: usize,
        close: usize,
        gap: impl Fn(&E, &E) -> bool,
        mut print: impl FnMut(&mut Self, &E),
    ) {
        let mut prev = open;
        for (n, e) in list.iter().enumerate() {
            let start = e.span().start;
            let forced = n > 0 && gap(&list[n - 1], e);
            if forced {
                self.blank();
            }
            let had_comments = self
                .comments
                .iter()
                .zip(self.used.borrow().iter())
                .any(|(c, u)| !u && c.start >= prev && c.start < start);
            let after = self.own_line_comments(prev, start, n == 0 && !forced);
            if n > 0 && !forced && !had_comments && self.blank_between(prev, start) {
                self.blank();
            }
            if had_comments && self.blank_between(after, start) {
                self.blank();
            }
            print(self, e);
            prev = self.trailing_comment(e.span().end);
        }
        self.own_line_comments(prev, close, list.is_empty());
    }

    // ---- items ----

    fn items(&mut self, items: &[Item]) {
        let together =
            |a: &Item, b: &Item| matches!((a, b), (Item::Unit(_), Item::Unit(_)) | (Item::Import(_), Item::Import(_)));
        self.entries(items, 0, self.text.len(), |a, b| !together(a, b), |p, item| p.item(item));
    }

    fn item(&mut self, item: &Item) {
        match item {
            Item::Import(i) => {
                // Names in a canonical order, so two imports of the same names read alike.
                let mut names: Vec<&str> = i.names.iter().map(|n| n.name.as_str()).collect();
                names.sort();
                self.line(&format!("import {}.{{{}}};", i.module(), names.join(", ")));
            }
            Item::Unit(u) => {
                let line = match &u.def {
                    Some(d) => format!("{}unit {} = {};", vis(u.public), u.name.name, self.squash(d.span)),
                    None => format!("{}unit {};", vis(u.public), u.name.name),
                };
                self.line(&line);
            }
            Item::Struct(s) => {
                let head = format!("{}struct {}{}", vis(s.public), s.name.name, type_params(&s.type_params));
                self.fields_block(&head, &s.fields, s.span);
            }
            Item::Enum(e) => {
                let open = self.after_brace(e.name.span.end);
                let head = format!("{}enum {}{}", vis(e.public), e.name.name, type_params(&e.type_params));
                if e.variants.is_empty() && !self.has_comments(open, e.span.end) {
                    self.line(&format!("{head} {{}}"));
                    return;
                }
                self.line(&format!("{head} {{"));
                self.depth += 1;
                let close = e.span.end - 1;
                self.entries(
                    &e.variants,
                    open,
                    close,
                    |_, _| false,
                    |p, v| {
                        let text = if v.fields.is_empty() {
                            format!("{},", v.name.name)
                        } else {
                            format!("{}({}),", v.name.name, p.field_list(&v.fields))
                        };
                        p.line(&text);
                    },
                );
                self.depth -= 1;
                self.line("}");
            }
            Item::Fn(f) => self.func(f),
            Item::Interface(i) => {
                let head = format!("{}interface {}", vis(i.public), i.name.name);
                let open = self.after_brace(i.name.span.end);
                if i.methods.is_empty() && !self.has_comments(open, i.span.end) {
                    self.line(&format!("{head} {{}}"));
                    return;
                }
                self.line(&format!("{head} {{"));
                self.depth += 1;
                self.entries(
                    &i.methods,
                    open,
                    i.span.end - 1,
                    |_, _| false,
                    |p, m| {
                        let t = format!("{};", p.signature(m));
                        p.line(&t);
                    },
                );
                self.depth -= 1;
                self.line("}");
            }
            Item::Test(t) => {
                let inputs = if t.inputs.is_empty() {
                    String::new()
                } else {
                    let ps: Vec<String> =
                        t.inputs.iter().map(|p| format!("{}: {}", p.name.name, self.ty(&p.ty))).collect();
                    format!(" for all {}", ps.join(", "))
                };
                let head = format!("test {}{inputs}{}", self.slice(t.name_span), uses(&t.uses));
                self.block(&head, &t.body, "");
            }
        }
    }

    /// `struct Name { field: T, ... }`, one field per line.
    fn fields_block(&mut self, head: &str, fields: &[(Ident, TypeExpr)], span: Span) {
        let open = self.after_brace(span.start);
        let close = span.end - 1;
        if fields.is_empty() && !self.has_comments(open, close) {
            self.line(&format!("{head} {{}}"));
            return;
        }
        self.line(&format!("{head} {{"));
        self.depth += 1;
        let entries: Vec<Field> = fields.iter().map(|(n, t)| Field(n.clone(), t.clone())).collect();
        self.entries(
            &entries,
            open,
            close,
            |_, _| false,
            |p, f| {
                let t = p.ty(&f.1);
                p.line(&format!("{}: {t},", f.0.name));
            },
        );
        self.depth -= 1;
        self.line("}");
    }

    fn func(&mut self, f: &FnDecl) {
        for a in &f.allows {
            let line = format!("allow {} {}", a.lint.name, self.slice(a.reason_span));
            self.line(&line);
        }
        let external = match &f.external {
            Some(e) => match &e.source {
                Some((_, span)) => format!("extern {} ", self.slice(*span)),
                None => "extern ".to_string(),
            },
            None => String::new(),
        };
        let head = format!("{}{external}{}", vis(f.public), self.signature(f));
        if f.external.is_some() {
            self.line(&format!("{head};"));
            return;
        }
        if f.requires.is_empty() && f.ensures.is_empty() {
            self.block(&head, &f.body, "");
            return;
        }
        self.line(&head);
        self.depth += 1;
        for e in &f.requires {
            let t = format!("requires {}", self.expr(e));
            self.line(&t);
        }
        for e in &f.ensures {
            let t = format!("ensures {}", self.expr(e));
            self.line(&t);
        }
        self.depth -= 1;
        self.block("", &f.body, "");
    }

    /// `fn name[T](params) -> R uses ... noalloc`; a method's `fn Type[T].name[U](self, ...)`.
    fn signature(&self, f: &FnDecl) -> String {
        let params: Vec<String> = f
            .params
            .iter()
            .map(|p| {
                if p.name.name == "self" {
                    "self".to_string()
                } else {
                    format!("{}: {}", p.name.name, self.ty(&p.ty))
                }
            })
            .collect();
        let (owner, own_params) = match &f.receiver {
            Some(r) => {
                (format!("{}{}.", r.ty.name, type_params(&r.type_params)), &f.type_params[r.type_params.len()..])
            }
            None => (String::new(), &f.type_params[..]),
        };
        let mut head = format!("fn {owner}{}{}({})", f.name.name, type_params(own_params), params.join(", "));
        if let Some(r) = &f.ret {
            head.push_str(&format!(" -> {}", self.ret_ty(r)));
        }
        head.push_str(&uses(&f.uses));
        if f.noalloc.is_some() {
            head.push_str(" noalloc");
        }
        head
    }

    /// `head {`, the statements, `}` + `tail`; an empty block without comments is `head {}`.
    /// An empty head puts `{` on a line of its own (after contracts).
    fn block(&mut self, head: &str, b: &Block, tail: &str) {
        let open = b.span.start + 1;
        let close = b.span.end - 1;
        let lead = if head.is_empty() { String::new() } else { format!("{head} ") };
        if b.stmts.is_empty() && !self.has_comments(open, close) {
            self.line(&format!("{lead}{{}}{tail}"));
            return;
        }
        self.line(&format!("{lead}{{"));
        self.depth += 1;
        self.entries(&b.stmts, open, close, |_, _| false, |p, s| p.stmt(s));
        self.depth -= 1;
        self.line(&format!("}}{tail}"));
    }

    // ---- statements ----

    fn stmt(&mut self, s: &Stmt) {
        if let Some(t) = self.simple(s) {
            match self.wrapped(s) {
                // A function literal's body already has its lines: the statement is not split.
                Some((head, items, close)) if self.depth * INDENT.len() + t.len() > WIDTH && !t.contains('\n') => {
                    self.line(&head);
                    self.depth += 1;
                    for i in items {
                        self.line(&format!("{i},"));
                    }
                    self.depth -= 1;
                    self.line(&close);
                }
                _ => self.line(&t),
            }
            return;
        }
        match &s.kind {
            StmtKind::If { .. } => self.if_chain(s, ""),
            StmtKind::While { cond, body } => {
                let head = format!("while {}", self.expr(cond));
                self.block(&head, body, "");
            }
            StmtKind::For { var, start, end, body } => {
                let head = format!("for {} in {}..{}", var.name, self.expr(start), self.expr(end));
                self.block(&head, body, "");
            }
            StmtKind::ForEach { var, iter, body } => {
                let head = format!("for {} in {}", var.name, self.expr(iter));
                self.block(&head, body, "");
            }
            StmtKind::Match { scrutinee, arms, span_end } => {
                let head = format!("match {}", self.expr(scrutinee));
                let open = self.after_brace(scrutinee.span.end);
                let close = span_end.start;
                if arms.is_empty() && !self.has_comments(open, close) {
                    self.line(&format!("{head} {{}}"));
                    return;
                }
                self.line(&format!("{head} {{"));
                self.depth += 1;
                self.entries(arms, open, close, |_, _| false, |p, a| p.arm(a));
                self.depth -= 1;
                self.line("}");
            }
            _ => unreachable!("simple statements are handled above"),
        }
    }

    /// A statement that is one line and contains no block, or None.
    fn simple(&self, s: &Stmt) -> Option<String> {
        Some(match &s.kind {
            StmtKind::Let { mutable, name, ty, init, .. } => {
                let kw = if *mutable { "var" } else { "let" };
                format!("{kw} {}: {} = {};", name.name, self.ty(ty), self.expr(init))
            }
            StmtKind::Assign { target, value } => format!("{} = {};", self.expr(target), self.expr(value)),
            StmtKind::Expr(e) => format!("{};", self.expr(e)),
            StmtKind::Return(None) => "return;".into(),
            StmtKind::Return(Some(e)) => format!("return {};", self.expr(e)),
            StmtKind::Break => "break;".into(),
            StmtKind::Continue => "continue;".into(),
            _ => return None,
        })
    }

    /// A simple statement whose value is a list of two or more (array, call arguments, struct fields), split into
    /// the line that opens the list, one line per element and the line that closes it. Used when
    /// the statement is longer than WIDTH; any other long statement stays on one line.
    fn wrapped(&self, s: &Stmt) -> Option<(String, Vec<String>, String)> {
        let (prefix, value) = match &s.kind {
            StmtKind::Let { mutable, name, ty, init, .. } => {
                let kw = if *mutable { "var" } else { "let" };
                (format!("{kw} {}: {} = ", name.name, self.ty(ty)), init)
            }
            StmtKind::Assign { target, value } => (format!("{} = ", self.expr(target)), value),
            StmtKind::Return(Some(e)) => ("return ".to_string(), e),
            StmtKind::Expr(e) => (String::new(), e),
            _ => return None,
        };
        let (value, tail) = match &value.kind {
            ExprKind::Try(inner) => (&**inner, "?;"),
            _ => (value, ";"),
        };
        let (open, items, close) = match &value.kind {
            ExprKind::Array(items) if items.len() > 1 => {
                ("[".to_string(), items.iter().map(|i| self.expr(i)).collect(), "]")
            }
            ExprKind::Call { callee, args } if args.len() > 1 => {
                (format!("{}(", callee.name), self.arg_list(args), ")")
            }
            ExprKind::Method { receiver, method, args } if args.len() > 1 => {
                (format!("{}.{}(", self.expr(receiver), method.name), self.arg_list(args), ")")
            }
            ExprKind::MapLit(entries) if entries.len() > 1 => ("[".to_string(), self.entry_list(entries), "]"),
            ExprKind::StructLit { name, fields } if fields.len() > 1 => {
                let fs = fields.iter().map(|(f, v)| format!("{}: {}", f.name, self.expr(v))).collect();
                (format!("{} {{", name.name), fs, "}")
            }
            _ => return None,
        };
        Some((format!("{prefix}{open}"), items, format!("{close}{tail}")))
    }

    /// `if c { } else if c { } else { }`, starting with `prefix` (`} else ` for later links).
    fn if_chain(&mut self, s: &Stmt, prefix: &str) {
        let StmtKind::If { cond, then, else_ } = &s.kind else { unreachable!() };
        let head = format!("{prefix}if {}", self.expr(cond));
        match else_.as_deref() {
            None => self.block(&head, then, ""),
            Some(e) => {
                self.block_open(&head, then);
                match e {
                    Else::Block(b) => self.block("} else", b, ""),
                    Else::If(next) => self.if_chain(next, "} else "),
                }
            }
        }
    }

    /// Like `block`, but leaves the closing `}` to the `else` that follows.
    fn block_open(&mut self, head: &str, b: &Block) {
        let open = b.span.start + 1;
        let close = b.span.end - 1;
        self.line(&format!("{head} {{"));
        self.depth += 1;
        self.entries(&b.stmts, open, close, |_, _| false, |p, s| p.stmt(s));
        self.depth -= 1;
    }

    fn arm(&mut self, a: &Arm) {
        let mut pat = a.variant.name.clone();
        if let Some(fields) = &a.fields {
            let fs: Vec<String> = fields
                .iter()
                .map(|f| match &f.binding {
                    Some(b) => format!("{}: {}", f.field.name, b.name),
                    None => f.field.name.clone(),
                })
                .collect();
            pat = format!("{pat}({})", fs.join(", "));
        }
        let head = format!("{pat} =>");
        let inner = Span::new(a.body.span.start + 1, a.body.span.end - 1);
        if let ([only], false) = (a.body.stmts.as_slice(), self.has_comments(inner.start, inner.end)) {
            if let Some(t) = self.simple(only) {
                let one = format!("{head} {{ {t} }}");
                if self.depth * INDENT.len() + one.len() <= WIDTH && !one.contains('\n') {
                    self.line(&one);
                    return;
                }
            }
        }
        self.block(&head, &a.body, "");
    }

    // ---- expressions and types ----

    fn expr(&self, e: &Expr) -> String {
        match &e.kind {
            ExprKind::Int(_) | ExprKind::Str(_) | ExprKind::Interp(_) => self.slice(e.span).to_string(),
            ExprKind::Float(_, None) => self.slice(e.span).to_string(),
            ExprKind::Float(_, Some(_)) => self.squash(e.span),
            ExprKind::Bool(b) => b.to_string(),
            ExprKind::Name(n) => n.name.clone(),
            ExprKind::Result => "result".into(),
            ExprKind::Field(base, f) => format!("{}.{}", self.expr(base), f.name),
            ExprKind::Call { callee, args } => format!("{}({})", callee.name, self.args(args)),
            ExprKind::Method { receiver, method, args } => {
                format!("{}.{}({})", self.expr(receiver), method.name, self.args(args))
            }
            ExprKind::Index(base, i) => format!("{}[{}]", self.expr(base), self.expr(i)),
            ExprKind::Slice(base, from, to) => format!("{}[{}..{}]", self.expr(base), self.expr(from), self.expr(to)),
            ExprKind::MapLit(entries) if entries.is_empty() => "[:]".into(),
            ExprKind::MapLit(entries) => format!("[{}]", self.entry_list(entries).join(", ")),
            ExprKind::Array(items) => {
                let items: Vec<String> = items.iter().map(|i| self.expr(i)).collect();
                format!("[{}]", items.join(", "))
            }
            ExprKind::Try(inner) => format!("{}?", self.expr(inner)),
            ExprKind::StructLit { name, fields } if fields.is_empty() => format!("{} {{}}", name.name),
            ExprKind::StructLit { name, fields } => {
                let fs: Vec<String> = fields.iter().map(|(f, v)| format!("{}: {}", f.name, self.expr(v))).collect();
                format!("{} {{ {} }}", name.name, fs.join(", "))
            }
            ExprKind::Unary(UnOp::Neg, x) => format!("-{}", self.expr(x)),
            ExprKind::Unary(UnOp::Not, x) => format!("!{}", self.expr(x)),
            ExprKind::Unary(UnOp::BitNot, x) => format!("~{}", self.expr(x)),
            ExprKind::Binary(op, l, r) => format!("{} {} {}", self.expr(l), op.text(), self.expr(r)),
            ExprKind::Paren(x) => format!("({})", self.expr(x)),
            ExprKind::Lambda(f) => self.lambda(f),
        }
    }

    /// `fn(x: i64) -> i64 {`, the body's lines one level deeper than the line it starts on, and
    /// `}` at that line's level: the text goes on the current line as one expression.
    fn lambda(&self, f: &FnDecl) -> String {
        let params: Vec<String> = f.params.iter().map(|p| format!("{}: {}", p.name.name, self.ty(&p.ty))).collect();
        let mut head = format!("fn({})", params.join(", "));
        if let Some(r) = &f.ret {
            head.push_str(&format!(" -> {}", self.ret_ty(r)));
        }
        head.push_str(&uses(&f.uses));
        if f.noalloc.is_some() {
            head.push_str(" noalloc");
        }
        let (open, close) = (f.body.span.start + 1, f.body.span.end - 1);
        if f.body.stmts.is_empty() && !self.has_comments(open, close) {
            return format!("{head} {{}}");
        }
        let mut inner = Printer {
            text: self.text,
            comments: self.comments.clone(),
            used: RefCell::new(self.used.take()),
            out: String::new(),
            depth: self.depth + 1,
        };
        inner.entries(&f.body.stmts, open, close, |_, _| false, |p, s| p.stmt(s));
        self.used.replace(inner.used.into_inner());
        format!("{head} {{\n{}{}}}", inner.out, INDENT.repeat(self.depth))
    }

    fn args(&self, args: &[Arg]) -> String {
        self.arg_list(args).join(", ")
    }

    fn entry_list(&self, entries: &[(Expr, Expr)]) -> Vec<String> {
        entries.iter().map(|(k, v)| format!("{}: {}", self.expr(k), self.expr(v))).collect()
    }

    fn arg_list(&self, args: &[Arg]) -> Vec<String> {
        args.iter()
            .map(|a| match &a.name {
                Some(n) => format!("{}: {}", n.name, self.expr(&a.value)),
                None => self.expr(&a.value),
            })
            .collect()
    }

    /// A return type: a function type there is in parentheses, so `uses` after it is the outer
    /// function's.
    fn ret_ty(&self, t: &TypeExpr) -> String {
        match &t.kind {
            TypeKind::Fn { .. } => format!("({})", self.ty(t)),
            _ => self.ty(t),
        }
    }

    fn ty(&self, t: &TypeExpr) -> String {
        match &t.kind {
            TypeKind::Fn { params, ret, uses: effects, noalloc } => {
                let ps: Vec<String> = params.iter().map(|p| self.ty(p)).collect();
                let mut s = format!("fn({})", ps.join(", "));
                if let Some(r) = ret {
                    s.push_str(&format!(" -> {}", self.ret_ty(r)));
                }
                s.push_str(&uses(effects));
                if noalloc.is_some() {
                    s.push_str(" noalloc");
                }
                s
            }
            TypeKind::Array(inner) => format!("[{}]", self.ty(inner)),
            TypeKind::Named { name, args, .. } if !args.is_empty() => {
                let a: Vec<String> = args.iter().map(|a| self.ty(a)).collect();
                format!("{}[{}]", name.name, a.join(", "))
            }
            TypeKind::Named { unit: Some(_), .. } => self.squash(t.span),
            TypeKind::Named { name, .. } => name.name.clone(),
        }
    }

    fn field_list(&self, fields: &[(Ident, TypeExpr)]) -> String {
        let f: Vec<String> = fields.iter().map(|(n, t)| format!("{}: {}", n.name, self.ty(t))).collect();
        f.join(", ")
    }

    // ---- source text ----

    fn slice(&self, s: Span) -> &str {
        &self.text[s.start..s.end]
    }

    /// The source of `s` without spaces: units and unit literals (`9.81[m/s^2]`).
    fn squash(&self, s: Span) -> String {
        self.slice(s).chars().filter(|c| !c.is_whitespace()).collect()
    }

    /// The offset just after the first `{` at or after `from`.
    fn after_brace(&self, from: usize) -> usize {
        let mut i = from;
        let bytes = self.text.as_bytes();
        while i < bytes.len() && bytes[i] != b'{' {
            i += 1;
        }
        (i + 1).min(bytes.len())
    }

    fn has_comments(&self, from: usize, to: usize) -> bool {
        self.comments.iter().any(|c| c.start >= from && c.start < to)
    }
}

/// `[T: ordered + text, U]`, or nothing.
fn type_params(params: &[TypeParamDecl]) -> String {
    if params.is_empty() {
        return String::new();
    }
    let ps: Vec<String> = params
        .iter()
        .map(|p| {
            let cs: Vec<&str> = p.constraints.iter().map(|c| c.name.as_str()).collect();
            if cs.is_empty() {
                p.name.name.clone()
            } else {
                format!("{}: {}", p.name.name, cs.join(" + "))
            }
        })
        .collect();
    format!("[{}]", ps.join(", "))
}

fn vis(public: bool) -> &'static str {
    if public {
        "pub "
    } else {
        ""
    }
}

/// ` uses io`, or nothing.
fn uses(names: &[Ident]) -> String {
    if names.is_empty() {
        return String::new();
    }
    // Effects in one order (io, fs, clock, random); names that are not effects keep theirs, last.
    let mut n: Vec<&str> = names.iter().map(|u| u.name.as_str()).collect();
    n.sort_by_key(|u| crate::check::EFFECTS.iter().position(|e| e == u).unwrap_or(crate::check::EFFECTS.len()));
    format!(" uses {}", n.join(", "))
}

struct Field(Ident, TypeExpr);

impl Entry for Field {
    fn span(&self) -> Span {
        self.0.span.to(self.1.span)
    }
}

impl Entry for Item {
    fn span(&self) -> Span {
        match self {
            Item::Import(i) => i.span,
            Item::Unit(u) => u.span,
            Item::Struct(s) => s.span,
            Item::Enum(e) => e.span,
            Item::Fn(f) => match f.allows.first() {
                Some(a) => a.span.to(f.span),
                None => f.span,
            },
            Item::Interface(i) => i.span,
            Item::Test(t) => t.span,
        }
    }
}

impl Entry for FnDecl {
    fn span(&self) -> Span {
        self.span
    }
}

impl Entry for Stmt {
    fn span(&self) -> Span {
        self.span
    }
}

impl Entry for Arm {
    fn span(&self) -> Span {
        self.span
    }
}

impl Entry for VariantDecl {
    fn span(&self) -> Span {
        self.span
    }
}
