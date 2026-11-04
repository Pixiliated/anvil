//! Recursive-descent parser. After a syntax error it skips to the next item keyword (`fn`,
//! `struct`, `enum`, `unit`, `test`, `allow`) and goes on, so one run reports the first error of
//! every item. Those keywords never appear inside an item, so the restart point is never a guess;
//! within an item the first error is the only one reported, since what follows it is.

use crate::ast::*;
use crate::diag::{Diagnostic, FixKind};
use crate::lexer::{LexPart, Tok, Token};
use crate::span::Span;

type PResult<T> = Result<T, Diagnostic>;

pub struct Parser {
    toks: Vec<Token>,
    pos: usize,
}

/// The items that parsed, and the first syntax error of each item that did not.
pub fn parse(toks: Vec<Token>) -> (File, Vec<Diagnostic>) {
    let mut p = Parser { toks, pos: 0 };
    let mut items = Vec::new();
    let mut errors = Vec::new();
    while p.peek() != &Tok::Eof {
        let start = p.pos;
        if p.peek() == &Tok::Import && items.iter().any(|i| !matches!(i, Item::Import(_))) {
            errors.push(Diagnostic::new("E0100", p.span(), "imports come before every other item"));
        }
        match p.item() {
            Ok(item) => items.push(item),
            Err(d) => {
                errors.push(d);
                p.pos = p.pos.max(start + 1);
                // `fn(` is a function literal or type inside the broken item, not the next item.
                while !matches!(
                    p.peek(),
                    Tok::Eof
                        | Tok::Struct
                        | Tok::Enum
                        | Tok::Unit
                        | Tok::Test
                        | Tok::Allow
                        | Tok::Import
                        | Tok::Pub
                        | Tok::Extern
                        | Tok::Interface
                ) && !(p.peek() == &Tok::Fn && p.peek_at(1) != &Tok::LParen)
                {
                    p.bump();
                }
            }
        }
    }
    (File { items }, errors)
}

/// One expression and nothing after it: a doc comment's example.
pub(crate) fn parse_expr(toks: Vec<Token>) -> PResult<Expr> {
    let mut p = Parser { toks, pos: 0 };
    let e = p.expr()?;
    if p.peek() != &Tok::Eof {
        return Err(Diagnostic::new("E0100", p.span(), "an example is one expression, and this comes after it"));
    }
    Ok(e)
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.pos].tok
    }
    fn peek_at(&self, n: usize) -> &Tok {
        &self.toks[(self.pos + n).min(self.toks.len() - 1)].tok
    }
    fn span(&self) -> Span {
        self.toks[self.pos].span
    }
    fn prev_span(&self) -> Span {
        self.toks[self.pos.saturating_sub(1)].span
    }
    fn bump(&mut self) -> Token {
        let t = self.toks[self.pos].clone();
        if t.tok != Tok::Eof {
            self.pos += 1;
        }
        t
    }
    fn eat(&mut self, tok: &Tok) -> bool {
        if self.peek() == tok {
            self.bump();
            true
        } else {
            false
        }
    }

    fn unexpected(&self, expected: &str) -> Diagnostic {
        Diagnostic::new("E0100", self.span(), format!("expected {expected}, found {}", self.peek().describe()))
    }

    fn expect(&mut self, tok: Tok) -> PResult<Span> {
        if self.peek() == &tok {
            Ok(self.bump().span)
        } else {
            let mut d = self.unexpected(&format!("`{}`", tok.text()));
            if tok == Tok::Semi {
                d = d.fix1(FixKind::Mechanical, "add `;`", self.prev_span().at_end(), ";");
            }
            Err(d)
        }
    }

    fn ident(&mut self, what: &str) -> PResult<Ident> {
        match self.peek().clone() {
            Tok::Ident(name) => {
                let span = self.bump().span;
                Ok(Ident { name, span })
            }
            Tok::Result => Err(self.unexpected(what).note("`result` is reserved for ensures clauses", None)),
            _ => Err(self.unexpected(what)),
        }
    }

    fn item(&mut self) -> PResult<Item> {
        match self.peek() {
            Tok::Import => self.import_decl().map(Item::Import),
            Tok::Pub => {
                let start = self.bump().span;
                let mut item = match self.peek() {
                    Tok::Unit => self.unit_decl().map(Item::Unit)?,
                    Tok::Struct => self.struct_decl().map(Item::Struct)?,
                    Tok::Enum => self.enum_decl().map(Item::Enum)?,
                    Tok::Fn => self.fn_decl(Vec::new(), false).map(|f| Item::Fn(Box::new(f)))?,
                    Tok::Extern => self.extern_decl(Vec::new()).map(|f| Item::Fn(Box::new(f)))?,
                    Tok::Interface => self.interface_decl().map(Item::Interface)?,
                    _ => {
                        return Err(
                            self.unexpected("`fn`, `extern`, `struct`, `enum`, `interface` or `unit` after `pub`")
                        )
                    }
                };
                match &mut item {
                    Item::Interface(d) => (d.public, d.span) = (true, start.to(d.span)),
                    Item::Unit(d) => (d.public, d.span) = (true, start.to(d.span)),
                    Item::Struct(d) => (d.public, d.span) = (true, start.to(d.span)),
                    Item::Enum(d) => (d.public, d.span) = (true, start.to(d.span)),
                    Item::Fn(d) => (d.public, d.span) = (true, start.to(d.span)),
                    _ => unreachable!(),
                }
                Ok(item)
            }
            Tok::Unit => self.unit_decl().map(Item::Unit),
            Tok::Struct => self.struct_decl().map(Item::Struct),
            Tok::Enum => self.enum_decl().map(Item::Enum),
            Tok::Fn => self.fn_decl(Vec::new(), false).map(|f| Item::Fn(Box::new(f))),
            Tok::Extern => self.extern_decl(Vec::new()).map(|f| Item::Fn(Box::new(f))),
            Tok::Interface => self.interface_decl().map(Item::Interface),
            Tok::Allow => {
                let mut allows = Vec::new();
                while self.peek() == &Tok::Allow {
                    allows.push(self.allow_decl()?);
                }
                let public = if self.peek() == &Tok::Pub { Some(self.bump().span) } else { None };
                let mut f = match self.peek() {
                    Tok::Fn => self.fn_decl(allows, false)?,
                    Tok::Extern => self.extern_decl(allows)?,
                    _ => {
                        return Err(
                            self.unexpected("`fn` or another `allow` (an allow applies to the function after it)")
                        )
                    }
                };
                if let Some(p) = public {
                    f.public = true;
                    f.span = p.to(f.span);
                }
                Ok(Item::Fn(Box::new(f)))
            }
            Tok::Test => self.test_decl().map(Item::Test),
            _ => Err(self.unexpected(
                "`fn`, `extern`, `struct`, `enum`, `interface`, `unit`, `test`, `allow`, `import` or `pub`",
            )),
        }
    }

    fn import_decl(&mut self) -> PResult<ImportDecl> {
        let start = self.bump().span;
        let mut path = vec![self.ident("a module name")?];
        while self.eat(&Tok::Dot) {
            if self.peek() == &Tok::LBrace {
                break;
            }
            path.push(self.ident("a module name")?);
        }
        if self.peek() != &Tok::LBrace {
            return Err(self
                .unexpected("`.{` and the names to import")
                .note("imports list their names: import geometry.{area, Shape};", None));
        }
        self.bump();
        let mut names = Vec::new();
        while self.peek() != &Tok::RBrace {
            names.push(self.ident("a name to import")?);
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RBrace)?;
        if names.is_empty() {
            return Err(Diagnostic::new("E0100", self.prev_span(), "an import names at least one item"));
        }
        let end = self.expect(Tok::Semi)?;
        Ok(ImportDecl { path, names, span: start.to(end) })
    }

    fn unit_decl(&mut self) -> PResult<UnitDecl> {
        let start = self.bump().span;
        let name = self.ident("unit name")?;
        let def = if self.eat(&Tok::Assign) { Some(self.unit_expr()?) } else { None };
        let end = self.expect(Tok::Semi)?;
        Ok(UnitDecl { public: false, name, def, span: start.to(end) })
    }

    fn unit_expr(&mut self) -> PResult<UnitExpr> {
        let start = self.span();
        let mut terms = Vec::new();
        let mut sign = 1;
        let mut first = true;
        loop {
            if first && self.peek() == &Tok::Int(1) && self.peek_at(1) == &Tok::Slash {
                // `1/s`
                self.bump();
            } else {
                let name = self.ident("unit name")?;
                let mut exp = 1;
                if self.eat(&Tok::Caret) {
                    let neg = self.eat(&Tok::Minus);
                    match self.peek().clone() {
                        Tok::Int(v) if v > 0 && v <= 64 => {
                            self.bump();
                            exp = if neg { -(v as i32) } else { v as i32 };
                        }
                        _ => return Err(self.unexpected("an exponent between 1 and 64")),
                    }
                }
                terms.push((name, exp * sign));
            }
            first = false;
            if self.eat(&Tok::Star) {
                sign = 1;
            } else if self.eat(&Tok::Slash) {
                sign = -1;
            } else {
                break;
            }
        }
        Ok(UnitExpr { terms, span: start.to(self.prev_span()) })
    }

    fn type_expr(&mut self) -> PResult<TypeExpr> {
        self.type_of(true)
    }

    /// The type after `->`. A function type there takes no `uses` or `noalloc`: those belong to
    /// the function being declared, unless the type is in parentheses.
    fn ret_type(&mut self) -> PResult<TypeExpr> {
        self.type_of(false)
    }

    fn type_of(&mut self, effects: bool) -> PResult<TypeExpr> {
        if self.peek() == &Tok::LParen {
            let start = self.bump().span;
            let inner = self.type_of(true)?;
            let end = self.expect(Tok::RParen)?;
            return Ok(TypeExpr { span: start.to(end), ..inner });
        }
        if self.peek() == &Tok::Fn {
            return self.fn_type(effects);
        }
        if self.peek() == &Tok::LBracket {
            let start = self.bump().span;
            let elem = self.type_expr()?;
            let end = self.expect(Tok::RBracket)?;
            return Ok(TypeExpr { kind: TypeKind::Array(Box::new(elem)), span: start.to(end) });
        }
        let name = self.ident("a type")?;
        let mut span = name.span;
        let mut unit = None;
        let mut args = Vec::new();
        if self.eat(&Tok::LBracket) {
            // Brackets after f64 hold a unit; after any other name, type arguments.
            if name.name == "f64" {
                unit = Some(self.unit_expr()?);
            } else {
                loop {
                    args.push(self.type_expr()?);
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
            }
            span = span.to(self.expect(Tok::RBracket)?);
        }
        Ok(TypeExpr { kind: TypeKind::Named { name, unit, args }, span })
    }

    /// `fn(T, U) -> R uses io noalloc`.
    fn fn_type(&mut self, effects: bool) -> PResult<TypeExpr> {
        let start = self.bump().span;
        self.expect(Tok::LParen)?;
        let mut params = Vec::new();
        while self.peek() != &Tok::RParen {
            params.push(self.type_of(true)?);
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RParen)?;
        let ret = if self.eat(&Tok::Arrow) { Some(Box::new(self.ret_type()?)) } else { None };
        let (mut uses, mut noalloc) = (Vec::new(), None);
        if effects {
            uses = self.type_uses()?;
            if self.peek() == &Tok::Noalloc {
                noalloc = Some(self.bump().span);
            }
        }
        Ok(TypeExpr { kind: TypeKind::Fn { params, ret, uses, noalloc }, span: start.to(self.prev_span()) })
    }

    /// `uses io, fs` inside a type, where a comma may also end the type (`f: fn() uses io, x: i64`):
    /// after a comma, only an effect name continues the list.
    fn type_uses(&mut self) -> PResult<Vec<Ident>> {
        if !self.eat(&Tok::Uses) {
            return Ok(Vec::new());
        }
        let mut uses = vec![self.ident("an effect name")?];
        while self.peek() == &Tok::Comma
            && matches!(self.peek_at(1), Tok::Ident(n) if crate::check::EFFECTS.contains(&n.as_str()))
        {
            self.bump();
            uses.push(self.ident("an effect name")?);
        }
        Ok(uses)
    }

    /// `[T: ordered + text, U]` after a declared name, or nothing.
    fn type_params(&mut self) -> PResult<Vec<TypeParamDecl>> {
        let mut out = Vec::new();
        if !self.eat(&Tok::LBracket) {
            return Ok(out);
        }
        while self.peek() != &Tok::RBracket {
            let name = self.ident("a type parameter name")?;
            let mut constraints = Vec::new();
            if self.eat(&Tok::Colon) {
                constraints.push(self.ident("a constraint: ordered, text or key")?);
                while self.eat(&Tok::Plus) {
                    constraints.push(self.ident("a constraint: ordered, text or key")?);
                }
            }
            out.push(TypeParamDecl { name, constraints });
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RBracket)?;
        if out.is_empty() {
            return Err(Diagnostic::new("E0100", self.prev_span(), "`[]` declares no type parameters"));
        }
        Ok(out)
    }

    /// `interface Name { fn m(self, ...) -> R uses ...; ... }`
    fn interface_decl(&mut self) -> PResult<InterfaceDecl> {
        let start = self.bump().span;
        let name = self.ident("interface name")?;
        self.expect(Tok::LBrace)?;
        let mut methods = Vec::new();
        while self.peek() != &Tok::RBrace {
            if self.peek() != &Tok::Fn {
                return Err(self.unexpected("`fn` and a method signature ending in `;`, or `}`"));
            }
            let fstart = self.bump().span;
            let mname = self.ident("method name")?;
            if self.peek() == &Tok::LBracket {
                return Err(self.unexpected("`(`").note("interface methods are not generic", None));
            }
            let self_ty = TypeExpr {
                kind: TypeKind::Named {
                    name: Ident { name: "Self".into(), span: mname.span },
                    unit: None,
                    args: Vec::new(),
                },
                span: mname.span,
            };
            let params = self.params(Some(self_ty))?;
            let ret = if self.eat(&Tok::Arrow) { Some(self.ret_type()?) } else { None };
            let uses_insert = self.prev_span().at_end();
            let (uses, uses_span) = self.uses_clause()?;
            let noalloc = if self.peek() == &Tok::Noalloc { Some(self.bump().span) } else { None };
            let semi = self.expect(Tok::Semi)?;
            methods.push(FnDecl {
                public: false,
                name: mname,
                type_params: Vec::new(),
                params,
                ret,
                uses,
                uses_span,
                uses_insert,
                noalloc,
                external: None,
                requires: Vec::new(),
                ensures: Vec::new(),
                body: Block { stmts: Vec::new(), span: semi },
                span: fstart.to(semi),
                allows: Vec::new(),
                receiver: None,
            });
        }
        let end = self.expect(Tok::RBrace)?;
        Ok(InterfaceDecl { public: false, name, methods, span: start.to(end) })
    }

    /// `(a: T, b: U)`. With `self_ty`, a method's: the first parameter is `self`, of that type.
    fn params(&mut self, self_ty: Option<TypeExpr>) -> PResult<Vec<Param>> {
        self.expect(Tok::LParen)?;
        let mut params = Vec::new();
        if let Some(ty) = self_ty {
            if self.peek() != &Tok::SelfValue {
                return Err(self.unexpected("`self`: a method's first parameter is `self`"));
            }
            let span = self.bump().span;
            params.push(Param { name: Ident { name: "self".into(), span }, ty: TypeExpr { span, ..ty } });
            if !self.eat(&Tok::Comma) {
                self.expect(Tok::RParen)?;
                return Ok(params);
            }
        } else if self.peek() == &Tok::SelfValue {
            return Err(self
                .unexpected("a parameter name")
                .note("`self` is the first parameter of a method: fn Type.name(self)", None));
        }
        while self.peek() != &Tok::RParen {
            let pname = self.ident("parameter name")?;
            self.expect(Tok::Colon)?;
            let ty = self.type_expr()?;
            params.push(Param { name: pname, ty });
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RParen)?;
        Ok(params)
    }

    fn struct_decl(&mut self) -> PResult<StructDecl> {
        let start = self.bump().span;
        let name = self.ident("struct name")?;
        let type_params = self.type_params()?;
        self.expect(Tok::LBrace)?;
        let mut fields = Vec::new();
        while self.peek() != &Tok::RBrace {
            let f = self.ident("field name")?;
            self.expect(Tok::Colon)?;
            let t = self.type_expr()?;
            fields.push((f, t));
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        let end = self.expect(Tok::RBrace)?;
        Ok(StructDecl { public: false, name, type_params, fields, span: start.to(end) })
    }

    fn enum_decl(&mut self) -> PResult<EnumDecl> {
        let start = self.bump().span;
        let name = self.ident("enum name")?;
        let type_params = self.type_params()?;
        self.expect(Tok::LBrace)?;
        let mut variants = Vec::new();
        while self.peek() != &Tok::RBrace {
            let vname = self.ident("variant name")?;
            let mut fields = Vec::new();
            if self.eat(&Tok::LParen) {
                while self.peek() != &Tok::RParen {
                    let f = self.ident("field name")?;
                    if self.peek() != &Tok::Colon {
                        return Err(self
                            .unexpected("`:` and a type")
                            .note("variant fields are named: Circle(radius: f64[m])", None));
                    }
                    self.bump();
                    fields.push((f, self.type_expr()?));
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                self.expect(Tok::RParen)?;
            }
            variants.push(VariantDecl { span: vname.span.to(self.prev_span()), name: vname, fields });
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        if variants.is_empty() {
            return Err(self.unexpected("a variant name").note("an enum needs at least one variant", None));
        }
        let end = self.expect(Tok::RBrace)?;
        Ok(EnumDecl { public: false, name, type_params, variants, span: start.to(end) })
    }

    fn uses_clause(&mut self) -> PResult<(Vec<Ident>, Option<Span>)> {
        if !self.eat(&Tok::Uses) {
            return Ok((Vec::new(), None));
        }
        let start = self.prev_span();
        let mut uses = vec![self.ident("an effect name")?];
        while self.eat(&Tok::Comma) {
            uses.push(self.ident("an effect name")?);
        }
        Ok((uses, Some(start.to(self.prev_span()))))
    }

    fn allow_decl(&mut self) -> PResult<AllowDecl> {
        let start = self.bump().span;
        let lint = self.ident("a lint name, such as `untested`")?;
        let (reason, reason_span) = match self.peek().clone() {
            Tok::Str(s) => (s, self.bump().span),
            _ => return Err(self.unexpected("the reason in quotes: allow untested \"why\"")),
        };
        Ok(AllowDecl { lint, reason, reason_span, span: start.to(reason_span) })
    }

    /// `extern ["file.c"] fn name(params) -> Ret uses ... noalloc;`: a C function, no body.
    fn extern_decl(&mut self, allows: Vec<AllowDecl>) -> PResult<FnDecl> {
        let start = self.bump().span;
        let source = match self.peek().clone() {
            Tok::Str(s) => Some((s, self.bump().span)),
            _ => None,
        };
        if self.peek() != &Tok::Fn {
            return Err(self.unexpected("`fn`, or the C file in quotes: extern \"lib.c\" fn name(...)"));
        }
        let mut f = self.fn_decl(allows, true)?;
        f.span = start.to(f.span);
        f.external = Some(Extern { source, span: start });
        Ok(f)
    }

    /// `external`: an extern's signature, which ends in `;` instead of a body.
    fn fn_decl(&mut self, allows: Vec<AllowDecl>, external: bool) -> PResult<FnDecl> {
        let start = self.bump().span;
        let mut name = self.ident("function name")?;
        let mut type_params = self.type_params()?;
        // `fn Circle.area(self)`, `fn Stack[T].top(self)`: a method of that type.
        let mut receiver = None;
        let mut self_ty = None;
        if self.eat(&Tok::Dot) {
            let args = type_params
                .iter()
                .map(|p: &TypeParamDecl| TypeExpr {
                    kind: TypeKind::Named { name: p.name.clone(), unit: None, args: Vec::new() },
                    span: p.name.span,
                })
                .collect();
            self_ty =
                Some(TypeExpr { kind: TypeKind::Named { name: name.clone(), unit: None, args }, span: name.span });
            receiver = Some(Receiver { ty: name, type_params: type_params.clone() });
            name = self.ident("method name")?;
            type_params.extend(self.type_params()?);
        }
        let params = self.params(self_ty)?;
        let ret = if self.eat(&Tok::Arrow) { Some(self.ret_type()?) } else { None };
        let uses_insert = self.prev_span().at_end();
        let (uses, uses_span) = self.uses_clause()?;
        let noalloc = if self.peek() == &Tok::Noalloc { Some(self.bump().span) } else { None };
        if external {
            if matches!(self.peek(), Tok::Requires | Tok::Ensures) {
                return Err(self.unexpected("`;`").note(
                    "an extern function cannot have contracts: wrap it in an Anvil function that checks them",
                    None,
                ));
            }
            let semi = self.expect(Tok::Semi)?;
            return Ok(FnDecl {
                public: false,
                name,
                type_params,
                params,
                ret,
                uses,
                uses_span,
                uses_insert,
                noalloc,
                external: None,
                requires: Vec::new(),
                ensures: Vec::new(),
                body: Block { stmts: Vec::new(), span: semi },
                span: start.to(semi),
                allows,
                receiver,
            });
        }
        let mut requires = Vec::new();
        let mut ensures = Vec::new();
        loop {
            match self.peek() {
                Tok::Requires => {
                    if !ensures.is_empty() {
                        return Err(Diagnostic::new(
                            "E0100",
                            self.span(),
                            "requires clauses come before ensures clauses",
                        ));
                    }
                    self.bump();
                    requires.push(self.expr_no_struct()?);
                }
                Tok::Ensures => {
                    self.bump();
                    ensures.push(self.expr_no_struct()?);
                }
                _ => break,
            }
        }
        let body = self.block()?;
        let span = start.to(body.span);
        Ok(FnDecl {
            public: false,
            name,
            type_params,
            params,
            ret,
            uses,
            uses_span,
            uses_insert,
            noalloc,
            external: None,
            requires,
            ensures,
            body,
            span,
            allows,
            receiver,
        })
    }

    fn test_decl(&mut self) -> PResult<TestDecl> {
        let start = self.bump().span;
        let (name, name_span) = match self.peek().clone() {
            Tok::Str(s) => (s, self.bump().span),
            _ => return Err(self.unexpected("a test name in quotes")),
        };
        let mut inputs = Vec::new();
        if self.eat(&Tok::For) {
            match self.peek().clone() {
                Tok::Ident(w) if w == "all" => {
                    self.bump();
                }
                _ => {
                    return Err(self
                        .unexpected("`all`")
                        .note("a property test names its inputs: test \"name\" for all n: i64 { ... }", None))
                }
            }
            loop {
                let pname = self.ident("an input name")?;
                self.expect(Tok::Colon)?;
                let ty = self.type_expr()?;
                inputs.push(Param { name: pname, ty });
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
        }
        let uses_insert = self.prev_span().at_end();
        let (uses, uses_span) = self.uses_clause()?;
        let body = self.block()?;
        let span = start.to(body.span);
        Ok(TestDecl { name, name_span, inputs, uses, uses_span, uses_insert, body, span })
    }

    fn block(&mut self) -> PResult<Block> {
        let start = self.expect(Tok::LBrace)?;
        let mut stmts = Vec::new();
        while self.peek() != &Tok::RBrace {
            if self.peek() == &Tok::Eof {
                return Err(self.unexpected("`}`"));
            }
            stmts.push(self.stmt()?);
        }
        let end = self.expect(Tok::RBrace)?;
        Ok(Block { stmts, span: start.to(end) })
    }

    fn stmt(&mut self) -> PResult<Stmt> {
        let start = self.span();
        let kind = match self.peek() {
            Tok::Let | Tok::Var => {
                let mutable = self.peek() == &Tok::Var;
                let kw_span = self.bump().span;
                let name = self.ident("variable name")?;
                if self.peek() != &Tok::Colon {
                    return Err(self
                        .unexpected("`:` and a type")
                        .note("every binding states its type: let speed: f64[m/s] = 3.0[m/s];", None));
                }
                self.bump();
                let ty = self.type_expr()?;
                self.expect(Tok::Assign)?;
                let init = self.expr()?;
                self.expect(Tok::Semi)?;
                StmtKind::Let { mutable, kw_span, name, ty, init }
            }
            Tok::If => return self.if_stmt(),
            Tok::While => {
                self.bump();
                let cond = self.expr_no_struct()?;
                let body = self.block()?;
                StmtKind::While { cond, body }
            }
            Tok::For => {
                self.bump();
                let var = self.ident("loop variable")?;
                self.expect(Tok::In)?;
                let s = self.add_no_struct()?;
                if self.eat(&Tok::DotDot) {
                    let e = self.add_no_struct()?;
                    let body = self.block()?;
                    StmtKind::For { var, start: s, end: e, body }
                } else {
                    let body = self.block()?;
                    StmtKind::ForEach { var, iter: s, body }
                }
            }
            Tok::Match => {
                self.bump();
                let scrutinee = self.expr_no_struct()?;
                self.expect(Tok::LBrace)?;
                let mut arms = Vec::new();
                while self.peek() != &Tok::RBrace {
                    if self.peek() == &Tok::Eof {
                        return Err(self.unexpected("`}`"));
                    }
                    arms.push(self.arm()?);
                }
                let span_end = self.expect(Tok::RBrace)?;
                StmtKind::Match { scrutinee, arms, span_end }
            }
            Tok::Return => {
                self.bump();
                let v = if self.peek() == &Tok::Semi { None } else { Some(self.expr()?) };
                self.expect(Tok::Semi)?;
                StmtKind::Return(v)
            }
            Tok::Break => {
                self.bump();
                self.expect(Tok::Semi)?;
                StmtKind::Break
            }
            Tok::Continue => {
                self.bump();
                self.expect(Tok::Semi)?;
                StmtKind::Continue
            }
            _ => {
                let e = self.expr()?;
                if self.eat(&Tok::Assign) {
                    let value = self.expr()?;
                    self.expect(Tok::Semi)?;
                    StmtKind::Assign { target: e, value }
                } else {
                    self.expect(Tok::Semi)?;
                    StmtKind::Expr(e)
                }
            }
        };
        Ok(Stmt { kind, span: start.to(self.prev_span()) })
    }

    fn arm(&mut self) -> PResult<Arm> {
        let variant = self.ident("a variant name")?;
        let mut fields = None;
        let mut fields_span = variant.span.at_end();
        if self.peek() == &Tok::LParen {
            let open = self.bump().span;
            let mut fs = Vec::new();
            while self.peek() != &Tok::RParen {
                let field = self.ident("a field name")?;
                let binding = if self.eat(&Tok::Colon) { Some(self.ident("a name or _")?) } else { None };
                let span = field.span.to(self.prev_span());
                fs.push(PatField { field, binding, span });
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            fields_span = open.to(self.expect(Tok::RParen)?);
            fields = Some(fs);
        }
        if self.peek() != &Tok::FatArrow {
            return Err(self.unexpected("`=>`").note("match arms look like: Circle(radius) => { ... }", None));
        }
        self.bump();
        let body = self.block()?;
        let span = variant.span.to(body.span);
        Ok(Arm { variant, fields, fields_span, body, span })
    }

    fn if_stmt(&mut self) -> PResult<Stmt> {
        let start = self.bump().span;
        let cond = self.expr_no_struct()?;
        let then = self.block()?;
        let else_ = if self.eat(&Tok::Else) {
            if self.peek() == &Tok::If {
                Some(Box::new(Else::If(Box::new(self.if_stmt()?))))
            } else {
                Some(Box::new(Else::Block(self.block()?)))
            }
        } else {
            None
        };
        Ok(Stmt { kind: StmtKind::If { cond, then, else_ }, span: start.to(self.prev_span()) })
    }

    // ---- expressions ----

    pub fn expr(&mut self) -> PResult<Expr> {
        self.or(true)
    }
    fn expr_no_struct(&mut self) -> PResult<Expr> {
        self.or(false)
    }

    fn bin(op: BinOp, l: Expr, r: Expr) -> Expr {
        let span = l.span.to(r.span);
        Expr { kind: ExprKind::Binary(op, Box::new(l), Box::new(r)), span }
    }

    fn or(&mut self, s: bool) -> PResult<Expr> {
        let mut l = self.and(s)?;
        while self.eat(&Tok::OrOr) {
            let r = self.and(s)?;
            l = Self::bin(BinOp::Or, l, r);
        }
        Ok(l)
    }

    fn and(&mut self, s: bool) -> PResult<Expr> {
        let mut l = self.cmp(s)?;
        while self.eat(&Tok::AndAnd) {
            let r = self.cmp(s)?;
            l = Self::bin(BinOp::And, l, r);
        }
        Ok(l)
    }

    fn cmp_op(&self) -> Option<BinOp> {
        Some(match self.peek() {
            Tok::EqEq => BinOp::Eq,
            Tok::NotEq => BinOp::Ne,
            Tok::Lt => BinOp::Lt,
            Tok::Le => BinOp::Le,
            Tok::Gt => BinOp::Gt,
            Tok::Ge => BinOp::Ge,
            _ => return None,
        })
    }

    fn cmp(&mut self, s: bool) -> PResult<Expr> {
        let l = self.bit_or(s)?;
        if let Some(op) = self.cmp_op() {
            self.bump();
            let r = self.bit_or(s)?;
            if self.cmp_op().is_some() {
                return Err(Diagnostic::new("E0391", l.span.to(self.span()), "comparisons cannot be chained"));
            }
            return Ok(Self::bin(op, l, r));
        }
        Ok(l)
    }

    // Bit operators bind as in C and Rust: shifts tighter than `&`, then `^`, then `|`, all tighter
    // than comparisons (so `x & mask == 0` compares the masked value).
    fn bit_or(&mut self, s: bool) -> PResult<Expr> {
        let mut l = self.bit_xor(s)?;
        while self.eat(&Tok::Pipe) {
            let r = self.bit_xor(s)?;
            l = Self::bin(BinOp::BitOr, l, r);
        }
        Ok(l)
    }

    fn bit_xor(&mut self, s: bool) -> PResult<Expr> {
        let mut l = self.bit_and(s)?;
        while self.eat(&Tok::Caret) {
            let r = self.bit_and(s)?;
            l = Self::bin(BinOp::Caret, l, r);
        }
        Ok(l)
    }

    fn bit_and(&mut self, s: bool) -> PResult<Expr> {
        let mut l = self.shift(s)?;
        while self.eat(&Tok::Amp) {
            let r = self.shift(s)?;
            l = Self::bin(BinOp::BitAnd, l, r);
        }
        Ok(l)
    }

    fn shift(&mut self, s: bool) -> PResult<Expr> {
        let mut l = self.add_s(s)?;
        loop {
            let op = match self.peek() {
                Tok::Shl => BinOp::Shl,
                Tok::Shr => BinOp::Shr,
                _ => break,
            };
            self.bump();
            let r = self.add_s(s)?;
            l = Self::bin(op, l, r);
        }
        Ok(l)
    }

    fn add_no_struct(&mut self) -> PResult<Expr> {
        self.add_s(false)
    }

    fn add_s(&mut self, s: bool) -> PResult<Expr> {
        let mut l = self.mul(s)?;
        loop {
            let op = match self.peek() {
                Tok::Plus => BinOp::Add,
                Tok::Minus => BinOp::Sub,
                _ => break,
            };
            self.bump();
            let r = self.mul(s)?;
            l = Self::bin(op, l, r);
        }
        Ok(l)
    }

    fn mul(&mut self, s: bool) -> PResult<Expr> {
        let mut l = self.unary(s)?;
        loop {
            let op = match self.peek() {
                Tok::Star => BinOp::Mul,
                Tok::Slash => BinOp::Div,
                Tok::Percent => BinOp::Rem,
                _ => break,
            };
            self.bump();
            let r = self.unary(s)?;
            l = Self::bin(op, l, r);
        }
        Ok(l)
    }

    fn unary(&mut self, s: bool) -> PResult<Expr> {
        let op = match self.peek() {
            Tok::Minus => UnOp::Neg,
            Tok::Bang => UnOp::Not,
            Tok::Tilde => UnOp::BitNot,
            _ => return self.postfix(s),
        };
        let start = self.bump().span;
        let e = self.unary(s)?;
        let span = start.to(e.span);
        Ok(Expr { kind: ExprKind::Unary(op, Box::new(e)), span })
    }

    fn postfix(&mut self, s: bool) -> PResult<Expr> {
        let mut e = self.primary(s)?;
        loop {
            if self.eat(&Tok::Dot) {
                let f = self.ident("field or method name")?;
                if self.eat(&Tok::LParen) {
                    let args = self.args()?;
                    let span = e.span.to(self.prev_span());
                    e = Expr { kind: ExprKind::Method { receiver: Box::new(e), method: f, args }, span };
                } else {
                    let span = e.span.to(f.span);
                    e = Expr { kind: ExprKind::Field(Box::new(e), f), span };
                }
            } else if self.eat(&Tok::LBracket) {
                let i = self.expr()?;
                if self.eat(&Tok::DotDot) {
                    let to = self.expr()?;
                    let end = self.expect(Tok::RBracket)?;
                    let span = e.span.to(end);
                    e = Expr { kind: ExprKind::Slice(Box::new(e), Box::new(i), Box::new(to)), span };
                    continue;
                }
                let end = self.expect(Tok::RBracket)?;
                let span = e.span.to(end);
                e = Expr { kind: ExprKind::Index(Box::new(e), Box::new(i)), span };
            } else if self.peek() == &Tok::Question {
                let end = self.bump().span;
                let span = e.span.to(end);
                e = Expr { kind: ExprKind::Try(Box::new(e)), span };
            } else {
                return Ok(e);
            }
        }
    }

    /// `fn(x: i64) -> i64 uses io noalloc { body }`, as a declaration with an empty name.
    fn lambda(&mut self) -> PResult<FnDecl> {
        let start = self.bump().span;
        self.expect(Tok::LParen)?;
        let mut params = Vec::new();
        while self.peek() != &Tok::RParen {
            let pname = self.ident("parameter name: a function literal names and types its parameters")?;
            self.expect(Tok::Colon)?;
            let ty = self.type_expr()?;
            params.push(Param { name: pname, ty });
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RParen)?;
        let ret = if self.eat(&Tok::Arrow) { Some(self.ret_type()?) } else { None };
        let uses_insert = self.prev_span().at_end();
        let (uses, uses_span) = self.uses_clause()?;
        let noalloc = if self.peek() == &Tok::Noalloc { Some(self.bump().span) } else { None };
        if matches!(self.peek(), Tok::Requires | Tok::Ensures) {
            return Err(self.unexpected("`{`").note(
                "a function literal has no contracts: check its arguments in its body, or name the function",
                None,
            ));
        }
        let body = self.block()?;
        Ok(FnDecl {
            public: false,
            name: Ident { name: String::new(), span: start },
            type_params: Vec::new(),
            params,
            ret,
            uses,
            uses_span,
            uses_insert,
            noalloc,
            external: None,
            requires: Vec::new(),
            ensures: Vec::new(),
            span: start.to(body.span),
            body,
            allows: Vec::new(),
            receiver: None,
        })
    }

    /// Call arguments after `(`, through `)`.
    fn args(&mut self) -> PResult<Vec<Arg>> {
        let mut args = Vec::new();
        while self.peek() != &Tok::RParen {
            let arg_name = if matches!(self.peek(), Tok::Ident(_)) && self.peek_at(1) == &Tok::Colon {
                let n = self.ident("argument name")?;
                self.bump();
                Some(n)
            } else {
                None
            };
            let value = self.expr()?;
            args.push(Arg { name: arg_name, value });
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RParen)?;
        Ok(args)
    }

    fn primary(&mut self, s: bool) -> PResult<Expr> {
        let start = self.span();
        let kind = match self.peek().clone() {
            Tok::Int(v) => {
                self.bump();
                ExprKind::Int(v)
            }
            Tok::Float(v) => {
                self.bump();
                let unit = if self.eat(&Tok::LBracket) {
                    let u = self.unit_expr()?;
                    self.expect(Tok::RBracket)?;
                    Some(u)
                } else {
                    None
                };
                ExprKind::Float(v, unit)
            }
            Tok::True => {
                self.bump();
                ExprKind::Bool(true)
            }
            Tok::False => {
                self.bump();
                ExprKind::Bool(false)
            }
            Tok::Str(v) => {
                self.bump();
                ExprKind::Str(v)
            }
            Tok::Interp(parts) => {
                self.bump();
                let mut out = Vec::new();
                for part in parts {
                    out.push(match part {
                        LexPart::Lit(text) => InterpPart::Lit(text),
                        LexPart::Code(toks) => {
                            let mut sub = Parser { toks, pos: 0 };
                            let e = sub.expr()?;
                            if sub.peek() != &Tok::Eof {
                                return Err(sub.unexpected("`}` to end the interpolation"));
                            }
                            InterpPart::Expr(e)
                        }
                    });
                }
                ExprKind::Interp(out)
            }
            Tok::LBracket => {
                self.bump();
                if self.peek() == &Tok::Colon && self.peek_at(1) == &Tok::RBracket {
                    self.bump();
                    self.bump();
                    return Ok(Expr { kind: ExprKind::MapLit(Vec::new()), span: start.to(self.prev_span()) });
                }
                let mut elems = Vec::new();
                let mut entries = Vec::new();
                while self.peek() != &Tok::RBracket {
                    let x = self.expr()?;
                    // The first element decides: `[k: v, ...]` is a map, `[x, ...]` an array.
                    if !entries.is_empty() || (elems.is_empty() && self.peek() == &Tok::Colon) {
                        self.expect(Tok::Colon)?;
                        entries.push((x, self.expr()?));
                    } else {
                        elems.push(x);
                    }
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                self.expect(Tok::RBracket)?;
                if entries.is_empty() {
                    ExprKind::Array(elems)
                } else {
                    ExprKind::MapLit(entries)
                }
            }
            Tok::Result => {
                self.bump();
                ExprKind::Result
            }
            Tok::SelfValue => {
                let span = self.bump().span;
                ExprKind::Name(Ident { name: "self".into(), span })
            }
            Tok::Fn => ExprKind::Lambda(Box::new(self.lambda()?)),
            Tok::LParen => {
                self.bump();
                let e = self.expr()?;
                self.expect(Tok::RParen)?;
                ExprKind::Paren(Box::new(e))
            }
            Tok::Ident(_) => {
                let name = self.ident("a name")?;
                if self.eat(&Tok::LParen) {
                    let args = self.args()?;
                    ExprKind::Call { callee: name, args }
                } else if s && self.peek() == &Tok::LBrace {
                    self.bump();
                    let mut fields = Vec::new();
                    while self.peek() != &Tok::RBrace {
                        let f = self.ident("field name")?;
                        self.expect(Tok::Colon)?;
                        let v = self.expr()?;
                        fields.push((f, v));
                        if !self.eat(&Tok::Comma) {
                            break;
                        }
                    }
                    self.expect(Tok::RBrace)?;
                    ExprKind::StructLit { name, fields }
                } else {
                    ExprKind::Name(name)
                }
            }
            _ => return Err(self.unexpected("an expression")),
        };
        Ok(Expr { kind, span: start.to(self.prev_span()) })
    }
}
